//! The curated plugin index (`nyxos-project/nyx-plugin-index`): plugins by name,
//! each pinned to a commit (and to its artifacts' hashes), so `nyx plugin add NAME`
//! installs exactly what was reviewed.
//!
//! ```toml
//! [[plugin]]
//! id = "acme.ping"
//! name = "Ping"
//! description = "Latency to a host"
//! kinds = ["bar", "control-center"]
//! repo = "https://github.com/acme/nyx-ping.git"
//! tag = "v1.2.0"
//! commit = "0123456789abcdef0123456789abcdef01234567"
//! author = "Acme"
//! license = "MIT"
//! reviewed = true
//! ```

use crate::manifest::Artifact;
use crate::paths;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::time::{Duration, SystemTime};

pub const DEFAULT_URL: &str = "https://raw.githubusercontent.com/nyxos-project/nyx-plugin-index/main/index.toml";
const FRESH: Duration = Duration::from_secs(24 * 3600);

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Index {
    #[serde(default)]
    pub plugin: Vec<Entry>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub kinds: Vec<String>,
    pub repo: String,
    #[serde(default)]
    pub tag: Option<String>,
    pub commit: String,
    /// A folder in the repo, when the plugin isn't at its root.
    #[serde(default)]
    pub subdir: Option<String>,
    #[serde(default)]
    pub artifacts: BTreeMap<String, Artifact>,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub license: String,
    #[serde(default)]
    pub reviewed: bool,
}

impl Index {
    pub fn parse(text: &str) -> Result<Index> {
        let index: Index = toml::from_str(text).context("reading the plugin index")?;
        for e in &index.plugin {
            if e.commit.len() != 40 || !e.commit.chars().all(|c| c.is_ascii_hexdigit()) {
                bail!("index entry {}: commit must be a full 40-character hash", e.id);
            }
        }
        Ok(index)
    }

    /// By id, by the last part of the id (`ping` for `acme.ping`), or by name.
    pub fn find(&self, name: &str) -> Option<&Entry> {
        let lower = name.to_lowercase();
        self.plugin
            .iter()
            .find(|e| e.id == name)
            .or_else(|| self.plugin.iter().find(|e| e.id.rsplit('.').next() == Some(name)))
            .or_else(|| self.plugin.iter().find(|e| e.name.to_lowercase() == lower))
    }

    pub fn search(&self, term: &str) -> Vec<&Entry> {
        let t = term.to_lowercase();
        self.plugin
            .iter()
            .filter(|e| {
                t.is_empty()
                    || e.id.to_lowercase().contains(&t)
                    || e.name.to_lowercase().contains(&t)
                    || e.description.to_lowercase().contains(&t)
            })
            .collect()
    }
}

/// The index: `$NYX_PLUGIN_INDEX` (a file, for tests and offline use), else the cached
/// copy when it's under a day old, else a fresh download (falling back to the cache).
pub fn load(refresh: bool) -> Result<Index> {
    if let Some(file) = std::env::var_os("NYX_PLUGIN_INDEX").filter(|v| !v.is_empty()) {
        return Index::parse(&std::fs::read_to_string(&file).with_context(|| format!("reading {}", file.to_string_lossy()))?);
    }
    let cache = paths::index_cache();
    let fresh = std::fs::metadata(&cache)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .is_some_and(|age| age < FRESH);
    if fresh && !refresh {
        return Index::parse(&std::fs::read_to_string(&cache)?);
    }
    let url = std::env::var("NYX_PLUGIN_INDEX_URL").ok().filter(|u| !u.is_empty()).unwrap_or_else(|| DEFAULT_URL.into());
    match download(&url) {
        Ok(text) => {
            let index = Index::parse(&text)?;
            let _ = paths::write_atomic(&cache, &text);
            Ok(index)
        }
        Err(e) => match std::fs::read_to_string(&cache) {
            Ok(text) => {
                eprintln!("nyx plugin: couldn't refresh the index ({e:#}); using the copy from before");
                Index::parse(&text)
            }
            Err(_) => Err(e),
        },
    }
}

fn download(url: &str) -> Result<String> {
    let out = std::process::Command::new("curl")
        .args(["-fsSL", "--proto", "=https", "--max-time", "30", "--", url])
        .output()
        .context("running curl")?;
    if !out.status.success() {
        bail!("downloading {url}: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_by_id_short_name_and_name() {
        let index = Index::parse(
            r#"
[[plugin]]
id = "acme.ping"
name = "Ping"
description = "Latency to a host"
repo = "https://example.com/ping.git"
commit = "0123456789abcdef0123456789abcdef01234567"
"#,
        )
        .unwrap();
        assert!(index.find("acme.ping").is_some());
        assert!(index.find("ping").is_some());
        assert!(index.find("Ping").is_some());
        assert_eq!(index.search("latency").len(), 1);
        assert!(index.find("pong").is_none());
        assert!(Index::parse("[[plugin]]\nid='a'\nname='a'\nrepo='r'\ncommit='abc'\n").is_err());
    }
}
