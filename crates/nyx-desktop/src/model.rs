//! Monitors, workspaces and windows, read from Hyprland and kept current from events.

use crate::event::{Event, address};
use crate::hypr::Instance;
use anyhow::Result;
use serde::Deserialize;
use std::ops::RangeInclusive;

/// The workspaces a user switches between. Everything else is internal: special
/// (scratchpad) workspaces, named ones (negative ids, such as Settings'
/// `settings-loop-start`), the loop placeholder 11, test outputs' workspaces (201…) and
/// any future agent-only workspace. Docks, widgets and wallpapers ignore those.
pub const USER_WORKSPACES: RangeInclusive<i64> = 1..=10;

/// Whether `id` is one of the user's workspaces (1 to 10).
pub fn is_user_workspace(id: i64) -> bool {
    USER_WORKSPACES.contains(&id)
}

#[derive(Debug, Clone, PartialEq)]
pub struct Monitor {
    pub id: i64,
    /// Connector name: `eDP-1`, `DP-2`. Changes when a cable moves to another port.
    pub name: String,
    pub description: String,
    pub make: String,
    pub model: String,
    pub serial: String,
    /// Pixels, before scale.
    pub width: i32,
    pub height: i32,
    pub x: i32,
    pub y: i32,
    pub scale: f64,
    pub transform: i32,
    pub focused: bool,
    pub disabled: bool,
    pub active_workspace: i64,
    /// The special workspace shown over this monitor, if one is open.
    pub special_workspace: Option<i64>,
    /// Space reserved by bars: left, top, right, bottom (logical pixels).
    pub reserved: [i32; 4],
}

impl Monitor {
    /// An id that follows the screen rather than the port: make, model and serial, or
    /// the connector name when the screen doesn't report them (virtual outputs).
    /// Laptop panels often report no serial; make and model still pick them out.
    pub fn stable_id(&self) -> String {
        if self.make.trim().is_empty() && self.model.trim().is_empty() {
            return self.name.clone();
        }
        format!("{}|{}|{}", self.make.trim(), self.model.trim(), self.serial.trim())
    }

    /// Headless outputs (test windows, remote sessions) rather than a real screen.
    pub fn is_virtual(&self) -> bool {
        self.name.starts_with("HEADLESS-")
    }

