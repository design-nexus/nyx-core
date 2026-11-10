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

/// Whether `id` is one of the user's workspaces (1 to 10). A virtual monitor's own
/// workspaces (201 to 210 for the second region, …) are user workspaces too; they need
/// the workspace's name to tell apart, so use [`Workspace::is_user`] or
/// [`Desktop::is_user_workspace`] where a workspace or the desktop is at hand.
pub fn is_user_workspace(id: i64) -> bool {
    USER_WORKSPACES.contains(&id)
}

/// The number a user sees for a workspace: 1 to 10, also for a virtual monitor's own
/// workspaces, which Hyprland numbers from the region's base (`base + n`, base a multiple
/// of 100 from 200) and names `n`.
pub fn user_number(id: i64, name: &str) -> Option<u32> {
    if is_user_workspace(id) {
        return Some(id as u32);
    }
    let n = id % 100;
    (id > 100 && USER_WORKSPACES.contains(&n) && name == n.to_string()).then_some(n as u32)
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
    /// Hz; 0 when Hyprland didn't say.
    pub refresh_rate: f64,
    pub focused: bool,
    pub disabled: bool,
    pub active_workspace: i64,
    /// The special workspace shown over this monitor, if one is open.
    pub special_workspace: Option<i64>,
    /// Space reserved by bars: left, top, right, bottom (logical pixels).
    pub reserved: [i32; 4],
}

/// Where a virtual monitor sits on its physical output.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct VirtualRegion {
    /// The physical output's connector name.
    pub host: String,
    /// The physical output's [`Monitor::stable_id`].
    pub host_stable_id: String,
    pub host_description: String,
    /// 1-based, left to right.
    pub index: u32,
    pub count: u32,
    /// Pixels from the physical output's left edge.
    pub pixel_offset: i32,
    /// Its own workspaces are `workspace_base + 1` to `workspace_base + 10`; 0 for the
    /// first region of the first split output, which keeps the ordinary 1 to 10.
    pub workspace_base: i64,
}

/// A physical output split into virtual monitors. It isn't a screen of its own any more
/// (no wl_output, no workspaces), so it is kept out of [`Desktop::monitors`].
#[derive(Debug, Clone, PartialEq, Default)]
pub struct VirtualHost {
    pub name: String,
    pub stable_id: String,
    pub description: String,
    pub width: i32,
    pub height: i32,
    pub x: i32,
    pub y: i32,
    pub scale: f64,
    /// Its regions' connector names, left to right.
    pub regions: Vec<String>,
}

impl Monitor {
    /// An id that follows the screen rather than the port: make, model and serial, or
    /// the connector name when the screen doesn't report them (virtual outputs).
    /// Laptop panels often report no serial; make and model still pick them out.
    ///
    /// A virtual monitor's is its physical screen's followed by `#v<n>`: Hyprland gives it
    /// the screen's make and model and the serial `<serial>#v<n>`.
    pub fn stable_id(&self) -> String {
        stable_id_of(&self.make, &self.model, &self.serial, &self.name)
    }

    /// Headless outputs (test windows, remote sessions) rather than a real screen. A
    /// virtual monitor of a real screen isn't one: it is part of that screen.
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
        self.number().is_some()
    }

    /// The number the user sees (1 to 10), see [`user_number`].
    pub fn number(&self) -> Option<u32> {
        user_number(self.id, &self.name)
    }

    pub fn is_special(&self) -> bool {
        self.name.starts_with("special:")
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Window {
    /// Without `0x`, lower case (see [`crate::event::address`]).
    pub address: String,
    /// Hyprland's `stableId`: the identifier the same window has in the standard
    /// `ext-foreign-toplevel-list` protocol (used to capture its image). Empty until
    /// Hyprland has been asked about a window that just opened.
    pub stable_id: String,
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
    /// The windows of its Hyprland group (tabs), in group order, itself included; empty
    /// when it isn't in a group. The visible tab is the one that isn't `hidden`.
    pub grouped: Vec<String>,
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
    /// Layer surfaces opened or closed.
    pub layers: bool,
}

impl Changes {
    pub const ALL: Changes =
        Changes { monitors: true, workspaces: true, active: true, windows: true, focus: true, titles: true, layers: true };

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
        self.layers |= other.layers;
    }
}

