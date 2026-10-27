# Codespaces

Open this repository in GitHub Codespaces and use the preinstalled stable Rust toolchain. The container installs the project’s system build dependencies at creation time; no prebuild is configured.

Verify changes with:

```sh
cargo fmt --check && cargo clippy --all-targets --locked -- -D warnings && cargo test --locked
```

Codespaces are for development and checks. Do not store release credentials here or publish from an agent session.