    /// Size in logical pixels, after scale and rotation.
    pub fn logical_size(&self) -> (i32, i32) {
        let scale = if self.scale > 0.0 { self.scale } else { 1.0 };
        let (w, h) = ((self.width as f64 / scale).round() as i32, (self.height as f64 / scale).round() as i32);
        if self.transform % 2 == 1 { (h, w) } else { (w, h) }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Workspace {
    pub id: i64,
    pub name: String,
    pub monitor: String,
    pub windows: u32,
    pub has_fullscreen: bool,
    /// Hyprland's tiled layout (`dwindle`, `scrolling`, …). Omarchy's floating mode
    /// isn't a layout; read it with [`crate::omarchy::workspace_mode`].
    pub tiled_layout: String,
}

impl Workspace {
    pub fn is_user(&self) -> bool {
        is_user_workspace(self.id)
    }

    pub fn is_special(&self) -> bool {
        self.name.starts_with("special:")
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Window {
    /// Without `0x`, lower case (see [`crate::event::address`]).
    pub address: String,
    pub class: String,
    pub initial_class: String,
    pub title: String,
    pub workspace: i64,
    pub monitor: i64,
    pub floating: bool,
    /// 0 none, 1 maximized, 2 fullscreen, 3 both.
    pub fullscreen: u8,
    /// Logical position and size of the window on the layout.
    pub at: [i32; 2],
    pub size: [i32; 2],
    pub pid: i32,
    /// 0 is the window focused last, 1 the one before, …
    pub focus_order: i32,
    pub pinned: bool,
    pub hidden: bool,
    pub urgent: bool,
}

impl Window {
    /// The address as Hyprland's dispatchers want it: `address:0x…`.
    pub fn selector(&self) -> String {
        format!("address:0x{}", self.address)
    }

    /// Whether the window overlaps the rectangle (x, y, w, h).
    pub fn overlaps(&self, x: i32, y: i32, w: i32, h: i32) -> bool {
        let [wx, wy] = self.at;
        let [ww, wh] = self.size;
        wx < x + w && x < wx + ww && wy < y + h && y < wy + wh
    }
}

/// What an update changed, so a consumer redraws only what it shows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Changes {
    pub monitors: bool,
    pub workspaces: bool,
    /// The workspace shown on some monitor, or which monitor has focus.
    pub active: bool,
    pub windows: bool,
    /// The focused window.
    pub focus: bool,
    pub titles: bool,
}

impl Changes {
    pub const ALL: Changes = Changes { monitors: true, workspaces: true, active: true, windows: true, focus: true, titles: true };

    pub fn any(&self) -> bool {
        *self != Changes::default()
    }

    pub fn merge(&mut self, other: Changes) {
        self.monitors |= other.monitors;
        self.workspaces |= other.workspaces;
        self.active |= other.active;
        self.windows |= other.windows;
        self.focus |= other.focus;
        self.titles |= other.titles;
    }
}

/// Everything the desktop apps track.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Desktop {
    pub monitors: Vec<Monitor>,
    pub workspaces: Vec<Workspace>,
    pub windows: Vec<Window>,
    pub active_window: Option<String>,
    /// False while Hyprland is unreachable (restarting); the rest is the last known state.
    pub connected: bool,
}

impl Desktop {
    /// Read everything from Hyprland.
    pub fn snapshot(instance: &Instance) -> Result<Desktop> {
        let mut d = Desktop { connected: true, ..Default::default() };
        d.refresh_layout(instance)?;
        d.refresh_windows(instance)?;
        Ok(d)
    }

    /// Read monitors and workspaces again.
    pub fn refresh_layout(&mut self, instance: &Instance) -> Result<()> {
        let monitors: Vec<RawMonitor> = instance.json("monitors all")?;
        let workspaces: Vec<RawWorkspace> = instance.json("workspaces")?;
        self.set_layout(monitors, workspaces);
        Ok(())
    }

    /// Read windows and the focused window again.
    pub fn refresh_windows(&mut self, instance: &Instance) -> Result<()> {
        let clients: Vec<RawClient> = instance.json("clients")?;
        let active: RawActive = instance.json("activewindow").unwrap_or_default();
        self.set_windows(clients, active.address.as_deref());
        Ok(())
    }

    fn set_layout(&mut self, monitors: Vec<RawMonitor>, workspaces: Vec<RawWorkspace>) {
        self.monitors = monitors.into_iter().map(Monitor::from).collect();
        self.workspaces = workspaces.into_iter().map(Workspace::from).collect();
        self.workspaces.sort_by_key(|w| w.id);
    }

    fn set_windows(&mut self, clients: Vec<RawClient>, active: Option<&str>) {
        let urgent: Vec<String> = self.windows.iter().filter(|w| w.urgent).map(|w| w.address.clone()).collect();
        self.windows = clients.into_iter().filter(|c| c.mapped).map(Window::from).collect();
        for w in &mut self.windows {
            w.urgent = urgent.contains(&w.address);
        }
        self.windows.sort_by_key(|w| w.focus_order);
        self.active_window = active.filter(|a| !a.is_empty() && *a != "0x0").map(address);
    }

    /// Apply one event. Events that need fresh data from Hyprland
    /// ([`Event::needs_windows`], [`Event::needs_layout`]) are applied as far as they go
    /// here; the watcher reads the rest.
    pub fn apply(&mut self, event: &Event) -> Changes {
        let mut c = Changes::default();
        match event {
            Event::Workspace { id, .. } => {
                if let Some(m) = self.monitors.iter_mut().find(|m| m.focused) {
                    m.active_workspace = *id;
                }
                c.active = true;
            }
            Event::FocusedMonitor { monitor, workspace } => {
                for m in &mut self.monitors {
                    m.focused = &m.name == monitor;
                    if m.focused {
                        m.active_workspace = *workspace;
                    }
                }
                c.active = true;
            }
            Event::ActiveWindow { address } => {
                self.active_window = address.clone();
                if let Some(addr) = address {
                    self.bring_to_front(addr);
                    if let Some(w) = self.window_mut(addr) {
                        w.urgent = false;
                    }
                }
                c.focus = true;
            }
            Event::OpenWindow { address, class, title, workspace } => {
                if self.window(address).is_none() {
                    let ws = self.workspaces.iter().find(|w| &w.name == workspace).map_or(0, |w| w.id);
                    for w in &mut self.windows {
                        w.focus_order += 1;
                    }
                    self.windows.insert(
                        0,
                        Window {
                            address: address.clone(),
                            class: class.clone(),
                            initial_class: class.clone(),
                            title: title.clone(),
                            workspace: ws,
                            monitor: -1,
                            floating: false,
                            fullscreen: 0,
                            at: [0, 0],
                            size: [0, 0],
                            pid: 0,
                            focus_order: 0,
                            pinned: false,
                            hidden: false,
                            urgent: false,
                        },
                    );
                }
                c.windows = true;
            }
            Event::CloseWindow { address } => {
                if let Some(i) = self.windows.iter().position(|w| &w.address == address) {
                    let gone = self.windows.remove(i);
                    for w in &mut self.windows {
                        if w.focus_order > gone.focus_order {
                            w.focus_order -= 1;
                        }
                    }
                }
                if self.active_window.as_ref() == Some(address) {
                    self.active_window = None;
                    c.focus = true;
                }
                c.windows = true;
            }
            Event::MoveWindow { address, workspace } => {
                if let Some(w) = self.window_mut(address) {
                    w.workspace = *workspace;
                }
                c.windows = true;
            }
            Event::WindowTitle { address, title } => {
                if let Some(w) = self.window_mut(address) {
                    w.title = title.clone();
                }
                c.titles = true;
            }
            Event::Floating { address, floating } => {
                if let Some(w) = self.window_mut(address) {
                    w.floating = *floating;
                }
                c.windows = true;
            }
            Event::Fullscreen { .. } => c.windows = true,
            Event::Urgent { address } => {
                if self.active_window.as_ref() != Some(address)
                    && let Some(w) = self.window_mut(address)
                {
                    w.urgent = true;
                    c.windows = true;
                }
            }
            Event::CreateWorkspace { id, name } => {
                if !self.workspaces.iter().any(|w| w.id == *id) {
                    let monitor = self.focused_monitor().map(|m| m.name.clone()).unwrap_or_default();
                    self.workspaces.push(Workspace {
                        id: *id,
                        name: name.clone(),
                        monitor,
                        windows: 0,
                        has_fullscreen: false,
                        tiled_layout: String::new(),
                    });
                    self.workspaces.sort_by_key(|w| w.id);
                }
                c.workspaces = true;
            }
            Event::DestroyWorkspace { id, .. } => {
                self.workspaces.retain(|w| w.id != *id);
                c.workspaces = true;
            }
            Event::MoveWorkspace { id, monitor } => {
                if let Some(w) = self.workspaces.iter_mut().find(|w| w.id == *id) {
                    w.monitor = monitor.clone();
                }
                c.workspaces = true;
            }
            Event::RenameWorkspace { id, name } => {
                if let Some(w) = self.workspaces.iter_mut().find(|w| w.id == *id) {
                    w.name = name.clone();
                }
                c.workspaces = true;
            }
            Event::ActiveSpecial { id, monitor } => {
                if let Some(m) = self.monitors.iter_mut().find(|m| &m.name == monitor) {
                    m.special_workspace = id.filter(|i| *i != 0);
                }
                c.active = true;
            }
            Event::MonitorAdded { .. } => c.monitors = true,
            Event::MonitorRemoved { name } => {
                self.monitors.retain(|m| &m.name != name);
                c.monitors = true;
            }
            Event::ConfigReloaded => c = Changes::ALL,
            Event::Other { .. } => {}
        }
        c
    }

    fn bring_to_front(&mut self, addr: &str) {
        let Some(old) = self.window(addr).map(|w| w.focus_order) else { return };
        for w in &mut self.windows {
            if w.address == addr {
                w.focus_order = 0;
            } else if w.focus_order < old {
                w.focus_order += 1;
            }
        }
        self.windows.sort_by_key(|w| w.focus_order);
    }

    pub fn window(&self, address: &str) -> Option<&Window> {
        self.windows.iter().find(|w| w.address == address)
    }

    fn window_mut(&mut self, address: &str) -> Option<&mut Window> {
        self.windows.iter_mut().find(|w| w.address == address)
    }

    pub fn monitor(&self, name: &str) -> Option<&Monitor> {
        self.monitors.iter().find(|m| m.name == name)
    }

    pub fn focused_monitor(&self) -> Option<&Monitor> {
        self.monitors.iter().find(|m| m.focused)
    }

    /// The workspace on screen on each monitor, as (monitor name, workspace id).
    pub fn active_workspaces(&self) -> impl Iterator<Item = (&str, i64)> {
        self.monitors.iter().filter(|m| !m.disabled).map(|m| (m.name.as_str(), m.active_workspace))
    }

    /// The user's workspaces that exist, in order.
    pub fn user_workspaces(&self) -> impl Iterator<Item = &Workspace> {
        self.workspaces.iter().filter(|w| w.is_user())
    }

    /// Windows of an app, most recently focused first. `class` matches either the
    /// current or the initial class, ignoring case (some apps change theirs).
    pub fn windows_of<'a>(&'a self, class: &'a str) -> impl Iterator<Item = &'a Window> + 'a {
        self.windows.iter().filter(move |w| w.class.eq_ignore_ascii_case(class) || w.initial_class.eq_ignore_ascii_case(class))
    }

    /// Windows shown on a monitor right now: its active workspace, its special
    /// workspace if open, and pinned windows on it.
    pub fn visible_windows<'a>(&'a self, monitor: &'a Monitor) -> impl Iterator<Item = &'a Window> + 'a {
        self.windows.iter().filter(move |w| {
            !w.hidden
                && (w.workspace == monitor.active_workspace
                    || Some(w.workspace) == monitor.special_workspace
                    || (w.pinned && w.monitor == monitor.id))
        })
    }
}

