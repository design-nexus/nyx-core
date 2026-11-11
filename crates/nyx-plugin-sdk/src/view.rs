//! Builders for views, so plugin code reads like the layout.
//!
//! ```
//! use nyx_plugin_sdk::view::*;
//! let v = hbox([icon("audio-volume-high-symbolic"), label("42%").id("pct")]).spacing(6);
//! let item = item("volume").view(v).tooltip("Volume");
//! ```

pub use crate::protocol::view::{Align, Draw, GraphStyle, Linear, Node, NodeKind, Orientation, Paint, Radial, Transition, Vector};
pub use crate::protocol::{Item, MenuEntry};
use serde_json::{Map, Value};

pub fn item(id: &str) -> Item {
    Item::new(id)
}

/// Builder methods on items.
pub trait ItemExt {
    fn view(self, v: Node) -> Item;
    fn text(self, t: impl Into<String>) -> Item;
    fn icon(self, i: impl Into<String>) -> Item;
    fn tooltip(self, t: impl Into<String>) -> Item;
    fn hidden(self, hide: bool) -> Item;
    fn class(self, c: impl Into<String>) -> Item;
    fn monitor(self, m: impl Into<String>) -> Item;
    fn on_click(self, argv: &[&str]) -> Item;
    fn on_right_click(self, argv: &[&str]) -> Item;
    fn on_middle_click(self, argv: &[&str]) -> Item;
}

fn argv(a: &[&str]) -> Option<Vec<String>> {
    Some(a.iter().map(|s| s.to_string()).collect())
}

impl ItemExt for Item {
    fn view(mut self, v: Node) -> Item {
        self.view = Some(v);
        self
    }
    fn text(mut self, t: impl Into<String>) -> Item {
        self.text = Some(t.into());
        self
    }
    fn icon(mut self, i: impl Into<String>) -> Item {
        self.icon = Some(i.into());
        self
    }
    fn tooltip(mut self, t: impl Into<String>) -> Item {
        self.tooltip = Some(t.into());
        self
    }
    fn hidden(mut self, hide: bool) -> Item {
        self.visible = !hide;
        self
    }
    fn class(mut self, c: impl Into<String>) -> Item {
        self.class.push(c.into());
        self
    }
    fn monitor(mut self, m: impl Into<String>) -> Item {
        self.monitor = Some(m.into());
        self
    }
    fn on_click(mut self, a: &[&str]) -> Item {
        self.on_click = argv(a);
        self
    }
    fn on_right_click(mut self, a: &[&str]) -> Item {
        self.on_right_click = argv(a);
        self
    }
    fn on_middle_click(mut self, a: &[&str]) -> Item {
        self.on_middle_click = argv(a);
        self
    }
}

/// Builder methods on nodes.
pub trait NodeExt {
    fn id(self, id: &str) -> Node;
    fn class(self, c: &str) -> Node;
    fn color(self, token: &str) -> Node;
    fn background(self, token: &str) -> Node;
    fn font_size(self, px: f64) -> Node;
    fn size(self, w: i32, h: i32) -> Node;
    fn width(self, w: i32) -> Node;
    fn height(self, h: i32) -> Node;
    fn margin(self, t: i32, r: i32, b: i32, l: i32) -> Node;
    fn tooltip(self, t: &str) -> Node;
    fn expand(self) -> Node;
    fn visible(self, v: bool) -> Node;
    fn spacing(self, s: i32) -> Node;
    fn hover(self, props: &[(&str, Value)]) -> Node;
    fn pressed(self, props: &[(&str, Value)]) -> Node;
    /// Clickable: a click sends an `action` event with `control` set to this.
    fn action(self, action: &str) -> Node;
}

fn map(props: &[(&str, Value)]) -> Map<String, Value> {
    props.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
}

impl NodeExt for Node {
    fn id(mut self, id: &str) -> Node {
        self.id = Some(id.into());
        self
    }
    fn class(mut self, c: &str) -> Node {
        self.class.push(c.into());
        self
    }
    fn color(mut self, token: &str) -> Node {
        self.color = Some(token.into());
        self
    }
    fn background(mut self, token: &str) -> Node {
        self.background = Some(token.into());
        self
    }
    fn font_size(mut self, px: f64) -> Node {
        self.font_size = Some(px);
        self
    }
    fn size(mut self, w: i32, h: i32) -> Node {
        self.width = Some(w);
        self.height = Some(h);
        self
    }
    fn width(mut self, w: i32) -> Node {
        self.width = Some(w);
        self
    }
    fn height(mut self, h: i32) -> Node {
        self.height = Some(h);
        self
    }
    fn margin(mut self, t: i32, r: i32, b: i32, l: i32) -> Node {
        self.margin = Some([t, r, b, l]);
        self
    }
    fn tooltip(mut self, t: &str) -> Node {
        self.tooltip = Some(t.into());
        self
    }
    fn expand(mut self) -> Node {
        self.expand = true;
        self
    }
    fn visible(mut self, v: bool) -> Node {
        self.visible = Some(v);
        self
    }
    fn spacing(mut self, s: i32) -> Node {
        if let NodeKind::Box { spacing, .. } = &mut self.kind {
            *spacing = s;
        }
        self
    }
    fn hover(mut self, props: &[(&str, Value)]) -> Node {
        self.hover = Some(map(props));
        self
    }
    fn pressed(mut self, props: &[(&str, Value)]) -> Node {
        self.pressed = Some(map(props));
        self
    }
    fn action(mut self, action: &str) -> Node {
        self.action = Some(action.into());
        self
    }
}

