//! PID 1 is authoritative. Installed secondary managers never select Pilot's backend.
use crate::Backend;
use std::{
    fs,
    path::{Path, PathBuf},
    sync::OnceLock,
};

#[derive(Clone, Debug)]
pub struct SystemManagerProfile {
    pub backend: Option<Backend>,
    pub comm: Option<String>,
    pub executable: Option<PathBuf>,
    pub overridden: bool,
    pub diagnostics: Vec<String>,
    pub definition_roots: Vec<PathBuf>,
    /// Explicit dinit control socket from PID 1's arguments.
    pub socket: Option<PathBuf>,
}
pub fn backend_name(name: &str) -> Option<Backend> {
    match name {
        "dinit" | "dinit-init" => Some(Backend::Dinit),
        "runit" | "runit-init" => Some(Backend::Runit),
        _ => None,
    }
}
impl SystemManagerProfile {
    pub fn detect() -> Self {
        Self::probe(Path::new("/proc"), None)
    }
    /// PID 1 cannot change during this process's lifetime; read its evidence once.
    pub fn system() -> &'static Self {
        static SYSTEM: OnceLock<SystemManagerProfile> = OnceLock::new();
        SYSTEM.get_or_init(Self::detect)
    }
    pub fn probe(proc_root: &Path, override_backend: Option<Backend>) -> Self {
        let comm = fs::read_to_string(proc_root.join("1/comm"))
            .ok()
            .map(|s| s.trim().to_string());
        let executable = fs::read_link(proc_root.join("1/exe")).ok();
        let exe_backend = executable
            .as_ref()
            .and_then(|p| p.file_name())
            .and_then(|s| s.to_str())
            .and_then(backend_name);
        let comm_backend = comm.as_deref().and_then(backend_name);
        let mut diagnostics = Vec::new();
        // A readable executable is stronger evidence than a mutable process title.
        let backend = if let Some(ref exe) = executable {
            let name = exe.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if name == "init" {
                comm_backend
            } else {
                exe_backend
            }
        } else {
            comm_backend
        };
        if exe_backend.is_some() && comm_backend.is_some() && exe_backend != comm_backend {
            diagnostics.push("PID 1 name differs from its executable; using the executable".into());
        }
        let backend = override_backend.or(backend);
        if override_backend.is_some() {
            diagnostics.push("Development override active; PID 1 selection bypassed".into());
        }
        if backend.is_none() {
            diagnostics.push("Pilot supports runit and dinit as PID 1. Definitions and user managers cannot establish a system backend.".into());
        }
        let cmdline = fs::read(proc_root.join("1/cmdline")).unwrap_or_default();
        let definition_roots = match backend {
            Some(Backend::Dinit) => dinit_roots(&cmdline),
            Some(Backend::Runit) => vec![PathBuf::from("/etc/sv")],
            None => Vec::new(),
        };
        let socket = (backend == Some(Backend::Dinit))
            .then(|| dinit_socket(&cmdline))
            .flatten();
        Self {
            backend,
            comm,
            executable,
            overridden: override_backend.is_some(),
            diagnostics,
            definition_roots,
            socket,
        }
    }
    pub fn evidence(&self) -> String {
        format!(
            "PID 1: {}\nExecutable: {}\n{}",
            self.comm.as_deref().unwrap_or("unreadable"),
            self.executable
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "unreadable".into()),
            self.diagnostics.join("\n")
        )
    }
}
/// PID 1 may use a custom system socket. Environment overrides remain excluded.
pub fn dinit_socket(cmdline: &[u8]) -> Option<PathBuf> {
    let args: Vec<_> = cmdline
        .split(|b| *b == 0)
        .filter_map(|s| std::str::from_utf8(s).ok())
        .collect();
    args.iter().enumerate().find_map(|(i, arg)| {
        let path = if matches!(*arg, "-p" | "--socket-path") {
            args.get(i + 1).copied()
        } else {
            arg.strip_prefix("--socket-path=")
        }?;
        Path::new(path).is_absolute().then(|| path.into())
    })
}
/// Explicit service paths replace dinit defaults; retain their command-line precedence.
pub fn dinit_roots(cmdline: &[u8]) -> Vec<PathBuf> {
    let args: Vec<_> = cmdline
        .split(|b| *b == 0)
        .filter_map(|s| std::str::from_utf8(s).ok())
        .collect();
    let mut roots = Vec::new();
    let mut args = args.into_iter().skip(1);
    while let Some(arg) = args.next() {
        let value = if matches!(arg, "-d" | "--services-dir") {
            args.next()
        } else {
            arg.strip_prefix("--services-dir=")
                .or_else(|| arg.strip_prefix("-d").filter(|s| !s.is_empty()))
        };
        if let Some(path) = value {
            if Path::new(path).is_absolute() {
                roots.push(PathBuf::from(path));
            }
        }
    }
    if roots.is_empty() {
        roots = [
            "/etc/dinit.d",
            "/run/dinit.d",
            "/usr/local/lib/dinit.d",
            "/lib/dinit.d",
        ]
        .iter()
        .map(PathBuf::from)
        .collect();
    }
    roots
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explicit_paths_replace_defaults_in_order() {
        assert_eq!(
            dinit_roots(b"dinit\0-d\0/custom\0--services-dir=/other\0"),
            vec![PathBuf::from("/custom"), PathBuf::from("/other")]
        );
        assert_eq!(
            dinit_socket(b"dinit\0--socket-path=/run/alt\0"),
            Some(PathBuf::from("/run/alt"))
        );
        assert_eq!(dinit_socket(b"dinit\0-p\0relative\0"), None);
    }
    #[test]
    fn probe_uses_pid1_and_preserves_missing_evidence() {
        let dir = std::env::temp_dir().join(format!("pilot-detect-{}", std::process::id()));
        fs::create_dir_all(dir.join("1")).unwrap();
        fs::write(dir.join("1/comm"), "init\n").unwrap();
        std::os::unix::fs::symlink("/sbin/runit", dir.join("1/exe")).unwrap();
        assert_eq!(
            SystemManagerProfile::probe(&dir, None).backend,
            Some(Backend::Runit)
        );
        fs::remove_file(dir.join("1/exe")).unwrap();
        assert_eq!(SystemManagerProfile::probe(&dir, None).backend, None);
        fs::write(dir.join("1/comm"), "dinit-init\n").unwrap();
        assert_eq!(
            SystemManagerProfile::probe(&dir, None).backend,
            Some(Backend::Dinit)
        );
        assert!(SystemManagerProfile::probe(&dir, Some(Backend::Runit)).overridden);
        fs::remove_dir_all(dir).unwrap();
    }
}
