@AGENTS.md

# pilot

GTK4 system service manager for dinit and runit (formerly Dinkit), with separate unprivileged UI and small
`pkexec` helper. Read `README.md` and local `DEVNOTES.md` (if present) for behavior and protocol.

- The GUI never runs as root. Keep the helper allowlisted and GTK-free.
- Pilot is system-only. Runner is the independent user-service app in ../runner.
- Treat dinit/runit output and service names as untrusted input. Runit system
  paths are fixed to `/etc/sv` and `/var/service`.
- Run `cargo fmt --check`, `cargo clippy --all-targets --locked -- -D warnings`,
  and `cargo test --locked`. For headless logic, use
  `cargo test --no-default-features`.
- The ignored dinit integration test uses a private temporary daemon. Run it
  when changing service command behavior; it must never use host services.
