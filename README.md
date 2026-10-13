# Pilot

A Rust and GTK4 desktop manager for **dinit and runit**, formerly Dinkit. Inspired by the service-management workflows in [Runkit](https://github.com/Letdown2491/runkit), with runit behavior informed by [QRunSvMgr](https://codeberg.org/ItsZariep/qrunsvmgr). Uses GTK4 directly; libadwaita is not required. There is no systemd backend.

- User and system service managers, searchable and filterable by status, with each scope's backend detected independently (a runit PID 1 with a per-user dinit instance via turnstile is a real, supported case).
- Loaded services plus available definitions from dinit's default directories; a dedicated Dependencies view shows both what a service depends on and what depends on it.
- Start, stop, restart, enable, disable, reload, unpin, unload, signal, ad hoc add-dep/rm-dep, and (user scope) create/delete definitions and set/unset the activation environment.
- Detailed status and buffered logs in a dedicated console panel, with errors displayed in the window; an auto-refresh toggle keeps the list current without a manual click.
- Background command execution keeps the interface responsive.
- System operations authenticate through a small, allowlisted polkit helper; the auto-refresh timer never triggers its own authentication prompt.

## Build and run

Requires Rust, GTK4 development files, pkg-config, polkit, and either dinit or runit. On Void:

```sh
sudo xbps-install -S rust cargo pkg-config gtk4-devel polkit
cargo build --release --locked
cargo run --locked
```

Pilot detects dinit or runit from PID 1 (`dinit`/`dinit-init` or `runit`/`runit-init`); use `pilot --dinit` or `pilot --runit` to force both scopes to one backend. Absent an override, the user scope's backend is detected independently of PID 1: if a live per-user dinit instance is found (e.g. started by turnstile) it's used for User scope even when the system init is runit, otherwise User scope matches the system backend. Dinit starts in user scope. A running user dinit instance is required for user operations — on Void this is provided by [turnstile](https://github.com/chimera-linux/turnstile) (`xbps-install turnstile`; see DEVNOTES.md for the config and a packaging gap it needs worked around), which starts one automatically at login. Pilot itself doesn't install or configure turnstile; it just talks to whatever user instance is listening at dinit's default socket path. `make install` ships the empty `/usr/lib/dinit.d/user/boot.d` directory turnstile's dinit backend expects to exist. Runit starts in system scope. For system mode install the helper and policy, and run a desktop polkit authentication agent:

```sh
sudo make install
pilot
```

Build before installation. `make install DESTDIR=/path/to/package` stages package files. The helper and policy have fixed `/usr` paths and must be installed root-owned, with no user write permission. The GUI never needs to run as root. The first system action spawns the helper through `pkexec` and keeps it alive for the rest of the session (same pattern as caerus), so later system actions in that session do not re-authenticate; quitting the app ends the session. User actions do not authenticate.

## Runit behavior

On Void, Pilot lists available definitions in `/etc/sv` and enabled services in `/var/service`. Start, stop, restart, Send HUP and status use `/usr/bin/sv`; enable creates a link from `/var/service/<name>` to `/etc/sv/<name>`, while disable removes only a link that points to the expected definition. Removing the link makes runsvdir stop supervising the service. System changes pass through Pilot's allowlisted polkit helper. Pilot does not talk to runit's control FIFOs directly.

For user scope, Pilot uses `~/.local/share/sv` for definitions and `~/.local/service` for the active links, following QRunSvMgr's defaults. A user `runsvdir` must already supervise `~/.local/service`; Pilot does not start one. The Add form writes a previewed executable `run` script, with an optional `down` file so the service exists but won't start automatically (`runsv(8)`) until manually started. Delete only removes a disabled user service consisting solely of the `run` script. Start now also enables the service. These paths are conventions, not a runit standard, so a different user service layout is not yet supported. Runit log locations are service-specific; the buffered-log and unpin controls are hidden in runit mode.

`sv status`'s own text already distinguishes more than Started/Stopped — Pilot now surfaces `finish:` (the finish script is running after the main process exited) as a distinct state, and promotes `, paused` (SIGSTOP'd) over the base state since a paused process still holds a PID and would otherwise look identical to a healthy one.

## Dinit behavior

Enable/disable delegates to dinit's persistent dependency commands, honoring `@meta enable-via` and defaulting to `boot`; selecting a service shows which target it would actually enable via. Enable can start a service; disable does not guarantee that it stops. The app asks before stop, restart, and disable, and never passes `--force`. Lifecycle commands use `--no-wait`, then poll briefly in the background so the refresh afterward shows the settled state instead of "Starting"/"Stopping". Status queries may load an available service into dinit.

Buffered logs require `log-type = buffer`; file and external logger output is not read. Discovery supports default service directories; managers started with custom directories can display their loaded services, but unloaded custom definitions are not discovered. A definition found only on disk shows "Unknown" until a successful listing confirms it's genuinely not loaded (rather than guessing based on the listing having failed). Socket override environment variables are deliberately removed from requests; custom control sockets are not yet supported.

User services only: "Add…" creates a minimal `type = process` definition in `~/.config/dinit.d` (name, command, buffered/no log, enable-via, optional start-now/enable-now), always showing the exact file text before writing it. "Delete" is enabled once a service is stopped; it runs `dinitctl unload` and only removes the file if that succeeds, so an enabled or still-depended-on service fails with dinit's own error instead of Pilot guessing. There's still no general editor for an existing definition, but "Enable buffered log" (user scope) is a narrow, targeted exception: it adds or replaces the `log-type` line and runs `dinitctl reload`, which works on an already-loaded service since `log-type` isn't among the settings `reload` documents as unchangeable while running. No `bgprocess`/`scripted`/`internal`/`triggered` service types in the Add form.

Dependency relationships have no dinitctl query (only `add-dep`/`rm-dep` to modify them), so the Dependencies view reads them directly from definition files: `depends-on`/`depends-ms`/`waits-for` lines and their `.d` directory variants, the same mechanism `dinitctl enable` itself uses, so an enabled-via relationship shows up there too. "Depended on by" is a real scan of every definition file in the backend's search path, done on demand when the dialog opens, not continuously.

## Dinit features

Implemented:

- List/discover services for both user and system scope, merging dinit's live state with on-disk definitions it hasn't loaded yet; filterable by status alongside the existing text search.
- Status (`status`) and buffered log (`catlog`) viewing in a dedicated console panel.
- Start / Stop / Restart, with a confirmation dialog for the disruptive ones and a brief background poll afterward so the display shows the settled state. The ten-character list indicator's compound transitions (e.g. "starting, will stop once started") are surfaced as a note, not collapsed into plain Starting/Stopping.
- Enable / Disable (with the resolved `@meta enable-via` target shown for the selected service) and Reload.
- Unpin (releases a pin created some other way — Pilot itself never pins anything). Pin state isn't queryable through dinitctl at all (only settable), so this stays always-available rather than gated on pin status.
- Signal: sends an arbitrary signal to a service's process.
- Ad hoc `add-dep`/`rm-dep` (need/milestone/waits-for), separate from the persistent enable-via mechanism, plus a read-only view of a service's existing dependencies and dependents (read from definition files; dinitctl has no query for either).
- `setenv`/`unsetenv` for the activation environment — user scope only; see "Not implemented".
- Add a new `type = process` user service via a guided form with a live, editable-field preview of the exact file about to be written.
- "Enable buffered log" on an *existing* user-scope service: adds/replaces its `log-type` setting and reloads it live.
- Delete a user service: gated to stopped services, runs `unload` first and only removes the file if that succeeds.
- Auto-refresh: a toggle that periodically re-lists services (paused while a manual action is in flight), updating only the rows that actually changed rather than rebuilding the list — and never spawning its own privileged-helper authentication, even if an existing System-scope session has died.
- System-scope privileged actions via a single `pkexec`-authenticated helper session kept alive for the app's lifetime (one authentication per session, not per click); the helper independently re-validates `signal`/`add-dep`/`rm-dep` rather than trusting the GUI wrapper, and refuses `setenv`/`unsetenv` for the system instance unconditionally.
- Headless build (`--no-default-features`) of the core logic and helper, with no GTK dependency, for testing or a minimal install.

Not implemented:

- System-scope add/delete, and system-scope `setenv`/`unsetenv`/"Enable buffered log" (all would need new privileged-helper write actions; the environment case specifically would let an authenticated session set `LD_PRELOAD` or similar for every system service, so it's refused outright rather than merely unimplemented).
- Viewing `log-type = file` logs (only `buffer`-type logs are readable today; `file` and `pipe` show nothing).
- True event-driven updates: auto-refresh is polling (every 5s, user-adjustable pause only), not a push notification via dinit's control socket. `dinitctl` itself has no subscribe/watch command; real push would mean linking Chimera Linux's `libdinitctl` (a new C dependency and `unsafe` FFI boundary) — not done, revisit only if polling proves insufficient.
- Querying whether a service is pinned (only `--pin`/`unpin` are documented; nothing reads the state back).
- General editing of an existing definition (Add only creates new ones; "Enable buffered log" is a narrow, single-field exception).
- Service types other than `process` in the Add form (`bgprocess`, `scripted`, `internal`, `triggered`).
- Custom control sockets (the `DINIT_SOCKET_PATH`/`DINIT_CS_FD` env vars are deliberately stripped from every request).
- Bulk/multi-service actions.
- Installing or configuring the user-instance starter (turnstile) itself — that's a one-time OS-level setup step, not something Pilot's code does.

## Validation

```sh
cargo fmt --check
cargo test --locked
cargo clippy --all-targets --locked -- -D warnings
cargo test --test dinit -- --ignored
```

The last test runs a private dinit daemon in a temporary directory and checks listing, start/status/logs, restart, enable/disable and stop. It does not touch host services. For a headless core/helper build use `cargo test --no-default-features`.
