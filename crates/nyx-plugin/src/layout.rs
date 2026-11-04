//! Placing parts in the hosts' layout files, keeping the user's comments: `bar.toml`
//! (sections of entries), `control-center.toml` (pages of tiles) and
//! `desktop-widgets.toml` (widgets with anchors).

use anyhow::{Context, Result};
use std::path::Path;
use toml_edit::{Array, DocumentMut, InlineTable, Item, Table, Value, value};

/// The bar's first layout, written when it first starts.
pub const DEFAULT_BAR: &str = r#"# Nyx Bar layout. Entries are plugins ("nyx.clock") or one of their items
# ("nyx.system/volume"). Which items show at all is chosen with `nyx plugin configure`;
# enabled items this file doesn't place go to the section their plugin asks for.
left = ["nyx.workspaces"]
center = ["nyx.clock"]
right = [
    "nyx.tray",
    "nyx.media",
    "nyx.weather",
    "nyx.stats",
    "nyx.system/network",
    "nyx.system/bluetooth",
    "nyx.system/volume",
    "nyx.system/battery",
    "nyx.panels/notifications",
    "nyx.panels/control-center",
]

# Bar height in logical pixels.
# height = 32

# The bar draws with GTK's cairo renderer, which uses far less memory. For the GPU one:
# renderer = "gpu"

# Settings for one plugin:
# [plugins."nyx.clock"]
# format = "%a %b %-d  %H:%M"

# Limits for one plugin (third-party plugins get strict defaults):
# [plugins."acme.ping".limits]
# memory = "256M"

# A different layout on one monitor (connector or make|model|serial):
# [monitors."HDMI-A-1"]
# right = ["nyx.clock"]
"#;

pub const SECTIONS: [&str; 3] = ["left", "center", "right"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Position {
    End,
    Index(usize),
    Before(String),
    After(String),
}

pub fn read(path: &Path) -> Result<DocumentMut> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    text.parse().with_context(|| format!("reading {}", path.display()))
}

pub fn read_bar(path: &Path) -> Result<DocumentMut> {
    if path.exists() { read(path) } else { Ok(DEFAULT_BAR.parse().expect("the default layout parses")) }
}

pub fn write(path: &Path, doc: &DocumentMut) -> Result<()> {
    crate::paths::write_atomic(path, &doc.to_string()).with_context(|| format!("writing {}", path.display()))
}

fn entry_matches(entry: &str, plugin_or_entry: &str) -> bool {
    entry == plugin_or_entry
        || (!plugin_or_entry.contains('/') && entry.split('/').next() == Some(plugin_or_entry))
}

fn sections_table<'a>(doc: &'a mut DocumentMut, monitor: Option<&str>) -> &'a mut Table {
    match monitor {
        None => doc.as_table_mut(),
        Some(m) => {
            if !doc.contains_table("monitors") {
                let mut t = Table::new();
                t.set_implicit(true);
                doc.insert("monitors", Item::Table(t));
            }
            let monitors = doc["monitors"].as_table_mut().expect("monitors is a table");
            if !monitors.contains_table(m) {
                monitors.insert(m, Item::Table(Table::new()));
            }
            monitors[m].as_table_mut().expect("a table")
        }
    }
}

fn strip(table: &mut Table, plugin_or_entry: &str) {
    for s in SECTIONS {
        if let Some(arr) = table.get_mut(s).and_then(|i| i.as_array_mut()) {
            arr.retain(|v| !v.as_str().is_some_and(|e| entry_matches(e, plugin_or_entry)));
        }
    }
}

fn array_of<'a>(table: &'a mut Table, section: &str) -> &'a mut Array {
    if !table.get(section).is_some_and(|i| i.is_array()) {
        table.insert(section, value(Array::new()));
    }
    table[section].as_array_mut().expect("an array")
}

