//! Workspace profiles: named sets of workspaces ("Work", "Home"), one TOML file each in
//! `~/.config/nyx/desktop/profiles/<id>.toml`, and which one is active
//! (`~/.config/nyx/desktop/active-profile`).
//!
//! A profile says how many workspaces always exist, what each one is like (name,
//! screen, layout mode, gaps, decorations; keys this crate doesn't know, such as a
//! later `widgets` or `wallpaper`, are kept as they are) and which apps open on which
//! workspace. Nyx Settings applies a profile (it writes Hyprland's settings); other apps
//! only read profiles.

use crate::Desktop;
use crate::actions::MINIMIZED;
use crate::apps::{Index, key_for};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// One workspace in a profile. Unset fields leave Hyprland's and Omarchy's own
/// behaviour alone.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Workspace {
    pub id: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The monitor (connector name) it lives on.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub monitor: Option<String>,
    /// dwindle, master, scrolling or floating (Omarchy's workspace modes).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// Inner and outer gaps.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gaps: Option<[i64; 2]>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub no_border: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub no_rounding: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub no_shadow: bool,
    /// Keys for later (desktop widgets, a wallpaper), kept untouched.
    #[serde(flatten)]
    pub extra: toml::Table,
}

/// An app that opens on a workspace.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct App {
    /// Desktop id (`org.mozilla.firefox`), or `class:<class>` for a window no installed
    /// app claims, as the dock and the switcher group windows.
    pub app: String,
    pub workspace: u32,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    /// The file name without `.toml` (not stored in the file).
    #[serde(skip)]
    pub id: String,
    pub name: String,
    /// Workspaces 1..=N always exist, even when empty (0: Hyprland's default).
    #[serde(default)]
    pub workspaces: u32,
    /// On switching to this profile, move open windows of its apps to their workspaces.
    #[serde(default = "yes")]
    pub move_open_windows: bool,
    #[serde(default, rename = "workspace", skip_serializing_if = "Vec::is_empty")]
    pub workspace_list: Vec<Workspace>,
    #[serde(default, rename = "app", skip_serializing_if = "Vec::is_empty")]
    pub apps: Vec<App>,
}

impl Profile {
    pub fn new(id: &str, name: &str) -> Profile {
        Profile {
            id: id.into(),
            name: name.into(),
            workspaces: 0,
            move_open_windows: true,
            workspace_list: Vec::new(),
            apps: Vec::new(),
        }
    }

    pub fn parse(id: &str, text: &str) -> Result<Profile, String> {
        let mut p: Profile = toml::from_str(text).map_err(|e| e.to_string())?;
        p.id = id.into();
        p.workspaces = p.workspaces.min(10);
        p.workspace_list.retain(|w| (1..=10).contains(&w.id));
        p.workspace_list.sort_by_key(|w| w.id);
        p.apps.retain(|a| !a.app.trim().is_empty() && (1..=10).contains(&a.workspace));
        Ok(p)
    }

    pub fn to_toml(&self) -> String {
        toml::to_string_pretty(self).unwrap_or_default()
    }

    pub fn workspace(&self, id: u32) -> Option<&Workspace> {
        self.workspace_list.iter().find(|w| w.id == id)
    }
}

/// Where profiles live (a folder, so tests can use their own).
#[derive(Debug, Clone)]
pub struct Store {
    pub dir: PathBuf,
}

impl Store {
    /// `~/.config/nyx/desktop` (or `$XDG_CONFIG_HOME/nyx/desktop`).
    pub fn user() -> Store {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config"));
        Store { dir: base.join("nyx/desktop") }
    }

    fn profiles_dir(&self) -> PathBuf {
        self.dir.join("profiles")
    }

    fn file(&self, id: &str) -> PathBuf {
        self.profiles_dir().join(format!("{id}.toml"))
    }

