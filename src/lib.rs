use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, Read, Write},
    path::PathBuf,
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex,
    },
    thread,
    time::{Duration, Instant},
};

mod runit;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    Dinit,
    Runit,
}

impl Backend {
    /// Prefer the system manager on runit systems without a per-user dinit instance (turnstile).
    /// A detected user dinit instance means the user's service scope is the useful default.
    pub fn default_scope(system: Self, user: Self) -> Scope {
        if system == Self::Runit && user == Self::Runit {
            Scope::System
        } else {
            Scope::User
        }
    }

    /// Detects the system-scope backend (PID 1) and, independently, the backend serving the
    /// user's own instance. These can differ: e.g. a runit PID 1 with a per-user dinit instance
    /// started by turnstile. `--dinit`/`--runit` force both scopes to the same value.
    pub fn detect() -> Result<(Self, Self), String> {
        if let Some(forced) = Self::forced_by_arg()? {
            return Ok((forced, forced));
        }
        let system = Self::detect_pid1()?;
        // Already known positive; skip the probe.
        let user = resolve_user_backend(system, system != Self::Dinit && user_dinit_responds());
        Ok((system, user))
    }

    fn forced_by_arg() -> Result<Option<Self>, String> {
        match std::env::args().nth(1).as_deref() {
            Some("--dinit") => Ok(Some(Self::Dinit)),
            Some("--runit") => Ok(Some(Self::Runit)),
            Some(arg) => Err(format!("Unknown option: {arg} (use --dinit or --runit)")),
            None => Ok(None),
        }
    }

    fn detect_pid1() -> Result<Self, String> {
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
        backend_for_comm(&name)
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

    /// Refreshes runit system service status through the privileged helper. Runit's supervise
    /// sockets are root-owned on Void, so an unprivileged `sv status` only reports access
    /// denied. The GUI calls this only from its explicit Refresh button; startup discovery stays
    /// unprivileged and can still show service definitions immediately.
    pub fn list_privileged(self, scope: Scope) -> Result<Vec<Service>, String> {
        if self != Self::Runit || scope != Scope::System {
            return self.list(scope);
        }
        let names = privileged_request(self, "list", &[])?;
        names
            .lines()
            .filter(|name| !name.is_empty())
            .map(|name| {
                runit::validate("status", Some(name))?;
                Ok(match privileged_request(self, "status", &[name]) {
                    Ok(text) => runit::service_from_status(name.to_owned(), &text),
                    Err(error) => Service {
                        name: name.to_owned(),
                        state: "Unknown".into(),
                        summary: format!("Status unavailable: {error}"),
                    },
                })
            })
            .collect()
    }

    /// Uses the current helper session when one is already authorized, but never starts a new
    /// authentication flow. This keeps post-action and selected-service refreshes accurate
    /// after the user explicitly authenticates with the toolbar Refresh button.
    pub fn list_with_session(self, scope: Scope) -> Result<Vec<Service>, String> {
        if self == Self::Runit && scope == Scope::System && helper_session_is_active() {
            self.list_privileged(scope)
        } else {
            self.list(scope)
        }
    }

    /// Refreshes a single service's live state — for the GUI to update just the row the user
    /// selected, instead of the whole list. Runit has a real per-service primitive (`sv
    /// status`) and uses that directly. Dinit's control protocol doesn't: `status`'s free-text
    /// report doesn't carry `list`'s richer Failed/Skipped/compound-transition states (see
    /// `parse_list`'s doc comment), so a from-scratch parser would risk showing a different
    /// state here than a full refresh would — this still goes through `list`/`discover` to stay
    /// byte-for-byte consistent, it just discards every entry but the one asked for.
    pub fn service(self, scope: Scope, name: &str) -> Result<Service, String> {
        match self {
            Self::Dinit => self
                .list(scope)?
                .into_iter()
                .find(|s| s.name == name)
                .ok_or_else(|| format!("{name}: no longer listed")),
            Self::Runit if scope == Scope::System && helper_session_is_active() => {
                let text = privileged_request(self, "status", &[name])?;
                Ok(runit::service_from_status(name.to_owned(), &text))
            }
            Self::Runit => runit::service(scope, name),
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
                let args: Vec<&str> = service.into_iter().collect();
                // Status reuses an already authorized helper session but never starts one.
                if scope == Scope::System && (action != "status" || helper_session_is_active()) {
                    privileged_request(self, action, &args)
                } else {
                    // Listing and status are read-only and must never trigger authentication.
                    // If the live supervisor denies access, show that status as unavailable;
                    // the user can still inspect every discovered service.
                    runit::request(scope, action, service)
                }
            }
        }
    }

