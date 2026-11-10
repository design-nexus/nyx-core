//! What Nyx apps can do for other programs, described once by each app and used the same
//! way by the `nyx` command, Glimpse and Nyx AI Core. The contract is
//! `docs/CONTRACT.md`.
//!
//! - [`spec`]: an app's description (its `Describe` answer and installed manifest).
//! - [`check`]: whether a caller may run an action with these arguments, shared by the
//!   registry and the apps' control helper (`nyx_core::control`), which has the last word.
//! - [`catalog`] and [`resolve`]: what is on offer, and turning typed words into a call.
//! - `registry` (feature `registry`): manifests, running apps, activation and routing.
//! - `server` (feature `server`): the control interface for programs on zbus (daemons).

pub mod catalog;
pub mod check;
pub mod error;
pub mod patch;
pub mod resolve;
pub mod spec;

#[cfg(feature = "registry")]
pub mod registry;
#[cfg(feature = "server")]
pub mod server;

pub use catalog::{Catalog, Entry, Listed, Source, State};
pub use error::{Error, ErrorKind};
pub use spec::{Action, Activation, Alias, App, Confirmation, Description, ResultKind, Risk};

#[cfg(feature = "registry")]
pub use registry::{InvokeOptions, Options, Registry, Watch};
