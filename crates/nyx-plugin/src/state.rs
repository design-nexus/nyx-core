//! `~/.config/nyx/plugins.toml`: which parts the user turned on for each kind, the
//! permissions they approved, and plugins a host stopped after repeated crashes.
//! Every host reads and watches it; `nyx plugin` writes it, keeping comments.
//!
//! ```toml
//! [enabled."nyx.weather"]
//! bar = ["now"]
//! control-center = ["tile"]
//! desktop = []
//!
//! [grants."acme.ping"]
//! hash = "sha256:…"
//! version = "1.2.0"
//! approved = "2026-11-03T17:00:00Z"
//!
//! [suspended."acme.ping"]
//! reason = "crashed 5 times in 10 minutes"
//! ```
//!
//! A kind missing from `enabled` means the manifest's defaults; an empty list means
//! none. A third-party plugin is only ever added with an explicit list.

use crate::discovery::Plugin;
use crate::manifest::{Kind, Manifest};
use crate::{paths, permissions};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use toml_edit::{DocumentMut, Item, Table, value};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct State {
    #[serde(default)]
    pub enabled: BTreeMap<String, BTreeMap<String, Vec<String>>>,
    #[serde(default)]
    pub grants: BTreeMap<String, Grant>,
    #[serde(default)]
    pub suspended: BTreeMap<String, Suspension>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Grant {
    pub hash: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub approved: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Suspension {
    pub reason: String,
    #[serde(default)]
    pub at: String,
}

impl State {
    pub fn load() -> State {
        State::load_from(&paths::state_file())
    }

    pub fn load_from(path: &Path) -> State {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).unwrap_or_else(|e| {
                eprintln!("nyx-plugin: {}: {e}; ignoring it", path.display());
                State::default()
            }),
            Err(_) => State::default(),
        }
    }

    /// The parts of `kind` to show, in the manifest's order.
    pub fn enabled_parts(&self, manifest: &Manifest, kind: Kind) -> Vec<String> {
        if self.suspended.contains_key(&manifest.id) || !manifest.has_kind(kind) {
            return vec![];
        }
        let parts = manifest.parts(kind);
        match self.enabled.get(&manifest.id).and_then(|k| k.get(kind.name())) {
            Some(chosen) => parts.into_iter().filter(|p| chosen.contains(&p.id)).map(|p| p.id).collect(),
            None => manifest.default_parts(kind),
        }
    }

    /// Whether the user has said anything about this plugin's parts.
    pub fn has_choice(&self, id: &str) -> bool {
        self.enabled.contains_key(id)
    }

    pub fn grant_matches(&self, manifest: &Manifest) -> bool {
        self.grants.get(&manifest.id).is_some_and(|g| g.hash == permissions::hash(&manifest.permissions))
    }

    /// Whether the host should run the plugin for `kind` at all.
    pub fn runs(&self, plugin: &Plugin, kind: Kind) -> bool {
        crate::authorized(plugin, self) && !self.enabled_parts(&plugin.manifest, kind).is_empty()
    }
}

/// Edits to `plugins.toml` that keep the user's comments and order.
pub struct Editor {
    path: PathBuf,
    doc: DocumentMut,
}

impl Editor {
    pub fn open() -> Result<Editor> {
        Editor::open_at(paths::state_file())
    }

    pub fn open_at(path: PathBuf) -> Result<Editor> {
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let doc: DocumentMut = text.parse().with_context(|| format!("reading {}", path.display()))?;
        Ok(Editor { path, doc })
    }

    fn table(&mut self, name: &str) -> &mut Table {
        if !self.doc.contains_table(name) {
            let mut t = Table::new();
            t.set_implicit(true);
            self.doc.insert(name, Item::Table(t));
        }
        self.doc[name].as_table_mut().expect("a table")
    }

    fn entry(&mut self, section: &str, id: &str) -> &mut Table {
        let t = self.table(section);
        if !t.contains_table(id) {
            t.insert(id, Item::Table(Table::new()));
        }
        t[id].as_table_mut().expect("a table")
    }

    pub fn set_enabled(&mut self, id: &str, kind: Kind, parts: &[String]) {
        let mut arr = toml_edit::Array::new();
        for p in parts {
            arr.push(p.as_str());
        }
        self.entry("enabled", id).insert(kind.name(), value(arr));
    }

