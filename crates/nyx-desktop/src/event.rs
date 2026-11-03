//! Hyprland's event socket (`.socket2.sock`): one `NAME>>DATA` line per event.
//!
//! Hyprland sends most events twice, an old form and a `v2` form with ids. Only the `v2`
//! form is read where one exists; the old one parses as [`Event::Other`].
//! Window addresses arrive without `0x` here but with it in `j/clients`; both are kept
//! without it (see [`address`]).

/// One event, with the fields the desktop apps use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// `workspacev2>>ID,NAME`: the focused monitor shows another workspace.
    Workspace { id: i64, name: String },
    /// `focusedmonv2>>MONITOR,WORKSPACE_ID`
    FocusedMonitor { monitor: String, workspace: i64 },
    /// `activewindowv2>>ADDRESS` (empty when nothing has focus).
    ActiveWindow { address: Option<String> },
    /// `openwindow>>ADDRESS,WORKSPACE_NAME,CLASS,TITLE`
    OpenWindow { address: String, workspace: String, class: String, title: String },
    /// `closewindow>>ADDRESS`
    CloseWindow { address: String },
    /// `movewindowv2>>ADDRESS,WORKSPACE_ID,WORKSPACE_NAME`
    MoveWindow { address: String, workspace: i64 },
    /// `windowtitlev2>>ADDRESS,TITLE`
    WindowTitle { address: String, title: String },
    /// `changefloatingmode>>ADDRESS,0|1`
    Floating { address: String, floating: bool },
    /// `fullscreen>>0|1` (the focused window).
    Fullscreen { on: bool },
    /// `urgent>>ADDRESS`
    Urgent { address: String },
    /// `createworkspacev2>>ID,NAME`
    CreateWorkspace { id: i64, name: String },
    /// `destroyworkspacev2>>ID,NAME`
    DestroyWorkspace { id: i64, name: String },
    /// `moveworkspacev2>>ID,NAME,MONITOR`
    MoveWorkspace { id: i64, monitor: String },
    /// `renameworkspace>>ID,NAME`
    RenameWorkspace { id: i64, name: String },
    /// `activespecialv2>>ID,NAME,MONITOR` (`ID` empty when closed).
    ActiveSpecial { id: Option<i64>, monitor: String },
    /// `monitoraddedv2>>ID,NAME,DESCRIPTION`
    MonitorAdded { name: String },
    /// `monitorremovedv2>>ID,NAME,DESCRIPTION`
    MonitorRemoved { name: String },
    /// `togglegroup>>0|1,ADDRESSES`, `moveintogroup>>ADDRESS`, `moveoutofgroup>>ADDRESS`:
    /// a window group (tabs) was made, broken up, joined or left.
    Group,
    /// `configreloaded>>`
    ConfigReloaded,
    /// Anything else, kept for logging.
    Other { name: String, data: String },
}

impl Event {
    /// Parse one line (without its newline). Never fails: an event that doesn't parse
    /// is [`Event::Other`].
    pub fn parse(line: &str) -> Event {
        let (name, data) = line.split_once(">>").unwrap_or((line, ""));
        Self::parse_parts(name, data).unwrap_or_else(|| Event::Other { name: name.into(), data: data.into() })
    }

    fn parse_parts(name: &str, data: &str) -> Option<Event> {
        let f = |n: usize| data.splitn(n, ',').map(str::to_string).collect::<Vec<_>>();
        Some(match name {
            "workspacev2" => {
                let v = f(2);
                Event::Workspace { id: v.first()?.parse().ok()?, name: v.get(1)?.clone() }
            }
            "focusedmonv2" => {
                let v = f(2);
                Event::FocusedMonitor { monitor: v.first()?.clone(), workspace: v.get(1)?.parse().ok()? }
            }
            "activewindowv2" => {
                Event::ActiveWindow { address: (!data.is_empty() && data != "0x0" && data != "0").then(|| address(data)) }
            }
            "openwindow" => {
                let v = f(4);
                Event::OpenWindow {
                    address: address(v.first()?),
                    workspace: v.get(1)?.clone(),
                    class: v.get(2)?.clone(),
                    title: v.get(3).cloned().unwrap_or_default(),
                }
            }
            "closewindow" => Event::CloseWindow { address: address(data) },
            "movewindowv2" => {
                let v = f(3);
                Event::MoveWindow { address: address(v.first()?), workspace: v.get(1)?.parse().ok()? }
            }
            "windowtitlev2" => {
                let v = f(2);
                Event::WindowTitle { address: address(v.first()?), title: v.get(1).cloned().unwrap_or_default() }
            }
            "changefloatingmode" => {
                let v = f(2);
                Event::Floating { address: address(v.first()?), floating: v.get(1)? == "1" }
            }
            "fullscreen" => Event::Fullscreen { on: data == "1" },
            "urgent" => Event::Urgent { address: address(data) },
            "createworkspacev2" => {
                let v = f(2);
                Event::CreateWorkspace { id: v.first()?.parse().ok()?, name: v.get(1)?.clone() }
            }
            "destroyworkspacev2" => {
                let v = f(2);
                Event::DestroyWorkspace { id: v.first()?.parse().ok()?, name: v.get(1)?.clone() }
            }
            "moveworkspacev2" => {
                let v = f(3);
                Event::MoveWorkspace { id: v.first()?.parse().ok()?, monitor: v.get(2)?.clone() }
            }
            "renameworkspace" => {
                let v = f(2);
                Event::RenameWorkspace { id: v.first()?.parse().ok()?, name: v.get(1)?.clone() }
            }
            "activespecialv2" => {
                let v = f(3);
                Event::ActiveSpecial { id: v.first()?.parse().ok(), monitor: v.get(2)?.clone() }
            }
            "monitoraddedv2" => Event::MonitorAdded { name: f(3).get(1)?.clone() },
            "monitorremovedv2" => Event::MonitorRemoved { name: f(3).get(1)?.clone() },
            "configreloaded" => Event::ConfigReloaded,
            "togglegroup" | "moveintogroup" | "moveoutofgroup" => Event::Group,
            _ => return None,
        })
    }

