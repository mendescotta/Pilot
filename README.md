# Pilot

Pilot is a GTK4 **system service manager for Void Linux**, supporting runit and
dinit as PID 1. User services now live in the independent **Runner** project.

## Interface

Pilot opens directly on system services, with health counts, search and status
filters, a contextual Start/Stop action, Restart, and a backend-specific More
menu. Overview shows configuration and supervisor facts; Definition, Relations
(dinit) / Components (runit), and Logs have separate views. Raw supervisor output
is available under Diagnostics.

Startup and background discovery do not authenticate. Definitions remain
browsable while live status is locked. **Unlock live status** authorizes the
small GTK-free polkit helper. Install the GUI and helper together: this
development version uses protocol **2** and rejects older helpers.

Failed refreshes retain the last confirmed state. Filesystem events and
authorized dinit-monitor notifications invalidate snapshots; a six-second
reconciliation catches missed changes. Background polling never renews the
helper's five-minute authorization lifetime. Navigation stays usable during
service operations, and results for an old selection cannot replace new detail.

## Backend detection

PID 1's executable and process name determine the backend. A secondary dinit,
user runsvdir, installed client, or Turnstile configuration cannot switch Pilot
to another manager. Unsupported systems show their detection evidence.

Dinit search paths and an explicit control socket are read from PID 1's
arguments when available. Otherwise the documented system defaults apply.
Runit uses /etc/sv and the current /var/service target.

The development override is `pilot --dev-backend dinit|runit`. It is visibly
marked; the installed helper still refuses requests for a backend that does not
match PID 1. The former `--dinit` and `--runit` flags are removed.

## Runit

- Available definitions and live-only service directories are both discoverable.
- Running, stopped, paused, finishing, unavailable status, initial-down state,
  supervision links, requested state, PID and uptime remain distinct facts.
- Start/stop/restart/reload wait for runit's result (seven-second timeout).
  Readiness checks, once, pause/continue, signals and force operations are
  available through More.
- Components include run, finish, check, conf, down, control hooks and the
  appendant log service. Main and log supervisor states are independent.
- Enable creates a supervision link; Disable removes only the expected link.
  Neither operation is presented as a boot-wide on/off switch.
- Logs can read the last 256 KiB of current when log/run has one recognizable
  literal svlogd destination beneath /var/log. Shell expressions and arbitrary
  output destinations are not evaluated.

## Dinit

Start/stop with optional pins, restart, wake/release, unpin, reload/unload,
trigger/untrigger, signals, forced lifecycle actions, buffered-log read/clear,
and online dinit-check are available. More also provides an explicit activation
target and typed runtime dependency changes.

Overview separates the configured enable target from evidence that a persistent
dependency exists. Relations distinguish need, milestone and waits-for settings,
including dependency directories and reverse relationships. This presentation
parser does **not** fully expand includes, substitutions or runtime-added
relationships; the UI states that limitation and online dinit-check remains the
validation authority.

Explicit file logs beneath /var/log and buffered output have separate readers.
Pipe logging points to the consumer service.

## Administrator definitions

The + button previews a new runit service or any of dinit's five service types.
Every generated file is shown before installation, including runit down,
log/run and rotation configuration.

The helper validates again, anchors writes to open directory handles, rejects
symlink traversal, stages validation, and publishes without overwriting an
existing name. Runit scripts pass sh -n; dinit descriptions pass an offline,
staged dinit-check using the system search paths. Staged validation uses the
helper's environment; use online Check after installation to validate against the
live manager's environment.

Dinit replacements are limited to Pilot-created definitions that xbps-query
confirms are not package-owned. A hidden .pilot-backup-* hard link preserves the
old file. Reload after replacement; command changes may also require a restart.
Runit changes currently use a new definition name rather than an in-place
editor. Package files are never edited in place.

A new runit logger uses /var/log/pilot/<service>, 1 MB rotation and ten retained
files. Creating a definition does not automatically enable or start it.

## Build and install

Requires Rust, pkg-config, GTK4 development files and polkit. The relevant
manager's tools must already be installed.

```sh
cargo build --release --locked
sudo ./install.sh
pilot
```

Run these commands from the Pilot repository. This installs the application,
matching helper, polkit policy, desktop entry and icon system-wide. Launch Pilot
as your regular user; authorize system operations through its Unlock action.
Install Runner separately for user services.

For development: `cargo run --locked`. `./install.sh --user` registers desktop
integration only; system actions require the installed helper and policy.
Pilot does not install session services or configure Turnstile.

See [installation and upgrades](docs/installation.md) for installed paths,
removal, and the local deployment record.

## Validation

```sh
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
cargo test --no-default-features --locked
cargo test --locked --lib -- --ignored
cargo test --locked --test dinit --test runit -- --ignored
```

Ignored tests require dinit, dinit-check, dinit-monitor, runsvdir, sv and
xbps-query (for the administrator backup test). They
use only temporary definitions, explicit private sockets and private supervisor
directories; they never control the host managers.

See [implementation plan](docs/plans/2026-10-19-system-services-redesign.md).
