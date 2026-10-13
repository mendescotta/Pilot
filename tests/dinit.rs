//! Opt-in test against a private daemon; never connects to the host manager.
use std::{
    fs,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

struct Daemon {
    child: Child,
    dir: std::path::PathBuf,
}
impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = Command::new("/usr/bin/dinitctl")
            .arg("--socket-path")
            .arg(self.dir.join("socket"))
            .arg("shutdown")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let deadline = Instant::now() + Duration::from_secs(3);
        while matches!(self.child.try_wait(), Ok(None)) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_dir_all(&self.dir);
    }
}

#[test]
#[ignore = "requires /usr/bin/dinit; run with cargo test --test dinit -- --ignored"]
fn private_daemon_lifecycle() {
    let dir = std::env::temp_dir().join(format!("pilot-test-{}", std::process::id()));
    fs::create_dir(&dir).unwrap();
    fs::write(dir.join("boot"), "type = internal\nwaits-for.d = boot.d\n").unwrap();
    fs::create_dir(dir.join("boot.d")).unwrap();
    fs::write(dir.join("worker"), "type = process\ncommand = /bin/sh -c \"echo pilot-log; exec /bin/sleep 300\"\nlog-type = buffer\n").unwrap();
    let socket = dir.join("socket");
    let child = Command::new("/usr/bin/dinit")
        .args(["--user", "--quiet", "--services-dir"])
        .arg(&dir)
        .arg("--socket-path")
        .arg(&socket)
        .arg("boot")
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let _daemon = Daemon { child, dir };
    let deadline = Instant::now() + Duration::from_secs(5);
    while !socket.exists() {
        assert!(Instant::now() < deadline, "daemon did not become ready");
        thread::sleep(Duration::from_millis(20));
    }
    let run = |action, name| {
        let mut cmd = pilot::command(pilot::Scope::User, action, name).unwrap();
        // Explicit socket allowed only here, on the isolated test instance.
        cmd.env("DINIT_SOCKET_PATH", &socket);
        pilot::output(cmd).unwrap()
    };
    assert!(pilot::parse_list(&run("list", None))
        .iter()
        .any(|s| s.name == "boot" && s.state == "Started"));
    run("start", Some("worker"));
    let wait_for = |state: &str| {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let listing = run("list", None);
            if pilot::parse_list(&listing)
                .iter()
                .any(|s| s.name == "worker" && s.state == state)
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "worker did not become {state}: {listing}\n{}",
                run("status", Some("worker"))
            );
            thread::sleep(Duration::from_millis(20));
        }
    };
    wait_for("Started");
    assert!(run("status", Some("worker")).contains("STARTED"));
    let deadline = Instant::now() + Duration::from_secs(5);
    while !run("catlog", Some("worker")).contains("pilot-log") {
        assert!(Instant::now() < deadline, "buffered log missing");
        thread::sleep(Duration::from_millis(20));
    }
    run("restart", Some("worker"));
    wait_for("Started");
    run("enable", Some("worker"));
    assert!(socket.parent().unwrap().join("boot.d/worker").exists());
    run("disable", Some("worker"));
    assert!(!socket.parent().unwrap().join("boot.d/worker").exists());
    run("stop", Some("worker"));
    wait_for("Stopped");
}
