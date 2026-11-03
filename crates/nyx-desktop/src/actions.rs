//! Acting on windows through Hyprland's dispatchers.
//!
//! Each action is tried in Hyprland's Lua form (`hl.dsp.…`, Hyprland 0.55+) first and
//! in the classic dispatcher form when that is refused, so the same code works with a
//! Lua or a hyprlang config. Addresses are written as the model keeps them: lowercase
//! hex without `0x`.
//!
//! Every action is fire-and-forget: a window that closed in the meantime makes Hyprland
//! refuse the dispatcher, and that is not an error worth reporting.

use crate::hypr::Instance;

/// Omarchy's workspace for minimized windows.
pub const MINIMIZED: &str = "special:minimized";

/// The two spellings of one action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dispatch {
    pub lua: String,
    pub classic: String,
}

impl Dispatch {
    fn new(lua: String, classic: String) -> Dispatch {
        Dispatch { lua, classic }
    }

    /// Run it on `hypr`: the Lua form, else the classic one.
    pub fn run_on(&self, hypr: &Instance) -> bool {
        hypr.dispatch(&self.lua).is_ok() || hypr.dispatch(&self.classic).is_ok()
    }

    /// Run it on the running Hyprland.
    pub fn run(&self) -> bool {
        Instance::find().is_ok_and(|h| self.run_on(&h))
    }
}

fn target(address: &str) -> String {
    format!("window = \"address:0x{address}\"")
}

/// Bring a minimized window back to `workspace` without following it there.
pub fn restore(address: &str, workspace: &str) -> Dispatch {
    move_to_workspace(address, workspace, false)
}

/// Focus a window. Hyprland switches to its workspace (and monitor) first; the window
/// itself stays where it is, so tiling is untouched.
pub fn focus_window(address: &str) -> Dispatch {
    Dispatch::new(format!("hl.dsp.focus({{ {} }})", target(address)), format!("focuswindow address:0x{address}"))
}

/// Raise a (floating) window above its neighbours.
pub fn raise(address: &str) -> Dispatch {
    Dispatch::new(
        format!("hl.dsp.window.alter_zorder({{ mode = \"top\", {} }})", target(address)),
        format!("alterzorder top,address:0x{address}"),
    )
}

/// Ask a window to close (the app may still ask about unsaved work).
pub fn close_window(address: &str) -> Dispatch {
    Dispatch::new(format!("hl.dsp.window.close({{ {} }})", target(address)), format!("closewindow address:0x{address}"))
}

/// Switch a window between floating and tiled.
pub fn toggle_floating(address: &str) -> Dispatch {
    Dispatch::new(
        format!("hl.dsp.window.float({{ action = \"toggle\", {} }})", target(address)),
        format!("togglefloating address:0x{address}"),
    )
}

/// Maximize a window, or bring it back to its own size. The classic form acts on the
/// focused window, which is the one whose button was pressed.
pub fn toggle_maximized(address: &str) -> Dispatch {
    Dispatch::new(
        format!("hl.dsp.window.fullscreen({{ mode = \"maximized\", action = \"toggle\", {} }})", target(address)),
        "fullscreen 1".into(),
    )
}

/// Add a tag to a window (`on`), or take it off. Window rules can match tags.
pub fn tag(address: &str, tag: &str, on: bool) -> Dispatch {
    let sign = if on { '+' } else { '-' };
    Dispatch::new(
        format!("hl.dsp.window.tag({{ tag = \"{sign}{tag}\", {} }})", target(address)),
        format!("tagwindow {sign}{tag} address:0x{address}"),
    )
}

/// Minimize a window through Omarchy, which keeps it on [`MINIMIZED`] for the dock to
/// bring back. Omarchy only minimizes on floating workspaces.
pub fn minimize(address: &str) -> bool {
    std::process::Command::new("omarchy-hyprland-window-minimize")
        .arg(format!("0x{address}"))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .is_ok()
}

/// Move a window to a workspace (an id, `name:…` or `special:…`); `follow` also takes
/// the user there.
pub fn move_to_workspace(address: &str, workspace: &str, follow: bool) -> Dispatch {
    let classic = if follow { "movetoworkspace" } else { "movetoworkspacesilent" };
    Dispatch::new(
        format!("hl.dsp.window.move({{ workspace = \"{workspace}\", follow = {follow}, {} }})", target(address)),
        format!("{classic} {workspace},address:0x{address}"),
    )
}

/// Move a window to a monitor (by connector name), onto that monitor's active workspace.
pub fn move_to_monitor(address: &str, monitor: &str) -> Dispatch {
    Dispatch::new(
        format!("hl.dsp.window.move({{ monitor = \"{monitor}\", {} }})", target(address)),
        format!("movewindow mon:{monitor},address:0x{address}"),
    )
}

/// Take a window out of its group (tabs); it tiles beside the group.
pub fn leave_group(address: &str) -> Dispatch {
    Dispatch::new(
        format!("hl.dsp.window.move({{ out_of_group = true, {} }})", target(address)),
        format!("moveoutofgroup address:0x{address}"),
    )
}

/// What to run to bring a window to the user: restore it if minimized (to the
/// workspace in front of the user, `current`), then focus and raise it.
pub fn activate(address: &str, minimized: bool, current: i64) -> Vec<Dispatch> {
    let mut steps = Vec::new();
    if minimized && current >= 1 {
        steps.push(restore(address, &current.to_string()));
    }
    steps.push(focus_window(address));
    steps.push(raise(address));
    steps
}

/// Run [`activate`] on the running Hyprland.
pub fn focus(address: &str, minimized: bool, current: i64) {
    let Ok(hypr) = Instance::find() else { return };
    for step in activate(address, minimized, current) {
        step.run_on(&hypr);
    }
}

/// Run [`close_window`] on the running Hyprland.
pub fn close(address: &str) {
    close_window(address).run();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activate_restores_only_minimized_windows() {
        let plain = activate("abc", false, 3);
        assert_eq!(plain.len(), 2);
        assert_eq!(plain[0].classic, "focuswindow address:0xabc");
        assert_eq!(plain[1].classic, "alterzorder top,address:0xabc");
        let min = activate("abc", true, 3);
        assert_eq!(min[0].classic, "movetoworkspacesilent 3,address:0xabc");
        assert_eq!(min[0].lua, "hl.dsp.window.move({ workspace = \"3\", follow = false, window = \"address:0xabc\" })");
        assert_eq!(activate("abc", true, -99).len(), 2, "no real workspace to restore to");
    }

    #[test]
    fn both_spellings() {
        assert_eq!(close_window("1f").lua, "hl.dsp.window.close({ window = \"address:0x1f\" })");
        assert_eq!(toggle_floating("1f").classic, "togglefloating address:0x1f");
        assert_eq!(move_to_workspace("1f", "4", true).classic, "movetoworkspace 4,address:0x1f");
        assert_eq!(move_to_monitor("1f", "DP-1").classic, "movewindow mon:DP-1,address:0x1f");
        assert_eq!(leave_group("1f").lua, "hl.dsp.window.move({ out_of_group = true, window = \"address:0x1f\" })");
        assert_eq!(
            toggle_maximized("1f").lua,
            "hl.dsp.window.fullscreen({ mode = \"maximized\", action = \"toggle\", window = \"address:0x1f\" })"
        );
        assert_eq!(
            tag("1f", "nyx-own-controls", true).lua,
            "hl.dsp.window.tag({ tag = \"+nyx-own-controls\", window = \"address:0x1f\" })"
        );
        assert_eq!(tag("1f", "x", false).classic, "tagwindow -x address:0x1f");
    }
}