/// Take a plugin (all its entries) or one entry out of every section.
pub fn bar_unplace(doc: &mut DocumentMut, plugin_or_entry: &str) {
    strip(doc.as_table_mut(), plugin_or_entry);
    if let Some(monitors) = doc.get_mut("monitors").and_then(|m| m.as_table_mut()) {
        for (_, item) in monitors.iter_mut() {
            if let Some(t) = item.as_table_mut() {
                strip(t, plugin_or_entry);
            }
        }
    }
}

/// Put an entry in a section (of one monitor, or everywhere), moving it if it's
/// already somewhere in that table.
pub fn bar_place(doc: &mut DocumentMut, monitor: Option<&str>, entry: &str, section: &str, pos: &Position) {
    let table = sections_table(doc, monitor);
    for s in SECTIONS {
        if let Some(arr) = table.get_mut(s).and_then(|i| i.as_array_mut()) {
            arr.retain(|v| v.as_str() != Some(entry));
        }
    }
    let arr = array_of(table, section);
    let entries: Vec<String> = arr.iter().filter_map(|v| v.as_str().map(String::from)).collect();
    let at = match pos {
        Position::End => entries.len(),
        Position::Index(i) => (*i).min(entries.len()),
        Position::Before(e) => entries.iter().position(|x| entry_matches(x, e)).unwrap_or(entries.len()),
        Position::After(e) => entries.iter().rposition(|x| entry_matches(x, e)).map_or(entries.len(), |i| i + 1),
    };
    arr.insert(at, entry);
    if arr.len() > 3 {
        for v in arr.iter_mut() {
            v.decor_mut().set_prefix("\n    ");
        }
        arr.set_trailing("\n");
        arr.set_trailing_comma(true);
    }
}

/// Whether an entry (or any of a plugin's) is placed anywhere.
pub fn bar_is_placed(doc: &DocumentMut, plugin_or_entry: &str) -> bool {
    let check = |t: &Table| {
        SECTIONS.iter().any(|s| {
            t.get(s).and_then(|i| i.as_array()).is_some_and(|a| a.iter().any(|v| v.as_str().is_some_and(|e| entry_matches(e, plugin_or_entry))))
        })
    };
    check(doc.as_table())
        || doc.get("monitors").and_then(|m| m.as_table()).is_some_and(|m| m.iter().any(|(_, t)| t.as_table().is_some_and(check)))
}

/// The Control Center's tile id for a plugin part.
pub fn tile_id(plugin: &str, part: &str) -> String {
    format!("plugin:{plugin}/{part}")
}

/// Add a tile at the end of the Control Center's last page.
pub fn cc_append(doc: &mut DocumentMut, widget: &str, size: &str) {
    if !doc.get("pages").is_some_and(|p| p.is_array_of_tables()) {
        doc.insert("pages", Item::ArrayOfTables(toml_edit::ArrayOfTables::new()));
    }
    let pages = doc["pages"].as_array_of_tables_mut().expect("pages");
    if pages.is_empty() {
        pages.push(Table::new());
    }
    let last = pages.iter_mut().last().expect("a page");
    // The Control Center writes `[[pages.tiles]]` tables; people may write inline arrays.
    match last.get_mut("tiles") {
        Some(Item::ArrayOfTables(tiles)) => {
            if tiles.iter().any(|t| t.get("widget").and_then(|w| w.as_str()) == Some(widget)) {
                return;
            }
            let mut t = Table::new();
            t.insert("widget", value(widget));
            t.insert("size", value(size));
            tiles.push(t);
        }
        _ => {
            let tiles = array_of(last, "tiles");
            if tiles.iter().any(|t| t.as_inline_table().and_then(|t| t.get("widget")).and_then(|w| w.as_str()) == Some(widget)) {
                return;
            }
            let mut t = InlineTable::new();
            t.insert("widget", Value::from(widget));
            t.insert("size", Value::from(size));
            tiles.push(Value::InlineTable(t));
        }
    }
}

