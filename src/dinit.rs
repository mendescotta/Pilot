//! Dinit system definitions and commands; no user-instance fallback.
use crate::{output, validate_name, Action, Inspection, Service, SystemManagerProfile};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    process::Command,
};

pub fn roots() -> Vec<PathBuf> {
    let profile = SystemManagerProfile::system();
    if profile.backend == Some(crate::Backend::Dinit) {
        profile.definition_roots.clone()
    } else {
        crate::detection::dinit_roots(&[])
    }
}
/// PID 1 may use a custom system socket. Environment overrides remain excluded.
pub fn socket_override() -> Option<PathBuf> {
    SystemManagerProfile::system().socket.clone()
}
pub fn resolve(name: &str) -> Option<PathBuf> {
    validate_name(name).ok()?;
    let (base, _) = name.split_once('@').unwrap_or((name, ""));
    roots()
        .into_iter()
        .flat_map(|r| {
            let mut paths = vec![r.join(name)];
            if name.contains('@') {
                paths.push(r.join(format!("{base}@")));
            }
            paths
        })
        .find(|p| p.is_file())
}
pub fn command(action: &str, name: Option<&str>) -> Result<Command, String> {
    if !matches!(action, "list" | "status" | "catlog") && action.parse::<Action>().is_err() {
        return Err("Unsupported dinit action".into());
    }
    if action == "list" {
        if name.is_some() {
            return Err("list takes no service".into());
        }
    } else {
        validate_name(name.ok_or("Select a service")?)?;
    }
    let mut cmd = Command::new("/usr/bin/dinitctl");
    cmd.arg("--system")
        .env_remove("DINIT_SOCKET_PATH")
        .env_remove("DINIT_CS_FD")
        .env("LC_ALL", "C");
    if let Some(socket) = socket_override() {
        cmd.arg("--socket-path").arg(socket);
    }
    cmd.arg(action);
    if matches!(action, "start" | "stop" | "restart" | "wake" | "release") {
        cmd.arg("--no-wait");
    }
    if let Some(name) = name {
        cmd.arg(name);
    }
    Ok(cmd)
}
pub fn relation_command(name: &str, relation: &crate::DinitRelation) -> Result<Command, String> {
    validate_name(name)?;
    let mut command = Command::new("/usr/bin/dinitctl");
    command
        .arg("--system")
        .env_remove("DINIT_SOCKET_PATH")
        .env_remove("DINIT_CS_FD")
        .env("LC_ALL", "C");
    if let Some(socket) = socket_override() {
        command.arg("--socket-path").arg(socket);
    }
    match relation {
        crate::DinitRelation::Persistent { from, enabled } => {
            validate_name(from)?;
            command
                .arg(if *enabled { "enable" } else { "disable" })
                .arg("--from")
                .arg(from)
                .arg(name);
        }
        crate::DinitRelation::Runtime { target, kind, add } => {
            validate_name(target)?;
            command
                .arg(if *add { "add-dep" } else { "rm-dep" })
                .arg(kind.wire())
                .arg(name)
                .arg(target);
        }
    }
    Ok(command)
}
pub fn execute(action: Action, name: &str) -> Result<String, String> {
    output(action_command(action, name)?)
}
pub fn action_command(action: Action, name: &str) -> Result<Command, String> {
    validate_name(name)?;
    if !action.supported(crate::Backend::Dinit) {
        return Err("Unsupported dinit action".into());
    }
    if action == Action::Check {
        let mut cmd = Command::new("/usr/bin/dinit-check");
        cmd.args(["--system", "--online"])
            .arg(name)
            .env_remove("DINIT_SOCKET_PATH")
            .env_remove("DINIT_CS_FD")
            .env("LC_ALL", "C");
        if let Some(socket) = socket_override() {
            cmd.arg("--socket-path").arg(socket);
        }
        return Ok(cmd);
    }
    let mut cmd = Command::new("/usr/bin/dinitctl");
    cmd.arg("--system")
        .env_remove("DINIT_SOCKET_PATH")
        .env_remove("DINIT_CS_FD")
        .env("LC_ALL", "C");
    if let Some(socket) = socket_override() {
        cmd.arg("--socket-path").arg(socket);
    }
    match action {
        Action::StartPinned => {
            cmd.args(["start", "--pin", "--no-wait"]);
        }
        Action::StopPinned => {
            cmd.args(["stop", "--pin", "--no-wait"]);
        }
        Action::ClearLog => {
            cmd.args(["catlog", "--clear"]);
        }
        Action::ForceStop => {
            cmd.args(["stop", "--force", "--no-wait"]);
        }
        Action::ForceRestart => {
            cmd.args(["restart", "--force", "--no-wait"]);
        }
        Action::Hup
        | Action::Interrupt
        | Action::Terminate
        | Action::Kill
        | Action::Alarm
        | Action::Quit
        | Action::User1
        | Action::User2 => {
            cmd.arg("signal").arg(match action {
                Action::Hup => "HUP",
                Action::Interrupt => "INT",
                Action::Terminate => "TERM",
                Action::Alarm => "ALRM",
                Action::Quit => "QUIT",
                Action::User1 => "USR1",
                Action::User2 => "USR2",
                _ => "KILL",
            });
        }
        _ => {
            cmd.arg(action.wire());
            if matches!(
                action,
                Action::Start | Action::Stop | Action::Restart | Action::Wake | Action::Release
            ) {
                cmd.arg("--no-wait");
            }
        }
    }
    cmd.arg(name);
    Ok(cmd)
}
pub fn discover(listing: Result<&str, &str>) -> Vec<Service> {
    let mut services = BTreeMap::new();
    for dir in roots() {
        if let Ok(entries) = dir.read_dir() {
            for entry in entries.flatten().filter(|e| e.path().is_file()) {
                if let Some(name) = entry
                    .file_name()
                    .to_str()
                    .filter(|n| crate::valid_name(n) && !n.starts_with('.'))
                {
                    services
                        .entry(name.to_owned())
                        .or_insert(if is_template(name) {
                            Service {
                                name: name.into(),
                                state: "Template".into(),
                                summary: format!("Template; instances are named {name}<argument>"),
                            }
                        } else {
                            Service {
                                name: name.into(),
                                state: "Unknown".into(),
                                summary: "Definition available; live status unconfirmed".into(),
                            }
                        });
                }
            }
        }
    }
    match listing {
        Ok(text) => {
            for service in services.values_mut().filter(|s| !is_template(&s.name)) {
                service.state = "Not loaded".into();
                service.summary = "Definition available; not loaded in the manager".into();
            }
            for service in parse_list(text) {
                services.insert(service.name.clone(), service);
            }
        }
        Err(error) => {
            for service in services.values_mut().filter(|s| !is_template(&s.name)) {
                service.summary = if crate::permission_error(error) {
                    "Live status locked · definition available".into()
                } else {
                    "Live status unavailable · definition available".into()
                };
            }
        }
    }
    services.into_values().collect()
}
/// A file named `base@` is a template; only `base@argument` instances can be loaded.
pub fn is_template(name: &str) -> bool {
    name.ends_with('@')
}
/// Relationships that make one service a dependency of another (unlike ordering).
const DEPENDENCY_KINDS: [&str; 3] = ["need", "milestone", "waits-for"];
#[derive(Debug, PartialEq, Eq)]
pub struct Dependency {
    pub kind: &'static str,
    pub name: String,
}
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
            ("before", "before"),
            ("after", "after"),
            ("chain-to", "chain-to"),
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
            if !DEPENDENCY_KINDS.contains(&kind) {
                continue;
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

/// Partial presentation parser. Online dinit-check remains authoritative.
pub fn settings(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.starts_with(['#', '@']) {
                return None;
            }
            let (key, value) = line.split_once(['=', ':'])?;
            Some((key.trim().into(), value.trim().into()))
        })
        .collect()
}
fn relation_text(name: &str, text: &str, base: &Path) -> String {
    let mut result = String::from(
        "Definition relationships (need / milestone / waits-for; ordering: before / after; chain-to)\n",
    );
    for dep in parse_dependencies(text, base) {
        result.push_str(&format!("{} → {}\n", dep.kind, dep.name));
    }
    result.push_str("\nReferenced by\n");
    let mut seen = BTreeSet::new();
    for root in roots() {
        if let Ok(entries) = root.read_dir() {
            for entry in entries.flatten().filter(|e| e.path().is_file()) {
                let other = entry.file_name().to_string_lossy().into_owned();
                if other.starts_with('.') || !seen.insert(other.clone()) || other == name {
                    continue;
                }
                if let Ok(text) = fs::read_to_string(entry.path()) {
                    for dep in parse_dependencies(&text, &root)
                        .into_iter()
                        .filter(|d| d.name == name)
                    {
                        result.push_str(&format!("{other} → {}\n", dep.kind));
                    }
                }
            }
        }
    }
    result.push_str("\nThis view reads direct settings and dependency directories. Includes, substitutions and runtime-added dependencies may add relationships; use online validation for the manager's interpretation.");
    result
}
pub fn inspect(name: &str) -> Result<Inspection, String> {
    validate_name(name)?;
    let path = resolve(name);
    let text = path
        .as_ref()
        .map(fs::read_to_string)
        .transpose()
        .map_err(|e| e.to_string())?
        .unwrap_or_default();
    let fields = settings(&text);
    let get = |key: &str| {
        fields
            .iter()
            .rev()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
            .unwrap_or_else(|| "Manager default".into())
    };
    let log = get("log-type");
    let target = text
        .lines()
        .find_map(|l| l.trim().strip_prefix("@meta enable-via "))
        .unwrap_or("boot")
        .trim();
    // Presence alone cannot prove enablement: only a matching direct dependency can.
    let enabled = resolve(target)
        .and_then(|p| fs::read_to_string(&p).ok().map(|s| (p, s)))
        .map(|(p, s)| {
            parse_dependencies(&s, p.parent().unwrap_or(Path::new("/")))
                .iter()
                .any(|d| d.name == name && DEPENDENCY_KINDS.contains(&d.kind))
        });
    let mut facts = vec![
        (
            "Definition".into(),
            path.as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "Not found in system search path".into()),
        ),
        ("Enable target".into(), target.into()),
        (
            "Persistent dependency".into(),
            match enabled {
                Some(true) => "Present",
                Some(false) => "Not found in direct settings (includes may add it)",
                None => "Unknown",
            }
            .into(),
        ),
    ];
    for key in [
        "type",
        "command",
        "restart",
        "restart-delay",
        "start-timeout",
        "stop-timeout",
        "ready-notification",
        "log-type",
        "logfile",
        "consumer-of",
    ] {
        facts.push((key.into(), get(key)));
    }
    let relations = relation_text(
        name,
        &text,
        path.as_ref()
            .and_then(|p| p.parent())
            .unwrap_or(Path::new("/")),
    );
    Ok(Inspection {
        name: name.into(),
        facts,
        definition: if text.is_empty() {
            "No readable definition. A loaded instance may use a generated or private description."
                .into()
        } else {
            text
        },
        relations,
        diagnostics: String::new(),
        log_available: log == "buffer" || file_log_path(name).is_some(),
        log_description: match log.as_str() {
            "buffer" => "Buffered output from the running manager.",
            "file" => "File logging: see logfile in Overview.",
            "pipe" => "Pipe logging: inspect the consumer service.",
            "none" => "This definition disables logging.",
            _ => "Log mode is not explicitly set here. Includes and manager defaults may apply.",
        }
        .into(),
    })
}
pub fn file_log_path(name: &str) -> Option<PathBuf> {
    let text = fs::read_to_string(resolve(name)?).ok()?;
    let fields = settings(&text);
    if fields
        .iter()
        .rev()
        .find(|(k, _)| k == "log-type")
        .is_none_or(|(_, v)| v != "file")
    {
        return None;
    }
    let path = fields
        .iter()
        .rev()
        .find(|(k, _)| k == "logfile")?
        .1
        .trim_matches('"');
    let path = PathBuf::from(path);
    path.starts_with("/var/log").then_some(path)
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
            crate::validate_name(name).ok()?;
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn status_transitions_and_failures() {
        let rows = parse_list(
            "[{+}     ] worker (pid: 12)\n[   <<{ }] pending\n{     {X}} broken\n[     {-}] idle\n",
        );
        assert_eq!(
            rows.iter().map(|s| s.state.as_str()).collect::<Vec<_>>(),
            ["Started", "Starting", "Failed", "Stopped"]
        );
        assert!(rows[1].summary.contains("will stop"));
        assert!(parse_list("garbage\n短い\n").is_empty());
    }
    #[test]
    fn commands_reject_injection_and_clear_socket_overrides() {
        for name in ["..", ".", "--help", "a/b", "a\nb"] {
            assert!(command("start", Some(name)).is_err());
        }
        assert!(command("shutdown", None).is_err());
        let cmd = command("start", Some("worker@one")).unwrap();
        assert_eq!(
            cmd.get_args().collect::<Vec<_>>(),
            ["--system", "start", "--no-wait", "worker@one"]
        );
        assert!(cmd
            .get_envs()
            .any(|(k, v)| k == "DINIT_SOCKET_PATH" && v.is_none()));
    }
    #[test]
    fn relationships_keep_kinds_separate() {
        let deps = parse_dependencies(
            "depends-on = dbus\ndepends-ms: network\nwaits-for = logger\nbefore = login\nafter: udevd\nchain-to = next\n",
            Path::new("/"),
        );
        assert_eq!(
            deps.iter()
                .map(|d| (d.kind, d.name.as_str()))
                .collect::<Vec<_>>(),
            [
                ("need", "dbus"),
                ("milestone", "network"),
                ("waits-for", "logger"),
                ("before", "login"),
                ("after", "udevd"),
                ("chain-to", "next")
            ]
        );
    }
}