    /// Every profile that reads, by name. A broken file is skipped (and reported).
    pub fn list(&self) -> Vec<Profile> {
        let mut out: Vec<Profile> = std::fs::read_dir(self.profiles_dir())
            .map(|d| {
                d.flatten()
                    .filter_map(|e| {
                        let path = e.path();
                        let id = path.file_stem()?.to_str()?.to_string();
                        (path.extension()? == "toml" && valid_id(&id)).then_some(id)
                    })
                    .filter_map(|id| match self.load(&id) {
                        Ok(p) => Some(p),
                        Err(e) => {
                            eprintln!("nyx-desktop: workspace profile {id}: {e}");
                            None
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then(a.id.cmp(&b.id)));
        out
    }

    pub fn load(&self, id: &str) -> Result<Profile, String> {
        if !valid_id(id) {
            return Err(format!("“{id}” isn't a profile id"));
        }
        let text = std::fs::read_to_string(self.file(id)).map_err(|e| e.to_string())?;
        Profile::parse(id, &text)
    }

    /// A profile by id, or by name (ignoring case).
    pub fn find(&self, id_or_name: &str) -> Option<Profile> {
        self.load(id_or_name).ok().or_else(|| self.list().into_iter().find(|p| p.name.eq_ignore_ascii_case(id_or_name.trim())))
    }

    pub fn save(&self, p: &Profile) -> Result<(), String> {
        if !valid_id(&p.id) {
            return Err(format!("“{}” isn't a profile id", p.id));
        }
        write_atomic(&self.file(&p.id), &p.to_toml())
    }

    pub fn delete(&self, id: &str) -> Result<(), String> {
        if !valid_id(id) {
            return Err(format!("“{id}” isn't a profile id"));
        }
        std::fs::remove_file(self.file(id)).map_err(|e| e.to_string())?;
        if self.active().as_deref() == Some(id) {
            let _ = std::fs::remove_file(self.dir.join("active-profile"));
        }
        Ok(())
    }

    /// The active profile's id, if one is set and still exists.
    pub fn active(&self) -> Option<String> {
        let id = std::fs::read_to_string(self.dir.join("active-profile")).ok()?.trim().to_string();
        (valid_id(&id) && self.file(&id).is_file()).then_some(id)
    }

    pub fn set_active(&self, id: &str) -> Result<(), String> {
        if !valid_id(id) {
            return Err(format!("“{id}” isn't a profile id"));
        }
        write_atomic(&self.dir.join("active-profile"), &format!("{id}\n"))
    }

    /// An unused id for a profile called `name`: `work`, `work-2`, …
    pub fn new_id(&self, name: &str) -> String {
        let base = slug(name);
        let mut id = base.clone();
        let mut n = 2;
        while self.file(&id).exists() {
            id = format!("{base}-{n}");
            n += 1;
        }
        id
    }
}

/// Ids are file names: lower-case letters, digits and dashes.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// `"Work & Play"` → `work-play`.
pub fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.trim().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let out = out.trim_end_matches('-').chars().take(40).collect::<String>();
    if out.is_empty() { "profile".into() } else { out }
}

fn write_atomic(path: &Path, text: &str) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}

/// Open windows of the profile's apps that aren't on their workspace: (address,
/// workspace). Minimized windows, special and agent workspaces are left alone.
pub fn moves(profile: &Profile, desktop: &Desktop, index: &Index) -> Vec<(String, u32)> {
    let mut out = Vec::new();
    for w in &desktop.windows {
        let ws_name = desktop.workspaces.iter().find(|ws| ws.id == w.workspace).map(|ws| ws.name.as_str()).unwrap_or("");
        if ws_name == MINIMIZED || ws_name.starts_with("special:") || w.workspace < 0 {
            continue;
        }
        let key = key_for(index, &w.class, &w.initial_class);
        if let Some(place) = profile.apps.iter().find(|a| a.app.eq_ignore_ascii_case(&key))
            && w.workspace != place.workspace as i64
        {
            out.push((w.address.clone(), place.workspace));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apps::Entry;
    use crate::model::{Window, Workspace as Ws};

    fn tempdir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nyx-desktop-profiles-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    const WORK: &str = r#"
name = "Work"
workspaces = 6

[[workspace]]
id = 2
name = "Mail"
mode = "scrolling"
gaps = [4, 8]
wallpaper = "~/Pictures/w.jpg"

[[workspace]]
id = 12

[[app]]
app = "org.mozilla.firefox"
workspace = 3
"#;

    #[test]
    fn reads_and_keeps_unknown_keys() {
        let p = Profile::parse("work", WORK).unwrap();
        assert_eq!((p.name.as_str(), p.workspaces, p.move_open_windows), ("Work", 6, true));
        assert_eq!(p.workspace_list.len(), 1, "workspace 12 is out of range");
        let ws = p.workspace(2).unwrap();
        assert_eq!(ws.gaps, Some([4, 8]));
        assert_eq!(ws.extra.get("wallpaper").and_then(|v| v.as_str()), Some("~/Pictures/w.jpg"));
        let again = Profile::parse("work", &p.to_toml()).unwrap();
        assert_eq!(again, p, "round trip, wallpaper included");
    }

    #[test]
    fn store_lists_finds_and_tracks_the_active_one() {
        let store = Store { dir: tempdir("store") };
        assert!(store.list().is_empty() && store.active().is_none());
        let mut work = Profile::parse("work", WORK).unwrap();
        store.save(&work).unwrap();
        let home = Profile::new(&store.new_id("Home"), "Home");
        store.save(&home).unwrap();
        assert_eq!(store.new_id("Work"), "work-2");
        let names: Vec<String> = store.list().into_iter().map(|p| p.name).collect();
        assert_eq!(names, ["Home", "Work"]);
        assert_eq!(store.find("work").unwrap().name, "Work");
        assert_eq!(store.find("HOME").unwrap().id, "home");
        store.set_active("work").unwrap();
        assert_eq!(store.active().as_deref(), Some("work"));
        work.workspaces = 3;
        store.save(&work).unwrap();
        assert_eq!(store.load("work").unwrap().workspaces, 3);
        store.delete("work").unwrap();
        assert!(store.active().is_none(), "deleting the active one clears it");
        assert!(store.load("../etc").is_err());
    }

    #[test]
    fn slugs() {
        assert_eq!(slug("Work & Play"), "work-play");
        assert_eq!(slug("  "), "profile");
        assert!(valid_id("home-2") && !valid_id("Home") && !valid_id("a/b"));
    }

    #[test]
    fn moves_only_placed_apps_off_their_workspace() {
        let index = Index::new(&[Entry { id: "org.mozilla.firefox".into(), exec: "firefox".into(), ..Default::default() }]);
        let p = Profile::parse("work", WORK).unwrap();
        let win = |addr: &str, class: &str, ws: i64| Window {
            address: addr.into(),
            stable_id: String::new(),
            class: class.into(),
            initial_class: class.into(),
            title: String::new(),
            workspace: ws,
            monitor: 0,
            floating: false,
            fullscreen: 0,
            at: [0, 0],
            size: [1, 1],
            pid: 1,
            focus_order: 0,
            pinned: false,
            hidden: false,
            urgent: false,
            grouped: Vec::new(),
        };
        let ws = |id: i64, name: &str| Ws {
            id,
            name: name.into(),
            monitor: "M".into(),
            windows: 1,
            has_fullscreen: false,
            tiled_layout: String::new(),
        };
        let desktop = Desktop {
            windows: vec![win("a", "firefox", 1), win("b", "firefox", 3), win("c", "kitty", 1), win("d", "firefox", -99)],
            workspaces: vec![ws(1, "1"), ws(3, "3"), ws(-99, MINIMIZED)],
            ..Default::default()
        };
        assert_eq!(moves(&p, &desktop, &index), [("a".to_string(), 3)]);
    }
}
