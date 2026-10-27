//! The storage a sync provider has to offer. WebDAV, and anything added later,
//! implements this. Callers merge and then advance their checkpoint themselves.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::future::Future;

/// The remote manifest changed between read and write. The caller merges and retries.
#[derive(Debug)]
pub struct PreconditionFailed;

impl fmt::Display for PreconditionFailed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("the remote manifest changed")
    }
}

impl std::error::Error for PreconditionFailed {}

pub fn is_conflict(err: &anyhow::Error) -> bool {
    err.downcast_ref::<PreconditionFailed>().is_some()
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Etag(pub String);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchId(pub String);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub schema: u32,
    pub generation: u64,
    pub head: Option<BatchId>,
    pub devices: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Batch {
    pub schema: u32,
    pub id: BatchId,
    pub device_id: String,
    pub base_generation: u64,
    /// Encoded operations. The provider stores the bytes and does not merge them.
    pub body: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct LoadedManifest {
    pub manifest: Option<Manifest>,
    pub etag: Option<Etag>,
}

/// Blocking provider used by the sync exchange. HTTP clients run this off the GTK thread.
pub trait Remote: Send + Sync {
    fn fetch_manifest(&self) -> anyhow::Result<LoadedManifest>;
    fn write_manifest(&self, manifest: &Manifest, if_match: Option<&Etag>) -> anyhow::Result<Etag>;
    fn fetch_batch(&self, id: &BatchId) -> anyhow::Result<Batch>;
    fn write_batch(&self, batch: &Batch) -> anyhow::Result<()>;
}

/// A failed call returns an error and must not leave a partial manifest behind.
pub trait SyncProvider {
    fn load_manifest(&self) -> impl Future<Output = anyhow::Result<Option<Manifest>>> + Send;
    fn store_manifest(&self, manifest: &Manifest, if_match: Option<&Etag>) -> impl Future<Output = anyhow::Result<Etag>> + Send;
    fn get_batch(&self, id: &BatchId) -> impl Future<Output = anyhow::Result<Batch>> + Send;
    fn put_batch(&self, batch: &Batch) -> impl Future<Output = anyhow::Result<()>> + Send;
}
