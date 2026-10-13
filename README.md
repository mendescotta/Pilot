# Pilot

A Rust and GTK4 desktop manager for **dinit and runit**, formerly Dinkit. Inspired by the service-management workflows in [Runkit](https://github.com/Letdown2491/runkit), with runit behavior informed by [QRunSvMgr](https://codeberg.org/ItsZariep/qrunsvmgr). Uses GTK4 directly; libadwaita is not required. There is no systemd backend.

- User and system service managers, searchable service names and states.
- Loaded services plus available definitions from dinit's default directories.
- Start, stop, restart, enable, disable, reload, unpin, unload, and (user scope) create/delete definitions.
- Detailed status and buffered logs, with errors displayed in the window.
- Background command execution keeps the interface responsive.
- System operations authenticate through a small, allowlisted polkit helper.

## Build and run

Requires Rust, GTK4 development files, pkg-config, polkit, and either dinit or runit. On Void:

```sh
sudo xbps-install -S rust cargo pkg-config gtk4-devel polkit
cargo build --release --locked
cargo run --locked
```

Pilot detects dinit or runit from PID 1; use `pilot --dinit` or `pilot --runit` to override detection. Dinit starts in user scope. A running user dinit instance is required for user operations — on Void this is provided by [turnstile](https://github.com/chimera-linux/turnstile) (`xbps-install turnstile`; see DEVNOTES.md for the config and a packaging gap it needs worked around), which starts one automatically at login. Pilot itself doesn't install or configure turnstile; it just talks to whatever user instance is listening at dinit's default socket path. `make install` ships the empty `/usr/lib/dinit.d/user/boot.d` directory turnstile's dinit backend expects to exist. Runit starts in system scope. For system mode install the helper and policy, and run a desktop polkit authentication agent:

```sh
sudo make install
pilot
```

Build before installation. `make install DESTDIR=/path/to/package` stages package files. The helper and policy have fixed `/usr` paths and must be installed root-owned, with no user write permission. The GUI never needs to run as root. The first system action spawns the helper through `pkexec` and keeps it alive for the rest of the session (same pattern as caerus), so later system actions in that session do not re-authenticate; quitting the app ends the session. User actions do not authenticate.

## Runit behavior

On Void, Pilot lists available definitions in `/etc/sv` and enabled services in `/var/service`. Start, stop, restart, Send HUP and status use `/usr/bin/sv`; enable creates a link from `/var/service/<name>` to `/etc/sv/<name>`, while disable removes only a link that points to the expected definition. Removing the link makes runsvdir stop supervising the service. System changes pass through Pilot's allowlisted polkit helper. Pilot does not talk to runit's control FIFOs directly.

For user scope, Pilot uses `~/.local/share/sv` for definitions and `~/.local/service` for the active links, following QRunSvMgr's defaults. A user `runsvdir` must already supervise `~/.local/service`; Pilot does not start one. The Add form writes a previewed executable `run` script, and Delete only removes a disabled user service consisting solely of that script. Start now also enables the service. These paths are conventions, not a runit standard, so a different user service layout is not yet supported. Runit log locations are service-specific; the buffered-log and unpin controls are hidden in runit mode.

## Dinit behavior

Enable/disable delegates to dinit's persistent dependency commands, honoring `@meta enable-via` and defaulting to `boot`; selecting a service shows which target it would actually enable via. Enable can start a service; disable does not guarantee that it stops. The app asks before stop, restart, and disable, and never passes `--force`. Lifecycle commands use `--no-wait`, then poll briefly in the background so the refresh afterward shows the settled state instead of "Starting"/"Stopping". Status queries may load an available service into dinit.

Buffered logs require `log-type = buffer`; file and external logger output is not read. Discovery supports default service directories; managers started with custom directories can display their loaded services, but unloaded custom definitions are not discovered. A definition found only on disk shows "Unknown" until a successful listing confirms it's genuinely not loaded (rather than guessing based on the listing having failed). Socket override environment variables are deliberately removed from requests; custom control sockets are not yet supported.

User services only: "Add…" creates a minimal `type = process` definition in `~/.config/dinit.d` (name, command, buffered/no log, enable-via, optional start-now/enable-now), always showing the exact file text before writing it. "Delete" is enabled once a service is stopped; it runs `dinitctl unload` and only removes the file if that succeeds, so an enabled or still-depended-on service fails with dinit's own error instead of Pilot guessing. No editor for existing definitions, and no `bgprocess`/`scripted`/`internal`/`triggered` service types in the Add form.

## Dinit features

Implemented:

- List/discover services for both user and system scope, merging dinit's live state with on-disk definitions it hasn't loaded yet.
- Search (filters the existing list in place; never rebuilds rows per keystroke).
- Status (`status`) and buffered log (`catlog`) viewing.
- Start / Stop / Restart, with a confirmation dialog for the disruptive ones and a brief background poll afterward so the display shows the settled state.
- Enable / Disable (with the resolved `@meta enable-via` target shown for the selected service) and Reload.
- Unpin (releases a pin created some other way — Pilot itself never pins anything).
- Add a new `type = process` user service via a guided form with a live, editable-field preview of the exact file about to be written.
- Delete a user service: gated to stopped services, runs `unload` first and only removes the file if that succeeds.
- System-scope privileged actions via a single `pkexec`-authenticated helper session kept alive for the app's lifetime (one authentication per session, not per click).
- Headless build (`--no-default-features`) of the core logic and helper, with no GTK dependency, for testing or a minimal install.

Not implemented:

- System-scope add/delete (would need new privileged-helper write actions; user scope only for now).
- Viewing `log-type = file` logs (only `buffer`-type logs are readable today; `file` and `pipe` show nothing).
- Live, event-driven updates (e.g. via `dinit-monitor`) — Pilot only polls: a manual refresh button and a short poll after its own actions, no push notification of changes made elsewhere.
- `setenv`/`unsetenv` (the activation environment dinit exposes to services).
- `signal` (sending an arbitrary signal to a service's process).
- `add-dep`/`rm-dep` (ad hoc dependency edits beyond what enable/disable already covers).
- Editing or viewing the raw definition of an *existing* service (Add only creates new ones).
- Service types other than `process` in the Add form (`bgprocess`, `scripted`, `internal`, `triggered`).
- Custom control sockets (the `DINIT_SOCKET_PATH`/`DINIT_CS_FD` env vars are deliberately stripped from every request).
- Bulk/multi-service actions.
- Installing or configuring the user-instance starter (turnstile) itself — that's a one-time OS-level setup step, not something Pilot's code does.
- Direct `libdinitctl` access: Pilot currently calls `dinitctl`; it does not link against Chimera Linux's `libdinitctl`.

## Validation

```sh
cargo fmt --check
cargo test --locked
cargo clippy --all-targets --locked -- -D warnings
cargo test --test dinit -- --ignored
```

The last test runs a private dinit daemon in a temporary directory and checks listing, start/status/logs, restart, enable/disable and stop. It does not touch host services. For a headless core/helper build use `cargo test --no-default-features`.