fn n(kind: NodeKind) -> Node {
    Node::new(kind)
}

pub fn hbox(children: impl IntoIterator<Item = Node>) -> Node {
    n(NodeKind::Box { orientation: Orientation::Horizontal, spacing: 4, align: Align::Center, children: children.into_iter().collect() })
}

pub fn vbox(children: impl IntoIterator<Item = Node>) -> Node {
    n(NodeKind::Box { orientation: Orientation::Vertical, spacing: 4, align: Align::Fill, children: children.into_iter().collect() })
}

pub fn overlay(children: impl IntoIterator<Item = Node>) -> Node {
    n(NodeKind::Overlay { children: children.into_iter().collect() })
}

pub fn stack(show: &str, children: impl IntoIterator<Item = Node>) -> Node {
    n(NodeKind::Stack { show: Some(show.into()), transition: Transition::Crossfade, children: children.into_iter().collect() })
}

pub fn label(text: impl Into<String>) -> Node {
    n(NodeKind::Label { text: text.into(), markup: false })
}

pub fn markup(text: impl Into<String>) -> Node {
    n(NodeKind::Label { text: text.into(), markup: true })
}

pub fn icon(name: &str) -> Node {
    n(NodeKind::Icon { name: name.into(), size: None })
}

pub fn icon_sized(name: &str, size: i32) -> Node {
    n(NodeKind::Icon { name: name.into(), size: Some(size) })
}

pub fn image_path(path: &str) -> Node {
    n(NodeKind::Image { path: Some(path.into()), data: None, radius: None })
}

pub fn image_data(base64: String) -> Node {
    n(NodeKind::Image { path: None, data: Some(base64), radius: None })
}

pub fn badge(text: impl Into<String>) -> Node {
    n(NodeKind::Badge { text: text.into() })
}

pub fn progress(value: f64) -> Node {
    n(NodeKind::Progress { value })
}

pub fn graph(values: Vec<f64>, max: Option<f64>) -> Node {
    n(NodeKind::Graph { values, max, style: GraphStyle::Line })
}

pub fn bars(values: Vec<f64>, max: Option<f64>) -> Node {
    n(NodeKind::Graph { values, max, style: GraphStyle::Bars })
}

pub fn spinner() -> Node {
    n(NodeKind::Spinner)
}

pub fn vector(view_box: [f64; 4], commands: Vec<Draw>) -> Node {
    n(NodeKind::Vector(Vector { svg: None, commands, view_box: Some(view_box) }))
}

pub fn svg(document: impl Into<String>) -> Node {
    n(NodeKind::Vector(Vector { svg: Some(document.into()), commands: vec![], view_box: None }))
}

pub fn frame(id: &str, w: i32, h: i32) -> Node {
    n(NodeKind::Frame).id(id).size(w, h)
}

pub fn row(icon: Option<&str>, title: impl Into<String>, subtitle: Option<String>, action: Option<&str>) -> Node {
    n(NodeKind::Row { icon: icon.map(String::from), title: title.into(), subtitle, action: action.map(String::from) })
}

pub fn toggle(id: &str, label: &str, active: bool) -> Node {
    n(NodeKind::Toggle { label: Some(label.into()), active }).id(id)
}

pub fn slider(id: &str, min: f64, max: f64, step: f64, value: f64, icon: Option<&str>) -> Node {
    n(NodeKind::Slider { min, max, step, value, icon: icon.map(String::from) }).id(id)
}

pub fn button(action: &str, label: Option<&str>, icon: Option<&str>) -> Node {
    n(NodeKind::Button { label: label.map(String::from), icon: icon.map(String::from), action: Some(action.into()) })
}

pub fn entry(id: &str, text: &str, placeholder: Option<&str>) -> Node {
    n(NodeKind::Entry { text: text.into(), placeholder: placeholder.map(String::from) }).id(id)
}

pub fn separator() -> Node {
    n(NodeKind::Separator)
}

pub fn scroll(max_height: i32, child: Node) -> Node {
    n(NodeKind::Scroll { max_height: Some(max_height), child: Box::new(child) })
}

/// A popover header: a bold title.
pub fn header(title: &str) -> Node {
    markup(format!("<b>{}</b>", escape(title)))
}

/// A popover's footer, below a divider: a link at the left (a click sends `action`) and,
/// optionally, something at the right ("Updates every 3 s"). The bar styles `pfoot`.
pub fn footer(action: &str, link: &str, right: Option<Node>) -> Node {
    let mut children = vec![button(action, Some(link), None).class("flat").class("link"), label("").expand()];
    children.extend(right);
    hbox(children).spacing(8).class("pfoot")
}

pub fn color(c: &str) -> Paint {
    Paint::Color(c.into())
}

/// For Pango markup.
pub fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

pub fn menu_entry(id: &str, label: &str) -> MenuEntry {
    MenuEntry { id: id.into(), label: label.into(), enabled: true, ..Default::default() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builders_make_the_documented_json() {
        let v = hbox([icon("x"), label("42%").id("pct").color("accent")]).spacing(6);
        let j = serde_json::to_value(&v).unwrap();
        assert_eq!(j["node"], "box");
        assert_eq!(j["spacing"], 6);
        assert_eq!(j["children"][1]["id"], "pct");
        assert_eq!(j["children"][1]["color"], "accent");
        let i = item("volume").view(v).on_right_click(&["wpctl", "set-mute", "@DEFAULT_AUDIO_SINK@", "toggle"]);
        assert_eq!(i.on_right_click.unwrap().len(), 4);
    }
}
