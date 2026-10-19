use crate::{output, validate as validate_name, Scope, Service};
use std::{
    collections::BTreeSet,
    fs,
    io::Write,
    os::unix::fs::symlink,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
};

fn dirs(scope: Scope) -> Result<(PathBuf, PathBuf), String> {
    match scope {
        Scope::System => Ok((PathBuf::from("/etc/sv"), PathBuf::from("/var/service"))),
        Scope::User => {
            let home = std::env::var_os("HOME").ok_or("HOME is unset")?;
            let home = PathBuf::from(home);
            Ok((home.join(".local/share/sv"), home.join(".local/service")))
        }
    }
}

pub fn validate(action: &str, service: Option<&str>) -> Result<(), String> {
    if !matches!(
        action,
        "status" | "start" | "stop" | "restart" | "enable" | "disable" | "reload"
    ) {
        return Err(format!("{action} is not supported for runit"));
    }
    validate_name("status", service)
}

fn paths(scope: Scope, name: &str) -> Result<(PathBuf, PathBuf), String> {
    validate_name("status", Some(name))?;
    let (available, active) = dirs(scope)?;
    Ok((available.join(name), active.join(name)))
}

fn exists(path: &std::path::Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

/// `sv status`'s text is already human-readable prose (unlike dinit's packed indicator), so the
/// only real gap is the base state: `runsv` writes `run`, `down`, or `finish` (the finish script
/// is running after the main process exited — not yet fully stopped) to `supervise/stat`, but
/// this used to bucket anything but `run:`/`down:` into "Unknown". `paused` (SIGSTOP'd) is
/// promoted over the base state since a paused process still holds a PID and looks alive
/// otherwise; `want up`/`want down`/`got TERM`/`normally up`/`normally down` stay in the full
/// text (already shown as the row's summary/tooltip) rather than growing the state vocabulary
/// further.
fn state_for_status(text: &str) -> &'static str {
    if text.contains(", paused") {
        "Paused"
    } else if text.starts_with("run:") {
        "Started"
    } else if text.starts_with("down:") {
        "Stopped"
    } else if text.starts_with("finish:") {
        "Finishing"
    } else {
        "Unknown"
    }
}

pub fn enabled(scope: Scope, name: &str) -> Option<String> {
    let (_, active) = paths(scope, name).ok()?;
    Some(format!(
        "Enabled: {}",
        if exists(&active) { "yes" } else { "no" }
    ))
}

pub fn user_definition_path(name: &str) -> Option<PathBuf> {
    let (available, _) = dirs(Scope::User).ok()?;
    validate_name("status", Some(name)).ok()?;
    Some(available.join(name))
}

/// Where `name`'s service directory actually lives, for either scope. `None` unless it exists
/// on disk — shown to the user, so an unconfirmed guess isn't useful here.
pub fn definition_path(scope: Scope, name: &str) -> Option<PathBuf> {
    validate_name("status", Some(name)).ok()?;
    let (available, _) = dirs(scope).ok()?;
    let dir = available.join(name);
    exists(&dir).then_some(dir)
}

/// Creates or removes the `down` file in `name`'s user-scope service directory: while present,
/// `runsv` does not start `./run` on its own (`runsv(8)`) — the service still exists and can
/// still be started manually (`sv up` / Pilot's Start), it just won't come up automatically,
/// including after the process next exits, not only on first discovery.
pub fn set_down(name: &str, down: bool) -> Result<(), String> {
    validate_name("status", Some(name))?;
    let (available, _) = dirs(Scope::User)?;
    let path = available.join(name).join("down");
    if down {
        fs::File::create(&path)
            .map(|_| ())
            .map_err(|e| format!("{}: {e}", path.display()))
    } else if exists(&path) {
        fs::remove_file(&path).map_err(|e| format!("{}: {e}", path.display()))
    } else {
        Ok(())
    }
}

/// The entered command is a shell command in a runit `run` script.
pub fn render_run_file(command: &str) -> Result<String, String> {
    if command.trim().is_empty() || command.chars().any(char::is_control) {
        return Err("Enter a single-line command".into());
    }
    Ok(format!("#!/bin/sh\nexec {command}\n"))
}

pub fn create_user_service(
    name: &str,
    contents: &str,
    existing: &[Service],
) -> Result<PathBuf, String> {
    let (available, _) = dirs(Scope::User)?;
    create_user_service_at(&available, name, contents, existing)
}

