//! Shared desktop state for the Nyx desktop apps (dock, control center, notification
//! center, desktop widgets).
//!
//! - [`hypr`]: finding the running Hyprland and asking it things (`j/monitors`, …).
//! - [`event`]: Hyprland's event socket, parsed into [`event::Event`]s.
//! - [`model`]: monitors, workspaces and windows, kept current from events.
//! - [`watch`]: a thread that holds the event socket, reconnects when Hyprland restarts
//!   and hands out [`watch::Update`]s. With the `glib` feature, [`watch::watch_local`]
//!   delivers them on the GTK main loop.
//! - [`omarchy`]: Omarchy's per-workspace mode (tiling, scrolling, floating).
//! - `panel` (`glib` feature): keeps the Control Center and Notification Center
//!   mutually exclusive over D-Bus.
//!
//! Nothing here polls: state changes come from Hyprland's events, and each process
//! subscribes on its own (no central daemon).

pub mod event;
pub mod hypr;
pub mod model;
pub mod omarchy;
#[cfg(feature = "glib")]
pub mod panel;
pub mod watch;

pub use model::{Desktop, Monitor, USER_WORKSPACES, Window, Workspace, is_user_workspace};
