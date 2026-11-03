//! A small sync journal Nyx apps can share.
//!
//! The clock, merge rules, and provider trait are independent of any one app.
//! `WebDav` talks to Nextcloud. It does not know about feeds or articles.
//! Default features retain the complete sync API. With `default-features = false`,
//! only Secret Service helpers are compiled; credentials consumers need no age/HTTP stack.

#[cfg(feature = "sync")]
mod backoff;
mod credentials;
#[cfg(feature = "sync")]
mod crypto;
#[cfg(feature = "sync")]
mod exchange;
#[cfg(feature = "sync")]
mod hlc;
#[cfg(feature = "sync")]
mod merge;
#[cfg(feature = "sync")]
mod provider;
#[cfg(feature = "sync")]
mod webdav;

#[cfg(feature = "sync")]
pub use backoff::{Delay, delay};
pub use credentials::{Secret, lookup_secret, secret_command, store_secret};
#[cfg(feature = "sync")]
pub use crypto::{decrypt_batch, encrypt_batch};
#[cfg(feature = "sync")]
pub use exchange::{Exchange, encode_batch, sync_exchange};
#[cfg(feature = "sync")]
pub use hlc::{Clock, Hlc};
#[cfg(feature = "sync")]
pub use merge::{Op, OpKind, Record, State, merge};
#[cfg(feature = "sync")]
pub use provider::{Batch, BatchId, Etag, LoadedManifest, Manifest, PreconditionFailed, Remote, SyncProvider, is_conflict};
#[cfg(feature = "sync")]
pub use webdav::{WebDav, WebDavConfig};

#[cfg(all(feature = "sync", any(test, feature = "testutil")))]
pub use webdav::StubServer;
