//! Which installed app (desktop entry) a window belongs to.
//!
//! A window's class is usually its app id (`io.github.design_nexus.nyx.Files`,
//! `firefox`), but not always: Chromium web apps set `StartupWMClass`, Flatpaks and
//! reverse-DNS ids end in the class (`org.mozilla.firefox`), and terminal apps get the
//! id their launcher passed with `--app-id=` (or `org.omarchy.<command>` from Omarchy's
//! TUI launcher).

use std::collections::HashMap;

/// What the matcher needs from a desktop entry.
#[derive(Debug, Clone, Default)]
pub struct Entry {
    /// Desktop id without `.desktop`.
    pub id: String,
    pub wm_class: Option<String>,
    /// The `Exec=` line.
    pub exec: String,
    pub terminal: bool,
}

#[derive(Debug, Default)]
pub struct Index {
    keys: HashMap<String, String>,
}

impl Index {
    pub fn new(entries: &[Entry]) -> Index {
        // Weakest first; a stronger key written later wins.
        let mut keys = HashMap::new();
        for e in entries {
            let program = e.exec.split_whitespace().next().unwrap_or("");
            let program = program.rsplit('/').next().unwrap_or(program);
            if !program.is_empty() && !e.terminal {
                keys.insert(program.to_lowercase(), e.id.clone());
            }
            if e.terminal && !program.is_empty() {
                let command = e.exec.split_whitespace().find(|w| !w.starts_with('%')).unwrap_or(program);
                let command = command.rsplit('/').next().unwrap_or(command);
                keys.insert(format!("org.omarchy.{}", command.to_lowercase()), e.id.clone());
            }
            if let Some(last) = e.id.rsplit('.').next().filter(|l| *l != e.id) {
                keys.entry(last.to_lowercase()).or_insert_with(|| e.id.clone());
            }
        }
        for e in entries {
            if let Some(class) = &e.wm_class {
                keys.insert(class.to_lowercase(), e.id.clone());
            }
            for word in e.exec.split_whitespace() {
                if let Some(app_id) = word.strip_prefix("--app-id=") {
                    keys.insert(app_id.to_lowercase(), e.id.clone());
                }
            }
        }
        for e in entries {
            keys.insert(e.id.to_lowercase(), e.id.clone());
        }
        Index { keys }
    }

    /// Every window class (lower case) that maps to desktop id `id`, for a window rule
    /// that matches the app's windows.
    pub fn classes_of(&self, id: &str) -> Vec<String> {
        let mut out: Vec<String> = self.keys.iter().filter(|(_, v)| v.eq_ignore_ascii_case(id)).map(|(k, _)| k.clone()).collect();
        out.sort();
        out
    }

    /// The desktop id for a window class, if an installed app claims it.
    pub fn find(&self, class: &str) -> Option<&str> {
        self.keys.get(&class.to_lowercase()).map(String::as_str)
    }
}

/// The installed apps, as GIO lists them (`glib` feature).
#[cfg(feature = "glib")]
pub fn from_gio() -> Index {
    use gio::prelude::*;
    let entries: Vec<Entry> = gio::AppInfo::all()
        .into_iter()
        .filter_map(|a| a.downcast::<gio_unix::DesktopAppInfo>().ok())
        .filter_map(|a| {
            let id = a.id()?.trim_end_matches(".desktop").to_string();
            Some(Entry {
                id,
                wm_class: a.startup_wm_class().map(|c| c.to_string()),
                exec: a.string("Exec").map(|c| c.to_string()).unwrap_or_default(),
                terminal: a.boolean("Terminal"),
            })
        })
        .collect();
    Index::new(&entries)
}

/// The key a window is grouped under: its app's desktop id, or `class:<class>` for a
/// window no installed app claims.
pub fn key_for(index: &Index, class: &str, initial_class: &str) -> String {
    index
        .find(class)
        .or_else(|| index.find(initial_class))
        .map(str::to_string)
        .unwrap_or_else(|| format!("class:{}", if class.is_empty() { initial_class } else { class }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries() -> Vec<Entry> {
        vec![
            Entry { id: "io.github.design_nexus.nyx.Files".into(), exec: "nyx-files %U".into(), ..Default::default() },
            Entry { id: "org.mozilla.firefox".into(), exec: "/usr/lib/firefox/firefox %u".into(), ..Default::default() },
            Entry {
                id: "Basecamp".into(),
                wm_class: Some("chrome-3.basecamp.com__-Default".into()),
                exec: "omarchy-launch-webapp https://3.basecamp.com".into(),
                ..Default::default()
            },
            Entry { id: "nvim".into(), exec: "nvim %F".into(), terminal: true, ..Default::default() },
            Entry { id: "btop".into(), exec: "xdg-terminal-exec --app-id=TUI.btop btop".into(), ..Default::default() },
            Entry { id: "Alacritty".into(), exec: "alacritty".into(), ..Default::default() },
        ]
    }

    #[test]
    fn matches_each_kind_of_class() {
        let idx = Index::new(&entries());
        assert_eq!(idx.find("io.github.design_nexus.nyx.Files"), Some("io.github.design_nexus.nyx.Files"));
        assert_eq!(idx.find("firefox"), Some("org.mozilla.firefox"), "reverse-DNS tail and program name");
        assert_eq!(idx.find("chrome-3.basecamp.com__-Default"), Some("Basecamp"), "StartupWMClass");
        assert_eq!(idx.find("org.omarchy.nvim"), Some("nvim"), "Omarchy's TUI launcher");
        assert_eq!(idx.find("TUI.btop"), Some("btop"), "--app-id");
        assert_eq!(idx.find("alacritty"), Some("Alacritty"), "case doesn't matter");
        assert_eq!(idx.find("kitty"), None);
        assert_eq!(idx.classes_of("org.mozilla.firefox"), ["firefox", "org.mozilla.firefox"]);
    }

    #[test]
    fn unknown_windows_group_by_class() {
        let idx = Index::new(&entries());
        assert_eq!(key_for(&idx, "kitty", "kitty"), "class:kitty");
        assert_eq!(key_for(&idx, "", "firefox"), "org.mozilla.firefox");
    }
}