/// Everything the desktop apps track.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Desktop {
    pub monitors: Vec<Monitor>,
    /// Physical outputs split into virtual monitors; their regions are in `monitors`.
    pub hosts: Vec<VirtualHost>,
    /// The monitors that are virtual monitors, by name, and where they sit.
    pub regions: std::collections::BTreeMap<String, VirtualRegion>,
    pub workspaces: Vec<Workspace>,
    pub windows: Vec<Window>,
    pub active_window: Option<String>,
    /// Mapped layer surfaces by namespace, with how many share it.
    pub layers: std::collections::BTreeMap<String, u32>,
    /// False while Hyprland is unreachable (restarting); the rest is the last known state.
    pub connected: bool,
}

impl Desktop {
    /// Read everything from Hyprland.
    pub fn snapshot(instance: &Instance) -> Result<Desktop> {
        let mut d = Desktop { connected: true, ..Default::default() };
        d.refresh_layout(instance)?;
        d.refresh_windows(instance)?;
        // Layers are extra: a reply that doesn't parse leaves them empty, never the rest.
        let _ = d.refresh_layers(instance);
        Ok(d)
    }

    /// Read the mapped layer surfaces again.
    pub fn refresh_layers(&mut self, instance: &Instance) -> Result<()> {
        let monitors: std::collections::HashMap<String, RawLayers> = instance.json("layers")?;
        self.set_layers(monitors);
        Ok(())
    }

    fn set_layers(&mut self, monitors: std::collections::HashMap<String, RawLayers>) {
        self.layers.clear();
        for m in monitors.into_values() {
            for l in m.levels.into_values().flatten() {
                *self.layers.entry(l.namespace).or_default() += 1;
            }
        }
    }

