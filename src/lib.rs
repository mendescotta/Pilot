use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, Read, Write},
    path::PathBuf,
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::Mutex,
};

mod runit;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    Dinit,
    Runit,
}

impl Backend {
    pub fn detect() -> Result<Self, String> {
        match std::env::args().nth(1).as_deref() {
            Some("--dinit") => return Ok(Self::Dinit),
            Some("--runit") => return Ok(Self::Runit),
            Some(arg) => return Err(format!("Unknown option: {arg} (use --dinit or --runit)")),
            None => {}
        }
        let comm = std::fs::read_to_string("/proc/1/comm").map_err(|e| e.to_string())?;
        let name = if comm.trim() == "init" {
            std::fs::read_link("/proc/1/exe")
                .ok()
                .and_then(|path| {
                    path.file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                })
                .unwrap_or_else(|| comm.trim().to_owned())
        } else {
            comm.trim().to_owned()
        };
        match name.as_str() {
            "dinit" => Ok(Self::Dinit),
            "runit" | "runit-init" => Ok(Self::Runit),
            other => Err(format!(
                "Cannot detect dinit or runit from PID 1 ({other}); pass --dinit or --runit"
            )),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Dinit => "Dinit",
            Self::Runit => "Runit",
        }
    }

    pub fn list(self, scope: Scope) -> Result<Vec<Service>, String> {
        match self {
            Self::Dinit => {
                let listing = request(scope, "list", None);
                let services = discover(
                    scope,
                    listing.as_ref().map(String::as_str).map_err(String::as_str),
                );
                match listing {
                    Ok(_) => Ok(services),
                    Err(_) if !services.is_empty() => Ok(services),
                    Err(error) => Err(error),
                }
            }
            Self::Runit => runit::list(scope),
        }
    }

    pub fn request(
        self,
        scope: Scope,
        action: &str,
        service: Option<&str>,
    ) -> Result<String, String> {
        match self {
            Self::Dinit => request(scope, action, service),
            Self::Runit => {
                runit::validate(action, service)?;
                if scope == Scope::System && action != "status" {
                    privileged_request(self, action, service)
                } else {
                    runit::request(scope, action, service).or_else(|error| {
                        if scope == Scope::System
                            && action == "status"
                            && error.to_ascii_lowercase().contains("permission denied")
                        {
                            privileged_request(self, action, service)
                        } else {
                            Err(error)
                        }
                    })
                }
            }
        }
    }

    pub fn execute_system(self, action: &str, service: Option<&str>) -> Result<String, String> {
        match self {
            Self::Dinit => command(Scope::System, action, service).and_then(output),
            Self::Runit => runit::request(Scope::System, action, service),
        }
    }

    pub fn enabled(self, scope: Scope, name: &str) -> Option<String> {
        match self {
            Self::Dinit => {
                enable_target(scope, name).map(|target| format!("Enabled via: {target}"))
            }
            Self::Runit => runit::enabled(scope, name),
        }
    }

    pub fn render_user_service(
        self,
        command: &str,
        buffered: bool,
        enable_via: &str,
    ) -> Result<String, String> {
        match self {
            Self::Dinit => render_service_file(command, buffered, enable_via),
            Self::Runit => runit::render_run_file(command),
        }
    }

    pub fn create_user_service(
        self,
        name: &str,
        contents: &str,
        existing: &[Service],
    ) -> Result<PathBuf, String> {
        match self {
            Self::Dinit => create_user_service(name, contents, existing),
            Self::Runit => runit::create_user_service(name, contents, existing),
        }
    }

    pub fn delete_user_service(self, name: &str) -> Result<PathBuf, String> {
        match self {
            Self::Dinit => delete_user_service(name),
            Self::Runit => runit::delete_user_service(name),
        }
    }

    pub fn user_definition_path(self, name: &str) -> Option<PathBuf> {
        match self {
            Self::Dinit => resolve_definition_path(Scope::User, name),
            Self::Runit => runit::user_definition_path(name),
        }
    }
}

