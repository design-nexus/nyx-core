//! Well-known locations. Every path honours the XDG overrides so the whole app
//! can be pointed at a scratch copy of `~/.config` for testing.

use crate::app;
use std::path::{Path, PathBuf};

pub fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

fn xdg(var: &str, fallback: &str) -> PathBuf {
    std::env::var_os(var).map(PathBuf::from).filter(|p| p.is_absolute()).unwrap_or_else(|| home().join(fallback))
}

pub fn config_home() -> PathBuf {
    xdg("XDG_CONFIG_HOME", ".config")
}

pub fn data_home() -> PathBuf {
    xdg("XDG_DATA_HOME", ".local/share")
}

pub fn state_home() -> PathBuf {
    xdg("XDG_STATE_HOME", ".local/state")
}

pub fn cache_home() -> PathBuf {
    xdg("XDG_CACHE_HOME", ".cache")
}

/// `~/.config/<app>`: everything the user can edit lives here.
pub fn app_dir() -> PathBuf {
    config_home().join(app::slug())
}

/// `~/.local/share/<app>`: libraries, databases, history.
pub fn data_dir() -> PathBuf {
    data_home().join(app::slug())
}

/// `~/.local/state/<app>`: window state and other things worth keeping but not editing.
pub fn state_dir() -> PathBuf {
    state_home().join(app::slug())
}

/// `~/.cache/<app>`: anything that can be rebuilt.
pub fn cache_dir() -> PathBuf {
    cache_home().join(app::slug())
}

pub fn prefs_file() -> PathBuf {
    app_dir().join("settings.toml")
}

pub fn custom_themes_dir() -> PathBuf {
    app_dir().join("themes")
}

/// `~/.config/nyx`: settings shared by every app in the suite.
pub fn suite_dir() -> PathBuf {
    config_home().join("nyx")
}

pub fn omarchy_theme_dir() -> PathBuf {
    state_home().join("omarchy/current/theme")
}

pub fn omarchy_colors() -> PathBuf {
    omarchy_theme_dir().join("colors.toml")
}

/// Replace `$HOME` with `~` for display.
pub fn pretty(path: &Path) -> String {
    match path.strip_prefix(home()) {
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

/// Expand a leading `~/`.
pub fn expand(path: &str) -> PathBuf {
    let path = path.trim();
    match path.strip_prefix("~/") {
        Some(rest) => home().join(rest),
        None if path == "~" => home(),
        None => PathBuf::from(path),
    }
}
