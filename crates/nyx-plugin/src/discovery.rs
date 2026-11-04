//! The installed plugins. Three kinds of folder, in this order; a plugin in a later
//! one replaces one with the same id in an earlier one:
//!
//! 1. packaged: `$XDG_DATA_DIRS/nyx/plugins/` (trusted);
//! 2. first-party: `~/.local/lib/nyx/plugins/` (trusted; `install.sh` on Omarchy);
//! 3. user: `~/.local/share/nyx/plugins/` (third-party: sandboxed, approved).

use crate::manifest::Manifest;
use crate::paths;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Trust {
    Packaged,
    FirstParty,
    ThirdParty,
}

impl Trust {
    pub fn is_trusted(self) -> bool {
        !matches!(self, Trust::ThirdParty)
    }

    pub fn label(self) -> &'static str {
        match self {
            Trust::Packaged => "packaged",
            Trust::FirstParty => "first-party",
            Trust::ThirdParty => "third-party",
        }
    }
}

/// Where a user plugin came from: `.nyx-plugin.toml` in its folder.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Origin {
    /// The git URL.
    pub source: String,
    #[serde(default)]
    pub git_ref: Option<String>,
    #[serde(default)]
    pub commit: String,
    #[serde(default)]
    pub installed: String,
    /// The index entry it was added from.
    #[serde(default)]
    pub index: Option<String>,
}

pub const ORIGIN_FILE: &str = ".nyx-plugin.toml";

impl Origin {
    pub fn load(dir: &Path) -> Option<Origin> {
        toml::from_str(&std::fs::read_to_string(dir.join(ORIGIN_FILE)).ok()?).ok()
    }

    pub fn save(&self, dir: &Path) -> std::io::Result<()> {
        std::fs::write(dir.join(ORIGIN_FILE), toml::to_string(self).unwrap_or_default())
    }
}

#[derive(Debug, Clone)]
pub struct Plugin {
    pub manifest: Manifest,
    pub dir: PathBuf,
    pub trust: Trust,
    pub origin: Option<Origin>,
    /// The folder is a link to a working copy (`nyx plugin link`).
    pub linked: bool,
}

impl Plugin {
    pub fn id(&self) -> &str {
        &self.manifest.id
    }

    /// How it got here, for `nyx plugin list`.
    pub fn source_label(&self) -> &'static str {
        match self.trust {
            Trust::Packaged => "packaged",
            Trust::FirstParty => "first-party",
            Trust::ThirdParty if self.linked => "linked",
            Trust::ThirdParty if self.origin.is_some() => "git",
            Trust::ThirdParty => "local",
        }
    }

    /// The commands in `needs` that aren't on PATH.
    pub fn missing_needs(&self) -> Vec<String> {
        self.manifest.needs.iter().filter(|n| !on_path(n)).cloned().collect()
    }
}

/// A folder that couldn't be read as a plugin.
#[derive(Debug, Clone)]
pub struct Problem {
    pub dir: PathBuf,
    pub error: String,
}

#[derive(Debug, Clone, Default)]
pub struct Found {
    pub plugins: Vec<Plugin>,
    pub problems: Vec<Problem>,
}

impl Found {
    pub fn get(&self, id: &str) -> Option<&Plugin> {
        self.plugins.iter().find(|p| p.manifest.id == id)
    }
}

/// Whether a data folder from `$XDG_DATA_DIRS` can hold trusted plugins: not one the
/// user can write without a package manager (anything that really lives under
/// `$HOME`; a Nix profile there resolves into the read-only store and is fine).
fn packaged_dir(d: &Path) -> bool {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from).filter(|h| h != Path::new("/")) else { return true };
    let real = d.canonicalize().unwrap_or_else(|_| d.to_path_buf());
    !real.starts_with(&home)
}

/// The folders to search, each with its trust, lowest priority first.
pub fn search_dirs() -> Vec<(PathBuf, Trust)> {
    let mut dirs: Vec<(PathBuf, Trust)> =
        paths::system_dirs().into_iter().rev().filter(|d| packaged_dir(d)).map(|d| (d, Trust::Packaged)).collect();
    dirs.push((paths::first_party_dir(), Trust::FirstParty));
    dirs.push((paths::user_dir(), Trust::ThirdParty));
    dirs
}

pub fn discover() -> Found {
    discover_in(&search_dirs())
}

pub fn discover_in(dirs: &[(PathBuf, Trust)]) -> Found {
    let mut found = Found::default();
    for (dir, trust) in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else { continue };
        let mut entries: Vec<_> = entries.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                continue;
            }
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            match Manifest::load(&path) {
                Ok(manifest) => {
                    let linked = std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink());
                    let origin = if *trust == Trust::ThirdParty { Origin::load(&path) } else { None };
                    found.plugins.retain(|p| p.manifest.id != manifest.id);
                    found.plugins.push(Plugin { manifest, dir: path, trust: *trust, origin, linked });
                }
                Err(e) => found.problems.push(Problem { dir: path, error: format!("{e:#}") }),
            }
        }
    }
    found.plugins.sort_by(|a, b| a.manifest.id.cmp(&b.manifest.id));
    found
}

pub fn on_path(cmd: &str) -> bool {
    if cmd.contains('/') {
        return Path::new(cmd).is_file();
    }
    std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(cmd).is_file()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plugin(dir: &Path, id: &str, name: &str) {
        let d = dir.join(id);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(
            d.join("plugin.toml"),
            format!("id = \"{id}\"\nname = \"{name}\"\napi_version = 1\nkinds = [\"bar\"]\nexec = [\"run\"]\n"),
        )
        .unwrap();
    }

    #[test]
    fn data_dirs_under_home_are_not_trusted() {
        let home = std::env::var("HOME").unwrap();
        assert!(!packaged_dir(&PathBuf::from(&home).join(".local/share/nyx/plugins")));
        assert!(packaged_dir(Path::new("/usr/share/nyx/plugins")));
    }

    #[test]
    fn later_folders_win_and_problems_are_kept() {
        let t = tempfile::tempdir().unwrap();
        let (sys, first, user) = (t.path().join("sys"), t.path().join("lib"), t.path().join("user"));
        plugin(&sys, "nyx.clock", "Packaged clock");
        plugin(&first, "nyx.clock", "First-party clock");
        plugin(&user, "nyx.clock", "My clock");
        plugin(&first, "nyx.stats", "Stats");
        std::fs::create_dir_all(user.join("broken")).unwrap();
        std::fs::write(user.join("broken/plugin.toml"), "id = ").unwrap();
        std::fs::create_dir_all(user.join(".add.tmp.1")).unwrap();
        let found =
            discover_in(&[(sys, Trust::Packaged), (first, Trust::FirstParty), (user.clone(), Trust::ThirdParty)]);
        assert_eq!(found.plugins.len(), 2);
        let clock = found.get("nyx.clock").unwrap();
        assert_eq!(clock.manifest.name, "My clock");
        assert_eq!(clock.trust, Trust::ThirdParty);
        assert_eq!(clock.source_label(), "local");
        assert_eq!(found.get("nyx.stats").unwrap().trust, Trust::FirstParty);
        assert_eq!(found.problems.len(), 1);
        assert_eq!(found.problems[0].dir, user.join("broken"));
    }
}
