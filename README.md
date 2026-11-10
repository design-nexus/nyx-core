# Core

Shared Rust workspace: nyx-core (identity, paths, preferences, control),
nyx-accounts (GOA/EDS metadata discovery), nyx-sync (WebDAV journal), nyx-desktop
(Hyprland state, Omarchy workspace modes and panel coordination for the desktop apps), and
nyx-actions (what apps do for other programs: the contract in
`crates/nyx-actions/docs/CONTRACT.md`, its checks, word resolution, the registry and the
`nyx-actions` command behind `nyx APP …`).

Check: `cargo check --locked --workspace`. Headless foundation: `cargo check --locked -p nyx-core --no-default-features`.

nyx-desktop: `cargo test -p nyx-desktop` (no windows; the panel test starts a private
D-Bus). `cargo run -p nyx-desktop --example watch` prints the live desktop and its
changes, read-only.

nyx-actions: `cargo test -p nyx-actions` (starts a private `dbus-daemon`; nothing stays
running). Control helper: `cargo test -p nyx-core --test control`.

Architecture and public API boundaries: [AGENTS.md](AGENTS.md).