/// Remove a plugin's tiles (all of a plugin's, or one part's).
pub fn cc_remove(doc: &mut DocumentMut, plugin: &str, part: Option<&str>) {
    let Some(pages) = doc.get_mut("pages").and_then(|p| p.as_array_of_tables_mut()) else { return };
    let prefix = format!("plugin:{plugin}/");
    let keep = |w: &str| match part {
        Some(p) => w != tile_id(plugin, p),
        None => !w.starts_with(&prefix),
    };
    for page in pages.iter_mut() {
        match page.get_mut("tiles") {
            Some(Item::ArrayOfTables(tiles)) => tiles.retain(|t| keep(t.get("widget").and_then(|w| w.as_str()).unwrap_or(""))),
            Some(item) => {
                if let Some(tiles) = item.as_array_mut() {
                    tiles.retain(|t| keep(t.as_inline_table().and_then(|t| t.get("widget")).and_then(|w| w.as_str()).unwrap_or("")));
                }
            }
            None => {}
        }
    }
}

/// Add a desktop widget on every monitor, top left below the others there, if it isn't
/// placed yet.
pub fn desktop_append(doc: &mut DocumentMut, part: &str, size: &str) {
    if !doc.contains_key("version") {
        doc.insert("version", value(1));
    }
    if !doc.contains_table("all") {
        doc.insert("all", Item::Table(Table::new()));
    }
    let all = doc["all"].as_table_mut().expect("all");
    if !all.get("widget").is_some_and(|w| w.is_array_of_tables()) {
        all.insert("widget", Item::ArrayOfTables(toml_edit::ArrayOfTables::new()));
    }
    let widgets = all["widget"].as_array_of_tables_mut().expect("widgets");
    if widgets.iter().any(|w| w.get("part").and_then(|p| p.as_str()) == Some(part)) {
        return;
    }
    // Below the top-left widgets already there, by their sizes.
    let below: i64 = widgets
        .iter()
        .filter(|w| w.get("anchor").and_then(|a| a.as_str()).unwrap_or("top-left") == "top-left")
        .map(|w| {
            let size = w.get("size").and_then(|s| s.as_str()).unwrap_or("medium");
            crate::manifest::desktop_size(size).map_or(160, |(_, h)| h) as i64 + 24
        })
        .sum();
    let mut t = Table::new();
    t.insert("part", value(part));
    t.insert("anchor", value("top-left"));
    let mut offset = Array::new();
    offset.push(48);
    offset.push(48 + below);
    t.insert("offset", value(offset));
    t.insert("size", value(size));
    widgets.push(t);
}