    /// Add or drop one part, starting from what's shown now.
    pub fn toggle_part(&mut self, manifest: &Manifest, current: &State, kind: Kind, part: &str, on: bool) {
        let mut parts = current.enabled_parts(manifest, kind);
        parts.retain(|p| p != part);
        if on {
            parts.push(part.to_string());
        }
        let order: Vec<String> = manifest.parts(kind).into_iter().map(|p| p.id).collect();
        parts.sort_by_key(|p| order.iter().position(|o| o == p));
        self.set_enabled(&manifest.id, kind, &parts);
    }

    pub fn grant(&mut self, manifest: &Manifest, approved: &str) {
        let t = self.entry("grants", &manifest.id);
        t.insert("hash", value(permissions::hash(&manifest.permissions)));
        t.insert("version", value(manifest.version.as_str()));
        t.insert("approved", value(approved));
    }

    pub fn revoke(&mut self, id: &str) {
        self.table("grants").remove(id);
    }

    pub fn suspend(&mut self, id: &str, reason: &str, at: &str) {
        let t = self.entry("suspended", id);
        t.insert("reason", value(reason));
        t.insert("at", value(at));
    }

    pub fn unsuspend(&mut self, id: &str) {
        self.table("suspended").remove(id);
    }

    /// Forget a plugin entirely (it was removed).
    pub fn forget(&mut self, id: &str) {
        for section in ["enabled", "grants", "suspended"] {
            self.table(section).remove(id);
        }
    }

    pub fn save(&self) -> Result<()> {
        paths::write_atomic(&self.path, &self.doc.to_string()).with_context(|| format!("writing {}", self.path.display()))
    }

    pub fn text(&self) -> String {
        self.doc.to_string()
    }
}

/// Now, as `2026-11-03T17:00:00Z`, without a date crate.
pub fn now_utc() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let (days, rem) = ((secs / 86_400) as i64, secs % 86_400);
    // Civil from days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::Permissions;

    fn manifest() -> Manifest {
        toml::from_str(
            r#"
id = "nyx.weather"
name = "Weather"
version = "1.0"
api_version = 1
kinds = ["bar", "desktop"]
exec = ["sh", "w.sh"]
[bar]
[[bar.items]]
id = "now"
name = "Now"
default = true
[[bar.items]]
id = "forecast"
name = "Forecast"
"#,
        )
        .unwrap()
    }

    #[test]
    fn defaults_until_the_user_chooses() {
        let m = manifest();
        let mut s = State::default();
        assert_eq!(s.enabled_parts(&m, Kind::Bar), vec!["now"]);
        assert_eq!(s.enabled_parts(&m, Kind::ControlCenter), Vec::<String>::new());
        s.enabled.entry(m.id.clone()).or_default().insert("bar".into(), vec!["forecast".into(), "gone".into()]);
        assert_eq!(s.enabled_parts(&m, Kind::Bar), vec!["forecast"]);
        s.suspended.insert(m.id.clone(), Suspension::default());
        assert!(s.enabled_parts(&m, Kind::Bar).is_empty());
    }

    #[test]
    fn edits_keep_comments_and_grants_follow_permissions() {
        let t = tempfile::tempdir().unwrap();
        let path = t.path().join("plugins.toml");
        std::fs::write(&path, "# my plugins\n[enabled.\"other\"] # keep me\nbar = []\n").unwrap();
        let mut m = manifest();
        let mut e = Editor::open_at(path.clone()).unwrap();
        e.toggle_part(&m, &State::load_from(&path), Kind::Bar, "forecast", true);
        e.grant(&m, "2026-11-03T00:00:00Z");
        e.save().unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# my plugins") && text.contains("# keep me"), "{text}");
        let s = State::load_from(&path);
        assert_eq!(s.enabled_parts(&m, Kind::Bar), vec!["now", "forecast"]);
        assert!(s.grant_matches(&m));
        m.permissions = Permissions { network: true, ..Default::default() };
        assert!(!s.grant_matches(&m));
    }

    #[test]
    fn the_date() {
        let s = now_utc();
        assert_eq!(s.len(), 20);
        assert!(s.starts_with("20"));
    }
}