    /// Whether the window list (or window placement) may have changed in ways the event
    /// doesn't carry, so `j/clients` should be read again.
    pub fn needs_windows(&self) -> bool {
        matches!(
            self,
            Event::OpenWindow { .. }
                | Event::CloseWindow { .. }
                | Event::MoveWindow { .. }
                | Event::Floating { .. }
                | Event::Fullscreen { .. }
                | Event::MoveWorkspace { .. }
                | Event::Group
                | Event::ConfigReloaded
        )
    }

    /// Whether workspaces or monitors should be read again.
    pub fn needs_layout(&self) -> bool {
        matches!(
            self,
            Event::CreateWorkspace { .. }
                | Event::DestroyWorkspace { .. }
                | Event::MoveWorkspace { .. }
                | Event::RenameWorkspace { .. }
                | Event::ActiveSpecial { .. }
                | Event::MonitorAdded { .. }
                | Event::MonitorRemoved { .. }
                | Event::ConfigReloaded
        )
    }
}

/// A window address without its `0x` prefix, lower case.
pub fn address(raw: &str) -> String {
    let raw = raw.trim();
    raw.strip_prefix("0x").unwrap_or(raw).to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_v2_events() {
        assert_eq!(Event::parse("workspacev2>>3,3"), Event::Workspace { id: 3, name: "3".into() });
        assert_eq!(Event::parse("focusedmonv2>>DP-1,7"), Event::FocusedMonitor { monitor: "DP-1".into(), workspace: 7 });
        assert_eq!(
            Event::parse("movewindowv2>>55ef7100a730,4,4"),
            Event::MoveWindow { address: "55ef7100a730".into(), workspace: 4 }
        );
        assert_eq!(
            Event::parse("activespecialv2>>-98,special:scratch,eDP-2"),
            Event::ActiveSpecial { id: Some(-98), monitor: "eDP-2".into() }
        );
        assert_eq!(Event::parse("activespecialv2>>,,eDP-2"), Event::ActiveSpecial { id: None, monitor: "eDP-2".into() });
        assert_eq!(Event::parse("monitorremovedv2>>1,HDMI-A-1,LG"), Event::MonitorRemoved { name: "HDMI-A-1".into() });
    }

    #[test]
    fn titles_keep_their_commas() {
        assert_eq!(
            Event::parse("openwindow>>55ef72c2b830,2,firefox,Hello, world — Firefox"),
            Event::OpenWindow {
                address: "55ef72c2b830".into(),
                workspace: "2".into(),
                class: "firefox".into(),
                title: "Hello, world — Firefox".into()
            }
        );
        assert_eq!(
            Event::parse("windowtitlev2>>abc,a, b, c"),
            Event::WindowTitle { address: "abc".into(), title: "a, b, c".into() }
        );
    }

    #[test]
    fn empty_focus_and_unknown_events() {
        assert_eq!(Event::parse("activewindowv2>>"), Event::ActiveWindow { address: None });
        assert_eq!(Event::parse("activewindowv2>>0x0"), Event::ActiveWindow { address: None });
        assert!(matches!(Event::parse("workspace>>3"), Event::Other { .. }));
        assert!(matches!(Event::parse("workspacev2>>notanumber,x"), Event::Other { .. }));
        assert!(matches!(Event::parse("garbage"), Event::Other { .. }));
    }

    #[test]
    fn addresses_match_between_events_and_clients() {
        assert_eq!(address("0x55EF7100A730"), address("55ef7100a730"));
    }
}