pub const ACTIONS: &[&str] = &[
    "list", "status", "catlog", "start", "stop", "restart", "enable", "disable", "reload", "unpin",
    "unload",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    User,
    System,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Service {
    pub name: String,
    pub state: String,
    pub summary: String,
}

pub fn validate(action: &str, service: Option<&str>) -> Result<(), String> {
    if !ACTIONS.contains(&action) {
        return Err("Unsupported action".into());
    }
    if action == "list" {
        return if service.is_none() {
            Ok(())
        } else {
            Err("list takes no service".into())
        };
    }
    let name = service.ok_or("Select a service")?;
    if name.is_empty()
        || name.len() > 255
        || name.starts_with('-')
        || name.contains('/')
        || name.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return Err("Invalid service name".into());
    }
    Ok(())
}

pub fn command(scope: Scope, action: &str, service: Option<&str>) -> Result<Command, String> {
    validate(action, service)?;
    let mut cmd = Command::new("/usr/bin/dinitctl");
    cmd.arg(if scope == Scope::System {
        "--system"
    } else {
        "--user"
    });
    // Never inherit a socket override: especially important inside the root helper.
    cmd.env_remove("DINIT_SOCKET_PATH")
        .env_remove("DINIT_CS_FD")
        .env("LC_ALL", "C");
    cmd.arg(action);
    if matches!(action, "start" | "stop" | "restart") {
        cmd.arg("--no-wait");
    }
    if let Some(name) = service {
        cmd.arg(name);
    }
    Ok(cmd)
}

pub fn output(mut cmd: Command) -> Result<String, String> {
    let result = cmd.output().map_err(|e| e.to_string())?;
    let stdout = String::from_utf8_lossy(&result.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&result.stderr);
    if result.status.success() {
        Ok(format!("{stdout}{stderr}"))
    } else {
        Err(format!("{stdout}{stderr}").trim().to_owned())
    }
}

pub fn request(scope: Scope, action: &str, service: Option<&str>) -> Result<String, String> {
    validate(action, service)?;
    if scope == Scope::User {
        return output(command(scope, action, service)?);
    }
    // Read directly when the socket is accessible; otherwise authenticate too.
    if matches!(action, "list" | "status" | "catlog") {
        if let Ok(text) = output(command(scope, action, service)?) {
            return Ok(text);
        }
    }
    privileged_request(Backend::Dinit, action, service)
}

/// Shuts down the kept-alive helper session, if one is running. Call on app
/// shutdown; the next privileged request spawns (and re-authenticates) a new one.
pub fn shutdown_helper() {
    if let Ok(mut guard) = HELPER.lock() {
        *guard = None;
    }
}

// The pkexec helper authenticates once and is then kept alive for the rest of
// the session (caerus pattern), so later system actions don't re-prompt.
static HELPER: Mutex<Option<Helper>> = Mutex::new(None);

enum Fault {
    /// The helper is still alive; the command itself failed.
    Command(String),
    /// The helper session is unusable; drop it so the next call respawns it.
    Transport(String),
}

struct Helper {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Helper {
    fn spawn() -> Result<Helper, String> {
        let mut child = Command::new("/usr/bin/pkexec")
            .arg("/usr/libexec/pilot-helper")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| e.to_string())?;
        let stdin = child.stdin.take().ok_or("pilot-helper: no stdin")?;
        let mut stdout = BufReader::new(child.stdout.take().ok_or("pilot-helper: no stdout")?);
        let mut greeting = String::new();
        if stdout.read_line(&mut greeting).map_err(|e| e.to_string())? == 0 {
            return Err("pilot-helper: closed before READY (authentication cancelled?)".into());
        }
        if greeting.trim_end() != "READY" {
            return Err(format!(
                "pilot-helper: unexpected greeting: {}",
                greeting.trim_end()
            ));
        }
        Ok(Helper {
            child,
            stdin,
            stdout,
        })
    }

