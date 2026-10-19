//! Dinit events invalidate snapshots; periodic reconciliation remains authoritative.
use std::{
    io::Read,
    os::unix::process::CommandExt,
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    thread,
};
pub struct EventMonitor {
    child: Child,
    pub names: Vec<String>,
    revision: Arc<AtomicU64>,
}
impl EventMonitor {
    pub fn start(names: Vec<String>) -> Result<Self, String> {
        Self::start_with_socket(names, crate::dinit::socket_override().as_deref())
    }
    pub fn start_with_socket(
        names: Vec<String>,
        socket: Option<&std::path::Path>,
    ) -> Result<Self, String> {
        if names.is_empty() || names.iter().any(|n| !crate::valid_name(n)) {
            return Err("No valid loaded services to monitor".into());
        }
        let mut command = Command::new("/usr/bin/dinit-monitor");
        command.args(["--system", "--initial", "--command", "/usr/bin/printf ."]);
        if let Some(socket) = socket {
            command.arg("--socket-path").arg(socket);
        }
        let mut child = command
            .args(&names)
            .env_remove("DINIT_SOCKET_PATH")
            .env_remove("DINIT_CS_FD")
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| e.to_string())?;
        let mut stdout = child.stdout.take().ok_or("Monitor has no stdout")?;
        let revision = Arc::new(AtomicU64::new(0));
        let counter = revision.clone();
        thread::spawn(move || {
            let mut byte = [0];
            while stdout.read_exact(&mut byte).is_ok() {
                counter.fetch_add(1, Ordering::Relaxed);
            }
        });
        Ok(Self {
            child,
            names,
            revision,
        })
    }
    pub fn revision(&mut self) -> Option<u64> {
        if self.child.try_wait().ok().flatten().is_some() {
            None
        } else {
            Some(self.revision.load(Ordering::Relaxed))
        }
    }
}
impl Drop for EventMonitor {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            crate::process::terminate_group(self.child.id());
        }
        let _ = self.child.wait();
    }
}
