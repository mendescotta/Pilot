//! Backend-neutral identity and typed operations. Backend capabilities remain explicit.
use std::str::FromStr;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    Dinit,
    Runit,
}

impl Backend {
    pub fn label(self) -> &'static str {
        match self {
            Self::Dinit => "Dinit",
            Self::Runit => "Runit",
        }
    }
    pub fn wire(self) -> &'static str {
        match self {
            Self::Dinit => "dinit",
            Self::Runit => "runit",
        }
    }
    pub fn actions(self) -> Vec<Action> {
        Action::ALL
            .iter()
            .copied()
            .filter(|a| a.supported(self))
            .collect()
    }
}
impl FromStr for Backend {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "dinit" => Ok(Self::Dinit),
            "runit" => Ok(Self::Runit),
            _ => Err(format!("Unsupported backend: {s}")),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Start,
    Stop,
    Restart,
    Enable,
    Disable,
    Reload,
    Unpin,
    Unload,
    Wake,
    Release,
    Trigger,
    Untrigger,
    StartPinned,
    StopPinned,
    ClearLog,
    Check,
    Once,
    Pause,
    Continue,
    Hup,
    Alarm,
    Interrupt,
    Quit,
    User1,
    User2,
    Terminate,
    Kill,
    ForceStop,
    ForceRestart,
}
impl Action {
    pub const ALL: &'static [Self] = &[
        Self::Start,
        Self::Stop,
        Self::Restart,
        Self::Enable,
        Self::Disable,
        Self::Reload,
        Self::Unpin,
        Self::Unload,
        Self::Wake,
        Self::Release,
        Self::Trigger,
        Self::Untrigger,
        Self::StartPinned,
        Self::StopPinned,
        Self::ClearLog,
        Self::Check,
        Self::Once,
        Self::Pause,
        Self::Continue,
        Self::Hup,
        Self::Alarm,
        Self::Interrupt,
        Self::Quit,
        Self::User1,
        Self::User2,
        Self::Terminate,
        Self::Kill,
        Self::ForceStop,
        Self::ForceRestart,
    ];
    pub fn wire(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Stop => "stop",
            Self::Restart => "restart",
            Self::Enable => "enable",
            Self::Disable => "disable",
            Self::Reload => "reload",
            Self::Unpin => "unpin",
            Self::Unload => "unload",
            Self::Wake => "wake",
            Self::Release => "release",
            Self::Trigger => "trigger",
            Self::Untrigger => "untrigger",
            Self::StartPinned => "start-pinned",
            Self::StopPinned => "stop-pinned",
            Self::ClearLog => "clear-log",
            Self::Check => "check",
            Self::Once => "once",
            Self::Pause => "pause",
            Self::Continue => "continue",
            Self::Hup => "hup",
            Self::Alarm => "alarm",
            Self::Interrupt => "interrupt",
            Self::Quit => "quit",
            Self::User1 => "user1",
            Self::User2 => "user2",
            Self::Terminate => "terminate",
            Self::Kill => "kill",
            Self::ForceStop => "force-stop",
            Self::ForceRestart => "force-restart",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::StartPinned => "Start and pin",
            Self::StopPinned => "Stop and pin",
            Self::ClearLog => "Clear buffered log",
            Self::Check => "Check readiness / definition",
            Self::ForceStop => "Force stop",
            Self::ForceRestart => "Force restart",
            Self::User1 => "Send USR1",
            Self::User2 => "Send USR2",
            Self::Hup => "Send HUP",
            Self::Alarm => "Send ALRM",
            Self::Interrupt => "Send INT",
            Self::Quit => "Send QUIT",
            Self::Terminate => "Send TERM",
            Self::Kill => "Send KILL",
            Self::Start => "Start",
            Self::Stop => "Stop",
            Self::Restart => "Restart",
            Self::Enable => "Enable",
            Self::Disable => "Disable",
            Self::Reload => "Reload",
            Self::Unpin => "Unpin",
            Self::Unload => "Unload",
            Self::Wake => "Wake",
            Self::Release => "Release",
            Self::Trigger => "Trigger",
            Self::Untrigger => "Untrigger",
            Self::Once => "Run once",
            Self::Pause => "Pause",
            Self::Continue => "Continue",
        }
    }
    pub fn supported(self, backend: Backend) -> bool {
        match self {
            Self::Once | Self::Pause | Self::Continue => backend == Backend::Runit,
            Self::Unpin
            | Self::Unload
            | Self::Wake
            | Self::Release
            | Self::Trigger
            | Self::Untrigger
            | Self::StartPinned
            | Self::StopPinned
            | Self::ClearLog => backend == Backend::Dinit,
            _ => true,
        }
    }
    pub fn needs_confirmation(self) -> bool {
        matches!(
            self,
            Self::Enable
                | Self::Disable
                | Self::Unload
                | Self::ClearLog
                | Self::Kill
                | Self::ForceStop
                | Self::ForceRestart
        )
    }
}
impl Action {
    /// What a confirmed action does to the running service, not only to its configuration.
    pub fn consequence(self, backend: Backend) -> &'static str {
        match (self, backend) {
            (Self::Enable, Backend::Dinit) => "Adds a waits-for link from the enable target (boot unless the definition names another) and starts the service now.",
            (Self::Enable, Backend::Runit) => "Links the definition into the active runsvdir. runsvdir starts supervising it within about five seconds; it runs unless a down file is present.",
            (Self::Disable, Backend::Dinit) => "Removes the waits-for link from the enable target and stops the service unless another service still requires it.",
            (Self::Disable, Backend::Runit) => "Removes the supervision link from the active runsvdir. runsvdir then stops the service and its log supervisor; the definition stays available.",
            (Self::ClearLog, _) => "Discard the manager's buffered output for this service.",
            _ => "This operation may interrupt the service and its dependents. Review the selected service before continuing.",
        }
    }
}
impl FromStr for Action {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .iter()
            .copied()
            .find(|a| a.wire() == s)
            .ok_or_else(|| format!("Unsupported action: {s}"))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeState {
    Running,
    Stopped,
    Starting,
    Stopping,
    Failed,
    Skipped,
    Paused,
    Finishing,
    Unloaded,
    Unavailable,
    Unknown,
}
impl RuntimeState {
    pub fn from_label(s: &str) -> Self {
        match s {
            "Started" => Self::Running,
            "Stopped" => Self::Stopped,
            "Starting" => Self::Starting,
            "Stopping" => Self::Stopping,
            "Failed" => Self::Failed,
            "Skipped" => Self::Skipped,
            "Paused" => Self::Paused,
            "Finishing" => Self::Finishing,
            "Not loaded" => Self::Unloaded,
            "Disabled" => Self::Unavailable,
            _ => Self::Unknown,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Service {
    pub name: String,
    pub state: String,
    pub summary: String,
}
impl Service {
    pub fn runtime(&self) -> RuntimeState {
        RuntimeState::from_label(&self.state)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Access {
    Connected,
    AuthenticationRequired,
    Unavailable(String),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DependencyKind {
    Need,
    Milestone,
    WaitsFor,
}
impl DependencyKind {
    pub fn wire(self) -> &'static str {
        match self {
            Self::Need => "need",
            Self::Milestone => "milestone",
            Self::WaitsFor => "waits-for",
        }
    }
}
impl FromStr for DependencyKind {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "need" => Ok(Self::Need),
            "milestone" => Ok(Self::Milestone),
            "waits-for" => Ok(Self::WaitsFor),
            _ => Err("Invalid dependency kind".into()),
        }
    }
}
#[derive(Clone, Debug)]
pub enum DinitRelation {
    Persistent {
        from: String,
        enabled: bool,
    },
    Runtime {
        target: String,
        kind: DependencyKind,
        add: bool,
    },
}
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub services: Vec<Service>,
    pub access: Access,
}
#[derive(Clone, Debug)]
pub struct Inspection {
    pub name: String,
    pub facts: Vec<(String, String)>,
    pub definition: String,
    pub relations: String,
    pub diagnostics: String,
    pub log_available: bool,
    pub log_description: String,
}
impl Inspection {
    pub fn apply_status(&mut self, backend: Backend, text: String) {
        if backend == Backend::Dinit {
            for line in text.lines() {
                if let Some((key, value)) = line.trim().split_once(':') {
                    if matches!(
                        key,
                        "State"
                            | "Activation"
                            | "Process ID"
                            | "Exit status"
                            | "Exit code"
                            | "Termination signal"
                            | "Pinned"
                    ) {
                        self.facts.insert(0, (key.into(), value.trim().into()));
                    }
                }
            }
        } else {
            let main = text.split(';').next().unwrap_or(&text);
            if let Some(rest) = main.split("(pid ").nth(1) {
                if let Some((pid, uptime)) = rest.split_once(')') {
                    self.facts.insert(0, ("Process ID".into(), pid.into()));
                    self.facts.insert(
                        1,
                        (
                            "Uptime".into(),
                            uptime.trim().split(',').next().unwrap_or("").into(),
                        ),
                    );
                }
            }
            self.facts.push((
                "Requested state".into(),
                if main.contains("want down") {
                    "Down"
                } else if main.contains("want up") {
                    "Up"
                } else {
                    "No pending change reported"
                }
                .into(),
            ));
            if let Some((_, log)) = text.split_once(';') {
                self.facts
                    .push(("Log supervisor state".into(), log.trim().into()));
            }
        }
        self.diagnostics = text;
    }
}