/// Remove a plugin's desktop widgets everywhere.
pub fn desktop_remove(doc: &mut DocumentMut, plugin: &str, part: Option<&str>) {
    let matches = |w: &Table| {
        let p = w.get("part").and_then(|p| p.as_str()).unwrap_or("");
        match part {
            Some(part) => p == format!("{plugin}/{part}"),
            None => p == plugin || p.starts_with(&format!("{plugin}/")),
        }
    };
    let strip = |t: &mut Table| {
        if let Some(ws) = t.get_mut("widget").and_then(|w| w.as_array_of_tables_mut()) {
            ws.retain(|w| !matches(w));
        }
    };
    if let Some(all) = doc.get_mut("all").and_then(|a| a.as_table_mut()) {
        strip(all);
    }
    if let Some(monitors) = doc.get_mut("monitor").and_then(|m| m.as_table_mut()) {
        for (_, m) in monitors.iter_mut() {
            if let Some(m) = m.as_table_mut() {
                strip(m);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(doc: &DocumentMut, section: &str) -> Vec<String> {
        doc[section].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect()
    }

    #[test]
    fn places_and_moves_keeping_comments() {
        let mut doc = read_bar(Path::new("/nonexistent")).unwrap();
        bar_place(&mut doc, None, "acme.ping", "right", &Position::Before("nyx.stats".into()));
        let right = entries(&doc, "right");
        assert_eq!(right[3], "acme.ping");
        assert_eq!(right[4], "nyx.stats");
        bar_place(&mut doc, None, "acme.ping", "left", &Position::End);
        assert!(!entries(&doc, "right").contains(&"acme.ping".to_string()));
        assert_eq!(entries(&doc, "left"), vec!["nyx.workspaces", "acme.ping"]);
        bar_place(&mut doc, None, "nyx.clock/date", "center", &Position::After("nyx.clock".into()));
        assert_eq!(entries(&doc, "center"), vec!["nyx.clock", "nyx.clock/date"]);
        let text = doc.to_string();
        assert!(text.contains("# Nyx Bar layout"));
        let again: DocumentMut = text.parse().unwrap();
        assert!(bar_is_placed(&again, "acme.ping"));
    }

    #[test]
    fn unplaces_a_plugin_everywhere() {
        let mut doc: DocumentMut = "right = [\"a/x\", \"a\", \"b\"]\n[monitors.\"DP-1\"]\nleft = [\"a/y\", \"c\"]\n".parse().unwrap();
        bar_unplace(&mut doc, "a");
        assert_eq!(entries(&doc, "right"), vec!["b"]);
        assert!(!bar_is_placed(&doc, "a"));
        assert!(bar_is_placed(&doc, "c"));
        bar_place(&mut doc, Some("DP-1"), "d", "right", &Position::Index(0));
        assert!(doc.to_string().contains("[monitors.\"DP-1\"]"));
    }

    #[test]
    fn control_center_tiles() {
        let mut doc: DocumentMut = "version = 3\n# mine\n[[pages]]\ntiles = [{ widget = \"wifi\", size = \"2x1\" }]\n".parse().unwrap();
        cc_append(&mut doc, &tile_id("nyx.weather", "tile"), "2x1");
        cc_append(&mut doc, &tile_id("nyx.weather", "tile"), "2x1");
        let text = doc.to_string();
        assert_eq!(text.matches("plugin:nyx.weather/tile").count(), 1);
        assert!(text.contains("# mine"));
        cc_remove(&mut doc, "nyx.weather", None);
        assert!(!doc.to_string().contains("plugin:"));
        assert!(doc.to_string().contains("wifi"));
    }

    #[test]
    fn control_center_tiles_as_tables() {
        // As the Control Center saves it: every tile its own table.
        let text = "version = 3\n\n[[pages]]\n\n[[pages.tiles]]\nwidget = \"wifi\"\nsize = \"2x1\"\n\n[[pages]]\n\n[[pages.tiles]]\nwidget = \"battery\"\nsize = \"1x1\"\n";
        let mut doc: DocumentMut = text.parse().unwrap();
        cc_append(&mut doc, &tile_id("nyx.weather", "tile"), "2x1");
        cc_append(&mut doc, &tile_id("nyx.weather", "tile"), "2x1");
        let out = doc.to_string();
        assert!(out.contains("battery"), "the last page keeps its tiles: {out}");
        assert_eq!(out.matches("plugin:nyx.weather/tile").count(), 1);
        assert_eq!(out.matches("[[pages]]").count(), 2, "no new page: {out}");
        cc_remove(&mut doc, "nyx.weather", None);
        let out = doc.to_string();
        assert!(!out.contains("plugin:") && out.contains("battery") && out.contains("wifi"), "{out}");
    }

    #[test]
    fn desktop_widgets() {
        let mut doc = DocumentMut::new();
        desktop_append(&mut doc, "nyx.clock/large", "large");
        desktop_append(&mut doc, "nyx.weather/card", "medium");
        desktop_append(&mut doc, "nyx.weather/card", "medium");
        assert_eq!(doc.to_string().matches("[[all.widget]]").count(), 2);
        // The second goes below the first, a large one: 48 + 336 + 24.
        assert!(doc.to_string().contains("offset = [48, 408]"), "{doc}");
        desktop_remove(&mut doc, "nyx.weather", None);
        assert_eq!(doc.to_string().matches("[[all.widget]]").count(), 1);
    }
}
