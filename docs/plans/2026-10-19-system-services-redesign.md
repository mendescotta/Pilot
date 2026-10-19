# Pilot system-services redesign and user-services split

**Status:** core implementation delivered and installed locally; follow-ups recorded below  
**Date:** 2026-10-19


## Implementation checkpoint

The working tree now implements the system-only split, PID 1 evidence and custom
dinit paths/socket discovery, typed backend commands, redesigned GTK views,
privilege protocol 2, filesystem invalidation, dinit-monitor invalidation with
reconciliation, and private supervisor tests.

Runner is an independent sibling repository at ../runner with its own package,
application ID, desktop integration, icon, CI and documentation. Its detection
fixtures cover all five layouts, stale sockets and ambiguous live managers.
Both apps have private dinit and runit lifecycle tests.

Both release builds were installed system-wide on 2026-10-19. The previous
Pilot installation was backed up before removal; the new GUI and protocol-2
helper were installed together. Installed files, ownership, permissions, helper
handshake and both GUI launches were verified. See the
[installation record](../installation.md#local-deployment-record-2026-10-19).
This local deployment does not complete the boot/login validation matrix below.

The first administration implementation creates all five dinit types and
previewed runit definitions with optional down and svlogd files. Directory-bound
staging validates before publication. Dinit replacement is restricted to
Pilot-created, non-package-owned descriptions and preserves a backup.

The following parts of the longer-term design remain follow-ups, not completed
acceptance claims:

- Replace presentation state strings with the full typed ServiceRecord and
  explicit origin/enablement metadata.
- Expand the dinit presentation parser for includes, fragments, substitutions
  and complete effective/runtime relationships. Current views explicitly mark
  their partial interpretation and provide authoritative online validation.
- Add a complete source editor and fragment workflow. Runit currently uses new
  administrator names rather than in-place editing; dinit replacement renders a
  complete reviewed draft rather than a fragment.
- Expand user-manager attribution for several custom dinit sockets/relative
  search paths, and expose deliberate readiness/lingering configuration changes.
- Validate the five layouts in actual boot/login environments; current coverage
  combines injected layout fixtures, isolated supervisors and a local dinit
  desktop visual check.

## Decision

Pilot becomes a system-service manager only. It detects the manager that is
actually running as PID 1, and presents a complete, manager-specific interface
for that system instance. The User/System scope selector and every user-service
operation leave Pilot.

A separate desktop project, named **Runner**, owns per-user dinit and runit instances, Turnstile discovery,
activation environments, user definitions, and login-readiness configuration.
It is a separate repository and package, not a second mode hidden inside Pilot.

This boundary matters because the following are independent facts:

1. which program is PID 1;
2. which manager supervises system services;
3. whether Turnstile coordinates login sessions;
4. which Turnstile backend launches the user's manager;
5. whether that user manager is currently reachable.

Before this redesign, Pilot compressed these facts into `(system_backend, user_backend)`.
That gives a plausible answer for a simple installation, but it cannot describe
the five supported Void layouts without inference and special cases.

## Supported layouts

| Installation | Pilot | Runner |
| --- | --- | --- |
| runit PID 1 only | Manage the system runsvdir | Report that no session-scoped manager was found; optionally discover a separately configured user `runsvdir` |
| dinit PID 1 only | Manage the system dinit instance | Report that no user dinit instance is reachable; optionally discover an independently started user instance |
| runit PID 1 + Turnstile/runit | Manage system runit | Manage Turnstile's runit directory and environment directory |
| dinit PID 1 + Turnstile/dinit | Manage system dinit | Manage Turnstile's user dinit instance |
| runit PID 1 + dinit + Turnstile/dinit | Manage system runit | Manage Turnstile's user dinit instance |

Turnstile is a session coordinator and user-manager launcher, not another
service-control backend. Its current source has distinct
[runit](https://raw.githubusercontent.com/chimera-linux/turnstile/master/backend/runit)
and
[dinit](https://raw.githubusercontent.com/chimera-linux/turnstile/master/backend/dinit)
backends. The runit backend defaults to `~/.config/service` and a shared
`~/.config/service-env`; the dinit backend constructs login targets and launches
`dinit --user` with user and system definition directories. The application
model must preserve that difference.

The upstream Turnstile README still says that only its dinit backend is
available, while its current source tree contains a runit backend and Void's
handbook documents it. Detection must use installed configuration and live
runtime evidence instead of README-era assumptions.

## Problems in Pilot 0.1.2

### Product model

- PID 1 detection returns a `Backend`, while user probing returns another value
  of the same type. The type cannot say *why* a backend was chosen, where its
  control endpoint is, whether it is reachable, or whether Turnstile owns it.
- `--dinit` and `--runit` force both scopes together. This creates an impossible
  model for the runit-PID-1/dinit-user layout.
- A live user dinit probe wins over all other user configurations. Pilot does
  not read Turnstile's configured backend or detect its runit service root.
- The runit user paths in Pilot (`~/.local/share/sv` and
  `~/.local/service`) do not match Turnstile's default
  `~/.config/service`, and do not match the simpler Void handbook example of
  `~/service` either. There is no single correct user runit path to guess.
- `Scope` branches appear throughout control, filesystem, privilege, and UI
  code. Adding one more layout increases cross-product complexity.

### Backend completeness

- Runit exposes only status, start, stop, restart, HUP, enable, and disable.
  It omits `once`, pause/continue, the supported signals, force-stop/reload/
  restart/shutdown, readiness checks, the appendant log service, custom control
  scripts, and the distinction between requested state and current process
  state. These operations are part of `sv` and `runsv`, not extensions invented
  by the UI ([sv(8)](https://www.smarden.org/runit/sv.8),
  [runsv(8)](https://www.smarden.org/runit/runsv.8)).
- Runit discovery treats `/var/service` as a fixed active directory without
  describing the current runsvdir or its link target. Void exposes the active
  runsvdir through `/var/service`, while persistent boot links may live in
  `/etc/runit/runsvdir/default`
  ([Void service management](https://docs.voidlinux.org/config/services/index.html)).
- Dinit has broad command coverage but its model is still string-based. It
  does not expose service type, restart policy, readiness, timeouts, log mode,
  loaded-versus-defined state as separate fields, service arguments, triggers,
  `wake`, `release`, or a clear pin model.
- Dinit dependency parsing reads only three directive families from top-level
  files. Dinit supports five service types, three dependency semantics,
  templates, substitutions, includes/fragments, readiness, restart limits,
  sockets, resource limits, and several log modes
  ([dinit-service(5)](https://davmac.org/projects/dinit/man-pages-html/dinit-service.5.html)).
- Configuration is never checked with `dinit-check`, even though it can detect
  syntax errors, invalid values, and dependency cycles using the live instance's
  directories and environment
  ([dinit-check(8)](https://davmac.org/projects/dinit/man-pages-html/dinit-check.8.html)).
- Refresh is mostly command-driven. State changes made outside Pilot are easy
  to miss.

### Interface

The current screen has a scope selector, search, state selector, refresh,
Environment, and Add in one toolbar. The detail side then gives the same visual
weight to six lifecycle buttons, four specialist actions, and a large raw-text
console. The result looks like an internal command harness:

- there is no system overview or count of failed services;
- running state and persistent enablement are mixed into terse labels;
- valid primary actions do not stand out from rare or destructive actions;
- details are mostly a path and command output rather than a structured service
  record;
- backend-specific concepts occupy permanent space even when they are not
  useful;
- the whole body becomes insensitive during a job;
- an unavailable privileged status channel produces a list that looks partly
  authoritative and partly guessed.

## Target architecture for Pilot

### Detection produces evidence, not a guess

Introduce a system-only profile:

```text
SystemManagerProfile
  pid1
    pid
    comm
    executable_path
  manager
    Dinit | Runit
  evidence[]
  control
    endpoint
    access: Available | AuthenticationRequired | Unavailable(reason)
  paths
    definition_roots[]
    active_root?
    boot_root?
  capabilities
  diagnostics[]
```

Detection order:

1. Read `/proc/1/comm` and `/proc/1/exe`. If `comm` is a generic `init`, resolve
   the executable basename and retain both values as evidence.
2. Match explicit aliases for dinit and runit. PID 1 is the authority for
   Pilot; a non-PID-1 dinit or `runsvdir` does not change the result.
3. Locate required client tools and known system paths. These checks refine
   availability and diagnostics; they never change a confirmed PID 1 backend.
4. Probe the control surface without mutating it. If permissions prevent the
   probe, return `AuthenticationRequired` rather than `Unavailable` or an empty
   service list.
5. If PID 1 is unsupported, open a useful unsupported-system page containing
   the collected evidence. Do not ask users to choose a backend in the normal
   application flow.

Keep a development-only backend override, renamed to make the risk clear, for
fixtures and non-booted chroots. It applies only to Pilot's system backend and
the UI must show an “overridden” diagnostic.

Make `/proc`, executable paths, tool paths, and service roots injectable in
tests. Detection tests then cover generic `/sbin/init` links, missing tools,
permission-denied control sockets, stale sockets, and unsupported PID 1 without
touching the host.

### Typed backend contract

Replace action strings and scattered backend branches with typed requests and
capabilities. Avoid forcing both managers through operations that only one can
perform.

```text
SystemServiceBackend
  profile()
  snapshot()
  inspect(ServiceId)
  execute(ServiceCommand)
  validate_definition(ServiceId or draft)
  subscribe(EventSink)
```

`ServiceRecord` carries common facts:

- stable identifier and display name;
- runtime state and pending transition;
- requested state;
- persistent enablement state;
- PID, uptime, last failure, and readiness when available;
- definition location and origin (administrator, package, runtime);
- log provider and availability;
- backend-specific detail data;
- the exact capabilities valid for this record.

Use enums such as `LifecycleCommand`, `RunitCommand`, and `DinitCommand` all
the way through the UI/helper boundary. The helper validates one typed request
at the privilege boundary and maps it to fixed binaries and roots. Version the
wire protocol before adding definition writes or event subscriptions.

Suggested modules:

```text
src/
  app.rs
  detection.rs
  domain.rs
  jobs.rs
  backend/
    mod.rs
    dinit/
      client.rs
      definitions.rs
      events.rs
      parser.rs
    runit/
      client.rs
      definitions.rs
      logs.rs
      status.rs
  privilege/
    client.rs
    protocol.rs
  ui/
    window.rs
    service_list.rs
    service_detail.rs
    dialogs.rs
src/bin/
  pilot-helper.rs
```

GTK remains outside the domain/backend modules so fixture and private-daemon
tests stay headless.

### Reliable refresh

- Build one immutable snapshot per refresh, then apply a diff to rows. Never
  merge failed live queries into guessed runtime states.
- For dinit, use `dinit-monitor` as an invalidation signal after authentication,
  then re-query authoritative detail. It can report started, stopped, and failed
  changes, but its own manual says notifications can be skipped under load, so
  keep a low-frequency reconciliation refresh
  ([dinit-monitor(8)](https://davmac.org/projects/dinit/man-pages-html/dinit-monitor.8.html)).
- For runit, watch the active runsvdir and supervise status/control entries for
  invalidation, while retaining periodic reconciliation because runsvdir scans
  directory changes on its own interval
  ([runsvdir(8)](https://www.smarden.org/runit/runsvdir.8)).
- Refresh the selected record immediately after a command and retain its
  transition state until the supervisor reports a settled result or a visible
  timeout occurs.
- A failed refresh leaves the last confirmed snapshot visible with a stale
  banner and timestamp. It does not rewrite states to `Unknown`.

## Backend feature plan

### Runit system backend

The runit UI should model supervision rather than imitate dinit dependencies.
Runit intentionally composes behavior through service directories, `run`,
`finish`, `check`, `control/*`, and optional `log` services.

Required first release:

- discover definitions in `/etc/sv`, active links/directories in the resolved
  `/var/service`, and persistent links in the current/default runsvdir;
- distinguish Available, Supervised, Up, Down, Finish, Paused, Want up, Want
  down, and status unavailable;
- show PID and uptime from `sv status`/supervise data;
- implement up, down, once, pause, continue, HUP, ALRM, INT, QUIT, USR1,
  USR2, TERM, and KILL;
- implement the waiting `sv` actions start, stop, restart, reload and their
  force variants with explicit timeout feedback;
- run and show the result of the service's `check` program when present;
- show the `down` file separately from supervision and boot enablement;
- show `run`, `finish`, `check`, `conf`, `control/*`, and the appendant `log`
  service as structured definition components;
- report the log supervisor independently from the main process;
- read `svlogd` output only when its destination can be determined safely.

Do not invent a dependency graph for runit. If a script calls `sv`, show that
as a detected script reference with a caveat, not as an authoritative
dependency. Runit's own dependency guidance uses service behavior and scripts
rather than a manager-maintained graph
([runit dependencies](https://www.smarden.org/runit/dependencies)).

Later administration features:

- create a custom service directory with exact file previews;
- copy a package definition to an administrator-owned name before editing,
  matching Void's recommendation rather than editing package files in place;
- add an appendant `svlogd` service and rotation settings;
- transactional edits through the helper, limited to fixed roots and guarded
  against symlink traversal.

### Dinit system backend

Required first release:

- discover the system search path in precedence order and retain definition
  origin; dinit's documented defaults include `/etc/dinit.d`, `/run/dinit.d`,
  `/usr/local/lib/dinit.d`, and `/lib/dinit.d`
  ([dinit(8)](https://davmac.org/projects/dinit/man-pages-html/dinit.8.html));
- distinguish defined, loaded, started, stopped, failed, skipped, starting,
  stopping, and compound pending transitions;
- show service type, activation/pin state, PID, exit/failure information,
  restart policy, timeouts, readiness, log mode, and definition path;
- implement start/stop with optional pins, restart, wake, release, unpin,
  reload, unload, trigger/untrigger, signals, and buffered-log read/clear as
  capability-gated actions from the documented `dinitctl` surface
  ([dinitctl(8)](https://davmac.org/projects/dinit/man-pages-html/dinitctl.8.html));
- show persistent enablement through its actual `enable-via`/`--from` target;
- present need, milestone, and waits-for relationships distinctly, including
  reverse relationships and directory-based dependencies;
- parse service arguments and include/meta commands without treating a
  partially parsed file as complete;
- run online `dinit-check` before presenting a definition as valid and after
  preparing any edit;
- support buffer, file, pipe-consumer, and no-log states. `catlog` applies only
  to buffer logging; file and pipe logging need different views.

The normal UI does not expose system shutdown. Pilot manages services; machine
power actions belong in the desktop/session shell and require different safety
and authorization treatment.

Later administration features:

- create all five service types using type-specific forms and an exact preview;
- edit an administrator-owned override or fragment;
- choose an enable target explicitly;
- validate a staged definition with `dinit-check`, atomically install it, then
  reload/unload as appropriate;
- preserve a recoverable backup and show which settings require a restart.

## New interface

Pilot opens directly on **System services**. There is no scope dropdown.

```text
┌ Pilot · System services              dinit PID 1 · Connected   ⟳  ⋮ ┐
│  42 running   1 failed   6 stopped   51 definitions               │
├─────────────────────────────┬──────────────────────────────────────┤
│ Search services…      All ▾ │ NetworkManager              Running │
│                             │ Enabled via boot · PID 812 · 3h 14m │
│ ● NetworkManager    Running │                                      │
│ ● dbus              Running │ [Stop] [Restart]                 ⋮   │
│ ! bluetoothd         Failed │                                      │
│ ○ cupsd             Stopped │ Overview  Definition  Relations Logs│
│ …                           │                                      │
│                             │ Type        Process                  │
│                             │ Command     /usr/bin/NetworkManager  │
│                             │ Restart     On failure               │
│                             │ Readiness   Process started          │
│                             │ Definition  /etc/dinit.d/...         │
└─────────────────────────────┴──────────────────────────────────────┘
```

Design rules:

- A compact summary bar gives immediate system health and doubles as status
  filters.
- Each row has a semantic state icon, service name, one concise secondary
  fact, and a separate enablement badge. Color is supplementary, never the
  only state cue.
- The selected service has one primary contextual action (Start or Stop), a
  Restart action when valid, and an overflow menu for signals, pins, force
  operations, validation, and other specialist commands.
- Overview is structured data. Raw supervisor output moves to a diagnostic
  expander instead of occupying the main pane.
- Definition uses a read-only source viewer initially, with origin and
  validation results above it.
- Relations appears only for dinit. For runit, the corresponding place is a
  Components view for `run`, `finish`, `check`, `control`, and `log`.
- Logs adapts to the provider. It explains why no log is available instead of
  offering a button that can only fail.
- Operations disable only the affected row and controls. Search, navigation,
  logs, and other service rows remain usable.
- Authentication-required state is explicit: definitions remain browsable and
  the summary offers “Unlock live status”. After authorization, the helper's
  five-minute authorization deadline is renewed by explicit privileged actions,
  while passive refreshes do not extend it.
- Empty, unsupported, disconnected, stale, and permission-denied states each
  have dedicated copy and recovery actions.
- Use GTK's native theme, symbolic icons, consistent 6/12/18 px spacing,
  restrained cards, and a small application CSS layer. Avoid hard-coded dark
  colors and theme-specific assumptions.

## Runner project

Runner gets the code and concepts that Pilot removes, but starts with a
new detection model rather than copying `Scope::User` branches.

```text
UserServiceProfile
  coordinator
    Turnstile | Standalone | None
  coordinator_evidence[]
  configured_backend?
  live_manager
    Dinit { socket, definition_roots[] }
    Runit { services_root, environment_root, supervisor_pid? }
    None
  ownership
    session_scoped | lingering | boot_scoped | unknown
  diagnostics[]
```

Detection combines, without conflating:

- readable Turnstile configuration and installed backend files;
- whether the Turnstile daemon/PAM integration appears installed;
- the live user dinit socket and a successful user-instance query;
- a same-UID `runsvdir` supervising the configured directory plus live
  supervise state;
- the standard defaults only as candidates, never proof of a running manager.

Backend behavior:

- Dinit uses `dinitctl --user`, the live instance's directories, login targets,
  user activation environment, and `~/.config/dinit.d/boot.d` readiness links.
- Turnstile/runit uses `~/.config/service` and
  `~/.config/service-env` by default, exposes `turnstile-ready/conf`, and wraps
  environment-aware services with `chpst -e "$TURNSTILE_ENV_DIR"` as documented
  by the
  [Void user-services guide](https://docs.voidlinux.org/config/services/user-services.html).
- Environment changes call the installed
  `turnstile-update-runit-env` interface when present instead of directly
  reproducing its envdir encoding
  ([upstream helper](https://github.com/chimera-linux/turnstile/blob/master/backend/turnstile-update-runit-env.in)).
- Normal service operations never require root. System Turnstile configuration
  is initially diagnostic/read-only; editing `/etc/turnstile` is a later,
  separately authorized feature.

Keep the projects release-independent. Do not create a runtime dependency in
either direction. If parser duplication becomes costly after both applications
stabilize, extract a small versioned crate with fixture-tested parsers; do not
start the split by designing a shared framework.

## Privilege and write safety

- The GUI remains unprivileged.
- The helper accepts typed, versioned, allowlisted requests only.
- Every system path is selected by the backend, never supplied freely by the
  GUI.
- Definition reads use no authentication when filesystem permissions permit.
- Live system status and actions authenticate on an explicit user gesture when
  the supervisor endpoint requires it.
- Future writes reject symlink components, use `openat`-style directory-bound
  resolution, stage a same-filesystem temporary file/directory, preserve mode
  and ownership deliberately, validate before commit, and rename atomically.
- Package-owned definitions are not edited in place.
- Destructive and force actions show the exact service, backend semantics, and
  consequence. Routine start/stop/restart actions remain one click.

## Delivery sequence

### 1. Freeze behavior with fixtures

- Capture representative dinit list/status/check output and runit status for
  up, down, finish, paused, failed-check, log-service, permission-denied, and
  missing-supervisor states.
- Add injectable filesystem/process probes and detection fixtures.
- Add a private `runsvdir` integration test beside the existing private dinit
  test.

**Exit:** the old behavior is reproducible without depending on the host PID 1.

### 2. Make Pilot system-only

- Introduce `SystemManagerProfile` and typed capabilities.
- Remove `Scope`, user backend probing, the scope selector, Environment, Add,
  Delete, and all user-definition writes from Pilot.
- Replace force-both CLI flags with the development-only system override.
- Update package metadata, README, desktop description, and polkit policy copy.

**Exit:** both runit-PID-1 and dinit-PID-1 installations select exactly one
backend from recorded evidence; no user service path remains in Pilot.

### 3. Complete the runit vertical slice

- Implement rich discovery/status, full safe `sv` commands, readiness checks,
  component inspection, log-supervisor status, and filesystem invalidation.
- Build the new list, summary, Overview, Components, and Logs UI against it.

**Exit:** common runit service work no longer requires a terminal, and every
displayed state has a fixture or integration test.

### 4. Complete the dinit vertical slice

- Add the typed service model, full lifecycle/activation operations, online
  validation, relationship model, log providers, and monitor-driven refresh.
- Build Overview, Definition, Relations, Logs, and contextual advanced actions.

**Exit:** Pilot represents dinit concepts directly rather than reducing them to
runit-like Start/Stop/Enabled labels.

### 5. Create Runner

- Start a new repository with its own application ID, package, README, and CI.
- Port only the useful user parsers and operations with history where practical;
  do not copy Pilot's scope/backend coupling.
- Implement Turnstile configuration/runtime evidence and both user backends.
- Test the three Turnstile layouts and the two no-Turnstile diagnostic states.

**Exit:** all five layouts in the matrix produce the correct system and user
manager identities without a manual backend switch.

### 6. Add safe definition administration

- Implement backend-specific creation/editing and previews.
- Add transactional helper writes and validation.
- Add runit log-service creation and dinit type-specific forms.

**Exit:** administration is recoverable, validated, and never modifies package
definitions in place.

## Acceptance criteria

- Pilot has no User scope, Turnstile probe, user socket, or home-directory
  service path.
- PID 1 detection reports its evidence and never changes backend because a
  secondary manager happens to be running.
- Permission denial, disconnected supervisor, undefined service, unloaded
  service, disabled service, and stopped service are distinct states.
- Runit and dinit expose separate capability sets and backend-native wording.
- External state changes appear promptly and reconciliation repairs missed
  notifications.
- The initial screen communicates backend identity, connection/authentication
  state, service health counts, and failures without selecting a row.
- The primary action for a selected service is visually clear; specialist
  actions do not crowd the main layout.
- Pilot's dinit and runit private-daemon tests pass without touching host
  services.
- Runner correctly identifies all five installation layouts from live
  evidence and configuration, including runit PID 1 with Turnstile/dinit.
- Both projects pass formatting, clippy with warnings denied, unit tests, and
  their private-manager integration tests.

## Source notes

- [Turnstile project and session lifecycle](https://github.com/chimera-linux/turnstile)
- [Turnstile runit backend](https://raw.githubusercontent.com/chimera-linux/turnstile/master/backend/runit)
- [Turnstile dinit backend](https://raw.githubusercontent.com/chimera-linux/turnstile/master/backend/dinit)
- [Dinit overview and instance paths](https://davmac.org/projects/dinit/man-pages-html/dinit.8.html)
- [Dinit service descriptions](https://davmac.org/projects/dinit/man-pages-html/dinit-service.5.html)
- [Dinit configuration checker](https://davmac.org/projects/dinit/man-pages-html/dinit-check.8.html)
- [Dinit control surface](https://davmac.org/projects/dinit/man-pages-html/dinitctl.8.html)
- [Dinit monitoring](https://davmac.org/projects/dinit/man-pages-html/dinit-monitor.8.html)
- [Runit overview](https://smarden.org/runit/)
- [Void runit services](https://docs.voidlinux.org/config/services/index.html)
- [Void user services and Turnstile](https://docs.voidlinux.org/config/services/user-services.html)
- [Void logging](https://docs.voidlinux.org/config/services/logging.html)
- [Chimera service management](https://chimera-linux.org/docs/configuration/services)
- [Void session management](https://docs.voidlinux.org/config/session-management.html)