fn create_user_service_at(
    available: &Path,
    name: &str,
    contents: &str,
    existing: &[Service],
) -> Result<PathBuf, String> {
    validate_name("status", Some(name))?;
    if existing.iter().any(|service| service.name == name) {
        return Err(format!("A service named \"{name}\" already exists"));
    }
    fs::create_dir_all(available).map_err(|e| format!("{}: {e}", available.display()))?;
    let dir = available.join(name);
    fs::create_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let run = dir.join("run");
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&run)
            .map_err(|e| format!("{}: {e}", run.display()))?;
        file.write_all(contents.as_bytes())
            .map_err(|e| format!("{}: {e}", run.display()))?;
        fs::set_permissions(&run, fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("{}: {e}", run.display()))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&run);
        let _ = fs::remove_dir(&dir);
    }
    result.map(|()| dir)
}

pub fn delete_user_service(name: &str) -> Result<PathBuf, String> {
    let (available, active) = dirs(Scope::User)?;
    delete_user_service_at(&available, &active, name)
}

fn delete_user_service_at(available: &Path, active: &Path, name: &str) -> Result<PathBuf, String> {
    validate_name("status", Some(name))?;
    if exists(&active.join(name)) {
        return Err("Disable this runit service before deleting it".into());
    }
    let dir = available.join(name);
    if fs::symlink_metadata(&dir)
        .map_err(|e| e.to_string())?
        .file_type()
        .is_symlink()
    {
        return Err("Refusing to delete a linked service directory".into());
    }
    let entries = fs::read_dir(&dir)
        .map_err(|e| e.to_string())?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    if entries.len() != 1 || entries[0] != "run" {
        return Err("Service has additional files; remove it manually after review".into());
    }
    fs::remove_file(dir.join("run")).map_err(|e| e.to_string())?;
    fs::remove_dir(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

pub fn list(scope: Scope) -> Result<Vec<Service>, String> {
    let (_, active) = dirs(scope)?;
    Ok(list_names(scope)?
        .into_iter()
        .map(|name| service_status(scope, &active, name))
        .collect())
}

/// Lists service names without querying supervisor state. This is also the first, low-cost
/// request used by Pilot's authenticated system refresh; status is then fetched through the
/// privileged helper so runit's root-owned supervise sockets are readable.
pub fn list_names(scope: Scope) -> Result<Vec<String>, String> {
    let (available, active) = dirs(scope)?;
    if !available.is_dir() && !active.is_dir() {
        return Err(format!(
            "No runit service directories: {} or {}",
            available.display(),
            active.display()
        ));
    }
    Ok(service_names(&available, &active)?.into_iter().collect())
}

fn service_names(available: &Path, active: &Path) -> Result<BTreeSet<String>, String> {
    let mut names = BTreeSet::new();
    for dir in [available, active] {
        match fs::metadata(dir) {
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => continue,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(format!("{}: {error}", dir.display())),
        }
        let entries = fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        for entry in entries {
            let entry = entry.map_err(|e| format!("{}: {e}", dir.display()))?;
            if let Some(name) = entry.file_name().to_str() {
                if validate_name("status", Some(name)).is_ok() {
                    match entry.metadata() {
                        Ok(metadata) if metadata.is_dir() => {
                            names.insert(name.to_string());
                        }
                        Ok(_) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(error) => {
                            return Err(format!("{}: {error}", entry.path().display()));
                        }
                    }
                }
            }
        }
    }
    Ok(names)
}

/// One entry of `list()`'s mapping, factored out so a single-service refresh (selecting a row)
/// can look up just that one service without re-querying every other one.
fn service_status(scope: Scope, active: &Path, name: String) -> Service {
    let target = active.join(&name);
    if !exists(&target) {
        return Service {
            name,
            state: "Disabled".into(),
            summary: "Available, not enabled".into(),
        };
    }
    match crate::Backend::Runit.request(scope, "status", Some(&name)) {
        Ok(text) => service_from_status(name, &text),
        Err(error) => Service {
            name,
            state: "Unknown".into(),
            summary: format!("Status unavailable: {error}"),
        },
    }
}

pub(crate) fn service_from_status(name: String, text: &str) -> Service {
    let state = if text.starts_with("Disabled:") {
        "Disabled"
    } else {
        state_for_status(text)
    };
    Service {
        name,
        state: state.into(),
        summary: text.trim().into(),
    }
}

/// Refreshes a single service's live state directly (one `sv status`), instead of `list()`'s
/// full directory scan — for when the GUI only needs to update the one row the user selected.
pub fn service(scope: Scope, name: &str) -> Result<Service, String> {
    validate_name("status", Some(name))?;
    let (_, active) = dirs(scope)?;
    Ok(service_status(scope, &active, name.to_owned()))
}

pub fn request(scope: Scope, action: &str, service: Option<&str>) -> Result<String, String> {
    validate(action, service)?;
    let name = service.ok_or("Select a service")?;
    let (available, active) = dirs(scope)?;
    request_at(&available, &active, action, name)
}

fn request_at(available: &Path, active: &Path, action: &str, name: &str) -> Result<String, String> {
    validate(action, Some(name))?;
    let source = available.join(name);
    let target = active.join(name);
    match action {
        "enable" => {
            if !source.join("run").is_file() {
                return Err(format!(
                    "Missing run script: {}",
                    source.join("run").display()
                ));
            }
            if exists(&target) {
                return Err(format!("Already enabled: {}", target.display()));
            }
            let parent = target.parent().ok_or("Invalid service directory")?;
            if !parent.is_dir() {
                return Err(format!("No active runit directory: {} (start a user runsvdir first if this is a user service)", parent.display()));
            }
            symlink(&source, &target).map_err(|e| format!("{}: {e}", target.display()))?;
            Ok(format!("Enabled {name} at {}", target.display()))
        }
        "disable" => {
            if !exists(&target) {
                return Err(format!("Not enabled: {name}"));
            }
            let same_source = match (fs::canonicalize(&target), fs::canonicalize(&source)) {
                (Ok(actual), Ok(expected)) => actual == expected,
                _ => false,
            };
            if !target.is_symlink() || !same_source {
                return Err(format!(
                    "Refusing to remove {}: it is not a link to {}",
                    target.display(),
                    source.display()
                ));
            }
            fs::remove_file(&target).map_err(|e| format!("{}: {e}", target.display()))?;
            Ok(format!(
                "Disabled {name}; runsvdir will stop supervising it"
            ))
        }
        _ => {
            if !exists(&target) {
                if action == "status" {
                    return Ok(format!("Disabled: {name} is not supervised by runit"));
                }
                return Err(format!(
                    "{name} is not enabled in {}",
                    target.parent().unwrap().display()
                ));
            }
            let sv_action = match action {
                "status" => "status",
                "start" => "up",
                "stop" => "down",
                "restart" => "restart",
                "reload" => "hup",
                _ => unreachable!(),
            };
            let mut cmd = Command::new("/usr/bin/sv");
            cmd.arg(sv_action).arg(&target).env("LC_ALL", "C");
            output(cmd)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Guards the two tests below that mutate the process-wide `HOME` env var — same reasoning
    /// as lib.rs's `XDG_CONFIG_HOME_LOCK`.
    static HOME_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn state_for_status_covers_base_and_paused() {
        assert_eq!(state_for_status("run: dbus: (pid 123) 10s"), "Started");
        assert_eq!(
            state_for_status("down: dbus: 5s, normally up, want up"),
            "Stopped"
        );
        assert_eq!(state_for_status("finish: dbus: (pid 123) 0s"), "Finishing");
        assert_eq!(
            state_for_status("run: dbus: (pid 123) 10s, paused"),
            "Paused"
        );
        assert_eq!(state_for_status("unexpected text"), "Unknown");
        assert_eq!(
            service_from_status("dbus".into(), "Disabled: dbus is not supervised by runit").state,
            "Disabled"
        );
    }
    #[test]
    fn only_expected_actions_and_names() {
        assert!(validate("start", Some("dbus")).is_ok());
        assert!(validate("catlog", Some("dbus")).is_err());
        assert!(validate("stop", Some("../etc/passwd")).is_err());
        assert_eq!(
            render_run_file("/usr/bin/demo --flag").unwrap(),
            "#!/bin/sh\nexec /usr/bin/demo --flag\n"
        );
        assert!(render_run_file("echo ok\nrm -rf /tmp").is_err());
    }

    #[test]
    fn service_names_merges_live_links_and_available_definitions() {
        let base = std::env::temp_dir().join(format!("pilot-runit-list-{}", std::process::id()));
        let available = base.join("sv");
        let active = base.join("service");
        fs::create_dir_all(available.join("dbus")).unwrap();
        fs::create_dir_all(active.join("orphan")).unwrap();
        symlink(available.join("dbus"), active.join("dbus")).unwrap();
        fs::write(active.join("not-a-service"), "ignored").unwrap();

        assert_eq!(
            service_names(&available, &active).unwrap(),
            BTreeSet::from(["dbus".to_string(), "orphan".to_string()])
        );
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn enable_disable_only_removes_its_own_link() {
        let base = std::env::temp_dir().join(format!("pilot-runit-{}", std::process::id()));
        let available = base.join("sv");
        let active = base.join("service");
        fs::create_dir_all(available.join("worker")).unwrap();
        fs::create_dir_all(&active).unwrap();
        fs::write(available.join("worker/run"), "#!/bin/sh\nexit 0\n").unwrap();
        request_at(&available, &active, "enable", "worker").unwrap();
        assert_eq!(
            fs::canonicalize(active.join("worker")).unwrap(),
            fs::canonicalize(available.join("worker")).unwrap()
        );
        assert!(request_at(&available, &active, "enable", "worker").is_err());
        fs::remove_file(active.join("worker")).unwrap();
        symlink("/etc", active.join("worker")).unwrap();
        assert!(request_at(&available, &active, "disable", "worker").is_err());
        assert!(exists(&active.join("worker")));
        fs::remove_file(active.join("worker")).unwrap();
        symlink(available.join("worker"), active.join("worker")).unwrap();
        request_at(&available, &active, "disable", "worker").unwrap();
        assert!(!exists(&active.join("worker")));
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn user_service_requires_disable_before_delete() {
        let base = std::env::temp_dir().join(format!("pilot-runit-create-{}", std::process::id()));
        let available = base.join("sv");
        let active = base.join("service");
        fs::create_dir_all(&active).unwrap();
        let contents = render_run_file("/bin/sleep 1").unwrap();
        let path = create_user_service_at(&available, "worker", &contents, &[]).unwrap();
        assert_eq!(fs::read_to_string(path.join("run")).unwrap(), contents);
        assert!(fs::metadata(path.join("run")).unwrap().permissions().mode() & 0o111 != 0);
        request_at(&available, &active, "enable", "worker").unwrap();
        assert!(delete_user_service_at(&available, &active, "worker").is_err());
        request_at(&available, &active, "disable", "worker").unwrap();
        delete_user_service_at(&available, &active, "worker").unwrap();
        assert!(!path.exists());
        fs::remove_dir_all(base).unwrap();
    }
    #[test]
    fn set_down_creates_and_removes_the_down_file() {
        let _guard = HOME_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let base = std::env::temp_dir().join(format!("pilot-runit-down-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        unsafe { std::env::set_var("HOME", &base) };

        let contents = render_run_file("/bin/sleep infinity").unwrap();
        create_user_service("worker", &contents, &[]).unwrap();
        let down_path = base.join(".local/share/sv/worker/down");
        assert!(!down_path.exists());

        set_down("worker", true).unwrap();
        assert!(down_path.exists());
        set_down("worker", true).unwrap(); // idempotent
        assert!(down_path.exists());

        set_down("worker", false).unwrap();
        assert!(!down_path.exists());
        set_down("worker", false).unwrap(); // idempotent when already absent

        unsafe { std::env::remove_var("HOME") };
        fs::remove_dir_all(base).unwrap();
    }
    #[test]
    fn service_looks_up_one_entry_without_listing_the_rest() {
        let _guard = HOME_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let base = std::env::temp_dir().join(format!("pilot-runit-service-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        unsafe { std::env::set_var("HOME", &base) };

        let contents = render_run_file("/bin/sleep infinity").unwrap();
        create_user_service("worker", &contents, &[]).unwrap();
        let disabled = service(Scope::User, "worker").unwrap();
        assert_eq!(disabled.state, "Disabled");

        let (available, active) = dirs(Scope::User).unwrap();
        fs::create_dir_all(&active).unwrap();
        request_at(&available, &active, "enable", "worker").unwrap();
        // No real runsv is supervising this directory, so `sv status` itself fails — still a
        // real, deterministic answer from `service()`, not a listing of every other service.
        let enabled = service(Scope::User, "worker").unwrap();
        assert_eq!(enabled.state, "Unknown");
        assert!(enabled.summary.starts_with("Status unavailable:"));

        assert!(service(Scope::User, "../etc/passwd").is_err());

        unsafe { std::env::remove_var("HOME") };
        fs::remove_dir_all(base).unwrap();
    }
}
