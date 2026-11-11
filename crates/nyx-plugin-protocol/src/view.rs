//! Layout trees: what an item, a popover or a tile looks like. The host builds the
//! widgets, so every plugin follows the theme; colours are theme token names
//! (`accent`, `success`, `text_tertiary`, …) or, discouraged, `#rrggbb`.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Node {
    #[serde(flatten)]
    pub kind: NodeKind,
    /// For patches and for events from controls.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub class: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<i32>,
    /// Top, right, bottom, left.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub margin: Option<[i32; 4]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_size: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tooltip: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visible: Option<bool>,
    /// Take the spare space along the parent box.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub expand: bool,
    /// Properties the host sets while the pointer is over the item, and puts back after.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hover: Option<Map<String, Value>>,
    /// The same while a button is held.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pressed: Option<Map<String, Value>>,
    /// Makes any node clickable (a box holding a day of a calendar, a workspace's
    /// number and icons): a click sends an `action` event with this as its `control`.
    /// Buttons and rows have their own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "node", rename_all = "snake_case")]
pub enum NodeKind {
    Box {
        #[serde(default)]
        orientation: Orientation,
        #[serde(default = "four")]
        spacing: i32,
        #[serde(default)]
        align: Align,
        #[serde(default)]
        children: Vec<Node>,
    },
    /// Children on top of each other, the first at the bottom.
    Overlay {
        #[serde(default)]
        children: Vec<Node>,
    },
    /// One child at a time: `show` is the id of the one to show.
    Stack {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        show: Option<String>,
        #[serde(default)]
        transition: Transition,
        #[serde(default)]
        children: Vec<Node>,
    },
    Label {
        #[serde(default)]
        text: String,
        /// Pango markup (`<b>`, `<span size="small">`, …).
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        markup: bool,
    },
    Icon {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        size: Option<i32>,
    },
    Image {
        /// A file the plugin can read (inside its sandbox: under /plugin or /storage).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
        /// The image itself, base64.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        data: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        radius: Option<f64>,
    },
    Badge {
        #[serde(default)]
        text: String,
    },
    Progress {
        #[serde(default)]
        value: f64,
    },
    Graph {
        #[serde(default)]
        values: Vec<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max: Option<f64>,
        #[serde(default)]
        style: GraphStyle,
    },
    Spinner,
    Vector(Vector),
    /// Pixels the plugin draws itself, over shared memory (needs `frames`).
    Frame,
    Row {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        icon: Option<String>,
        #[serde(default)]
        title: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subtitle: Option<String>,
        /// Sent back as an `action` event's `control`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        action: Option<String>,
    },
    Toggle {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(default)]
        active: bool,
    },
    Slider {
        #[serde(default)]
        min: f64,
        #[serde(default = "hundred")]
        max: f64,
        #[serde(default = "one")]
        step: f64,
        #[serde(default)]
        value: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        icon: Option<String>,
    },
    Button {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        icon: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        action: Option<String>,
    },
    Entry {
        #[serde(default)]
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        placeholder: Option<String>,
    },
    Separator,
    Scroll {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_height: Option<i32>,
        child: Box<Node>,
    },
}

impl Default for NodeKind {
    fn default() -> Self {
        NodeKind::Box { orientation: Orientation::default(), spacing: 4, align: Align::default(), children: vec![] }
    }
}

fn four() -> i32 {
    4
}

fn one() -> f64 {
    1.0
}

