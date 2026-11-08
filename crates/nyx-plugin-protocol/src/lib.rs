//! Messages between a Nyx plugin host (the bar, the Control Center, the desktop
//! widgets) and a plugin: one JSON object per line, the host's on the plugin's stdin,
//! the plugin's on its stdout. Diagnostics go to stderr.
//!
//! Every message has a `type`. Unknown fields are ignored, so a newer host or plugin
//! can add fields without breaking an older one; `api_version` changes only for
//! changes that would.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub mod view;

pub use view::{Align, Draw, GraphStyle, Node, NodeKind, Orientation, Paint, Transition, Vector};

pub const API_VERSION: u32 = 1;

/// From the host to the plugin.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HostMsg {
    Hello(Hello),
    /// The monitors, again whenever they change.
    Monitors { monitors: Vec<MonitorInfo> },
    /// The plugin's settings changed.
    Settings { settings: Value },
    /// Whether the plugin's parts can be seen. A plugin should pause work nobody sees.
    /// `item` narrows it to one item; without it, it applies to all of them.
    Visible {
        visible: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        item: Option<String>,
    },
    /// Hyprland's workspaces, for plugins granted `desktop`.
    Desktop(DesktopState),
    /// The desktop widgets placed from this plugin, again whenever one is added,
    /// removed, moved to another monitor or resized.
    Widgets { widgets: Vec<Widget> },
    /// The theme changed (light or dark, its name).
    Theme { theme: ThemeInfo },
    Event(Event),
    /// Exit now; the host kills plugins that haven't after a second.
    Shutdown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hello {
    pub api_version: u32,
    /// `bar`, `control-center` or `desktop`.
    pub kind: String,
    pub plugin: String,
    /// The parts the user enabled, in the manifest's order.
    #[serde(default)]
    pub items: Vec<String>,
    #[serde(default)]
    pub settings: Value,
    #[serde(default)]
    pub monitors: Vec<MonitorInfo>,
    #[serde(default)]
    pub theme: ThemeInfo,
    /// The permissions granted, by name (`network`, `desktop`, `frames`, …).
    #[serde(default)]
    pub permissions: Vec<String>,
    /// Where to connect for shared-memory frames, when `frames` is granted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_socket: Option<String>,
    /// Logical height of the bar, for bar plugins.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bar_height: Option<i32>,
    /// The desktop widgets placed from this plugin, for desktop plugins.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub widgets: Vec<Widget>,
}

/// One placed desktop widget. An item whose `monitor` is set shows on that monitor's
/// widget only; without one, on every widget of its part.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Widget {
    /// The part, which is also the item to send for it.
    pub item: String,
    pub monitor: String,
    /// The size the user picked (`compact`, `medium`, `large`, or `free`).
    pub size: String,
    /// Logical pixels the view gets.
    pub width: i32,
    pub height: i32,
    /// This placement's own options, from the layout file.
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub options: Value,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MonitorInfo {
    /// The connector (`DP-1`).
    pub name: String,
    /// Make, model and serial, or the connector when the screen doesn't say.
    #[serde(default)]
    pub stable_id: String,
    /// Logical size.
    #[serde(default)]
    pub width: i32,
    #[serde(default)]
    pub height: i32,
    #[serde(default = "one")]
    pub scale: f64,
    #[serde(default)]
    pub focused: bool,
}

