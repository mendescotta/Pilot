# Pilot

A Rust and GTK4 desktop manager for **dinit and runit**, formerly Dinkit. Inspired by the service-management workflows in [Runkit](https://github.com/Letdown2491/runkit), with runit behavior informed by [QRunSvMgr](https://codeberg.org/ItsZariep/qrunsvmgr). Uses GTK4 directly; libadwaita is not required.

- User and system service managers, searchable and filterable by status. Each scope's backend is detected independently, so a runit system with a per-user dinit instance (via turnstile) works correctly.
- Start, stop, restart, enable, disable, reload, unpin, signal, ad hoc add-dep/rm-dep, and (user scope) create/delete service definitions and set/unset the activation environment.
- A Dependencies view shows both what a service depends on and what depends on it.
- Detailed status and buffered logs in a dedicated console panel.
- Selecting a service refreshes only that service's row and detail, not the whole list.
- System actions authenticate once through a small, allowlisted polkit helper for the rest of the session, which drops itself after five minutes idle so a root process never lingers unused.
- Background command execution keeps the interface responsive.

## Install

Prebuilt package (Void Linux, x86_64):

```sh
curl -fLO https://github.com/mendescotta/pilot/releases/download/v0.1.2/pilot-x86_64.xbps
xbps-rindex -a pilot-x86_64.xbps
sudo xbps-install --repository=. -S pilot
```

From source, requires Rust, GTK4 development files, pkg-config and polkit (`sudo xbps-install -S rust cargo pkg-config gtk4-devel polkit` on Void):

```sh
cargo build --release --locked
sudo ./install.sh        # or ./install.sh --user for a no-root, desktop-only install
```

`cargo run --locked` works directly for development, without installing anything.

## Detection and scope

Pilot detects dinit or runit from PID 1 (`dinit`/`dinit-init` or `runit`/`runit-init`); pass `--dinit` or `--runit` to force both scopes to one backend. Otherwise, the user scope's backend is detected independently: a live per-user dinit instance (started by [turnstile](https://github.com/chimera-linux/turnstile)) is used for User scope even when the system init is runit, otherwise User scope matches the system backend.

For dinit, a running user instance handles User-scope operations — on Void that's turnstile (`xbps-install turnstile`), which starts one automatically at login. System-scope actions authenticate through `pkexec` on first use and stay authenticated for the rest of the session.

## Runit

Pilot lists available definitions in `/etc/sv` and enabled services in `/var/service`, using `/usr/bin/sv` for lifecycle actions; enable links `/var/service/<name>` to `/etc/sv/<name>`, and disable removes only a link that points there. For user scope, Pilot uses `~/.local/share/sv` for definitions and `~/.local/service` for active links, matching QRunSvMgr's conventions — a user `runsvdir` must already supervise `~/.local/service`.

On runit system scope, startup discovery lists services without authentication. Use the toolbar Refresh button to authenticate through `pkexec` and load live status from the root-owned supervise sockets.

The Add form writes a previewed executable `run` script, optionally with a `down` file so the service exists but won't start automatically until started manually. `sv status`'s own text already distinguishes more than Started/Stopped — Pilot surfaces `finish:` (the finish script is running) as its own state, and promotes `, paused` (SIGSTOP'd) over the base state.

## Dinit

Enable/disable delegates to dinit's persistent dependency commands, honoring `@meta enable-via` (defaulting to `boot`), with the resolved target shown for the selected service. Lifecycle commands use `--no-wait`, then poll briefly in the background so the display shows the settled state instead of "Starting"/"Stopping". The list indicator's compound transitions (e.g. "starting, will stop once started") are surfaced as a note rather than collapsed into plain Starting/Stopping.

"Add…" creates a minimal `type = process` user-scope definition with a live preview of the exact file before writing it. "Enable buffered log" turns on buffered logging for an *existing* service by adding or replacing its `log-type` setting and reloading it live. "Delete" runs `dinitctl unload` first and only removes the file if that succeeds.

Dependency relationships (`depends-on`/`depends-ms`/`waits-for` and their `.d` directory variants) are read directly from definition files for the Dependencies view, including what a service depends on and everything that depends on it — the same mechanism `dinitctl enable` itself uses, so an enabled-via relationship shows up too.

## Validation

```sh
cargo fmt --check
cargo test --locked
cargo clippy --all-targets --locked -- -D warnings
cargo test --test dinit -- --ignored
```

The last test runs a private dinit daemon in a temporary directory and checks listing, start/status/logs, restart, enable/disable and stop. It does not touch host services. For a headless core/helper build use `cargo test --no-default-features`.
