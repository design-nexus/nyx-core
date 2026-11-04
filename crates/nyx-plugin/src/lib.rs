//! Nyx plugins, shared by every host (the bar, the Control Center, desktop widgets) and
//! the `nyx plugin` command.
//!
//! - [`manifest`]: `plugin.toml`, with the same field names as Glimpse's.
//! - [`paths`]: where plugins, their data and the shared files live.
//! - [`discovery`]: the plugins in the three plugin folders, later folders winning.
//! - [`state`]: `~/.config/nyx/plugins.toml`, the parts the user chose for each kind
//!   and the permissions they approved.
//! - [`permissions`]: hashing and describing permissions.
//! - [`install`]: adding, updating, removing, linking and cloning plugins.
//! - [`index`]: the curated plugin index.
//! - [`layout`]: placing parts in the hosts' layout files without losing comments.

pub mod discovery;
pub mod index;
pub mod install;
pub mod layout;
pub mod manifest;
pub mod paths;
pub mod permissions;
pub mod state;

pub use discovery::{Plugin, Trust, discover};
pub use manifest::{Kind, Manifest, Mode, Part, Permissions};
pub use state::State;

/// Whether a plugin may run: trusted plugins always, third-party ones only with a grant
/// for exactly the permissions their manifest asks for now.
pub fn authorized(plugin: &Plugin, state: &State) -> bool {
    plugin.trust.is_trusted() || state.grant_matches(&plugin.manifest)
}