    fn request(
        &mut self,
        backend: Backend,
        action: &str,
        service: Option<&str>,
    ) -> Result<String, Fault> {
        writeln!(
            self.stdin,
            "{} {}",
            backend.label().to_lowercase(),
            request_line(action, service)
        )
        .and_then(|()| self.stdin.flush())
        .map_err(|e| Fault::Transport(e.to_string()))?;
        let mut header = String::new();
        if self
            .stdout
            .read_line(&mut header)
            .map_err(|e| Fault::Transport(e.to_string()))?
            == 0
        {
            return Err(Fault::Transport("pilot-helper: connection closed".into()));
        }
        let (ok, len) = parse_response_header(header.trim_end()).map_err(Fault::Transport)?;
        let mut body = vec![0u8; len];
        self.stdout
            .read_exact(&mut body)
            .map_err(|e| Fault::Transport(e.to_string()))?;
        let text = String::from_utf8_lossy(&body).into_owned();
        if ok {
            Ok(text)
        } else {
            Err(Fault::Command(text))
        }
    }
}

impl Drop for Helper {
    fn drop(&mut self) {
        let _ = writeln!(self.stdin, "QUIT").and_then(|()| self.stdin.flush());
        let _ = self.child.wait();
    }
}

fn request_line(action: &str, service: Option<&str>) -> String {
    match service {
        Some(name) => format!("{action} {name}"),
        None => action.to_string(),
    }
}

fn parse_response_header(header: &str) -> Result<(bool, usize), String> {
    let (status, len) = header
        .split_once(' ')
        .ok_or_else(|| format!("pilot-helper: malformed response: {header:?}"))?;
    let len: usize = len
        .parse()
        .map_err(|_| format!("pilot-helper: malformed response length: {header:?}"))?;
    match status {
        "OK" => Ok((true, len)),
        "ERROR" => Ok((false, len)),
        _ => Err(format!(
            "pilot-helper: malformed response status: {header:?}"
        )),
    }
}

fn privileged_request(
    backend: Backend,
    action: &str,
    service: Option<&str>,
) -> Result<String, String> {
    let mut guard = HELPER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if guard.is_none() {
        *guard = Some(Helper::spawn()?);
    }
    match guard.as_mut().unwrap().request(backend, action, service) {
        Ok(text) => Ok(text),
        Err(Fault::Command(msg)) => Err(msg),
        Err(Fault::Transport(msg)) => {
            *guard = None;
            Err(msg)
        }
    }
}

pub fn parse_list(text: &str) -> Vec<Service> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            // Dinit uses a fixed ten-character status field, containing nested brackets.
            if line.len() < 10 || !line.is_char_boundary(10) {
                return None;
            }
            let (indicator, tail) = line.split_at(10);
            if !indicator.starts_with(['[', '{']) || !indicator.ends_with([']', '}']) {
                return None;
            }
            let name = tail.split_whitespace().next()?;
            validate("status", Some(name)).ok()?;
            let state = if indicator.contains("<<") {
                "Starting"
            } else if indicator.contains(">>") {
                "Stopping"
            } else if indicator.contains('X') {
                "Failed"
            } else if indicator.contains('+') {
                "Started"
            } else if indicator.contains('s') {
                "Skipped"
            } else if indicator.contains('-') {
                "Stopped"
            } else {
                "Unknown"
            };
            Some(Service {
                name: name.into(),
                state: state.into(),
                summary: tail.trim().into(),
            })
        })
        .collect()
}

/// The user's own `dinit.d` config directory ($XDG_CONFIG_HOME or
/// $HOME/.config), i.e. the only user-scope directory a plain user can
/// write to (the others `service_dirs` scans are system-wide admin paths).
pub fn user_config_dir() -> Option<PathBuf> {
    if let Some(config) = std::env::var_os("XDG_CONFIG_HOME") {
        return Some(PathBuf::from(config).join("dinit.d"));
    }
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config/dinit.d"))
}

