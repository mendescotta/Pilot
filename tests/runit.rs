//! Private supervisor only. No /etc/sv or /var/service access.
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};
struct Supervisor {
    child: Child,
    root: PathBuf,
}
impl Drop for Supervisor {
    fn drop(&mut self) {
        for action in ["force-stop", "exit"] {
            let _ = Command::new("/usr/bin/sv")
                .args(["-w", "3", action])
                .arg(self.root.join("worker"))
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_dir_all(&self.root);
    }
}
#[test]
#[ignore = "requires runsvdir and sv; uses only a private temporary supervisor"]
fn private_supervisor_lifecycle_and_readiness() {
    let root = std::env::temp_dir().join(format!("pilot-private-runit-{}", std::process::id()));
    fs::create_dir_all(root.join("worker")).unwrap();
    for (name, text) in [
        ("run", "#!/bin/sh\nexec /bin/sleep 300\n"),
        ("check", "#!/bin/sh\nexit 0\n"),
    ] {
        let path = root.join("worker").join(name);
        fs::write(&path, text).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    fs::write(root.join("worker/down"), "").unwrap();
    let child = Command::new("/usr/bin/runsvdir")
        .arg(&root)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let _supervisor = Supervisor {
        child,
        root: root.clone(),
    };
    let deadline = Instant::now() + Duration::from_secs(6);
    while !root.join("worker/supervise/ok").exists() {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(25));
    }
    let run = |action| {
        pilot::output(pilot::runit::sv_command(action, &root.join("worker")).unwrap()).unwrap()
    };
    run(pilot::Action::Start);
    run(pilot::Action::Check);
    let status = || {
        let mut c = Command::new("/usr/bin/sv");
        c.arg("status").arg(root.join("worker"));
        pilot::output(c).unwrap()
    };
    assert_eq!(
        pilot::runit::from_status("worker".into(), &status()).state,
        "Started"
    );
    run(pilot::Action::Pause);
    thread::sleep(Duration::from_millis(40));
    assert_eq!(
        pilot::runit::from_status("worker".into(), &status()).state,
        "Paused"
    );
    run(pilot::Action::Continue);
    run(pilot::Action::Restart);
    run(pilot::Action::Stop);
    assert_eq!(
        pilot::runit::from_status("worker".into(), &status()).state,
        "Stopped"
    );
    fs::write(root.join("worker/check"), "#!/bin/sh\nexit 1\n").unwrap();
    let result = pilot::output(
        pilot::runit::sv_command(pilot::Action::Start, &root.join("worker")).unwrap(),
    );
    assert!(result.is_err());
}
