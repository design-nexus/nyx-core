//! Where things live. Every path honours the XDG variables, so tests and test copies
//! run against scratch folders.

use std::path::PathBuf;

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

fn xdg(var: &str, fallback: &str) -> PathBuf {
    std::env::var_os(var).filter(|v| !v.is_empty()).map(PathBuf::from).unwrap_or_else(|| home().join(fallback))
}

pub fn config_home() -> PathBuf {
    xdg("XDG_CONFIG_HOME", ".config")
}

pub fn data_home() -> PathBuf {
    xdg("XDG_DATA_HOME", ".local/share")
}

pub fn cache_home() -> PathBuf {
    xdg("XDG_CACHE_HOME", ".cache")
}

pub fn state_home() -> PathBuf {
    xdg("XDG_STATE_HOME", ".local/state")
}

pub fn runtime_dir() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
}

/// Packaged plugins, in `$XDG_DATA_DIRS` order (the first wins among them).
pub fn system_dirs() -> Vec<PathBuf> {
    let dirs = std::env::var("XDG_DATA_DIRS")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    dirs.split(':').filter(|d| !d.is_empty()).map(|d| PathBuf::from(d).join("nyx/plugins")).collect()
}

/// First-party plugins installed outside a package (`install.sh` on Omarchy).
pub fn first_party_dir() -> PathBuf {
    std::env::var_os("NYX_PLUGIN_LIB_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".local/lib/nyx/plugins"))
}

/// Plugins the user added, linked or wrote.
pub fn user_dir() -> PathBuf {
    data_home().join("nyx/plugins")
}

/// A plugin's own writable folder (`$NYX_PLUGIN_STORAGE`).
pub fn storage_dir(id: &str) -> PathBuf {
    data_home().join("nyx/plugin-data").join(id)
}

pub fn state_file() -> PathBuf {
    config_home().join("nyx/plugins.toml")
}

pub fn index_cache() -> PathBuf {
    cache_home().join("nyx/plugin-index.toml")
}

/// Sockets the hosts make for plugins (D-Bus proxies, frames).
pub fn plugin_runtime_dir(host: &str, id: &str) -> PathBuf {
    runtime_dir().join("nyx-plugins").join(host).join(id)
}

/// A host's layout file: `bar.toml`, `control-center.toml`, `desktop-widgets.toml`.
pub fn layout_file(kind: crate::Kind) -> PathBuf {
    config_home().join("nyx").join(match kind {
        crate::Kind::Bar => "bar.toml",
        crate::Kind::ControlCenter => "control-center.toml",
        crate::Kind::Desktop => "desktop-widgets.toml",
    })
}

/// Write a file whole: a temporary file beside it, then a rename.
pub fn write_atomic(path: &std::path::Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    std::fs::write(&tmp, text)?;
    std::fs::rename(tmp, path)
}
