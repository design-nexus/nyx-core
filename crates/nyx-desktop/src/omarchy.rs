//! Omarchy's per-workspace mode: tiling (Hyprland's `dwindle`), scrolling or floating.
//!
//! Floating isn't a Hyprland layout, so Omarchy keeps each workspace's mode in
//! `$XDG_STATE_HOME/omarchy/workspace-layouts/<id>.lua` (`mode = "floating"`) and the
//! default in `default.lua` (`default = "scrolling"`). This reads those files the way
//! `omarchy-hyprland-workspace-layout-current` does, and changes the mode only through
//! Omarchy's own command, so Omarchy stays the one that applies it.
//!
//! The folder changes when the mode does; watch it (`gio::FileMonitor` on
//! [`layouts_dir`]) to stay current without polling.

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Hyprland's `dwindle`; the menu and bar call it tiling.
    Tiling,
    Scrolling,
    Floating,
}

impl Mode {
    pub const ALL: [Mode; 3] = [Mode::Tiling, Mode::Scrolling, Mode::Floating];

    /// The name in Omarchy's files and Hyprland's `tiledLayout`.
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Tiling => "dwindle",
            Mode::Scrolling => "scrolling",
            Mode::Floating => "floating",
        }
    }

    /// The name people see.
    pub fn label(self) -> &'static str {
        match self {
            Mode::Tiling => "Tiling",
            Mode::Scrolling => "Scrolling",
            Mode::Floating => "Floating",
        }
    }

    pub fn parse(name: &str) -> Option<Mode> {
        match name {
            "dwindle" => Some(Mode::Tiling),
            "scrolling" => Some(Mode::Scrolling),
            "floating" => Some(Mode::Floating),
            _ => None,
        }
    }

    /// The mode Omarchy's toggle goes to next (`dwindle → scrolling → floating`).
    pub fn next(self) -> Mode {
        match self {
            Mode::Tiling => Mode::Scrolling,
            Mode::Scrolling => Mode::Floating,
            Mode::Floating => Mode::Tiling,
        }
    }
}

/// `$XDG_STATE_HOME/omarchy/workspace-layouts`.
pub fn layouts_dir() -> PathBuf {
    let state = std::env::var_os("XDG_STATE_HOME")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/state"));
    state.join("omarchy/workspace-layouts")
}

/// The mode of workspace `id`. `tiled_layout` is Hyprland's `tiledLayout` for it (from
/// [`crate::Workspace`]), used when Omarchy has no file saying otherwise.
pub fn workspace_mode(id: i64, tiled_layout: &str) -> Mode {
    workspace_mode_in(&layouts_dir(), id, tiled_layout, default_from_hyprland)
}

/// As [`workspace_mode`], reading `dir`; `hypr_default` gives Hyprland's
/// `general.layout` when `default.lua` doesn't exist.
pub fn workspace_mode_in(dir: &Path, id: i64, tiled_layout: &str, hypr_default: impl FnOnce() -> Option<String>) -> Mode {
    let mut mode = None;
    if id >= 1 {
        mode = read_key(&dir.join(format!("{id}.lua")), "mode");
        if mode.is_none() && dir.join("default.lua").exists() {
            mode = read_key(&dir.join("default.lua"), "default").or_else(hypr_default);
        }
    }
    let name = mode.unwrap_or_else(|| tiled_layout.to_string());
    Mode::parse(&name).unwrap_or(Mode::Tiling)
}

/// The value of `key = "value"` in a Lua state file, as Omarchy's `sed` reads it.
fn read_key(path: &Path, key: &str) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let needle = format!("{key} = \"");
    text.lines().find_map(|line| {
        let start = line.find(&needle)? + needle.len();
        let rest = &line[start..];
        let value = &rest[..rest.find('"')?];
        (!value.is_empty() && value.bytes().all(|b| b.is_ascii_lowercase())).then(|| value.to_string())
    })
}

fn default_from_hyprland() -> Option<String> {
    let out = Command::new("hyprctl").args(["getoption", "general.layout", "-j"]).output().ok()?;
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    v.get("str")?.as_str().map(str::to_string)
}

/// Move the **active** workspace to its next mode (or the previous one with `back`),
/// through `omarchy-hyprland-workspace-layout-toggle`. Omarchy only changes the active
/// workspace; it also shows its own one-line toast for the change.
pub fn cycle(back: bool) -> Result<()> {
    let mut cmd = Command::new("omarchy-hyprland-workspace-layout-toggle");
    if back {
        cmd.arg("back");
    }
    let status = cmd.status().context("run omarchy-hyprland-workspace-layout-toggle")?;
    if !status.success() {
        bail!("omarchy-hyprland-workspace-layout-toggle failed ({status})");
    }
    Ok(())
}

/// The toggle steps (forward, or back) that take `from` to `to` in the fewest presses.
pub fn steps(from: Mode, to: Mode) -> (usize, bool) {
    let forward = (0..3).find(|n| (0..*n).fold(from, |m, _| m.next()) == to).unwrap_or(0);
    if forward <= 1 { (forward, false) } else { (3 - forward, true) }
}

/// Set the active workspace's mode to `to`, one or two presses of Omarchy's toggle.
pub fn set_active(from: Mode, to: Mode) -> Result<()> {
    let (count, back) = steps(from, to);
    for _ in 0..count {
        cycle(back)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("nyx-desktop-omarchy-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn workspace_file_wins() {
        let d = dir("file");
        std::fs::write(d.join("3.lua"), "o.workspace_mode({ workspace = \"3\", mode = \"floating\" })\n").unwrap();
        std::fs::write(d.join("default.lua"), "o.workspace_default({ default = \"scrolling\" })\n").unwrap();
        assert_eq!(workspace_mode_in(&d, 3, "dwindle", || None), Mode::Floating);
        assert_eq!(workspace_mode_in(&d, 4, "dwindle", || None), Mode::Scrolling, "no file: the default");
    }

    #[test]
    fn without_files_hyprland_decides() {
        let d = dir("none");
        assert_eq!(workspace_mode_in(&d, 1, "scrolling", || None), Mode::Scrolling);
        assert_eq!(workspace_mode_in(&d, 1, "", || None), Mode::Tiling);
        assert_eq!(workspace_mode_in(&d, -1337, "scrolling", || None), Mode::Scrolling, "named workspaces have no file");
    }

    #[test]
    fn old_files_without_mode_follow_the_default() {
        let d = dir("old");
        std::fs::write(d.join("2.lua"), "o.workspace_layout({ workspace = \"2\", layout = \"dwindle\" })\n").unwrap();
        std::fs::write(d.join("default.lua"), "-- empty\n").unwrap();
        assert_eq!(workspace_mode_in(&d, 2, "dwindle", || Some("scrolling".into())), Mode::Scrolling);
    }

    #[test]
    fn fewest_presses() {
        assert_eq!(steps(Mode::Tiling, Mode::Tiling), (0, false));
        assert_eq!(steps(Mode::Tiling, Mode::Scrolling), (1, false));
        assert_eq!(steps(Mode::Tiling, Mode::Floating), (1, true));
        assert_eq!(steps(Mode::Floating, Mode::Tiling), (1, false));
        assert_eq!(steps(Mode::Scrolling, Mode::Tiling), (1, true));
    }
}