pub fn service_dirs(scope: Scope) -> Vec<PathBuf> {
    if scope == Scope::System {
        return [
            "/etc/dinit.d",
            "/run/dinit.d",
            "/usr/local/lib/dinit.d",
            "/lib/dinit.d",
        ]
        .iter()
        .map(PathBuf::from)
        .collect();
    }
    let mut dirs = Vec::new();
    dirs.extend(user_config_dir());
    dirs.push(PathBuf::from("/etc/dinit.d/user"));
    dirs.push(PathBuf::from("/usr/lib/dinit.d/user"));
    dirs.push(PathBuf::from("/usr/local/lib/dinit.d/user"));
    dirs
}

/// The path to `name`'s definition file, wherever it actually lives among
/// `service_dirs(scope)`. `None` if it isn't found in any of them.
pub fn resolve_definition_path(scope: Scope, name: &str) -> Option<PathBuf> {
    validate("status", Some(name)).ok()?;
    service_dirs(scope)
        .into_iter()
        .map(|dir| dir.join(name))
        .find(|path| path.is_file())
}

/// `listing` is the result of a `dinitctl list`. On success every definition
/// left at the placeholder state is confirmed not loaded (list enumerates
/// every loaded service); on failure we never had a chance to confirm
/// anything, so the placeholder state is left as "Unknown" rather than
/// silently presented as if it were a real, queried status.
pub fn discover(scope: Scope, listing: Result<&str, &str>) -> Vec<Service> {
    let mut services = BTreeMap::new();
    for dir in service_dirs(scope) {
        if let Ok(entries) = dir.read_dir() {
            for entry in entries.flatten() {
                if !entry.path().is_file() {
                    continue;
                }
                if let Some(name) = entry.file_name().to_str() {
                    if validate("status", Some(name)).is_ok() {
                        services.entry(name.to_owned()).or_insert(Service {
                            name: name.into(),
                            state: "Unknown".into(),
                            summary: "No status available for this definition".into(),
                        });
                    }
                }
            }
        }
    }
    merge_listing(services, listing)
}

fn merge_listing(
    mut services: BTreeMap<String, Service>,
    listing: Result<&str, &str>,
) -> Vec<Service> {
    match listing {
        Ok(loaded) => {
            for service in parse_list(loaded) {
                services.insert(service.name.clone(), service);
            }
            for service in services.values_mut() {
                if service.state == "Unknown" {
                    service.state = "Not loaded".into();
                    service.summary = "Service definition available".into();
                }
            }
        }
        Err(error) => {
            for service in services.values_mut() {
                service.summary = format!("Status unavailable: {error}");
            }
        }
    }
    services.into_values().collect()
}

/// The service that `dinitctl enable` would add a dependency onto: the file's
/// own `@meta enable-via` directive if present, otherwise dinit's "boot"
/// default. `None` means the definition file couldn't be read at all.
pub fn enable_target(scope: Scope, name: &str) -> Option<String> {
    let text = std::fs::read_to_string(resolve_definition_path(scope, name)?).ok()?;
    Some(parse_enable_via(&text).unwrap_or_else(|| "boot".into()))
}

fn parse_enable_via(text: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let rest = line.trim().strip_prefix('@')?.trim_start();
        let rest = rest.strip_prefix("meta")?.trim_start();
        let target = rest.strip_prefix("enable-via")?.trim();
        (!target.is_empty()).then(|| target.to_owned())
    })
}

fn has_control_char(s: &str) -> bool {
    s.chars().any(char::is_control)
}

/// Builds the literal text of a minimal `type = process` service definition.
/// Rejects a control character (e.g. an embedded newline) in either field: a
/// definition file line is a trust boundary, same as a command argument.
pub fn render_service_file(
    command: &str,
    buffered_log: bool,
    enable_via: &str,
) -> Result<String, String> {
    if command.trim().is_empty() {
        return Err("Command is required".into());
    }
    if has_control_char(command) {
        return Err("Command must not contain control characters".into());
    }
    let enable_via = enable_via.trim();
    if enable_via.is_empty() {
        return Err("Enable-via target is required".into());
    }
    if has_control_char(enable_via) {
        return Err("Enable-via target must not contain control characters".into());
    }
    let log_type = if buffered_log { "buffer" } else { "none" };
    Ok(format!(
        "type = process\ncommand = {command}\nlog-type = {log_type}\n@meta enable-via {enable_via}\n"
    ))
}

