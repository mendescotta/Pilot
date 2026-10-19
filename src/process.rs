//! Bounded subprocess execution; isolate children so timed-out checks cannot linger.
use std::{
    io::Read,
    os::unix::process::CommandExt,
    process::{Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};
unsafe extern "C" {
    fn kill(pid: i32, signal: i32) -> i32;
}
pub(crate) fn terminate_group(id: u32) {
    if let Ok(id) = i32::try_from(id) {
        // SAFETY: process_group(0) creates a dedicated group with this child's PID.
        // A negative PID addresses only that group, never the application's group.
        unsafe {
            kill(-id, 9);
        }
    }
}
pub fn output(cmd: Command) -> Result<String, String> {
    run(cmd, Duration::from_secs(15))
}
fn run(mut cmd: Command, timeout: Duration) -> Result<String, String> {
    cmd.process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("{}: {e}", cmd.get_program().to_string_lossy()))?;
    let id = child.id();
    let out = child.stdout.take().ok_or("Missing stdout")?;
    let err = child.stderr.take().ok_or("Missing stderr")?;
    let reader = |mut stream: Box<dyn Read + Send>| {
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let mut bytes = Vec::new();
            let result = stream
                .by_ref()
                .take(4 * 1024 * 1024 + 1)
                .read_to_end(&mut bytes);
            let _ = tx.send(result.map(|_| bytes));
        });
        rx
    };
    let out = reader(Box::new(out));
    let err = reader(Box::new(err));
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
            result => {
                terminate_group(id);
                let _ = child.wait();
                return Err(match result {
                    Err(e) => e.to_string(),
                    _ => "Command timed out; its process group was terminated".into(),
                });
            }
        }
    };
    let mut text = Vec::new();
    for stream in [out, err] {
        match stream.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(Ok(bytes)) if bytes.len() <= 4 * 1024 * 1024 => text.extend(bytes),
            Ok(Ok(_)) => {
                terminate_group(id);
                return Err("Command output exceeded 4 MiB".into());
            }
            Ok(Err(e)) => return Err(e.to_string()),
            Err(_) => {
                terminate_group(id);
                return Err("Command output did not close before timeout".into());
            }
        }
    }
    let text = String::from_utf8_lossy(&text).trim().to_string();
    if status.success() {
        Ok(text)
    } else {
        Err(if text.is_empty() {
            format!("Command exited with {status}")
        } else {
            text
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn timeout_includes_children_that_inherit_output() {
        let mut c = Command::new("/bin/sh");
        c.args(["-c", "sleep 5 & wait"]);
        let before = Instant::now();
        assert!(run(c, Duration::from_millis(150))
            .unwrap_err()
            .contains("timed out"));
        assert!(before.elapsed() < Duration::from_secs(2));
    }
    #[test]
    fn failure_preserves_diagnostics() {
        let mut c = Command::new("/bin/sh");
        c.args(["-c", "echo diagnostic >&2; exit 3"]);
        assert_eq!(output(c).unwrap_err(), "diagnostic");
    }
}
