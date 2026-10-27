//! A small sync journal Nyx apps can share.
//!
//! The clock, merge rules, and provider trait are independent of any one app.
//! `WebDav` talks to Nextcloud. It does not know about feeds or articles.

mod backoff;
mod credentials;
mod crypto;
mod exchange;
mod hlc;
mod merge;
mod provider;
mod webdav;

pub use backoff::{Delay, delay};
pub use credentials::{Secret, lookup_secret, secret_command, store_secret};
pub use crypto::{decrypt_batch, encrypt_batch};
pub use exchange::{Exchange, encode_batch, sync_exchange};
pub use hlc::{Clock, Hlc};
pub use merge::{Op, OpKind, Record, State, merge};
pub use provider::{Batch, BatchId, Etag, LoadedManifest, Manifest, PreconditionFailed, Remote, SyncProvider, is_conflict};
pub use webdav::{WebDav, WebDavConfig};

#[cfg(any(test, feature = "testutil"))]
pub use webdav::StubServer;
