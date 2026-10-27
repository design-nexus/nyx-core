//! Pull remote batches, push one local batch, and retry when the manifest loses a race.
//!
//! A batch file that was stored and then lost the manifest update stays unread until
//! a later manifest names it. This exchange adopts that file only when it is the
//! batch we just wrote.

use crate::merge::Op;
use crate::provider::{Batch, BatchId, Etag, LoadedManifest, Manifest, Remote, is_conflict};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug)]
pub struct Exchange {
    pub ops: Vec<Op>,
    pub generation: u64,
    pub etag: Option<Etag>,
    pub pushed: bool,
}

#[derive(Serialize, Deserialize)]
struct BatchDoc {
    schema: u32,
    id: String,
    device_id: String,
    base_generation: u64,
    ops: Vec<Op>,
}

pub fn encode_batch(device_id: &str, generation: u64, base_generation: u64, ops: &[Op]) -> anyhow::Result<Batch> {
    let id = generation.to_string();
    let doc = BatchDoc { schema: 1, id: id.clone(), device_id: device_id.to_string(), base_generation, ops: ops.to_vec() };
    Ok(Batch { schema: 1, id: BatchId(id), device_id: device_id.to_string(), base_generation, body: serde_json::to_vec(&doc)? })
}

fn decode_ops(batch: &Batch) -> anyhow::Result<Vec<Op>> {
    let doc: BatchDoc = serde_json::from_slice(&batch.body)?;
    Ok(doc.ops)
}

struct Pulled {
    ops: Vec<Op>,
    generation: u64,
    etag: Option<Etag>,
    devices: Vec<String>,
}

fn pull(remote: &impl Remote, have: u64) -> anyhow::Result<Pulled> {
    let LoadedManifest { manifest, etag } = remote.fetch_manifest()?;
    let Some(manifest) = manifest else {
        return Ok(Pulled { ops: Vec::new(), generation: 0, etag, devices: Vec::new() });
    };
    let mut ops = Vec::new();
    let start = have.saturating_add(1);
    if manifest.generation >= start {
        for generation in start..=manifest.generation {
            let batch = remote.fetch_batch(&BatchId(generation.to_string()))?;
            ops.extend(decode_ops(&batch)?);
        }
    }
    Ok(Pulled { ops, generation: manifest.generation, etag, devices: manifest.devices })
}

/// Merge is the caller's job. This only moves batches. `pending` is what this device still has to upload.
pub fn sync_exchange(remote: &impl Remote, device_id: &str, have_generation: u64, pending: &[Op]) -> anyhow::Result<Exchange> {
    let mut have = have_generation;
    let mut seen = Vec::new();
    for _ in 0..4 {
        let pulled = pull(remote, have)?;
        seen.extend(pulled.ops);
        have = pulled.generation;
        let Pulled { etag, mut devices, .. } = pulled;
        if pending.is_empty() {
            return Ok(Exchange { ops: seen, generation: have, etag, pushed: false });
        }
        let next = have.saturating_add(1);
        let batch = encode_batch(device_id, next, have, pending)?;
        if !devices.iter().any(|id| id == device_id) {
            devices.push(device_id.to_string());
        }
        let manifest = Manifest { schema: 1, generation: next, head: Some(BatchId(next.to_string())), devices };
        match push_one(remote, &batch, &manifest, etag.as_ref()) {
            Ok(etag) => return Ok(Exchange { ops: seen, generation: next, etag: Some(etag), pushed: true }),
            Err(err) if is_conflict(&err) => continue,
            Err(err) => return Err(err),
        }
    }
    anyhow::bail!("sync gave up after another device kept winning the race")
}

fn push_one(remote: &impl Remote, batch: &Batch, manifest: &Manifest, etag: Option<&Etag>) -> anyhow::Result<Etag> {
    match remote.write_batch(batch) {
        Ok(()) => {}
        Err(err) if is_conflict(&err) => {
            let existing = remote.fetch_batch(&batch.id)?;
            if existing.device_id != batch.device_id || existing.body != batch.body {
                return Err(err);
            }
        }
        Err(err) => return Err(err),
    }
    remote.write_manifest(manifest, etag)
}
