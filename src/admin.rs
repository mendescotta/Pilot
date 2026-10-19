//! Previewed administrator definitions. Directory handles anchor every write;
//! package definitions are never replaced. Linux-specific open flags are isolated here.
use crate::{output, validate_name, Backend};
use std::{
    fs::{self, File, OpenOptions, Permissions},
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::fs::{chown, symlink, DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    },
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

// Linux O_DIRECTORY and O_NOFOLLOW: reject symlinks when opening a directory or file.
const DIRECTORY: i32 = 0x10000;
const NOFOLLOW: i32 = 0x20000;
const NONBLOCK: i32 = 0x800;
const MARKER: &str = "# Created by Pilot; administrator-owned definition\n";
static NEXT: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug)]
pub struct DefinitionDraft {
    pub name: String,
    pub kind: String,
    pub command: String,
    pub pid_file: String,
    pub buffered: bool,
    pub initially_down: bool,
    pub logger: bool,
}
impl DefinitionDraft {
    pub fn preview(&self, backend: Backend) -> Result<String, String> {
        let text = self.render(backend)?;
        if backend == Backend::Dinit {
            return Ok(format!("# /etc/dinit.d/{} (0644)\n{text}", self.name));
        }
        let mut preview = format!("# /etc/sv/{}/run (0755)\n{text}", self.name);
        preview.push_str(&format!(
            "\n# /etc/sv/{0}/supervise -> {1}\n",
            self.name,
            supervise_target(&self.name)
        ));
        if self.initially_down {
            preview.push_str(&format!(
                "\n# /etc/sv/{}/down (0644; empty file)\n",
                self.name
            ));
        }
        if self.logger {
            preview.push_str(&format!("\n# /etc/sv/{}/log/run (0755)\n{}\n# /etc/sv/{}/log/config (0644)\ns1000000\nn10\n# /etc/sv/{}/log/supervise -> {}\n\n# Create log directory: /var/log/pilot/{}\n",self.name,self.log_script(),self.name,self.name,supervise_target(&format!("{}-log", self.name)),self.name));
        }
        Ok(preview)
    }
    fn log_script(&self) -> String {
        format!(
            "#!/bin/sh\nexec svlogd -tt {}\n",
            shell_quote(&format!("/var/log/pilot/{}", self.name))
        )
    }
    pub fn render(&self, backend: Backend) -> Result<String, String> {
        validate_name(&self.name)?;
        if self.name.starts_with('.') {
            return Err("Definition names cannot begin with a dot".into());
        }
        for value in [&self.command, &self.pid_file] {
            if value.len() > 16 * 1024 || value.chars().any(char::is_control) {
                return Err("Use single-line command and PID file values (at most 16 KiB)".into());
            }
        }
        if backend == Backend::Runit {
            if self.command.trim().is_empty() {
                return Err("A run command is required".into());
            }
            // svlogd reads only stdout; route stderr there when a logger is attached.
            let redirect = if self.logger { "exec 2>&1\n" } else { "" };
            return Ok(format!(
                "#!/bin/sh\n{MARKER}{redirect}exec {}\n",
                self.command
            ));
        }
        if !["process", "bgprocess", "scripted", "internal", "triggered"]
            .contains(&self.kind.as_str())
        {
            return Err("Unknown dinit service type".into());
        }
        let executable = !matches!(self.kind.as_str(), "internal" | "triggered");
        if executable && self.command.trim().is_empty() {
            return Err("This service type requires a command".into());
        }
        if self.kind == "bgprocess" && !self.pid_file.starts_with('/') {
            return Err("Background processes require an absolute PID file".into());
        }
        let mut text = format!("{MARKER}type = {}\n", self.kind);
        if executable {
            text.push_str(&format!("command = {}\n", self.command));
        }
        if self.kind == "bgprocess" {
            text.push_str(&format!("pid-file = {}\n", self.pid_file));
        }
        if matches!(self.kind.as_str(), "process" | "bgprocess") {
            text.push_str(&format!(
                "log-type = {}\n",
                if self.buffered { "buffer" } else { "none" }
            ));
        }
        text.push_str("@meta enable-via boot\n");
        Ok(text)
    }
    pub fn wire(&self) -> Vec<String> {
        vec![
            self.name.clone(),
            self.kind.clone(),
            encode(&self.command),
            encode(&self.pid_file),
            format!(
                "{}{}{}",
                u8::from(self.buffered),
                u8::from(self.initially_down),
                u8::from(self.logger)
            ),
        ]
    }
    pub fn from_wire(args: &[&str]) -> Result<Self, String> {
        if args.len() != 5
            || args[4].len() != 3
            || !args[4].bytes().all(|b| matches!(b, b'0' | b'1'))
        {
            return Err("Invalid definition request".into());
        }
        Ok(Self {
            name: args[0].into(),
            kind: args[1].into(),
            command: decode(args[2])?,
            pid_file: decode(args[3])?,
            buffered: args[4].as_bytes()[0] == b'1',
            initially_down: args[4].as_bytes()[1] == b'1',
            logger: args[4].as_bytes()[2] == b'1',
        })
    }
}
fn encode(text: &str) -> String {
    if text.is_empty() {
        return "-".into();
    }
    text.bytes().map(|b| format!("{b:02x}")).collect()
}
fn decode(text: &str) -> Result<String, String> {
    if text == "-" {
        return Ok(String::new());
    }
    if text.len() > 32768 || !text.len().is_multiple_of(2) || !text.is_ascii() {
        return Err("Invalid definition encoding".into());
    }
    let bytes = (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "Invalid hex data")?;
    String::from_utf8(bytes).map_err(|_| "Definition is not UTF-8".into())
}
fn open_dir(path: &Path) -> Result<File, String> {
    File::options()
        .read(true)
        .custom_flags(DIRECTORY | NOFOLLOW)
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))
}
fn anchored(file: &File) -> PathBuf {
    PathBuf::from(format!("/proc/self/fd/{}", file.as_raw_fd()))
}
fn write_new(path: &Path, contents: &str, executable: bool) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .custom_flags(NOFOLLOW)
        .mode(if executable { 0o755 } else { 0o644 })
        .open(path)
        .map_err(|e| e.to_string())?;
    file.write_all(contents.as_bytes())
        .and_then(|_| file.sync_all())
        .map_err(|e| e.to_string())
}
/// Void keeps runsv state under /run so /etc/sv stays free of runtime files.
fn supervise_target(name: &str) -> String {
    format!("/run/runit/supervise.{name}")
}
fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\"'\"'"))
}
struct Stage(PathBuf);
impl Drop for Stage {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub fn install(backend: Backend, draft: &DefinitionDraft, replace: bool) -> Result<String, String> {
    let root = Path::new(match backend {
        Backend::Dinit => "/etc/dinit.d",
        Backend::Runit => "/etc/sv",
    });
    if backend == Backend::Dinit && !crate::dinit::roots().iter().any(|r| r == root) {
        return Err(
            "The live dinit search path excludes /etc/dinit.d; creation is unavailable".into(),
        );
    }
    install_at(root, backend, draft, replace, true)
}
fn install_at(
    root: &Path,
    backend: Backend,
    draft: &DefinitionDraft,
    replace: bool,
    validate: bool,
) -> Result<String, String> {
    let text = draft.render(backend)?;
    let parent = open_dir(root)?;
    let parent_path = anchored(&parent);
    let target = parent_path.join(&draft.name);
    if !replace && fs::symlink_metadata(&target).is_ok() {
        return Err(
            "A definition with this name already exists; choose a new administrator name".into(),
        );
    }
    if backend == Backend::Runit && replace {
        return Err(
            "Create a new runit definition name; in-place script replacement is not available"
                .into(),
        );
    }
    let old = if replace {
        let mut file = File::options()
            .read(true)
            .custom_flags(NOFOLLOW | NONBLOCK)
            .open(&target)
            .map_err(|e| e.to_string())?;
        if !file.metadata().map_err(|e| e.to_string())?.is_file() {
            return Err("Only regular definitions can be replaced".into());
        }
        let mut text = String::new();
        (&mut file)
            .take(1024 * 1024 + 1)
            .read_to_string(&mut text)
            .map_err(|e| e.to_string())?;
        if text.len() > 1024 * 1024 {
            return Err("Definition is too large to replace".into());
        }
        if !text.starts_with(MARKER) {
            return Err("Only Pilot-created administrator definitions can be replaced; copy this service to a new name".into());
        }
        let mut query = Command::new("/usr/bin/xbps-query");
        query.arg("-o").arg(root.join(&draft.name));
        let result = output(query).map_err(|e| format!("Cannot verify package ownership: {e}"))?;
        // xbps-query -o succeeds with empty output when no package owns the path.
        // Errors and any owner/diagnostic output are conservatively refused.
        if !result.is_empty() {
            return Err("Package ownership is present or uncertain; create an administrator copy under a new name".into());
        }
        Some(file.metadata().map_err(|e| e.to_string())?)
    } else {
        None
    };
    let id = format!(
        "{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    let stage_path = parent_path.join(format!(".pilot-stage-{id}"));
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&stage_path)
        .map_err(|e| e.to_string())?;
    let mut stage = Stage(stage_path);
    let directory = open_dir(&stage.0)?;
    let staged = anchored(&directory);
    let file_name = if backend == Backend::Dinit {
        draft.name.as_str()
    } else {
        "run"
    };
    write_new(&staged.join(file_name), &text, backend == Backend::Runit)?;
    if backend == Backend::Runit {
        if draft.initially_down {
            write_new(&staged.join("down"), "", false)?;
        }
        symlink(supervise_target(&draft.name), staged.join("supervise"))
            .map_err(|e| e.to_string())?;
    }
    if validate {
        let mut check = Command::new(if backend == Backend::Dinit {
            "/usr/bin/dinit-check"
        } else {
            "/bin/sh"
        });
        if backend == Backend::Dinit {
            check.args(["--system", "--services-dir"]).arg(&stage.0);
            for root in crate::dinit::roots() {
                check.arg("--services-dir").arg(root);
            }
            check
                .arg(&draft.name)
                .env_remove("DINIT_SOCKET_PATH")
                .env_remove("DINIT_CS_FD");
        } else {
            check.arg("-n").arg(staged.join("run"));
        }
        // /proc/self refers to the check child, whose close-on-exec descriptors differ.
        // Use the helper's PID to address its still-open directory descriptor.
        let parent_pid = std::process::id().to_string();
        let args: Vec<_> = check
            .get_args()
            .map(|a| {
                a.to_string_lossy()
                    .replace("/proc/self/", &format!("/proc/{parent_pid}/"))
            })
            .collect();
        let program = check.get_program().to_owned();
        let mut bounded = Command::new(program);
        bounded
            .args(args)
            .env_remove("DINIT_SOCKET_PATH")
            .env_remove("DINIT_CS_FD");
        output(bounded)?;
    }
    if backend == Backend::Runit && draft.logger {
        create_log_directory(&draft.name)?;
        fs::create_dir(staged.join("log")).map_err(|e| e.to_string())?;
        write_new(&staged.join("log/run"), &draft.log_script(), true)?;
        write_new(&staged.join("log/config"), "s1000000\nn10\n", false)?;
        symlink(
            supervise_target(&format!("{}-log", draft.name)),
            staged.join("log/supervise"),
        )
        .map_err(|e| e.to_string())?;
        open_dir(&staged.join("log"))?
            .sync_all()
            .map_err(|e| e.to_string())?;
    }
    if let Some(old) = old {
        let now = fs::symlink_metadata(&target).map_err(|e| e.to_string())?;
        if now.ino() != old.ino()
            || now.dev() != old.dev()
            || now.mtime() != old.mtime()
            || now.mtime_nsec() != old.mtime_nsec()
        {
            return Err(
                "Definition changed while being prepared; reload and review it again".into(),
            );
        }
        let backup = parent_path.join(format!(".pilot-backup-{id}"));
        fs::hard_link(&target, &backup).map_err(|e| e.to_string())?;
        chown(staged.join(file_name), Some(old.uid()), Some(old.gid()))
            .map_err(|e| e.to_string())?;
        fs::set_permissions(
            staged.join(file_name),
            Permissions::from_mode(old.mode() & 0o777),
        )
        .map_err(|e| e.to_string())?;
        File::open(staged.join(file_name))
            .and_then(|f| f.sync_all())
            .map_err(|e| e.to_string())?;
        fs::rename(staged.join(file_name), &target).map_err(|e| e.to_string())?;
        parent.sync_all().map_err(|e| e.to_string())?;
        return Ok(format!("Replaced {}; recoverable backup: {}. Reload the definition; restart if command settings changed.",root.join(&draft.name).display(),root.join(backup.file_name().ok_or("Invalid backup path")?).display()));
    }
    if backend == Backend::Dinit {
        fs::hard_link(staged.join(file_name), &target).map_err(|e| e.to_string())?;
    } else {
        // Publish a completed directory atomically with a create-new symlink.
        // The backing directory keeps a stable hidden name that runsvdir never scans.
        let backing_name = format!(".pilot-sv-{}", draft.name);
        let backing = parent_path.join(&backing_name);
        if fs::symlink_metadata(&backing).is_ok() {
            return Err(format!(
                "{} already exists from an earlier definition; review and remove it first",
                root.join(&backing_name).display()
            ));
        }
        directory
            .set_permissions(Permissions::from_mode(0o755))
            .map_err(|e| e.to_string())?;
        directory.sync_all().map_err(|e| e.to_string())?;
        fs::rename(&stage.0, &backing).map_err(|e| e.to_string())?;
        stage.0 = PathBuf::new();
        symlink(root.join(&backing_name), &target).map_err(|e| e.to_string())?;
    }
    parent.sync_all().map_err(|e| e.to_string())?;
    Ok(format!(
        "Created {}. Enable and start it when ready.",
        root.join(&draft.name).display()
    ))
}
fn create_log_directory(name: &str) -> Result<(), String> {
    let root = open_dir(Path::new("/var/log"))?;
    let path = anchored(&root).join("pilot");
    match fs::create_dir(&path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e.to_string()),
    }
    let pilot = open_dir(&path)?;
    let path = anchored(&pilot).join(name);
    match fs::create_dir(&path) {
        Ok(()) => Ok(()),
        // Reuse an earlier service's log directory, but never through a symlink.
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => open_dir(&path).map(drop),
        Err(e) => Err(e.to_string()),
    }
}
/// Read a log beneath /var/log without following symlinks at any component.
pub fn read_log(path: &Path) -> Result<String, String> {
    let relative = path
        .strip_prefix("/var/log")
        .map_err(|_| "Only logs under /var/log are readable")?;
    read_log_at(Path::new("/var/log"), relative)
}
fn read_log_at(root: &Path, relative: &Path) -> Result<String, String> {
    let mut directory = open_dir(root)?;
    let components: Vec<_> = relative.components().collect();
    let (last, parents) = components.split_last().ok_or("Missing log file")?;
    for component in parents {
        let std::path::Component::Normal(name) = component else {
            return Err("Invalid log path".into());
        };
        directory = open_dir(&anchored(&directory).join(name))?;
    }
    let std::path::Component::Normal(name) = last else {
        return Err("Invalid log filename".into());
    };
    let mut file = File::options()
        .read(true)
        .custom_flags(NOFOLLOW | NONBLOCK)
        .open(anchored(&directory).join(name))
        .map_err(|e| e.to_string())?;
    use std::io::{Seek, SeekFrom};
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    if !metadata.is_file() {
        return Err("Only regular log files can be read".into());
    }
    let size = metadata.len();
    file.seek(SeekFrom::Start(size.saturating_sub(256 * 1024)))
        .map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    file.take(256 * 1024)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "requires dinit-check; stages only in a private temporary directory"]
    fn staged_validation_and_runit_publication() {
        let root =
            std::env::temp_dir().join(format!("pilot-admin-validate-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let mut draft = DefinitionDraft {
            name: "worker".into(),
            kind: "internal".into(),
            command: String::new(),
            pid_file: String::new(),
            buffered: false,
            initially_down: true,
            logger: false,
        };
        install_at(&root, Backend::Dinit, &draft, false, true).unwrap();
        draft.name = "invalid".into();
        draft.kind = "process".into();
        draft.command = "/bin/echo \"unterminated".into();
        assert!(install_at(&root, Backend::Dinit, &draft, false, true).is_err());
        assert!(!root.join("invalid").exists());
        draft.name = "runit-worker".into();
        draft.command = "/bin/sleep 10".into();
        install_at(&root, Backend::Runit, &draft, false, true).unwrap();
        assert!(root.join("runit-worker").is_symlink());
        assert!(root.join(".pilot-sv-runit-worker/run").is_file());
        assert!(root.join("runit-worker/down").is_file());
        assert_eq!(
            fs::read_link(root.join("runit-worker/supervise")).unwrap(),
            PathBuf::from("/run/runit/supervise.runit-worker")
        );
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    #[ignore = "requires xbps-query; replaces only a private temporary definition"]
    fn replacement_preserves_permissions_and_backup() {
        let root = std::env::temp_dir().join(format!("pilot-admin-backup-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let mut draft = DefinitionDraft {
            name: "worker".into(),
            kind: "internal".into(),
            command: String::new(),
            pid_file: String::new(),
            buffered: false,
            initially_down: false,
            logger: false,
        };
        install_at(&root, Backend::Dinit, &draft, false, false).unwrap();
        let original = fs::read_to_string(root.join("worker")).unwrap();
        fs::set_permissions(root.join("worker"), Permissions::from_mode(0o600)).unwrap();
        draft.kind = "triggered".into();
        install_at(&root, Backend::Dinit, &draft, true, false).unwrap();
        assert_eq!(
            fs::metadata(root.join("worker")).unwrap().mode() & 0o777,
            0o600
        );
        assert!(fs::read_to_string(root.join("worker"))
            .unwrap()
            .contains("type = triggered"));
        let backup = fs::read_dir(&root)
            .unwrap()
            .flatten()
            .find(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with(".pilot-backup-")
            })
            .unwrap();
        assert_eq!(fs::read_to_string(backup.path()).unwrap(), original);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn create_never_overwrites_and_rejects_linked_root() {
        let root = std::env::temp_dir().join(format!("pilot-admin-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let draft = DefinitionDraft {
            name: "worker".into(),
            kind: "internal".into(),
            command: String::new(),
            pid_file: String::new(),
            buffered: false,
            initially_down: false,
            logger: false,
        };
        install_at(&root, Backend::Dinit, &draft, false, false).unwrap();
        assert!(install_at(&root, Backend::Dinit, &draft, false, false).is_err());
        let link = root.with_extension("link");
        symlink(&root, &link).unwrap();
        assert!(install_at(&link, Backend::Dinit, &draft, false, false).is_err());
        fs::remove_file(link).unwrap();
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn wire_roundtrip_and_invalid_payload() {
        let draft = DefinitionDraft {
            name: "demo".into(),
            kind: "process".into(),
            command: "/bin/echo hello world".into(),
            pid_file: String::new(),
            buffered: true,
            initially_down: true,
            logger: false,
        };
        let wire = draft.wire();
        let args = wire.iter().map(String::as_str).collect::<Vec<_>>();
        assert_eq!(
            DefinitionDraft::from_wire(&args)
                .unwrap()
                .render(Backend::Dinit),
            draft.render(Backend::Dinit)
        );
        assert!(decode("f").is_err());
        assert!(decode("é").is_err());
        assert!(read_log(Path::new("/etc/shadow")).is_err());
    }
    #[test]
    fn runit_logger_captures_stderr_and_keeps_supervise_under_run() {
        let mut draft = DefinitionDraft {
            name: "demo".into(),
            kind: String::new(),
            command: "/bin/demo".into(),
            pid_file: String::new(),
            buffered: false,
            initially_down: false,
            logger: true,
        };
        let run = draft.render(Backend::Runit).unwrap();
        assert!(run.contains("exec 2>&1\nexec /bin/demo\n"));
        let preview = draft.preview(Backend::Runit).unwrap();
        assert!(preview.contains("supervise -> /run/runit/supervise.demo\n"));
        assert!(preview.contains("log/supervise -> /run/runit/supervise.demo-log\n"));
        draft.logger = false;
        assert!(!draft.render(Backend::Runit).unwrap().contains("2>&1"));
    }
    #[test]
    fn log_reader_rejects_symlink_components_and_parent_traversal() {
        let root = std::env::temp_dir().join(format!("pilot-log-read-{}", std::process::id()));
        fs::create_dir_all(root.join("safe")).unwrap();
        fs::write(root.join("safe/current"), "logged output").unwrap();
        assert_eq!(
            read_log_at(&root, Path::new("safe/current")).unwrap(),
            "logged output"
        );
        symlink(root.join("safe"), root.join("linked")).unwrap();
        symlink(root.join("safe/current"), root.join("linked-file")).unwrap();
        assert!(read_log_at(&root, Path::new("linked/current")).is_err());
        assert!(read_log_at(&root, Path::new("linked-file")).is_err());
        assert!(read_log_at(&root, Path::new("../safe/current")).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