fn hundred() -> f64 {
    100.0
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Orientation {
    #[default]
    Horizontal,
    Vertical,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Align {
    #[default]
    Center,
    Start,
    End,
    Fill,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Transition {
    #[default]
    Crossfade,
    SlideUp,
    SlideDown,
    SlideLeft,
    SlideRight,
    None,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphStyle {
    #[default]
    Line,
    Bars,
}

/// A drawing: an SVG document, or commands in a `view_box` (default: the node's size).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Vector {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub svg: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commands: Vec<Draw>,
    /// x, y, width, height.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view_box: Option<[f64; 4]>,
}

impl Vector {
    /// Elements drawn, for the host's limit.
    pub fn size(&self) -> usize {
        self.commands.len() + self.svg.as_deref().map_or(0, |s| s.matches('<').count())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Draw {
    /// SVG path data (`M L H V C S Q T Z`, absolute or relative).
    Path {
        d: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fill: Option<Paint>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stroke: Option<Paint>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        width: Option<f64>,
    },
    Line {
        x1: f64,
        y1: f64,
        x2: f64,
        y2: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stroke: Option<Paint>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        width: Option<f64>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        round: bool,
    },
    /// Angles in degrees, clockwise from 3 o'clock.
    Arc {
        cx: f64,
        cy: f64,
        r: f64,
        start: f64,
        end: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fill: Option<Paint>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stroke: Option<Paint>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        width: Option<f64>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        round: bool,
    },
    Circle {
        cx: f64,
        cy: f64,
        r: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fill: Option<Paint>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stroke: Option<Paint>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        width: Option<f64>,
    },
    Rect {
        x: f64,
        y: f64,
        w: f64,
        h: f64,
        #[serde(default)]
        radius: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fill: Option<Paint>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stroke: Option<Paint>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        width: Option<f64>,
    },
    Text {
        x: f64,
        y: f64,
        text: String,
        #[serde(default = "twelve")]
        size: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fill: Option<Paint>,
        /// `start`, `center` or `end`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        align: Option<String>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        bold: bool,
    },
    Save,
    Restore,
    Translate {
        x: f64,
        y: f64,
    },
    Rotate {
        deg: f64,
    },
    Scale {
        x: f64,
        y: f64,
    },
}

fn twelve() -> f64 {
    12.0
}

/// A colour (token or `#rrggbb[aa]`), or a gradient.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Paint {
    Color(String),
    Linear { linear: Linear },
    Radial { radial: Radial },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Linear {
    pub x1: f64,
    pub y1: f64,
    pub x2: f64,
    pub y2: f64,
    /// Offset 0–1 and colour.
    pub stops: Vec<(f64, String)>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Radial {
    pub cx: f64,
    pub cy: f64,
    pub r: f64,
    pub stops: Vec<(f64, String)>,
}

impl Node {
    pub fn new(kind: NodeKind) -> Node {
        Node { kind, ..Default::default() }
    }

    pub fn id(mut self, id: impl Into<String>) -> Node {
        self.id = Some(id.into());
        self
    }

    pub fn class(mut self, class: impl Into<String>) -> Node {
        self.class.push(class.into());
        self
    }

    pub fn children(&self) -> &[Node] {
        match &self.kind {
            NodeKind::Box { children, .. } | NodeKind::Overlay { children } | NodeKind::Stack { children, .. } => {
                children
            }
            NodeKind::Scroll { child, .. } => std::slice::from_ref(child),
            _ => &[],
        }
    }

    /// Every node in the tree, depth first.
    pub fn walk(&self, f: &mut impl FnMut(&Node)) {
        f(self);
        for c in self.children() {
            c.walk(f);
        }
    }

    /// Apply a patch's properties to the node with its id (so a host can draw the item
    /// again later as it now looks). Returns whether the node was found.
    pub fn apply_patch(&mut self, id: &str, set: &Map<String, Value>) -> bool {
        fn walk(v: &mut Value, id: &str, set: &Map<String, Value>) -> bool {
            let Value::Object(m) = v else { return false };
            if m.get("id").and_then(Value::as_str) == Some(id) {
                for (k, val) in set {
                    m.insert(k.clone(), val.clone());
                }
                return true;
            }
            for key in ["children", "child"] {
                match m.get_mut(key) {
                    Some(Value::Array(a)) => {
                        if a.iter_mut().any(|c| walk(c, id, set)) {
                            return true;
                        }
                    }
                    Some(c @ Value::Object(_)) => {
                        if walk(c, id, set) {
                            return true;
                        }
                    }
                    _ => {}
                }
            }
            false
        }
        let Ok(mut v) = serde_json::to_value(&*self) else { return false };
        if !walk(&mut v, id, set) {
            return false;
        }
        match serde_json::from_value(v) {
            Ok(n) => {
                *self = n;
                true
            }
            Err(_) => false,
        }
    }

    /// Nodes in the tree, for the host's limits.
    pub fn count(&self) -> usize {
        let mut n = 0;
        self.walk(&mut |node| {
            n += 1;
            if let NodeKind::Vector(v) = &node.kind {
                n += v.size();
            }
        });
        n
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_two_line_item() {
        let v = json!({
            "node": "box", "orientation": "vertical", "spacing": 0,
            "children": [
                {"node": "label", "id": "time", "text": "12:00", "font_size": 13},
                {"node": "label", "text": "<small>Tue 3</small>", "markup": true, "color": "dim_text"}
            ],
            "hover": {"background": "hover"}
        });
        let node: Node = serde_json::from_value(v).unwrap();
        assert_eq!(node.children().len(), 2);
        assert_eq!(node.children()[0].id.as_deref(), Some("time"));
        assert_eq!(node.count(), 3);
        let back = serde_json::to_value(&node).unwrap();
        let again: Node = serde_json::from_value(back).unwrap();
        assert_eq!(again, node);
    }

    #[test]
    fn a_vector_ring() {
        let v = json!({"node": "vector", "view_box": [0, 0, 24, 24], "commands": [
            {"op": "circle", "cx": 12, "cy": 12, "r": 9, "stroke": "border", "width": 3},
            {"op": "arc", "cx": 12, "cy": 12, "r": 9, "start": -90, "end": 90, "stroke": "accent", "width": 3, "round": true},
            {"op": "rect", "x": 0, "y": 0, "w": 4, "h": 4, "fill": {"linear": {"x1": 0, "y1": 0, "x2": 4, "y2": 0, "stops": [[0, "accent"], [1, "#ff000080"]]}}}
        ]});
        let node: Node = serde_json::from_value(v).unwrap();
        let NodeKind::Vector(vec) = &node.kind else { panic!() };
        assert_eq!(vec.commands.len(), 3);
        assert!(matches!(vec.commands[2], Draw::Rect { fill: Some(Paint::Linear { .. }), .. }));
        assert_eq!(node.count(), 4);
    }

    #[test]
    fn patches_apply_to_stored_views() {
        let mut node: Node = serde_json::from_value(json!({"node": "box", "children": [
            {"node": "label", "id": "pct", "text": "1%"},
            {"node": "scroll", "child": {"node": "label", "id": "deep", "text": "a"}}
        ]}))
        .unwrap();
        let set = json!({"text": "42%"}).as_object().unwrap().clone();
        assert!(node.apply_patch("pct", &set));
        assert!(node.apply_patch("deep", &set));
        assert!(!node.apply_patch("missing", &set));
        let NodeKind::Label { text, .. } = &node.children()[0].kind else { panic!() };
        assert_eq!(text, "42%");
    }

    #[test]
    fn svg_counts_elements() {
        let v = Vector { svg: Some("<svg><path d='M0 0'/><circle/></svg>".into()), ..Default::default() };
        assert_eq!(v.size(), 4);
    }
}