// Hyprland's JSON. Fields not listed are ignored; missing ones take defaults so a newer
// or older Hyprland still parses.

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawRef {
    id: i64,
    name: String,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct RawMonitor {
    id: i64,
    name: String,
    description: String,
    make: String,
    model: String,
    serial: String,
    width: i32,
    height: i32,
    x: i32,
    y: i32,
    scale: f64,
    transform: i32,
    focused: bool,
    disabled: bool,
    active_workspace: RawRef,
    special_workspace: RawRef,
    reserved: Vec<i32>,
}

impl From<RawMonitor> for Monitor {
    fn from(r: RawMonitor) -> Self {
        let mut reserved = [0; 4];
        for (slot, v) in reserved.iter_mut().zip(r.reserved) {
            *slot = v;
        }
        Monitor {
            id: r.id,
            name: r.name,
            description: r.description,
            make: r.make,
            model: r.model,
            serial: r.serial,
            width: r.width,
            height: r.height,
            x: r.x,
            y: r.y,
            scale: r.scale,
            transform: r.transform,
            focused: r.focused,
            disabled: r.disabled,
            active_workspace: r.active_workspace.id,
            special_workspace: (r.special_workspace.id != 0).then_some(r.special_workspace.id),
            reserved,
        }
    }
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct RawWorkspace {
    id: i64,
    name: String,
    monitor: String,
    windows: u32,
    #[serde(rename = "hasfullscreen")]
    has_fullscreen: bool,
    tiled_layout: String,
}

impl From<RawWorkspace> for Workspace {
    fn from(r: RawWorkspace) -> Self {
        Workspace {
            id: r.id,
            name: r.name,
            monitor: r.monitor,
            windows: r.windows,
            has_fullscreen: r.has_fullscreen,
            tiled_layout: r.tiled_layout,
        }
    }
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct RawClient {
    address: String,
    mapped: bool,
    hidden: bool,
    at: [i32; 2],
    size: [i32; 2],
    workspace: RawRef,
    floating: bool,
    pinned: bool,
    monitor: i64,
    class: String,
    title: String,
    initial_class: String,
    pid: i32,
    fullscreen: u8,
    #[serde(rename = "focusHistoryID")]
    focus_history_id: i32,
}

impl From<RawClient> for Window {
    fn from(r: RawClient) -> Self {
        Window {
            address: address(&r.address),
            class: r.class,
            initial_class: r.initial_class,
            title: r.title,
            workspace: r.workspace.id,
            monitor: r.monitor,
            floating: r.floating,
            fullscreen: r.fullscreen,
            at: r.at,
            size: r.size,
            pid: r.pid,
            focus_order: r.focus_history_id,
            pinned: r.pinned,
            hidden: r.hidden,
            urgent: false,
        }
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawActive {
    address: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    const MONITORS: &str = include_str!("../tests/data/monitors.json");
    const WORKSPACES: &str = include_str!("../tests/data/workspaces.json");
    const CLIENTS: &str = include_str!("../tests/data/clients.json");

    fn desktop() -> Desktop {
        let mut d = Desktop { connected: true, ..Default::default() };
        d.set_layout(serde_json::from_str(MONITORS).unwrap(), serde_json::from_str(WORKSPACES).unwrap());
        d.set_windows(serde_json::from_str(CLIENTS).unwrap(), Some("0x55ef7100a730"));
        d
    }

    #[test]
    fn reads_hyprland_json() {
        let d = desktop();
        assert_eq!(d.monitors.len(), 2);
        let laptop = d.monitor("eDP-2").unwrap();
        assert_eq!(laptop.stable_id(), "Samsung Display Corp.|ATNA60DL01-0|");
        assert_eq!(laptop.logical_size(), (1600, 1000));
        assert_eq!(laptop.reserved, [0, 26, 0, 0]);
        assert!(!laptop.is_virtual());
        let headless = d.monitor("HEADLESS-20").unwrap();
        assert!(headless.is_virtual());
        assert_eq!(headless.stable_id(), "HEADLESS-20");
        assert_eq!(d.windows.len(), 3, "unmapped clients are left out");
        assert_eq!(d.windows[0].address, "55ef7100a730");
        assert_eq!(d.active_window.as_deref(), Some("55ef7100a730"));
    }

    #[test]
    fn only_1_to_10_are_user_workspaces() {
        let d = desktop();
        let ids: Vec<i64> = d.user_workspaces().map(|w| w.id).collect();
        assert_eq!(ids, vec![1, 2, 5], "not -1337 (loop start), 11 (loop end), 201 (headless) or special");
        assert!(!is_user_workspace(0));
        assert!(is_user_workspace(10));
        assert!(!is_user_workspace(11));
    }

    #[test]
    fn workspace_and_monitor_focus() {
        let mut d = desktop();
        let c = d.apply(&Event::parse("workspacev2>>2,2"));
        assert!(c.active && !c.windows);
        assert_eq!(d.monitor("eDP-2").unwrap().active_workspace, 2);
        d.apply(&Event::parse("focusedmonv2>>HEADLESS-20,201"));
        assert!(d.monitor("HEADLESS-20").unwrap().focused);
        assert!(!d.monitor("eDP-2").unwrap().focused);
        d.apply(&Event::parse("workspacev2>>202,202"));
        assert_eq!(d.monitor("HEADLESS-20").unwrap().active_workspace, 202);
        assert_eq!(d.monitor("eDP-2").unwrap().active_workspace, 2, "the other monitor keeps its workspace");
    }

    #[test]
    fn focus_order_is_most_recent_first() {
        let mut d = desktop();
        let firefox: Vec<&str> = d.windows_of("firefox").map(|w| w.address.as_str()).collect();
        assert_eq!(firefox, vec!["55ef7100a730", "55ef72c2b830"]);
        d.apply(&Event::parse("activewindowv2>>55ef72c2b830"));
        let firefox: Vec<&str> = d.windows_of("FireFox").map(|w| w.address.as_str()).collect();
        assert_eq!(firefox, vec!["55ef72c2b830", "55ef7100a730"]);
        let orders: Vec<i32> = d.windows.iter().map(|w| w.focus_order).collect();
        assert_eq!(orders, vec![0, 1, 2]);
    }

    #[test]
    fn open_move_close() {
        let mut d = desktop();
        let c = d.apply(&Event::parse("openwindow>>abc123,5,kitty,~"));
        assert!(c.windows);
        let w = d.window("abc123").unwrap();
        assert_eq!((w.workspace, w.focus_order), (5, 0));
        d.apply(&Event::parse("movewindowv2>>abc123,2,2"));
        assert_eq!(d.window("abc123").unwrap().workspace, 2);
        d.apply(&Event::parse("closewindow>>abc123"));
        assert!(d.window("abc123").is_none());
        let orders: Vec<i32> = d.windows.iter().map(|w| w.focus_order).collect();
        assert_eq!(orders, vec![0, 1, 2]);
    }

    #[test]
    fn urgent_until_focused() {
        let mut d = desktop();
        d.apply(&Event::parse("urgent>>55ef710f3ae0"));
        assert!(d.window("55ef710f3ae0").unwrap().urgent);
        d.apply(&Event::parse("activewindowv2>>55ef710f3ae0"));
        assert!(!d.window("55ef710f3ae0").unwrap().urgent);
    }

    #[test]
    fn visible_windows_and_overlap() {
        let d = desktop();
        let laptop = d.monitor("eDP-2").unwrap();
        let visible: Vec<&str> = d.visible_windows(laptop).map(|w| w.address.as_str()).collect();
        assert_eq!(visible, vec!["55ef7100a730"]);
        let w = d.window("55ef7100a730").unwrap();
        // Window at (10, 36) size 1580×954: reaches y = 990 of a 1000-high screen.
        assert!(w.overlaps(700, 940, 200, 56));
        assert!(!w.overlaps(700, 992, 200, 8));
    }

    #[test]
    fn special_workspace_and_monitor_removal() {
        let mut d = desktop();
        d.apply(&Event::parse("activespecialv2>>-98,special:scratch,eDP-2"));
        assert_eq!(d.monitor("eDP-2").unwrap().special_workspace, Some(-98));
        d.apply(&Event::parse("activespecialv2>>,,eDP-2"));
        assert_eq!(d.monitor("eDP-2").unwrap().special_workspace, None);
        let c = d.apply(&Event::parse("monitorremovedv2>>1,HEADLESS-20,"));
        assert!(c.monitors);
        assert!(d.monitor("HEADLESS-20").is_none());
    }
}
