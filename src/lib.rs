//! System service management. Session managers belong to Runner.
use std::{
    io::{BufRead, BufReader, Read, Write},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex,
    },
    thread,
    time::{Duration, Instant},
};
mod process;
pub use process::output;
pub mod admin;
pub mod detection;
pub mod dinit;
pub mod domain;
pub mod events;
pub mod runit;
pub use detection::SystemManagerProfile;
pub use dinit::parse_list;
pub use domain::*;

pub fn valid_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 255
        && s != "."
        && s != ".."
        && !s.starts_with('-')
        && !s.contains('/')
        && !s.chars().any(|c| c.is_whitespace() || c.is_control())
}
pub fn validate_name(s: &str) -> Result<(), String> {
    if valid_name(s) {
        Ok(())
    } else {
        Err("Invalid service name".into())
    }
}
/// Fixed executable, no shell, bounded output and time.
fn passive_request(backend: Backend, action: &str, args: &[&str]) -> Result<String, String> {
    let mut guard = HELPER.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(session) = guard.as_mut().filter(|s| Instant::now() < s.deadline) {
        match session.helper.request(backend, action, args) {
            Ok(text) => {
                return Ok(text);
            }
            Err(Fault::Command(e)) => return Err(e),
            Err(Fault::Transport(e)) => {
                *guard = None;
                return Err(e);
            }
        }
    }
    Err("Unlock live status to authenticate".into())
}
impl Backend {
    pub fn change_relation(self, name: &str, relation: DinitRelation) -> Result<String, String> {
        if self != Self::Dinit {
            return Err("Runtime dependencies are a dinit capability".into());
        }
        dinit::relation_command(name, &relation)?;
        match relation {
            DinitRelation::Persistent { from, enabled } => privileged_request(
                self,
                if enabled {
                    "enable-from"
                } else {
                    "disable-from"
                },
                &[&from, name],
            ),
            DinitRelation::Runtime { target, kind, add } => privileged_request(
                self,
                if add { "add-dep" } else { "rm-dep" },
                &[kind.wire(), name, &target],
            ),
        }
    }
    pub fn event_revision(self) -> Result<Option<u64>, String> {
        if self != Self::Dinit || !helper_session_is_active() {
            return Ok(None);
        }
        Ok(passive_request(self, "revision", &[])?.parse().ok())
    }
    pub fn install_definition(
        self,
        draft: &admin::DefinitionDraft,
        replace: bool,
    ) -> Result<String, String> {
        draft.render(self)?;
        let args = draft.wire();
        let args = args.iter().map(String::as_str).collect::<Vec<_>>();
        privileged_request(self, if replace { "replace" } else { "create" }, &args)
    }
    pub fn snapshot(self, unlock: bool) -> Result<Snapshot, String> {
        match self {
            Self::Dinit => {
                let direct = output(dinit::command("list", None)?);
                let listing = direct.or_else(|e| {
                    if unlock {
                        privileged_request(self, "list", &[])
                    } else if helper_session_is_active() {
                        passive_request(self, "list", &[])
                    } else {
                        Err(e)
                    }
                });
                let access = match &listing {
                    Ok(_) => Access::Connected,
                    Err(e) if permission_error(e) => Access::AuthenticationRequired,
                    Err(e) => Access::Unavailable(e.clone()),
                };
                let services = dinit::discover(listing.as_deref().map_err(String::as_str));
                Ok(Snapshot { services, access })
            }
            Self::Runit => {
                if unlock || helper_session_is_active() {
                    let text = if unlock {
                        privileged_request(self, "list", &[])
                    } else {
                        passive_request(self, "list", &[])
                    }?;
                    Ok(Snapshot {
                        services: runit::parse_snapshot(&text),
                        access: Access::Connected,
                    })
                } else {
                    runit::snapshot()
                }
            }
        }
    }
    pub fn execute(self, action: Action, name: &str) -> Result<String, String> {
        validate_name(name)?;
        if !action.supported(self) {
            return Err("Action is not supported by this manager".into());
        }
        privileged_request(self, action.wire(), &[name])
    }
    pub fn status(self, name: &str) -> Result<String, String> {
        validate_name(name)?;
        if helper_session_is_active() {
            return passive_request(self, "status", &[name]);
        }
        match self {
            Self::Dinit => output(dinit::command("status", Some(name))?),
            Self::Runit => runit::request("status", Some(name)),
        }
    }
    pub fn logs(self, name: &str) -> Result<String, String> {
        validate_name(name)?;
        match self {
            Self::Dinit => {
                if let Some(path) = dinit::file_log_path(name) {
                    return if helper_session_is_active() {
                        passive_request(self, "logs", &[name])
                    } else {
                        admin::read_log(&path)
                    };
                }
                if helper_session_is_active() {
                    passive_request(self, "catlog", &[name])
                } else {
                    output(dinit::command("catlog", Some(name))?)
                }
            }
            Self::Runit => {
                if helper_session_is_active() {
                    passive_request(self, "logs", &[name])
                } else {
                    runit::log_path(name)
                        .ok_or_else(|| "No static svlogd destination found under /var/log".into())
                        .and_then(|p| admin::read_log(&p))
                }
            }
        }
    }
    pub fn inspect(self, name: &str) -> Result<Inspection, String> {
        validate_name(name)?;
        match self {
            Self::Dinit => dinit::inspect(name),
            Self::Runit => runit::inspect(name),
        }
    }
    /// Revalidates typed actions and exact arity at the privilege boundary.
    pub fn execute_system(self, action: &str, args: &[&str]) -> Result<String, String> {
        if self == Self::Dinit {
            match action {
                "enable-from" | "disable-from" => {
                    if args.len() != 2 {
                        return Err("Expected from and service".into());
                    }
                    return output(dinit::relation_command(
                        args[1],
                        &DinitRelation::Persistent {
                            from: args[0].into(),
                            enabled: action == "enable-from",
                        },
                    )?);
                }
                "add-dep" | "rm-dep" => {
                    if args.len() != 3 {
                        return Err("Expected dependency kind, service and target".into());
                    }
                    return output(dinit::relation_command(
                        args[1],
                        &DinitRelation::Runtime {
                            target: args[2].into(),
                            kind: args[0].parse()?,
                            add: action == "add-dep",
                        },
                    )?);
                }
                _ => {}
            }
        }
        if matches!(action, "create" | "replace") {
            return admin::install(
                self,
                &admin::DefinitionDraft::from_wire(args)?,
                action == "replace",
            );
        }
        if action == "list" && args.is_empty() {
            return match self {
                Self::Dinit => output(dinit::command("list", None)?),
                Self::Runit => runit::snapshot_text(),
            };
        }
        if args.len() != 1 {
            return Err("Expected exactly one service".into());
        }
        validate_name(args[0])?;
        if action == "logs" {
            return match self {
                Self::Runit => runit::log_path(args[0]),
                Self::Dinit => dinit::file_log_path(args[0]),
            }
            .ok_or_else(|| "No static svlogd destination found under /var/log".into())
            .and_then(|p| admin::read_log(&p));
        }
        if matches!(action, "status" | "catlog") {
            return match self {
                Self::Dinit => output(dinit::command(action, Some(args[0]))?),
                Self::Runit => runit::request(action, Some(args[0])),
            };
        }
        let action: Action = action.parse()?;
        if !action.supported(self) {
            return Err("Unsupported backend action".into());
        }
        match self {
            Self::Dinit => dinit::execute(action, args[0]),
            Self::Runit => runit::execute(action, args[0]),
        }
    }
}
pub fn permission_error(text: &str) -> bool {
    let lower = text.to_lowercase();
    lower.contains("permission denied")
        || lower.contains("access denied")
        || lower.contains("unlock live status")
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
        let error=match stdout.read_line(&mut greeting) {
            Ok(0)=>Some("pilot-helper: closed before READY (authentication cancelled?)".into()),
            Ok(_) if greeting.trim_end()!="READY 2"=>Some(format!("pilot-helper protocol mismatch ({}); rebuild and reinstall Pilot and its helper together",greeting.trim_end())),
            Err(e)=>Some(e.to_string()),_=>None,
        };
        if let Some(error) = error {
            drop(stdin);
            drop(stdout);
            let _ = child.wait();
            return Err(error);
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
            "2 {} {}",
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
        // The root helper cannot be killed from here. A helper busy with a bounded command
        // still sees QUIT or EOF afterwards, so never block the UI or shutdown on it.
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if !matches!(self.child.try_wait(), Ok(None)) {
                return;
            }
            thread::sleep(Duration::from_millis(20));
        }
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
    if len > 8 * 1024 * 1024 {
        return Err("pilot-helper: response exceeds size limit".into());
    }
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

#[cfg(test)]
mod boundary_tests {
    use super::*;
    #[test]
    fn helper_rejects_extra_arguments_and_wrong_capabilities_before_running_commands() {
        for backend in [Backend::Dinit, Backend::Runit] {
            assert!(backend
                .execute_system("start", &["worker", "other"])
                .is_err());
            assert!(backend.execute_system("start", &[".."]).is_err());
            assert!(backend
                .execute_system("setenv", &["LD_PRELOAD=/tmp/bad"])
                .is_err());
            assert!(backend.execute_system("shutdown", &[]).is_err());
            assert!(backend.execute_system("create", &["worker"]).is_err());
        }
        assert!(Backend::Runit
            .execute_system("trigger", &["worker"])
            .is_err());
        assert!(Backend::Dinit
            .execute_system("add-dep", &["invalid-kind", "a", "b"])
            .is_err());
        assert!(Backend::Dinit
            .execute_system("enable-from", &["../boot", "worker"])
            .is_err());
    }
    #[test]
    fn protocol_rejects_oversized_and_invalid_responses() {
        assert_eq!(parse_response_header("OK 0").unwrap(), (true, 0));
        assert!(parse_response_header("OK 999999999").is_err());
        assert!(parse_response_header("MAYBE 2").is_err());
    }
}