/// Writes a new user-scope service definition at `~/.config/dinit.d/<name>`.
/// Fails if the name is invalid, already known (`existing`, the in-memory
/// service list — checked first so a duplicate name gives a clear error
/// before touching the filesystem), the user config dir can't be determined
/// (no $HOME/$XDG_CONFIG_HOME), or the file already exists on disk (the
/// create-new open fails rather than silently overwriting).
pub fn create_user_service(
    name: &str,
    contents: &str,
    existing: &[Service],
) -> Result<PathBuf, String> {
    validate("status", Some(name))?;
    if existing.iter().any(|s| s.name == name) {
        return Err(format!("A service named \"{name}\" already exists"));
    }
    let dir =
        user_config_dir().ok_or("No $HOME or $XDG_CONFIG_HOME to write a user service into")?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join(name);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    file.write_all(contents.as_bytes())
        .map_err(|e| e.to_string())?;
    Ok(path)
}

/// Removes a user-scope service definition. The caller is responsible for
/// having already `unload`ed it through dinit; this only touches the file.
pub fn delete_user_service(name: &str) -> Result<PathBuf, String> {
    let path = resolve_definition_path(Scope::User, name)
        .ok_or("No definition file found for this service")?;
    std::fs::remove_file(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn states_and_metadata() {
        let services = parse_list("[{+}     ] sshd (pid: 42)\n{   >>{-}} network\n{     {X}} broken\n{   <<{ }} pending\n{     {-}} stopped\n[{s}     ] skipped\n");
        assert_eq!(
            services
                .iter()
                .map(|s| s.state.as_str())
                .collect::<Vec<_>>(),
            ["Started", "Stopping", "Failed", "Starting", "Stopped", "Skipped"]
        );
        assert_eq!(services[0].name, "sshd");
    }
    #[test]
    fn reject_option_and_path_injection() {
        for name in ["", "--force", "../boot", "foo\nbar", "foo bar"] {
            assert!(validate("stop", Some(name)).is_err());
        }
        assert!(validate("shutdown", None).is_err());
        assert!(validate("list", Some("boot")).is_err());
        assert!(validate("start", Some("getty@tty1")).is_ok());
    }
    #[test]
    fn explicit_scope_and_no_wait() {
        let cmd = command(Scope::System, "restart", Some("sshd")).unwrap();
        assert_eq!(
            cmd.get_args().collect::<Vec<_>>(),
            ["--system", "restart", "--no-wait", "sshd"]
        );
        assert!(cmd
            .get_envs()
            .any(|(k, v)| k == "DINIT_SOCKET_PATH" && v.is_none()));
    }
    #[test]
    fn request_line_appends_service_only_when_present() {
        assert_eq!(request_line("list", None), "list");
        assert_eq!(request_line("stop", Some("sshd")), "stop sshd");
    }
    #[test]
    fn response_header_parses_status_and_length() {
        assert_eq!(parse_response_header("OK 0").unwrap(), (true, 0));
        assert_eq!(parse_response_header("OK 12").unwrap(), (true, 12));
        assert_eq!(parse_response_header("ERROR 5").unwrap(), (false, 5));
        assert!(parse_response_header("OK").is_err());
        assert!(parse_response_header("OK not-a-number").is_err());
        assert!(parse_response_header("MAYBE 3").is_err());
    }
    fn placeholder(name: &str) -> Service {
        Service {
            name: name.into(),
            state: "Unknown".into(),
            summary: "No status available for this definition".into(),
        }
    }
    #[test]
    fn successful_listing_confirms_unloaded_placeholders() {
        let mut services = BTreeMap::new();
        services.insert("extra".into(), placeholder("extra"));
        let result = merge_listing(services, Ok("[{+}     ] extra\n"));
        assert_eq!(result[0].state, "Started");
    }
    #[test]
    fn empty_but_successful_listing_confirms_not_loaded() {
        let mut services = BTreeMap::new();
        services.insert("extra".into(), placeholder("extra"));
        let result = merge_listing(services, Ok(""));
        assert_eq!(result[0].state, "Not loaded");
    }
    #[test]
    fn failed_listing_leaves_state_unknown_with_the_error_in_summary() {
        let mut services = BTreeMap::new();
        services.insert("extra".into(), placeholder("extra"));
        let result = merge_listing(services, Err("permission denied"));
        assert_eq!(result[0].state, "Unknown");
        assert!(result[0].summary.contains("permission denied"));
    }
    #[test]
    fn enable_via_directive_is_parsed() {
        assert_eq!(
            parse_enable_via("type = process\n@meta enable-via multi-user\n"),
            Some("multi-user".into())
        );
    }
    #[test]
    fn enable_via_absent_falls_through_to_none() {
        assert_eq!(
            parse_enable_via("type = process\ncommand = /bin/true\n"),
            None
        );
    }
    #[test]
    fn commented_enable_via_is_ignored() {
        assert_eq!(
            parse_enable_via("# @meta enable-via commented\ntype = process\n"),
            None
        );
    }
    #[test]
    fn render_service_file_builds_the_expected_template() {
        let text = render_service_file("/bin/sleep infinity", true, "boot").unwrap();
        assert_eq!(
            text,
            "type = process\ncommand = /bin/sleep infinity\nlog-type = buffer\n@meta enable-via boot\n"
        );
        let unbuffered = render_service_file("/bin/true", false, "boot").unwrap();
        assert!(unbuffered.contains("log-type = none"));
    }
    #[test]
    fn render_service_file_rejects_empty_or_unsafe_fields() {
        assert!(render_service_file("", true, "boot").is_err());
        assert!(render_service_file("   ", true, "boot").is_err());
        assert!(render_service_file("/bin/true\nEVIL", true, "boot").is_err());
        assert!(render_service_file("/bin/true", true, "").is_err());
        assert!(render_service_file("/bin/true", true, "boot\nEVIL").is_err());
    }
    #[test]
    fn create_user_service_rejects_a_name_already_known_in_memory() {
        let existing = [placeholder("worker")];
        let err = create_user_service("worker", "type = process\ncommand = /bin/true\n", &existing)
            .unwrap_err();
        assert!(err.contains("worker"));
    }
    #[test]
    fn create_user_service_rejects_an_invalid_name() {
        let err = create_user_service("../escape", "type = process\n", &[]).unwrap_err();
        assert!(!err.is_empty());
    }
    #[test]
    fn create_enable_target_and_delete_user_service_roundtrip() {
        let dir = std::env::temp_dir().join(format!("pilot-lib-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // Single test function, start to finish on one thread: no other
        // test reads or writes XDG_CONFIG_HOME, so there's no data race on
        // this process-wide env var despite cargo test's default threading.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &dir) };

        let contents = render_service_file("/bin/sleep infinity", true, "boot").unwrap();
        let path = create_user_service("roundtrip-worker", &contents, &[]).unwrap();
        assert_eq!(path, dir.join("dinit.d/roundtrip-worker"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), contents);

        assert!(create_user_service("roundtrip-worker", &contents, &[]).is_err());
        assert_eq!(
            enable_target(Scope::User, "roundtrip-worker"),
            Some("boot".into())
        );

        let removed = delete_user_service("roundtrip-worker").unwrap();
        assert_eq!(removed, path);
        assert!(!path.exists());
        assert!(delete_user_service("roundtrip-worker").is_err());

        unsafe { std::env::remove_var("XDG_CONFIG_HOME") };
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
