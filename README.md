# Core

Shared Rust workspace: nyx-core (identity, paths, preferences, control),
nyx-accounts (GOA/EDS metadata discovery), nyx-sync (WebDAV journal), and nyx-desktop
(Hyprland state, Omarchy workspace modes and panel coordination for the desktop apps).

Check: `cargo check --locked --workspace`. Headless foundation: `cargo check --locked -p nyx-core --no-default-features`.

nyx-desktop: `cargo test -p nyx-desktop` (no windows; the panel test starts a private
D-Bus). `cargo run -p nyx-desktop --example watch` prints the live desktop and its
changes, read-only.

Architecture and public API boundaries: [AGENTS.md](AGENTS.md).