    /// Whether a layer surface with this namespace is mapped.
    pub fn layer_open(&self, namespace: &str) -> bool {
        self.layers.get(namespace).is_some_and(|n| *n > 0)
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
        let (hosts, monitors): (Vec<_>, Vec<_>) =
            monitors.into_iter().partition(|m| m.r#virtual.as_ref().is_some_and(|v| v.role == "host"));
        self.hosts = hosts.into_iter().map(VirtualHost::from).collect();
        self.regions =
            monitors.iter().filter_map(|m| Some((m.name.clone(), VirtualRegion::from_raw(m.r#virtual.as_ref()?)?))).collect();
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
                            stable_id: String::new(),
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
                            grouped: Vec::new(),
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
            Event::OpenLayer { namespace } => {
                *self.layers.entry(namespace.clone()).or_default() += 1;
                c.layers = true;
            }
            Event::CloseLayer { namespace } => {
                if let Some(n) = self.layers.get_mut(namespace) {
                    *n = n.saturating_sub(1);
                    if *n == 0 {
                        self.layers.remove(namespace);
                    }
                }
                c.layers = true;
            }
            Event::MonitorsChanged | Event::VirtualMonitors { .. } => c.monitors = true,
            Event::Group => c.windows = true,
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

    /// Whether the workspace with this id is one of the user's (see [`user_number`]).
    pub fn is_user_workspace(&self, id: i64) -> bool {
        is_user_workspace(id) || self.workspaces.iter().any(|w| w.id == id && w.is_user())
    }

    /// The user's workspaces on one monitor, in order. Every monitor shows its own; a
    /// virtual monitor's are its region's.
    pub fn user_workspaces_on<'a>(&'a self, monitor: &'a str) -> impl Iterator<Item = &'a Workspace> + 'a {
        self.workspaces.iter().filter(move |w| w.monitor == monitor && w.is_user())
    }

    /// Where a monitor sits on its physical output, when it is a virtual monitor.
    pub fn region(&self, monitor: &str) -> Option<&VirtualRegion> {
        self.regions.get(monitor)
    }

    /// The physical output a monitor belongs to: itself, or its host when it's a region.
    pub fn physical_name<'a>(&'a self, monitor: &'a str) -> &'a str {
        self.region(monitor).map_or(monitor, |r| r.host.as_str())
    }

    /// The workspace id a user's workspace `number` (1 to 10) has on a monitor.
    pub fn workspace_id(&self, monitor: &str, number: u32) -> i64 {
        self.region(monitor).map_or(0, |r| r.workspace_base) + number as i64
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
    refresh_rate: f64,
    focused: bool,
    disabled: bool,
    active_workspace: RawRef,
    special_workspace: RawRef,
    reserved: Vec<i32>,
    /// Nyx's Hyprland: `null`, or a region's or a host's virtual monitor details.
    r#virtual: Option<RawVirtual>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct RawVirtual {
    role: String,
    host: String,
    host_description: String,
    host_make: String,
    host_model: String,
    host_serial: String,
    index: u32,
    count: u32,
    pixel_offset: i32,
    workspace_base: i64,
    regions: Vec<String>,
}

fn stable_id_of(make: &str, model: &str, serial: &str, name: &str) -> String {
    if make.trim().is_empty() && model.trim().is_empty() {
        return name.to_string();
    }
    format!("{}|{}|{}", make.trim(), model.trim(), serial.trim())
}

impl VirtualRegion {
    fn from_raw(v: &RawVirtual) -> Option<VirtualRegion> {
        (v.role == "region").then(|| VirtualRegion {
            host_stable_id: stable_id_of(&v.host_make, &v.host_model, &v.host_serial, &v.host),
            host: v.host.clone(),
            host_description: v.host_description.clone(),
            index: v.index,
            count: v.count,
            pixel_offset: v.pixel_offset,
            workspace_base: v.workspace_base,
        })
    }
}

impl From<RawMonitor> for VirtualHost {
    fn from(r: RawMonitor) -> Self {
        VirtualHost {
            stable_id: stable_id_of(&r.make, &r.model, &r.serial, &r.name),
            name: r.name,
            description: r.description,
            width: r.width,
            height: r.height,
            x: r.x,
            y: r.y,
            scale: r.scale,
            regions: r.r#virtual.map(|v| v.regions).unwrap_or_default(),
        }
    }
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
            refresh_rate: r.refresh_rate,
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
    #[serde(rename = "stableId")]
    stable_id: String,
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
    #[serde(default)]
    grouped: Vec<String>,
}

impl From<RawClient> for Window {
    fn from(r: RawClient) -> Self {
        Window {
            address: address(&r.address),
            stable_id: r.stable_id,
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
            grouped: r.grouped.iter().map(|a| address(a)).collect(),
        }
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawActive {
    address: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawLayers {
    levels: std::collections::HashMap<String, Vec<RawLayer>>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawLayer {
    namespace: String,
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
        assert_eq!(d.windows[0].stable_id, "18000369");
        assert_eq!(d.active_window.as_deref(), Some("55ef7100a730"));
    }

    #[test]
    fn reads_window_groups() {
        let json = r#"[
            {"address":"0xA1","stableId":"1","mapped":true,"hidden":false,"at":[0,0],"size":[1,1],
             "workspace":{"id":1,"name":"1"},"floating":false,"pinned":false,"monitor":0,"class":"a",
             "title":"","initialClass":"a","pid":1,"fullscreen":0,"focusHistoryID":0,"grouped":["0xA1","0xB2"]},
            {"address":"0xB2","stableId":"2","mapped":true,"hidden":true,"at":[0,0],"size":[1,1],
             "workspace":{"id":1,"name":"1"},"floating":false,"pinned":false,"monitor":0,"class":"b",
             "title":"","initialClass":"b","pid":2,"fullscreen":0,"focusHistoryID":1}
        ]"#;
        let clients: Vec<RawClient> = serde_json::from_str(json).unwrap();
        let mut d = Desktop::default();
        d.set_windows(clients, None);
        assert_eq!(d.windows[0].grouped, ["a1", "b2"]);
        assert!(d.windows[1].grouped.is_empty(), "missing means not grouped");
        assert!(Event::parse("moveintogroup>>a1").needs_windows());
        assert!(Event::parse("togglegroup>>1,a1,b2").needs_windows());
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
    fn layers_open_and_close() {
        let mut d = desktop();
        let json = r#"{"eDP-2":{"levels":{"0":[{"namespace":"wallpaper"}],"3":[{"namespace":"nyx-control-center"},{"namespace":"nyx-control-center-catcher"}]}},
                       "HEADLESS-20":{"levels":{"0":[{"namespace":"wallpaper"}]}}}"#;
        d.set_layers(serde_json::from_str(json).unwrap());
        assert_eq!(d.layers["wallpaper"], 2);
        assert!(d.layer_open("nyx-control-center"));
        let c = d.apply(&Event::parse("closelayer>>nyx-control-center"));
        assert!(c.layers && !c.windows);
        assert!(!d.layer_open("nyx-control-center"));
        assert!(!d.layers.contains_key("nyx-control-center"));
        d.apply(&Event::parse("closelayer>>nyx-control-center"));
        assert!(!d.layer_open("nyx-control-center"), "an extra close never goes below zero");
        d.apply(&Event::parse("openlayer>>nyx-control-center"));
        assert!(d.layer_open("nyx-control-center"));
    }

    #[test]
    fn virtual_monitors_are_regions_of_a_hidden_host() {
        let monitors = r#"[
            {"id":3,"name":"DP-3.v1","make":"Samsung","model":"Odyssey G95NC","serial":"H1AK#v1","width":3840,"height":2160,
             "x":0,"y":0,"scale":1.25,"activeWorkspace":{"id":3,"name":"3"},
             "virtual":{"role":"region","host":"DP-3","hostId":1,"hostDescription":"Samsung Odyssey G95NC H1AK","hostMake":"Samsung",
                        "hostModel":"Odyssey G95NC","hostSerial":"H1AK","index":1,"count":2,"pixelOffset":0,"workspaceBase":0}},
            {"id":4,"name":"DP-3.v2","make":"Samsung","model":"Odyssey G95NC","serial":"H1AK#v2","width":3840,"height":2160,
             "x":3072,"y":0,"scale":1.25,"activeWorkspace":{"id":204,"name":"4"},
             "virtual":{"role":"region","host":"DP-3","hostId":1,"hostDescription":"Samsung Odyssey G95NC H1AK","hostMake":"Samsung",
                        "hostModel":"Odyssey G95NC","hostSerial":"H1AK","index":2,"count":2,"pixelOffset":3840,"workspaceBase":200}},
            {"id":1,"name":"DP-3","make":"Samsung","model":"Odyssey G95NC","serial":"H1AK","width":7680,"height":2160,"x":0,"y":0,
             "scale":1.25,"activeWorkspace":{"id":0,"name":""},"virtual":{"role":"host","regions":["DP-3.v1","DP-3.v2"]}},
            {"id":0,"name":"eDP-1","make":"BOE","model":"NE160","serial":"","width":2560,"height":1600,"x":6144,"y":0,"scale":1.6,
             "activeWorkspace":{"id":1,"name":"1"},"virtual":null}
        ]"#;
        let workspaces = r#"[
            {"id":1,"name":"1","monitor":"eDP-1"},{"id":3,"name":"3","monitor":"DP-3.v1"},
            {"id":201,"name":"1","monitor":"DP-3.v2"},{"id":204,"name":"4","monitor":"DP-3.v2"},
            {"id":205,"name":"205","monitor":"DP-3.v2"}
        ]"#;
        let mut d = Desktop::default();
        d.set_layout(serde_json::from_str(monitors).unwrap(), serde_json::from_str(workspaces).unwrap());

        assert_eq!(d.monitors.len(), 3, "the host isn't a screen of its own");
        assert_eq!(d.hosts.len(), 1);
        assert_eq!(d.hosts[0].regions, ["DP-3.v1", "DP-3.v2"]);
        assert_eq!(d.hosts[0].stable_id, "Samsung|Odyssey G95NC|H1AK");

        let right = d.monitor("DP-3.v2").unwrap();
        let region = d.region("DP-3.v2").unwrap();
        assert_eq!((region.index, region.count, region.workspace_base), (2, 2, 200));
        assert_eq!(region.host_stable_id, "Samsung|Odyssey G95NC|H1AK");
        assert_eq!(right.stable_id(), format!("{}#v2", region.host_stable_id), "a region's id is its screen's plus #v<n>");
        assert!(!right.is_virtual(), "a region of a real screen is a real place to put things");
        assert_eq!(d.workspace_id("DP-3.v2", 3), 203);
        assert_eq!(d.workspace_id("DP-3.v1", 3), 3);
        assert_eq!(d.workspace_id("eDP-1", 3), 3);
        assert_eq!(d.physical_name("DP-3.v2"), "DP-3");
        assert!(d.region("eDP-1").is_none());

        let numbers: Vec<(i64, Option<u32>)> = d.workspaces.iter().map(|w| (w.id, w.number())).collect();
        assert_eq!(numbers, vec![(1, Some(1)), (3, Some(3)), (201, Some(1)), (204, Some(4)), (205, None)]);
        let on_right: Vec<i64> = d.user_workspaces_on("DP-3.v2").map(|w| w.id).collect();
        assert_eq!(on_right, vec![201, 204]);
        assert!(d.is_user_workspace(204));
        assert!(!d.is_user_workspace(205), "a workspace that only happens to have a high id isn't a region's");

        assert!(Event::parse("virtualmonitorv2>>DP-3,merged,0").needs_layout());
        assert!(d.apply(&Event::parse("virtualmonitorv2>>DP-3,split,2")).monitors);
    }

    #[test]
    fn refresh_rate_is_read() {
        assert_eq!(desktop().monitor("eDP-2").unwrap().refresh_rate, 120.0);
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