fn one() -> f64 {
    1.0
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ThemeInfo {
    #[serde(default)]
    pub light: bool,
    #[serde(default)]
    pub name: String,
    /// The theme engine's generation the host applied; 0 from older hosts.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub generation: u64,
}

fn is_zero(v: &u64) -> bool {
    *v == 0
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DesktopState {
    #[serde(default)]
    pub monitors: Vec<DesktopMonitor>,
    #[serde(default)]
    pub workspaces: Vec<DesktopWorkspace>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_class: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_title: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DesktopMonitor {
    pub name: String,
    pub active_workspace: i64,
    #[serde(default)]
    pub focused: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DesktopWorkspace {
    pub id: i64,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub monitor: String,
    #[serde(default)]
    pub windows: u32,
    #[serde(default)]
    pub urgent: bool,
    #[serde(default)]
    pub fullscreen: bool,
}

/// Something the user did to an item.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub item: String,
    pub kind: EventKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monitor: Option<String>,
    /// Mouse button: 1 left, 2 middle, 3 right.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub button: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction: Option<Direction>,
    /// The node a control event came from (its `id`), or a menu entry's id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control: Option<String>,
    /// A toggle's state, a slider's value, an entry's text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
    /// Where the item is on screen, for clicks: so a plugin can open a window under it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<Anchor>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    Click,
    Scroll,
    /// A button, row or menu entry with an `action`.
    Action,
    Toggle,
    /// A slider moved.
    Change,
    /// Enter in an entry.
    Submit,
    PopoverOpen,
    PopoverClose,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Up,
    Down,
    Left,
    Right,
}

/// An item's place on its monitor, in logical pixels.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Anchor {
    pub monitor: String,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    /// The screen edge the host sits on (`top` for the bar).
    #[serde(default)]
    pub edge: String,
}

/// From the plugin to the host.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[allow(clippy::large_enum_variant)]
pub enum PluginMsg {
    Item(Item),
    Patch(Patch),
    Menu(Menu),
    Popover(Popover),
    Remove {
        item: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        monitor: Option<String>,
    },
    Log {
        #[serde(default)]
        level: LogLevel,
        message: String,
    },
    /// Ask for shared-memory buffers for a `frame` node (needs `frames`).
    FrameRequest {
        item: String,
        node: String,
        width: i32,
        height: i32,
    },
    /// A buffer is drawn and can be shown.
    FrameReady {
        item: String,
        node: String,
        buffer: u8,
    },
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogLevel {
    Debug,
    #[default]
    Info,
    Warning,
    Error,
}

/// One item. Sending it again with the same `id` (and monitor) replaces it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Item {
    pub id: String,
    /// For per-monitor plugins: the monitor this copy is for. None: every monitor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monitor: Option<String>,
    /// A layout tree. Without one, `icon`, `text`, `badge` and `progress` make a row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view: Option<Node>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub badge: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tooltip: Option<String>,
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub visible: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub class: Vec<String>,
    /// Commands the host runs on a click. Third-party plugins may only name commands
    /// from their manifest's `host_commands`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_click: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_middle_click: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_right_click: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_scroll_up: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_scroll_down: Option<Vec<String>>,
}

fn yes() -> bool {
    true
}

fn is_true(b: &bool) -> bool {
    *b
}

impl Item {
    pub fn new(id: impl Into<String>) -> Item {
        Item { id: id.into(), visible: true, ..Default::default() }
    }

    /// Every command the item asks the host to run.
    pub fn commands(&self) -> impl Iterator<Item = &Vec<String>> {
        [&self.on_click, &self.on_middle_click, &self.on_right_click, &self.on_scroll_up, &self.on_scroll_down]
            .into_iter()
            .flatten()
    }
}

/// Change properties of nodes in an item's view, by node id.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Patch {
    pub item: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monitor: Option<String>,
    pub nodes: Vec<NodePatch>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct NodePatch {
    pub id: String,
    /// Property names as in the node (`text`, `value`, `values`, `icon`, `active`,
    /// `visible`, `color`, `class`, `svg`, `commands`, `tooltip`, …).
    pub set: Map<String, Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Menu {
    pub item: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monitor: Option<String>,
    pub entries: Vec<MenuEntry>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MenuEntry {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked: Option<bool>,
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub separator: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<MenuEntry>,
}

/// The popover a left click opens. `view: None` removes it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Popover {
    pub item: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monitor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view: Option<Node>,
}

/// One line for the other side.
pub fn encode<T: Serialize>(msg: &T) -> String {
    let mut s = serde_json::to_string(msg).unwrap_or_default();
    s.push('\n');
    s
}

/// A line from a plugin. Interval plugins may print plain text instead of JSON: that
/// becomes the text of an item named after the plugin's first enabled part.
pub fn parse_plugin_line(line: &str, plain_item: Option<&str>) -> Result<PluginMsg, String> {
    let line = line.trim();
    if line.starts_with('{') {
        return serde_json::from_str(line).map_err(|e| e.to_string());
    }
    match plain_item {
        Some(id) if !line.is_empty() => Ok(PluginMsg::Item(Item { text: Some(line.to_string()), ..Item::new(id) })),
        _ => Err("not a JSON message".into()),
    }
}

pub fn parse_host_line(line: &str) -> Result<HostMsg, String> {
    serde_json::from_str(line.trim()).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn round<T: Serialize + for<'de> Deserialize<'de> + PartialEq + std::fmt::Debug>(v: &T) {
        let s = encode(v);
        assert!(s.ends_with('\n'));
        let back: T = serde_json::from_str(&s).unwrap();
        assert_eq!(&back, v);
    }

    #[test]
    fn host_messages_round_trip() {
        round(&HostMsg::Hello(Hello {
            api_version: API_VERSION,
            kind: "bar".into(),
            plugin: "nyx.clock".into(),
            items: vec!["time".into()],
            settings: json!({"format": "%H:%M"}),
            monitors: vec![MonitorInfo { name: "DP-1".into(), scale: 1.5, ..Default::default() }],
            theme: ThemeInfo { light: false, name: "nyx-dark".into(), generation: 3 },
            permissions: vec!["network".into()],
            frame_socket: None,
            bar_height: Some(32),
            widgets: vec![],
        }));
        round(&HostMsg::Widgets {
            widgets: vec![Widget {
                item: "large".into(),
                monitor: "DP-1".into(),
                size: "large".into(),
                width: 336,
                height: 336,
                options: json!({"seconds": true}),
            }],
        });
        round(&HostMsg::Visible { visible: false, item: None });
        round(&HostMsg::Event(Event {
            item: "volume".into(),
            kind: EventKind::Scroll,
            monitor: Some("DP-1".into()),
            button: None,
            direction: Some(Direction::Up),
            control: None,
            value: None,
            anchor: None,
        }));
        round(&HostMsg::Shutdown);
    }

    #[test]
    fn plugin_messages_round_trip() {
        let mut item = Item::new("time");
        item.text = Some("12:00".into());
        item.on_click = Some(vec!["nyx-calendar".into()]);
        round(&PluginMsg::Item(item));
        round(&PluginMsg::Patch(Patch {
            item: "cpu".into(),
            monitor: None,
            nodes: vec![NodePatch { id: "pct".into(), set: json!({"text": "12%"}).as_object().unwrap().clone() }],
        }));
        round(&PluginMsg::Menu(Menu {
            item: "tray-1".into(),
            monitor: None,
            entries: vec![MenuEntry { id: "quit".into(), label: "Quit".into(), enabled: true, ..Default::default() }],
        }));
        round(&PluginMsg::Remove { item: "x".into(), monitor: None });
    }

    #[test]
    fn minimal_item_from_a_script() {
        let msg = parse_plugin_line(r#"{"type":"item","id":"now","text":"☾ 71°"}"#, None).unwrap();
        let PluginMsg::Item(item) = msg else { panic!() };
        assert!(item.visible);
        assert_eq!(item.text.as_deref(), Some("☾ 71°"));
    }

    #[test]
    fn plain_text_from_an_interval_script() {
        let msg = parse_plugin_line("12:00\n", Some("time")).unwrap();
        assert_eq!(msg, PluginMsg::Item(Item { text: Some("12:00".into()), ..Item::new("time") }));
        assert!(parse_plugin_line("12:00", None).is_err());
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let msg = parse_plugin_line(r#"{"type":"remove","item":"a","future":1}"#, None).unwrap();
        assert_eq!(msg, PluginMsg::Remove { item: "a".into(), monitor: None });
    }
}
