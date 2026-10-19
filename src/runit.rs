//! Void system runit. Fixed roots; a secondary runsvdir never selects this backend.
use crate::{output, validate_name, Access, Action, Inspection, Service, Snapshot};
use std::{
    collections::BTreeSet,
    fs,
    os::unix::fs::symlink,
    path::{Path, PathBuf},
    process::Command,
};

pub const AVAILABLE: &str = "/etc/sv";
pub const ACTIVE: &str = "/var/service";
fn exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}
pub fn names_at(available: &Path, active: &Path) -> Result<Vec<String>, String> {
    let mut names = BTreeSet::new();
    for dir in [available, active] {
        match fs::read_dir(dir) {
            Ok(entries) => {
                for entry in entries {
                    let entry = entry.map_err(|e| e.to_string())?;
                    if let Some(name) = entry
                        .file_name()
                        .to_str()
                        .filter(|s| crate::valid_name(s) && !s.starts_with('.'))
                    {
                        if entry.path().is_dir() || entry.path().is_symlink() {
                            names.insert(name.to_string());
                        }
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("{}: {e}", dir.display())),
        }
    }
    if !available.is_dir() && !active.is_dir() {
        return Err("No system runit service directories found".into());
    }
    Ok(names.into_iter().collect())
}
pub fn from_status(name: String, text: &str) -> Service {
    let main = text.split(';').next().unwrap_or(text);
    let state = if main.starts_with("Available:") {
        "Available"
    } else if main.contains(", paused") {
        "Paused"
    } else if main.starts_with("run:") {
        "Started"
    } else if main.starts_with("down:") {
        "Stopped"
    } else if main.starts_with("finish:") {
        "Finishing"
    } else {
        "Unknown"
    };
    Service {
        name,
        state: state.into(),
        summary: text.trim().into(),
    }
}
pub fn snapshot() -> Result<Snapshot, String> {
    let names = names_at(Path::new(AVAILABLE), Path::new(ACTIVE))?;
    let mut access = Access::Connected;
    let services = names
        .into_iter()
        .map(|name| {
            // One permission failure means every supervise directory is closed to this user;
            // do not fork sv for each remaining service.
            if access == Access::AuthenticationRequired && exists(&Path::new(ACTIVE).join(&name)) {
                return Service {
                    name,
                    state: "Unknown".into(),
                    summary: "Live status locked · definition available".into(),
                };
            }
            match request("status", Some(&name)) {
                Ok(text) => from_status(name, &text),
                Err(e) => {
                    if crate::permission_error(&e) {
                        access = Access::AuthenticationRequired;
                    }
                    Service {
                        name,
                        state: "Unknown".into(),
                        summary: format!("Supervisor status unavailable: {e}"),
                    }
                }
            }
        })
        .collect();
    Ok(Snapshot { services, access })
}
/// A single helper round trip for the whole snapshot, including status failures per record.
pub fn snapshot_text() -> Result<String, String> {
    Ok(snapshot()?
        .services
        .into_iter()
        .map(|s| {
            format!(
                "{}\t{}\t{}",
                s.name,
                s.state,
                s.summary.replace(['\n', '\r', '\t'], " ")
            )
        })
        .collect::<Vec<_>>()
        .join("\n"))
}
pub fn parse_snapshot(text: &str) -> Vec<Service> {
    text.lines()
        .filter_map(|l| {
            let mut fields = l.splitn(3, '\t');
            let name = fields.next()?;
            validate_name(name).ok()?;
            Some(Service {
                name: name.into(),
                state: fields.next()?.into(),
                summary: fields.next()?.into(),
            })
        })
        .collect()
}
pub fn definition(name: &str) -> Option<PathBuf> {
    validate_name(name).ok()?;
    let available = Path::new(AVAILABLE).join(name);
    if available.is_dir() {
        Some(available)
    } else {
        let active = Path::new(ACTIVE).join(name);
        active.is_dir().then_some(active)
    }
}
/// Recognize only literal svlogd destinations; never evaluate shell expressions.
pub fn log_path(name: &str) -> Option<PathBuf> {
    let text = fs::read_to_string(definition(name)?.join("log/run")).ok()?;
    text.lines().find_map(|line| {
        if line.trim().starts_with('#') {
            return None;
        }
        let tokens = line.split_whitespace().collect::<Vec<_>>();
        let program = tokens
            .iter()
            .position(|t| matches!(*t, "svlogd" | "/usr/bin/svlogd"))?;
        if program != 0 && !(program == 1 && tokens[0] == "exec") {
            return None;
        }
        let candidates = tokens[program + 1..]
            .iter()
            .filter(|t| !t.starts_with('-'))
            .collect::<Vec<_>>();
        if candidates.len() != 1 {
            return None;
        }
        let path = candidates[0].trim_matches(['\'', '"']);
        if !path.starts_with("/var/log/")
            || !path
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "/_-.".contains(c))
        {
            return None;
        }
        Some(PathBuf::from(path).join("current"))
    })
}
pub fn sv_command(action: Action, path: &Path) -> Result<Command, String> {
    if !action.supported(crate::Backend::Runit) {
        return Err("Unsupported runit action".into());
    }
    let control = match action {
        Action::Start => "start",
        Action::Stop => "stop",
        Action::Restart => "restart",
        Action::Reload => "reload",
        Action::Once => "once",
        Action::Pause => "pause",
        Action::Continue => "cont",
        Action::Hup => "hup",
        Action::Alarm => "alarm",
        Action::Interrupt => "interrupt",
        Action::Quit => "quit",
        Action::User1 => "1",
        Action::User2 => "2",
        Action::Terminate => "term",
        Action::Kill => "kill",
        Action::Check => "check",
        Action::ForceStop => "force-stop",
        Action::ForceRestart => "force-restart",
        _ => return Err("This action changes service links, not supervisor state".into()),
    };
    let mut cmd = Command::new("/usr/bin/sv");
    cmd.args(["-w", "7", control]).arg(path).env("LC_ALL", "C");
    Ok(cmd)
}
pub fn request(action: &str, name: Option<&str>) -> Result<String, String> {
    let name = name.ok_or("Select a service")?;
    validate_name(name)?;
    if action != "status" {
        return execute(action.parse()?, name);
    }
    let path = Path::new(ACTIVE).join(name);
    if !exists(&path) {
        return Ok(format!(
            "Available: {name} is not linked into the active runsvdir"
        ));
    }
    let mut cmd = Command::new("/usr/bin/sv");
    cmd.arg("status").arg(path).env("LC_ALL", "C");
    output(cmd)
}
/// Void selects the runlevel by pointing /var/service at /etc/runit/runsvdir/<level>.
pub fn runlevel() -> Option<String> {
    fs::canonicalize(ACTIVE)
        .ok()?
        .file_name()?
        .to_str()
        .map(Into::into)
}
pub fn execute(action: Action, name: &str) -> Result<String, String> {
    let mut text = execute_at(Path::new(AVAILABLE), Path::new(ACTIVE), action, name)?;
    if matches!(action, Action::Enable | Action::Disable) {
        if let Some(level) = runlevel().filter(|l| l != "default") {
            text.push_str(&format!(
                " This changed the active runlevel ({level}); boot uses default, which is unchanged."
            ));
        }
    }
    Ok(text)
}
pub fn execute_at(
    available: &Path,
    active: &Path,
    action: Action,
    name: &str,
) -> Result<String, String> {
    validate_name(name)?;
    let source = available.join(name);
    let target = active.join(name);
    match action {
        Action::Enable => {
            if !source.join("run").is_file() {
                return Err("Definition has no run script".into());
            }
            if exists(&target) {
                return Err("Service is already linked; refusing to overwrite".into());
            }
            symlink(&source, &target).map_err(|e| e.to_string())?;
            Ok(format!(
                "Enabled {name}; runsvdir will discover it on its next scan"
            ))
        }
        Action::Disable => {
            if !target.is_symlink()
                || fs::canonicalize(&target)
                    .ok()
                    .zip(fs::canonicalize(&source).ok())
                    .is_none_or(|(a, b)| a != b)
            {
                return Err("Refusing to remove a service entry that is not a link to its available definition".into());
            }
            fs::remove_file(target).map_err(|e| e.to_string())?;
            Ok(format!(
                "Disabled {name}; runsvdir will retire supervision on its next scan"
            ))
        }
        _ => {
            if !target.is_dir() {
                return Err("This service is not supervised in the active runsvdir".into());
            }
            output(sv_command(action, &target)?)
        }
    }
}
pub fn inspect(name: &str) -> Result<Inspection, String> {
    validate_name(name)?;
    let dir = definition(name).ok_or("No readable system service directory")?;
    let active = Path::new(ACTIVE).join(name);
    let mut facts = vec![
        ("Definition".into(), dir.display().to_string()),
        (
            "Active runsvdir".into(),
            fs::canonicalize(ACTIVE)
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| ACTIVE.into()),
        ),
        (
            "Runlevel".into(),
            runlevel().unwrap_or_else(|| "Unknown".into()),
        ),
        (
            "Supervision link".into(),
            if exists(&active) { "Present" } else { "Absent" }.into(),
        ),
        (
            "Initial state".into(),
            if dir.join("down").exists() {
                "Down file present"
            } else {
                "Normally up"
            }
            .into(),
        ),
        (
            "Readiness".into(),
            if dir.join("check").is_file() {
                "Check script available"
            } else {
                "No check script; process state only"
            }
            .into(),
        ),
    ];
    let mut definition = String::new();
    let mut components = String::new();
    for name in [
        "run",
        "finish",
        "check",
        "conf",
        "down",
        "log/run",
        "log/config",
    ] {
        let path = dir.join(name);
        if exists(&path) {
            components.push_str(&format!("{name}  {}\n", path.display()));
            if path.is_file() {
                match fs::read_to_string(&path) {
                    Ok(text) => definition.push_str(&format!("# ── {name} ──\n{text}\n")),
                    Err(e) => definition.push_str(&format!("# {name}: {e}\n")),
                }
            }
        }
    }
    if let Ok(entries) = dir.join("control").read_dir() {
        for e in entries.flatten() {
            components.push_str(&format!("control/{}\n", e.file_name().to_string_lossy()));
        }
    }
    let log = dir.join("log").is_dir();
    facts.push((
        "Log supervisor".into(),
        if log {
            "Appendant log service present; sv status includes its independent state"
        } else {
            "No appendant log service"
        }
        .into(),
    ));
    components.push_str("\nRunit has no authoritative dependency graph. Scripts and control hooks define service relationships.\nThe down file controls initial state; it does not determine whether the service is currently running.");
    Ok(Inspection{name:name.into(),facts,definition,relations:components,diagnostics:String::new(),log_available:log_path(name).is_some(),
        log_description:if log {"The appendant log service has its own supervisor state, shown under Diagnostics. Review log/run for the output destination; shell scripts may compute it dynamically."}else{"No appendant log service. Output may be redirected by the run script or a system logger."}.into()})
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn main_and_log_states_are_independent() {
        assert_eq!(
            from_status(
                "demo".into(),
                "run: demo: (pid 12) 8s; down: log: 3s, paused"
            )
            .state,
            "Started"
        );
        assert_eq!(
            from_status(
                "demo".into(),
                "run: demo: (pid 12) 8s, paused; run: log: (pid 13) 9s"
            )
            .state,
            "Paused"
        );
        assert_eq!(
            from_status("demo".into(), "finish: demo: (pid 12) 0s, want down").state,
            "Finishing"
        );
        assert_eq!(
            from_status("demo".into(), "warning: unable to open supervise/ok").state,
            "Unknown"
        );
    }
    #[test]
    fn links_and_live_only_directories_are_discovered_safely() {
        let root = std::env::temp_dir().join(format!("pilot-runit-links-{}", std::process::id()));
        let available = root.join("sv");
        let active = root.join("service");
        fs::create_dir_all(available.join("worker")).unwrap();
        fs::create_dir_all(available.join(".pilot-stage-hidden")).unwrap();
        fs::create_dir_all(active.join("live")).unwrap();
        fs::write(available.join("worker/run"), "#!/bin/sh\n").unwrap();
        execute_at(&available, &active, Action::Enable, "worker").unwrap();
        assert_eq!(names_at(&available, &active).unwrap(), ["live", "worker"]);
        assert!(execute_at(&available, &active, Action::Enable, "worker").is_err());
        fs::remove_file(active.join("worker")).unwrap();
        symlink("/etc", active.join("worker")).unwrap();
        assert!(execute_at(&available, &active, Action::Disable, "worker").is_err());
        fs::remove_file(active.join("worker")).unwrap();
        symlink(available.join("worker"), active.join("worker")).unwrap();
        execute_at(&available, &active, Action::Disable, "worker").unwrap();
        fs::remove_dir_all(root).unwrap();
    }
}
