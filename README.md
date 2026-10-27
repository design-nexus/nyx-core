# Core

Shared Rust workspace: nyx-core (identity, paths, preferences, control),
nyx-accounts (GOA/EDS metadata discovery), and nyx-sync (WebDAV journal).

Check: `cargo check --locked --workspace`. Headless foundation: `cargo check --locked -p nyx-core --no-default-features`.

Architecture and public API boundaries: [AGENTS.md](AGENTS.md).
