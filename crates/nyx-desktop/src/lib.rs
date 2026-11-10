//! Shared desktop state for the Nyx desktop apps (dock, control center, notification
//! center, desktop widgets).
//!
//! - [`hypr`]: finding the running Hyprland and asking it things (`j/monitors`, …).
//! - [`event`]: Hyprland's event socket, parsed into [`event::Event`]s.
//! - [`model`]: monitors, workspaces and windows, kept current from events.
//! - [`watch`]: a thread that holds the event socket, reconnects when Hyprland restarts
//!   and hands out [`watch::Update`]s. With the `glib` feature, [`watch::watch_local`]
//!   delivers them on the GTK main loop.
//! - [`apps`]: which installed app a window belongs to (grouping by desktop id).
//! - [`actions`]: focus, close, float, maximize, minimize, tag, move: Hyprland
//!   dispatchers for one window.
//! - `capture` (`capture` feature): window thumbnails over `ext-image-copy-capture`.
//! - [`profiles`]: workspace profiles, named sets of workspaces and app placements.
//! - [`omarchy`]: Omarchy's per-workspace mode (tiling, scrolling, floating).
//! - [`virtual_monitors`]: one screen split into two or three monitors by Nyx's Hyprland:
//!   the saved layouts, Hyprland's state and the typed changes.
//! - `panel` (`glib` feature): keeps the Control Center and Notification Center
//!   mutually exclusive over D-Bus.
//!
//! Nothing here polls: state changes come from Hyprland's events, and each process
//! subscribes on its own (no central daemon).

pub mod actions;
pub mod apps;
#[cfg(feature = "capture")]
pub mod capture;
pub mod event;
pub mod hypr;
pub mod model;
pub mod omarchy;
#[cfg(feature = "glib")]
pub mod panel;
pub mod profiles;
pub mod virtual_monitors;
pub mod watch;

pub use model::{
    Desktop, Monitor, USER_WORKSPACES, VirtualHost, VirtualRegion, Window, Workspace, is_user_workspace, user_number,
};