    /// Runs `action` as root, for the privileged helper process itself. `args` is already
    /// whitespace-tokenized off the wire. This re-validates independently of the GUI-side
    /// wrappers (`signal`/`add_dep`/`rm_dep`) rather than trusting their caller: the helper is
    /// the actual privilege boundary, so it must not assume a well-behaved client sent it.
    pub fn execute_system(self, action: &str, args: &[&str]) -> Result<String, String> {
        match self {
            Self::Dinit => match action {
                "signal" => {
                    if args.len() != 2 || !valid_token(args[0]) || !valid_token(args[1]) {
                        return Err("Invalid signal request".into());
                    }
                    output(command_multi(Scope::System, action, args))
                }
                "add-dep" | "rm-dep" => {
                    if args.len() != 3
                        || !DEP_TYPES.contains(&args[0])
                        || !valid_token(args[1])
                        || !valid_token(args[2])
                    {
                        return Err("Invalid dependency request".into());
                    }
                    output(command_multi(Scope::System, action, args))
                }
                // Never exposed for the system instance: it would let an authenticated Pilot
                // session poison the loader/interpreter environment (LD_PRELOAD and similar)
                // for every subsequently-started system service, not just manage one service.
                // Refused here independent of `setenv`/`unsetenv`'s own scope check below, in
                // case some other bug or future caller ever routes it here anyway.
                "setenv" | "unsetenv" => Err(format!(
                    "{action} is not available for the system dinit instance"
                )),
                _ => command(Scope::System, action, args.first().copied()).and_then(output),
            },
            Self::Runit => {
                if action == "list" && args.is_empty() {
                    return runit::list_names(Scope::System).map(|names| names.join("\n"));
                }
                if args.len() > 1 {
                    return Err(format!("{action} is not supported for runit"));
                }
                runit::request(Scope::System, action, args.first().copied())
            }
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

    /// Where `name`'s definition actually lives, for either scope. `None` if it isn't found
    /// (dinit) or the runit service directory doesn't exist (runit) — shown to the user, not
    /// relied on for a safety check the way `user_definition_path` is in the delete flow.
    pub fn definition_path(self, scope: Scope, name: &str) -> Option<PathBuf> {
        match self {
            Self::Dinit => resolve_definition_path(scope, name),
            Self::Runit => runit::definition_path(scope, name),
        }
    }

    /// Runit-only: creates or removes the `down` file in a user-scope service's directory (see
    /// `runit::set_down`). A no-op for dinit, which has no equivalent concept — auto-start is
    /// already controlled entirely by `enable-via`/`boot` wiring.
    pub fn set_down(self, name: &str, down: bool) -> Result<(), String> {
        match self {
            Self::Dinit => Ok(()),
            Self::Runit => runit::set_down(name, down),
        }
    }
}

/// Maps a PID 1 `comm`/`exe` basename to a backend. Separate aliases per backend because the
/// binary name varies by distro packaging: Void's `dinit-void-core` runs PID 1 as `dinit-init`,
/// not plain `dinit`; dinit's own upstream default is `dinit`. Likewise `runit-init` alongside
/// plain `runit`.
fn backend_for_comm(name: &str) -> Result<Backend, String> {
    match name {
        "dinit" | "dinit-init" => Ok(Backend::Dinit),
        "runit" | "runit-init" => Ok(Backend::Runit),
        other => Err(format!(
            "Cannot detect dinit or runit from PID 1 ({other}); pass --dinit or --runit"
        )),
    }
}

/// Picks the user-scope backend: dinit if the system backend already is, or if a live per-user
/// dinit instance was found independently of PID 1 (e.g. started by turnstile); otherwise it
/// matches the system backend, preserving today's behavior on a single-init box.
fn resolve_user_backend(system: Backend, user_dinit_found: bool) -> Backend {
    if system == Backend::Dinit || user_dinit_found {
        Backend::Dinit
    } else {
        system
    }
}

/// Probes for a live per-user dinit instance, independent of PID 1's own backend.
fn user_dinit_responds() -> bool {
    Command::new("/usr/bin/dinitctl")
        .arg("--user")
        .arg("list")
        .env_remove("DINIT_SOCKET_PATH")
        .env_remove("DINIT_CS_FD")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
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
    if !valid_token(name) {
        return Err("Invalid service name".into());
    }
    Ok(())
}

/// A service/dependency-type/signal name's safety rule: the leading-dash CLI-option-injection
/// guard, no path separator, and no embedded whitespace or control characters. The whitespace
/// rule also keeps the privileged helper's plain whitespace-joined wire protocol lossless —
/// every token on that wire is one of these, never a value that could itself contain a space.
fn valid_token(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 255
        && !s.starts_with('-')
        && !s.contains('/')
        && !s.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// A `setenv` token (`NAME` or `NAME=value`): same whitespace/control/leading-dash rule as
/// `valid_token` (for the wire-protocol reason above, this applies to the whole token, value
/// included — a value with a literal space can't be set through Pilot), but the value half may
/// contain `/` (e.g. `PATH=/usr/bin`) and isn't itself length-limited.
fn valid_env_assignment(token: &str) -> bool {
    if token.is_empty()
        || token.starts_with('-')
        || token.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return false;
    }
    let name = token.split('=').next().unwrap_or(token);
    !name.is_empty() && name.len() <= 255 && !name.contains('/')
}

pub const DEP_TYPES: &[&str] = &["need", "milestone", "waits-for"];

/// Shared dinitctl invocation setup for actions with more than one positional argument
/// (`signal`, `add-dep`/`rm-dep`, `setenv`/`unsetenv`). These don't fit `command()`'s single
/// optional-service shape, and none of them need its `--no-wait` handling.
fn command_multi(scope: Scope, action: &str, args: &[&str]) -> Command {
    let mut cmd = Command::new("/usr/bin/dinitctl");
    cmd.arg(if scope == Scope::System {
        "--system"
    } else {
        "--user"
    });
    cmd.env_remove("DINIT_SOCKET_PATH")
        .env_remove("DINIT_CS_FD")
        .env("LC_ALL", "C");
    cmd.arg(action);
    for arg in args {
        cmd.arg(arg);
    }
    cmd
}

/// Routes a dinit action to the right place: direct for user scope, otherwise straight to the
/// privileged helper (unlike `request()`, these are all mutating actions, never worth trying
/// unprivileged first).
fn dispatch(scope: Scope, action: &str, args: &[&str]) -> Result<String, String> {
    if scope == Scope::User {
        return output(command_multi(scope, action, args));
    }
    privileged_request(Backend::Dinit, action, args)
}

/// Sends an arbitrary signal to a running service's process (`dinitctl signal`).
pub fn signal(scope: Scope, signal_id: &str, service: &str) -> Result<String, String> {
    if !valid_token(signal_id) {
        return Err("Invalid signal".into());
    }
    validate("status", Some(service))?;
    dispatch(scope, "signal", &[signal_id, service])
}

/// Adds an ad hoc dependency between two services, outside the persistent `enable-via`
/// mechanism (`dinitctl add-dep`).
pub fn add_dep(scope: Scope, dep_type: &str, from: &str, to: &str) -> Result<String, String> {
    if !DEP_TYPES.contains(&dep_type) {
        return Err("Invalid dependency type".into());
    }
    validate("status", Some(from))?;
    validate("status", Some(to))?;
    dispatch(scope, "add-dep", &[dep_type, from, to])
}

/// Removes an ad hoc dependency added with `add_dep` (`dinitctl rm-dep`). Can stop `to` if
/// nothing else keeps it active.
pub fn rm_dep(scope: Scope, dep_type: &str, from: &str, to: &str) -> Result<String, String> {
    if !DEP_TYPES.contains(&dep_type) {
        return Err("Invalid dependency type".into());
    }
    validate("status", Some(from))?;
    validate("status", Some(to))?;
    dispatch(scope, "rm-dep", &[dep_type, from, to])
}

/// Sets one or more activation-environment variables; affects services started or restarted
/// after the call, not ones already running (`dinitctl setenv`).
/// User scope only: the system dinit instance's activation environment is never exposed here,
/// since any name is structurally valid (`valid_env_assignment` only rejects whitespace/control
/// characters, not semantics) — an authenticated Pilot session could otherwise set `LD_PRELOAD`
/// or similar for every subsequently-started *system* service, not just manage one.
pub fn setenv(scope: Scope, assignments: &[String]) -> Result<String, String> {
    if scope == Scope::System {
        return Err(
            "Setting environment variables for the system dinit instance isn't supported".into(),
        );
    }
    if assignments.is_empty() {
        return Err("Enter at least one NAME or NAME=value".into());
    }
    if let Some(bad) = assignments.iter().find(|a| !valid_env_assignment(a)) {
        return Err(format!("Invalid environment variable: {bad}"));
    }
    let args: Vec<&str> = assignments.iter().map(String::as_str).collect();
    dispatch(scope, "setenv", &args)
}

/// Unsets one or more activation-environment variables (`dinitctl unsetenv`).
/// User scope only — see `setenv`'s doc comment; same activation environment, same restriction.
pub fn unsetenv(scope: Scope, names: &[String]) -> Result<String, String> {
    if scope == Scope::System {
        return Err(
            "Unsetting environment variables for the system dinit instance isn't supported".into(),
        );
    }
    if names.is_empty() {
        return Err("Enter at least one variable name".into());
    }
    if let Some(bad) = names.iter().find(|n| !valid_token(n)) {
        return Err(format!("Invalid variable name: {bad}"));
    }
    let args: Vec<&str> = names.iter().map(String::as_str).collect();
    dispatch(scope, "unsetenv", &args)
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
    let args: Vec<&str> = service.into_iter().collect();
    privileged_request(Backend::Dinit, action, &args)
}

/// Shuts down the kept-alive helper session, if one is running. Call on app
/// shutdown; the next privileged request spawns (and re-authenticates) a new one.
pub fn shutdown_helper() {
    if let Ok(mut guard) = HELPER.lock() {
        *guard = None;
    }
}

// The pkexec helper authenticates once and is then kept alive for the rest of
// the session (caerus pattern), so later system actions don't re-prompt — but
// only while actually used. `IDLE_TIMEOUT` (same value and reasoning as
// caerus's `backend::transaction::IDLE_TIMEOUT`) bounds how long a root
// process may sit around unused: each request pushes the deadline out, and a
// dedicated watcher thread per session (`spawn_idle_watcher`) proactively
// drops the session once it elapses, so the next privileged action
// re-authenticates instead of riding an indefinitely-lived root process.
const IDLE_TIMEOUT: Duration = Duration::from_secs(5 * 60);

struct HelperSession {
    helper: Helper,
    id: u64,
    deadline: Instant,
}

static HELPER: Mutex<Option<HelperSession>> = Mutex::new(None);
static NEXT_SESSION_ID: AtomicU64 = AtomicU64::new(0);

fn helper_session_is_active() -> bool {
    HELPER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .as_ref()
        .is_some_and(|session| Instant::now() < session.deadline)
}

/// Polls once a second for the named session to go idle past `IDLE_TIMEOUT` and drops it when it
/// does, so an unused root process doesn't linger forever. Exits without acting the moment `id`
/// is no longer the live session — either a fresher session replaced it (whose own watcher is
/// now responsible) or the session is already gone (e.g. `shutdown_helper`).
fn spawn_idle_watcher(id: u64) {
    thread::spawn(move || loop {
        thread::sleep(Duration::from_secs(1));
        let mut guard = HELPER
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match guard.as_ref() {
            Some(session) if session.id == id => {
                if Instant::now() >= session.deadline {
                    *guard = None;
                    return;
                }
            }
            _ => return,
        }
    });
}

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

    fn request(&mut self, backend: Backend, action: &str, args: &[&str]) -> Result<String, Fault> {
        writeln!(
            self.stdin,
            "{} {}",
            backend.label().to_lowercase(),
            request_line(action, args)
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

fn request_line(action: &str, args: &[&str]) -> String {
    std::iter::once(action)
        .chain(args.iter().copied())
        .collect::<Vec<_>>()
        .join(" ")
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

fn privileged_request(backend: Backend, action: &str, args: &[&str]) -> Result<String, String> {
    let mut guard = HELPER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if guard.as_ref().is_some_and(|s| Instant::now() >= s.deadline) {
        // Idle past IDLE_TIMEOUT; the watcher thread may not have gotten to it yet. Drop it here
        // too so this call re-authenticates immediately rather than waiting on the watcher's
        // own 1s poll.
        *guard = None;
    }
    if guard.is_none() {
        let id = NEXT_SESSION_ID.fetch_add(1, Ordering::Relaxed);
        *guard = Some(HelperSession {
            helper: Helper::spawn()?,
            id,
            deadline: Instant::now() + IDLE_TIMEOUT,
        });
        spawn_idle_watcher(id);
    }
    let session = guard.as_mut().unwrap();
    session.deadline = Instant::now() + IDLE_TIMEOUT;
    match session.helper.request(backend, action, args) {
        Ok(text) => Ok(text),
        Err(Fault::Command(msg)) => Err(msg),
        Err(Fault::Transport(msg)) => {
            *guard = None;
            Err(msg)
        }
    }
}

/// Simplifies `dinitctl`'s own connection-failure wording (raw subprocess stderr) into something
/// a GUI user can act on. Falls back to the raw text verbatim for anything else — a command's
/// own specific error (e.g. "service is enabled") is already clear and shouldn't be reworded.
pub fn friendly_unavailable_reason(raw: &str) -> String {
    if raw.contains("perhaps no user instance is running") {
        "no user dinit instance is running yet (start one, e.g. via turnstile, by logging out and back in)".into()
    } else if raw.contains("No such file or directory") || raw.contains("Connection refused") {
        "could not reach the dinit service manager".into()
    } else {
        raw.trim().into()
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
            let summary = match transition_note(indicator) {
                Some(note) => format!("{} ({note})", tail.trim()),
                None => tail.trim().into(),
            };
            Some(Service {
                name: name.into(),
                state: state.into(),
                summary,
            })
        })
        .collect()
}

/// `dinitctl list`'s ten-char indicator conflates two pairs of distinct states into the same
/// "Starting"/"Stopping" bucket: besides the plain transition, a service can be starting but
/// already slated to stop the moment it finishes, or stopping but slated to restart (`dinitctl
/// list`'s own legend, see `dinitctl(8)`). The outer bracket (position 0) is dropped by the
/// caller; within what remains, whichever of the transition arrow or the inner `{ }` triplet
/// comes *first* decides which of the pair this is:
/// - `<<` before `{`: starting, will stop once started.
/// - `{` before `>>`: stopping, will start once stopped.
///
/// The opposite order in each case is the plain transition, which gets no note.
fn transition_note(indicator: &str) -> Option<&'static str> {
    if indicator.len() < 9 || !indicator.is_char_boundary(1) || !indicator.is_char_boundary(9) {
        return None;
    }
    let mid = &indicator[1..9];
    let brace = mid.find('{')?;
    if let Some(arrow) = mid.find("<<") {
        return (arrow < brace).then_some("will stop once started");
    }
    if let Some(arrow) = mid.find(">>") {
        return (brace < arrow).then_some("will start once stopped");
    }
    None
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

/// One dependency relationship read from a service definition file (not from dinitctl — it has
/// no query for this, only `add-dep`/`rm-dep` to modify). `kind` matches dinitctl's own
/// vocabulary (`DEP_TYPES`): "need" for `depends-on`, "milestone" for `depends-ms`, "waits-for"
/// for `waits-for`.
#[derive(Debug, PartialEq, Eq)]
pub struct Dependency {
    pub kind: &'static str,
    pub name: String,
}

/// Dinit only: `name`'s own outgoing dependencies, read directly from its definition file.
/// Empty if the file can't be found or read.
pub fn dependencies(scope: Scope, name: &str) -> Vec<Dependency> {
    let Some(path) = resolve_definition_path(scope, name) else {
        return Vec::new();
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    let base_dir = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    parse_dependencies(&text, base_dir)
}

/// Dinit only: every other service in `scope`'s search path whose own definition depends on
/// `name`. There's no dinitctl query for this either, so every definition file in the search
/// path is parsed the same way `dependencies` parses one — a real directory scan, meant to be
/// called on demand (opening the Dependencies dialog), not on every row of the service list.
pub fn dependents(scope: Scope, name: &str) -> Vec<Dependency> {
    let mut found = Vec::new();
    for dir in service_dirs(scope) {
        let Ok(entries) = dir.read_dir() else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let Some(other_name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if other_name == name || validate("status", Some(other_name)).is_err() {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            for dep in parse_dependencies(&text, &dir) {
                if dep.name == name {
                    found.push(Dependency {
                        kind: dep.kind,
                        name: other_name.to_owned(),
                    });
                }
            }
        }
    }
    found
}

/// Parses a service description file's own dependency directives: `depends-on`/`depends-ms`/
/// `waits-for` single-name lines, and the `.d` directory variants, where each non-dot entry in
/// the named directory is one more dependency of that kind — this is also how `dinitctl enable`
/// records persistent enablement (a symlink in the target's `waits-for.d`), so an enabled-via
/// relationship shows up here too, not just a manually-authored one. `.d` directory paths are
/// relative to `base_dir` (the file's own containing directory) per `dinit-service(5)`.
fn parse_dependencies(text: &str, base_dir: &std::path::Path) -> Vec<Dependency> {
    // dinit treats ':' and '=' as interchangeable setting separators.
    fn strip_directive<'a>(line: &'a str, directive: &str) -> Option<&'a str> {
        let rest = line.strip_prefix(directive)?.trim_start();
        rest.strip_prefix(':').or_else(|| rest.strip_prefix('='))
    }
    let mut deps = Vec::new();
    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        for (directive, kind) in [
            ("depends-on", "need"),
            ("depends-ms", "milestone"),
            ("waits-for", "waits-for"),
        ] {
            if let Some(name) = strip_directive(line, directive) {
                let name = name.trim();
                if !name.is_empty() {
                    deps.push(Dependency {
                        kind,
                        name: name.to_owned(),
                    });
                }
            }
            if let Some(dir) = strip_directive(line, &format!("{directive}.d")) {
                let dir = dir.trim();
                if dir.is_empty() {
                    continue;
                }
                if let Ok(entries) = base_dir.join(dir).read_dir() {
                    for entry in entries.flatten() {
                        if let Some(name) = entry.file_name().to_str() {
                            if !name.starts_with('.') {
                                deps.push(Dependency {
                                    kind,
                                    name: name.to_owned(),
                                });
                            }
                        }
                    }
                }
            }
        }
    }
    deps
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

/// User scope only: enables buffered logging on an *existing* service by adding or replacing its
/// `log-type` setting, then `dinitctl reload`s it so an already-loaded service picks up the
/// change without a full unload/reload — `log-type` isn't among the settings `reload` documents
/// as unchangeable while running (`dinitctl(8)`: type, inittab-id/-line, pid-file,
/// runs-on-console/shares-console). System scope isn't supported: writing to a system-owned
/// definition directory needs a privileged-helper write action, which doesn't exist yet (the
/// same reason Add/Delete are user-scope only).
pub fn enable_buffered_log(name: &str) -> Result<String, String> {
    let path = resolve_definition_path(Scope::User, name)
        .ok_or("No definition file found for this service")?;
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let Some(updated) = with_buffered_log_type(&text) else {
        return Ok("Already buffered.".into());
    };
    std::fs::write(&path, updated).map_err(|e| format!("{}: {e}", path.display()))?;
    request(Scope::User, "reload", Some(name))
}

/// Rewrites `text` so its `log-type` setting is `buffer`: replaces an existing `log-type` line
/// (whichever of `:`/`=` separator it used) if its value isn't already `buffer`, otherwise
/// appends a new `log-type = buffer` line. Returns `None` if it was already `buffer` — nothing
/// to change.
fn with_buffered_log_type(text: &str) -> Option<String> {
    fn log_type_value(line: &str) -> Option<&str> {
        let rest = line.trim().strip_prefix("log-type")?.trim_start();
        rest.strip_prefix(':')
            .or_else(|| rest.strip_prefix('='))
            .map(str::trim)
    }
    let lines: Vec<&str> = text.lines().collect();
    if let Some(index) = lines.iter().position(|line| log_type_value(line).is_some()) {
        if log_type_value(lines[index]) == Some("buffer") {
            return None;
        }
        let mut owned: Vec<String> = lines.iter().map(|s| (*s).to_owned()).collect();
        owned[index] = "log-type = buffer".to_owned();
        let mut result = owned.join("\n");
        result.push('\n');
        return Some(result);
    }
    let mut result = text.to_owned();
    if !result.is_empty() && !result.ends_with('\n') {
        result.push('\n');
    }
    result.push_str("log-type = buffer\n");
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Guards the two tests below that mutate the process-wide `XDG_CONFIG_HOME` env var:
    /// `cargo test`'s default thread-per-test runner would otherwise let them interleave and
    /// stomp on each other's value mid-test (each test genuinely does need the real env var, not
    /// a fake handed to it, since that's exactly what `user_config_dir()` reads).
    static XDG_CONFIG_HOME_LOCK: Mutex<()> = Mutex::new(());

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
    fn transitional_substates_are_noted_in_summary() {
        // Exact patterns from dinitctl(8)'s list legend.
        let services = parse_list(
            "[{+}     ] started\n\
             [{ }<<   ] starting-plain\n\
             [   <<{ }] starting-will-stop\n\
             [{ }>>   ] stopping-will-start\n\
             [   >>{ }] stopping-plain\n\
             [     {-}] stopped\n",
        );
        let summaries: Vec<(&str, &str)> = services
            .iter()
            .map(|s| (s.state.as_str(), s.summary.as_str()))
            .collect();
        assert_eq!(
            summaries,
            [
                ("Started", "started"),
                ("Starting", "starting-plain"),
                ("Starting", "starting-will-stop (will stop once started)"),
                ("Stopping", "stopping-will-start (will start once stopped)"),
                ("Stopping", "stopping-plain"),
                ("Stopped", "stopped"),
            ]
        );
    }
    #[test]
    fn resolve_user_backend_prefers_dinit_when_probe_succeeds() {
        assert_eq!(resolve_user_backend(Backend::Runit, true), Backend::Dinit);
    }
    #[test]
    fn resolve_user_backend_falls_back_to_system_when_probe_fails() {
        assert_eq!(resolve_user_backend(Backend::Runit, false), Backend::Runit);
    }
    #[test]
    fn resolve_user_backend_is_dinit_when_system_already_is() {
        assert_eq!(resolve_user_backend(Backend::Dinit, false), Backend::Dinit);
    }

    #[test]
    fn default_scope_uses_system_runit_only_without_user_dinit() {
        assert_eq!(
            Backend::default_scope(Backend::Runit, Backend::Runit),
            Scope::System
        );
        assert_eq!(
            Backend::default_scope(Backend::Runit, Backend::Dinit),
            Scope::User
        );
        assert_eq!(
            Backend::default_scope(Backend::Dinit, Backend::Dinit),
            Scope::User
        );
    }
    #[test]
    fn backend_for_comm_accepts_distro_naming_variants() {
        assert_eq!(backend_for_comm("dinit"), Ok(Backend::Dinit));
        assert_eq!(backend_for_comm("dinit-init"), Ok(Backend::Dinit));
        assert_eq!(backend_for_comm("runit"), Ok(Backend::Runit));
        assert_eq!(backend_for_comm("runit-init"), Ok(Backend::Runit));
        assert!(backend_for_comm("systemd").is_err());
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
    fn command_multi_builds_expected_args_and_env() {
        let cmd = command_multi(Scope::System, "add-dep", &["need", "a", "b"]);
        assert_eq!(
            cmd.get_args().collect::<Vec<_>>(),
            ["--system", "add-dep", "need", "a", "b"]
        );
        assert!(cmd
            .get_envs()
            .any(|(k, v)| k == "DINIT_SOCKET_PATH" && v.is_none()));
    }
    #[test]
    fn signal_rejects_invalid_signal_or_service() {
        assert!(signal(Scope::User, "../etc", "sshd").is_err());
        assert!(signal(Scope::User, "TERM", "../etc").is_err());
    }
    #[test]
    fn add_dep_rejects_invalid_type_or_names() {
        assert!(add_dep(Scope::User, "bogus", "a", "b").is_err());
        assert!(add_dep(Scope::User, "need", "../a", "b").is_err());
        assert!(add_dep(Scope::User, "need", "a", "../b").is_err());
        assert!(add_dep(Scope::User, "regular", "a", "b").is_err());
    }
    #[test]
    fn rm_dep_rejects_invalid_type_or_names() {
        assert!(rm_dep(Scope::User, "bogus", "a", "b").is_err());
    }
    #[test]
    fn setenv_rejects_empty_or_invalid_assignments() {
        assert!(setenv(Scope::User, &[]).is_err());
        assert!(setenv(Scope::User, &["--evil".to_string()]).is_err());
        assert!(setenv(Scope::User, &["FOO BAR=1".to_string()]).is_err());
        assert!(setenv(Scope::User, &["FOO=hello world".to_string()]).is_err());
    }
    #[test]
    fn setenv_and_unsetenv_refuse_system_scope_even_with_otherwise_valid_input() {
        // Structurally valid per valid_env_assignment (no whitespace/control chars, name has no
        // '/'), but LD_PRELOAD for the *system* instance is exactly the loader-hijack case this
        // restriction exists to block.
        assert!(setenv(Scope::System, &["LD_PRELOAD=/tmp/evil.so".to_string()]).is_err());
        assert!(unsetenv(Scope::System, &["PATH".to_string()]).is_err());
    }
    #[test]
    fn execute_system_revalidates_signal_and_dep_args_independent_of_the_gui_wrappers() {
        // The helper is the actual privilege boundary; it must reject bad input on its own,
        // not merely assume `signal`/`add_dep`/`rm_dep` already checked it.
        assert!(Backend::Dinit
            .execute_system("signal", &["../etc", "sshd"])
            .is_err());
        assert!(Backend::Dinit.execute_system("signal", &["TERM"]).is_err());
        assert!(Backend::Dinit
            .execute_system("add-dep", &["bogus", "a", "b"])
            .is_err());
        assert!(Backend::Dinit
            .execute_system("add-dep", &["need", "a"])
            .is_err());
    }
    #[test]
    fn execute_system_refuses_setenv_and_unsetenv_for_the_system_instance() {
        assert!(Backend::Dinit
            .execute_system("setenv", &["FOO=bar"])
            .is_err());
        assert!(Backend::Dinit.execute_system("unsetenv", &["FOO"]).is_err());
    }
    #[test]
    fn unsetenv_rejects_empty_or_invalid_names() {
        assert!(unsetenv(Scope::User, &[]).is_err());
        assert!(unsetenv(Scope::User, &["../etc".to_string()]).is_err());
    }
    #[test]
    fn valid_env_assignment_accepts_paths_in_values() {
        assert!(valid_env_assignment("PATH=/usr/bin:/bin"));
        assert!(valid_env_assignment("FOO"));
        assert!(!valid_env_assignment("FOO=bar baz"));
        assert!(!valid_env_assignment("-FOO=bar"));
    }
    #[test]
    fn friendly_unavailable_reason_rewrites_known_dinitctl_errors() {
        assert_eq!(
            friendly_unavailable_reason(
                "dinitctl: connecting to socket: /run/user/1000/dinitctl: No such file or directory\ndinitctl: perhaps no user instance is running?\n"
            ),
            "no user dinit instance is running yet (start one, e.g. via turnstile, by logging out and back in)"
        );
        assert_eq!(
            friendly_unavailable_reason(
                "dinitctl: connecting to socket: /run/dinitctl: Connection refused"
            ),
            "could not reach the dinit service manager"
        );
        assert_eq!(
            friendly_unavailable_reason("dinitctl: sshd: service is currently enabled"),
            "dinitctl: sshd: service is currently enabled"
        );
    }
    #[test]
    fn request_line_appends_args_only_when_present() {
        assert_eq!(request_line("list", &[]), "list");
        assert_eq!(request_line("stop", &["sshd"]), "stop sshd");
        assert_eq!(
            request_line("add-dep", &["need", "a", "b"]),
            "add-dep need a b"
        );
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
    fn parse_dependencies_reads_single_line_directives_with_either_separator() {
        let deps = parse_dependencies(
            "type = process\ndepends-on: rcboot\ndepends-ms = udevd\nwaits-for:   network  \n# depends-on: commented-out\n",
            std::path::Path::new("/irrelevant"),
        );
        assert_eq!(
            deps,
            [
                Dependency {
                    kind: "need",
                    name: "rcboot".into()
                },
                Dependency {
                    kind: "milestone",
                    name: "udevd".into()
                },
                Dependency {
                    kind: "waits-for",
                    name: "network".into()
                },
            ]
        );
    }
    #[test]
    fn parse_dependencies_reads_d_directories_relative_to_base_dir() {
        let base = std::env::temp_dir().join(format!("pilot-deps-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let waits_for_dir = base.join("boot.d");
        std::fs::create_dir_all(&waits_for_dir).unwrap();
        std::fs::write(waits_for_dir.join("agetty-tty1"), "").unwrap();
        std::fs::write(waits_for_dir.join(".hidden"), "").unwrap();

        let deps = parse_dependencies("type = internal\nwaits-for.d: boot.d\n", &base);
        assert_eq!(
            deps,
            [Dependency {
                kind: "waits-for",
                name: "agetty-tty1".into()
            }]
        );
        std::fs::remove_dir_all(base).unwrap();
    }
    #[test]
    fn parse_dependencies_does_not_confuse_d_directive_with_single_line_form() {
        // "depends-on" is a prefix of "depends-on.d"; each must only match its own form.
        let deps = parse_dependencies("depends-on.d: missing-dir\n", std::path::Path::new("/x"));
        assert!(deps.is_empty());
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
        let _guard = XDG_CONFIG_HOME_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let dir = std::env::temp_dir().join(format!("pilot-lib-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
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
    #[test]
    fn with_buffered_log_type_replaces_an_existing_line() {
        assert_eq!(
            with_buffered_log_type("type = process\nlog-type = none\ncommand = /bin/true\n"),
            Some("type = process\nlog-type = buffer\ncommand = /bin/true\n".to_string())
        );
    }
    #[test]
    fn with_buffered_log_type_is_none_when_already_buffer() {
        assert_eq!(
            with_buffered_log_type("type = process\nlog-type: buffer\n"),
            None
        );
    }
    #[test]
    fn with_buffered_log_type_appends_when_absent() {
        assert_eq!(
            with_buffered_log_type("type = process\ncommand = /bin/true\n"),
            Some("type = process\ncommand = /bin/true\nlog-type = buffer\n".to_string())
        );
    }
    #[test]
    fn enable_buffered_log_rewrites_the_definition_file() {
        let _guard = XDG_CONFIG_HOME_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let dir = std::env::temp_dir().join(format!("pilot-buflog-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &dir) };

        let contents = render_service_file("/bin/sleep infinity", false, "boot").unwrap();
        let path = create_user_service("buflog-worker", &contents, &[]).unwrap();
        assert!(contents.contains("log-type = none"));

        // The file rewrite happens before the dinitctl reload attempt, which fails here (no
        // live user dinit instance in a test environment) — expected, and not what's tested.
        let _ = enable_buffered_log("buflog-worker");
        assert!(std::fs::read_to_string(&path)
            .unwrap()
            .contains("log-type = buffer"));

        unsafe { std::env::remove_var("XDG_CONFIG_HOME") };
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
