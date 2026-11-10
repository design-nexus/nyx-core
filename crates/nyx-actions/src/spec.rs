//! An app's description: the JSON its `Describe` returns and its installed manifest
//! hold (the contract's section 2). API 1 documents (a plain MCP tool list) are read
//! too; anything written is API 2.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::time::Duration;

pub const API: u32 = 2;
pub const INTERFACE: &str = "io.github.design_nexus.nyx.Control";
pub const BUS_PREFIX: &str = "io.github.design_nexus.nyx.";
pub const MAX_ACTIONS: usize = 64;
pub const MAX_DESCRIPTION: usize = 256 * 1024;
pub const MAX_ARGUMENTS: usize = 256 * 1024;
pub const DEFAULT_TIMEOUT_MS: u64 = 30_000;
pub const MAX_TIMEOUT_MS: u64 = 300_000;
const MAX_TEXT: usize = 2000;

/// What an action does to the world.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Risk {
    ReadOnly,
    /// Changes the person's data or settings.
    Writes,
    /// Sends something off the computer.
    External,
    /// Deletes or overwrites.
    Destructive,
    /// Needs authorization beyond the session (the app asks Polkit or its helper).
    Privileged,
    /// Not declared: asks first, and only Nyx programs may run it.
    #[default]
    Unknown,
}

impl Risk {
    fn rank(self) -> u8 {
        match self {
            Risk::ReadOnly => 0,
            Risk::Writes => 1,
            Risk::External => 2,
            Risk::Destructive | Risk::Unknown => 3,
            Risk::Privileged => 4,
        }
    }

    /// The one needing more care; between equals, the declared one.
    pub fn stricter(self, other: Risk) -> Risk {
        match other.rank().cmp(&self.rank()) {
            std::cmp::Ordering::Greater => other,
            std::cmp::Ordering::Equal if self == Risk::Unknown => other,
            _ => self,
        }
    }

    /// Whether it asks first whatever the app declares.
    pub fn always_asks(self) -> bool {
        matches!(self, Risk::Destructive | Risk::Privileged | Risk::Unknown)
    }

    /// In words, for a confirmation.
    pub fn label(self) -> &'static str {
        match self {
            Risk::ReadOnly => "only reads",
            Risk::Writes => "changes your data",
            Risk::External => "sends something off this computer",
            Risk::Destructive => "deletes or overwrites",
            Risk::Privileged => "needs administrator rights",
            Risk::Unknown => "may change things",
        }
    }
}

/// Whether an action needs the person's go-ahead first.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confirmation {
    #[default]
    Never,
    Ask,
}

/// Another verb for an action, optionally with arguments it presets.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Alias {
    Word(String),
    Preset {
        word: String,
        #[serde(default)]
        arguments: Map<String, Value>,
    },
}

impl Alias {
    pub fn word(&self) -> &str {
        match self {
            Alias::Word(w) | Alias::Preset { word: w, .. } => w,
        }
    }

    pub fn arguments(&self) -> Option<&Map<String, Value>> {
        match self {
            Alias::Word(_) => None,
            Alias::Preset { arguments, .. } => Some(arguments),
        }
    }
}

/// How a frontend shows a result without code for the app.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResultKind {
    List,
    Object,
    Message,
    None,
}

/// One action, as an API 2 reader sees it. `confirmation` is the effective one (an
/// action whose risk always asks has `ask` whatever it declared).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Action {
    pub id: String,
    pub title: String,
    pub description: String,
    pub input_schema: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<Value>,
    pub risk: Risk,
    pub confirmation: Confirmation,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<Alias>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keywords: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub positional: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub permissions: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requires: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    /// Reads private data (clipboard history, …): only trusted callers, even though
    /// it changes nothing.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub sensitive: bool,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub suggest: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<ResultKind>,
}

impl Action {
    /// The word before the first dot (`notes` in `notes.create`).
    pub fn namespace(&self) -> &str {
        self.id.split_once('.').map_or(self.id.as_str(), |(ns, _)| ns)
    }

    /// The rest (`create`, or `trash.restore`).
    pub fn verb(&self) -> &str {
        self.id.split_once('.').map_or("", |(_, verb)| verb)
    }

    pub fn asks(&self) -> bool {
        self.confirmation == Confirmation::Ask
    }

    pub fn timeout(&self) -> Duration {
        Duration::from_millis(self.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS).min(MAX_TIMEOUT_MS))
    }

    /// Required argument names, from the schema.
    pub fn required(&self) -> Vec<&str> {
        self.input_schema
            .get("required")
            .and_then(Value::as_array)
            .map(|r| r.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default()
    }

    /// The schema of one argument.
    pub fn property(&self, name: &str) -> Option<&Value> {
        self.input_schema.get("properties").and_then(|p| p.get(name))
    }

    /// The description's first sentence.
    pub fn short(&self) -> &str {
        first_sentence(&self.description)
    }

    /// The action in API 2's tool form (MCP tool plus `x-nyx`).
    pub fn to_tool(&self) -> Value {
        let mut annotations = Map::new();
        annotations.insert("title".into(), json!(self.title));
        let hints: Option<(bool, bool, bool)> = match self.risk {
            Risk::ReadOnly => Some((true, false, false)),
            Risk::Writes => Some((false, false, false)),
            Risk::External => Some((false, false, true)),
            // API 1 has no privileged class: its strictest reading.
            Risk::Destructive | Risk::Privileged => Some((false, true, false)),
            Risk::Unknown => None,
        };
        if let Some((read_only, destructive, open_world)) = hints {
            annotations.insert("readOnlyHint".into(), json!(read_only));
            annotations.insert("destructiveHint".into(), json!(destructive));
            annotations.insert("openWorldHint".into(), json!(open_world));
        }
        let mut x = Map::new();
        x.insert("risk".into(), json!(self.risk));
        x.insert("confirmation".into(), json!(self.confirmation));
        let mut put = |key: &str, value: Value, empty: bool| {
            if !empty {
                x.insert(key.into(), value);
            }
        };
        put("aliases", json!(self.aliases), self.aliases.is_empty());
        put("keywords", json!(self.keywords), self.keywords.is_empty());
        put("positional", json!(self.positional), self.positional.is_empty());
        put("permissions", json!(self.permissions), self.permissions.is_empty());
        put("requires", json!(self.requires), self.requires.is_empty());
        put("timeout_ms", json!(self.timeout_ms), self.timeout_ms.is_none());
        put("suggest", json!(self.suggest), self.suggest.is_empty());
        put("sensitive", json!(true), !self.sensitive);
        put("result", json!(self.result), self.result.is_none());
        let mut tool = json!({
            "name": self.id,
            "description": self.description,
            "inputSchema": self.input_schema,
            "annotations": annotations,
            "x-nyx": x,
        });
        if let Some(output) = &self.output_schema {
            tool["outputSchema"] = output.clone();
        }
        tool
    }
}

/// Whether an app can be started to answer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Activation {
    /// D-Bus starts it headless.
    Dbus,
    /// Only while it runs.
    #[default]
    Running,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Open {
    pub action: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Topic {
    pub name: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub sensitive: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PanelSize {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<[u32; 2]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred: Option<[u32; 2]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<[u32; 2]>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Panel {
    pub id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub topics: Vec<String>,
    #[serde(default)]
    pub size: PanelSize,
}

/// The app, from its description's `nyx` object (made up from the bus name for API 1).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct App {
    pub api: u32,
    pub app: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub icon: String,
    pub namespaces: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    pub bus_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub object_path: Option<String>,
    #[serde(default)]
    pub activation: Activation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open: Option<Open>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub topics: Vec<Topic>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub panel: Option<Panel>,
}

impl App {
    /// Where its control interface is.
    pub fn path(&self) -> String {
        self.object_path.clone().unwrap_or_else(|| object_path(&self.bus_name))
    }

    /// An API 1 app: known by its bus name and its tools' namespaces.
    pub fn legacy(bus_name: &str, actions: &[Action]) -> App {
        let mut namespaces: Vec<String> = Vec::new();
        for a in actions {
            if !namespaces.iter().any(|n| n == a.namespace()) {
                namespaces.push(a.namespace().to_string());
            }
        }
        let name = bus_name.rsplit('.').next().unwrap_or(bus_name).trim_end_matches(char::is_numeric).to_string();
        App {
            api: 1,
            app: bus_name.to_string(),
            name,
            icon: String::new(),
            namespaces,
            aliases: Vec::new(),
            bus_name: bus_name.to_string(),
            object_path: None,
            activation: Activation::Running,
            default: None,
            open: None,
            topics: Vec::new(),
            panel: None,
        }
    }
}

/// One app's description.
#[derive(Clone, Debug, PartialEq)]
pub struct Description {
    pub app: App,
    pub actions: Vec<Action>,
}

impl Description {
    /// Read a `Describe` answer or manifest. `bus_name` names an API 1 app (which has
    /// no `nyx` object). API 1 tools that can't be used are skipped, as before; an API 2
    /// document must be valid as a whole.
    pub fn parse(text: &str, bus_name: Option<&str>) -> Result<Description, String> {
        if text.len() > MAX_DESCRIPTION {
            return Err(format!("the description is larger than {} KiB", MAX_DESCRIPTION / 1024));
        }
        let value: Value = serde_json::from_str(text).map_err(|e| format!("not JSON: {e}"))?;
        Self::from_value(&value, bus_name)
    }

    pub fn from_value(value: &Value, bus_name: Option<&str>) -> Result<Description, String> {
        let tools = match value {
            Value::Array(tools) => tools.as_slice(),
            v => v.get("tools").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default(),
        };
        let Some(nyx) = value.get("nyx") else {
            let bus_name = bus_name.ok_or("an API 1 description needs its bus name")?;
            let actions: Vec<Action> = tools.iter().filter_map(|t| parse_tool(t, false).ok()).take(MAX_ACTIONS).collect();
            let app = App::legacy(bus_name, &actions);
            return Ok(Description { app, actions });
        };
        let app: App = serde_json::from_value(nyx.clone()).map_err(|e| format!("nyx: {e}"))?;
        if tools.len() > MAX_ACTIONS {
            return Err(format!("more than {MAX_ACTIONS} actions"));
        }
        let mut actions = Vec::new();
        for tool in tools {
            let name = tool.get("name").and_then(Value::as_str).unwrap_or("?");
            actions.push(parse_tool(tool, true).map_err(|e| format!("{name}: {e}"))?);
        }
        let description = Description { app, actions };
        description.check()?;
        Ok(description)
    }

    /// API 2's rules for a whole description.
    fn check(&self) -> Result<(), String> {
        let app = &self.app;
        if app.api < 2 {
            return Err(format!("nyx.api is {}, expected {API}", app.api));
        }
        let nyx_name = |n: &str| {
            n.strip_prefix(BUS_PREFIX).is_some_and(|rest| {
                !rest.is_empty()
                    && rest
                        .split('.')
                        .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'))
            })
        };
        if !nyx_name(&app.app) {
            return Err(format!("{} isn't a Nyx application ID", app.app));
        }
        if app.bus_name != app.app && !nyx_name(&app.bus_name) {
            return Err(format!("{} isn't a Nyx bus name", app.bus_name));
        }
        if app.name.trim().is_empty() {
            return Err("nyx.name is empty".into());
        }
        if app.namespaces.is_empty() || !app.namespaces.iter().all(|n| valid_word(n)) {
            return Err("nyx.namespaces needs one or more lowercase words".into());
        }
        for (i, a) in self.actions.iter().enumerate() {
            if !app.namespaces.iter().any(|n| n == a.namespace()) {
                return Err(format!("{} isn't in the app's namespaces", a.id));
            }
            if self.actions[..i].iter().any(|b| b.id == a.id) {
                return Err(format!("{} is listed twice", a.id));
            }
        }
        let action = |id: &str| self.actions.iter().find(|a| a.id == id);
        if let Some(default) = &app.default {
            match action(default) {
                Some(a) if a.risk == Risk::ReadOnly => {}
                Some(_) => return Err(format!("the default action {default} must be read-only")),
                None => return Err(format!("the default action {default} isn't listed")),
            }
        }
        if let Some(open) = &app.open
            && action(&open.action).is_none()
        {
            return Err(format!("the open action {} isn't listed", open.action));
        }
        if let Some(panel) = &app.panel
            && let Some(t) = panel.topics.iter().find(|t| !app.topics.iter().any(|u| &u.name == *t))
        {
            return Err(format!("the panel's topic {t} isn't listed"));
        }
        Ok(())
    }

    pub fn action(&self, id: &str) -> Option<&Action> {
        self.actions.iter().find(|a| a.id == id)
    }

    /// The API 2 document (what `Describe` returns and the manifest holds).
    pub fn to_json(&self) -> Value {
        json!({
            "tools": self.actions.iter().map(Action::to_tool).collect::<Vec<_>>(),
            "nyx": self.app,
        })
    }
}

/// One MCP tool with optional `x-nyx`. `strict`: an API 2 tool, where anything wrong is
/// an error rather than a default.
fn parse_tool(tool: &Value, strict: bool) -> Result<Action, String> {
    let id = tool.get("name").and_then(Value::as_str).ok_or("no name")?.to_string();
    if !valid_action_id(&id) {
        return Err(format!("{id:?} isn't an action ID (namespace.verb, lowercase)"));
    }
    let description: String =
        tool.get("description").and_then(Value::as_str).unwrap_or_default().trim().chars().take(MAX_TEXT).collect();
    if strict && description.is_empty() {
        return Err("no description".into());
    }
    let hint = |key: &str| tool.pointer(&format!("/annotations/{key}")).and_then(Value::as_bool);
    let hinted = match (hint("readOnlyHint"), hint("destructiveHint"), hint("openWorldHint")) {
        (Some(true), _, _) => Risk::ReadOnly,
        (_, Some(true), _) => Risk::Destructive,
        (_, _, Some(true)) => Risk::External,
        (Some(false), Some(false), _) => Risk::Writes,
        _ => Risk::Unknown,
    };
    let empty = Value::Object(Map::new());
    let x = tool.get("x-nyx").unwrap_or(&empty);
    if strict && !x.is_object() {
        return Err("x-nyx isn't an object".into());
    }
    // An API 1 tool's odd field is ignored; an API 2 tool's is an error.
    fn get<T: serde::de::DeserializeOwned>(x: &Value, key: &str, strict: bool) -> Result<Option<T>, String> {
        let Some(v) = x.get(key).filter(|v| !v.is_null()) else { return Ok(None) };
        match serde_json::from_value(v.clone()) {
            Ok(t) => Ok(Some(t)),
            Err(e) if strict => Err(format!("x-nyx.{key}: {e}")),
            Err(_) => Ok(None),
        }
    }
    let risk = get::<Risk>(x, "risk", strict)?.unwrap_or(hinted);
    let declared = get::<Confirmation>(x, "confirmation", strict)?;
    let confirmation = if risk.always_asks() {
        Confirmation::Ask
    } else {
        declared.unwrap_or(if matches!(risk, Risk::ReadOnly | Risk::Writes) { Confirmation::Never } else { Confirmation::Ask })
    };
    let schema = |key: &str| -> Result<Option<Value>, String> {
        match tool.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(s) if s.is_object() && jsonschema::validator_for(s).is_ok() => Ok(Some(s.clone())),
            Some(_) if strict => Err(format!("{key} isn't a JSON Schema that compiles")),
            Some(_) => Ok(None),
        }
    };
    let input_schema = schema("inputSchema")?.unwrap_or_else(|| json!({"type": "object"}));
    let output_schema = schema("outputSchema")?;
    let title = tool
        .pointer("/annotations/title")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(|t| t.chars().take(120).collect())
        .unwrap_or_else(|| first_sentence(&description).to_string());
    let positional: Vec<String> = get(x, "positional", strict)?.unwrap_or_default();
    if strict {
        let props = input_schema.get("properties");
        if let Some(p) = positional.iter().find(|p| props.and_then(|props| props.get(p.as_str())).is_none()) {
            return Err(format!("positional argument {p} isn't in inputSchema's properties"));
        }
    }
    let timeout_ms: Option<u64> = get(x, "timeout_ms", strict)?;
    Ok(Action {
        id,
        title,
        description,
        input_schema,
        output_schema,
        risk,
        confirmation,
        aliases: get(x, "aliases", strict)?.unwrap_or_default(),
        keywords: get(x, "keywords", strict)?.unwrap_or_default(),
        positional,
        permissions: get(x, "permissions", strict)?.unwrap_or_default(),
        requires: get(x, "requires", strict)?.unwrap_or_default(),
        timeout_ms: timeout_ms.map(|t| t.min(MAX_TIMEOUT_MS)),
        suggest: get(x, "suggest", strict)?.unwrap_or_default(),
        sensitive: get(x, "sensitive", strict)?.unwrap_or(false),
        result: get(x, "result", strict)?,
    })
}

fn first_sentence(text: &str) -> &str {
    let end = text
        .char_indices()
        .find(|&(i, c)| (c == '.' && text[i + 1..].starts_with([' ', '\n'])) || c == '\n')
        .map(|(i, _)| i)
        .unwrap_or(text.len());
    text[..end].trim_end_matches('.').trim()
}

fn valid_word(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "_-".contains(c))
}

/// `namespace.verb[.more]`: lowercase letters, digits, `_` and `-`; at most 64 characters.
pub fn valid_action_id(id: &str) -> bool {
    id.len() <= 64 && id.split_once('.').is_some_and(|(ns, verb)| valid_word(ns) && verb.split('.').all(valid_word))
}

/// GApplication's object path for an application ID.
pub fn object_path(app_id: &str) -> String {
    let mut path = String::from("/");
    for c in app_id.chars() {
        path.push(match c {
            '.' => '/',
            c if c.is_ascii_alphanumeric() || c == '_' => c,
            _ => '_',
        });
    }
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    fn notes() -> Value {
        json!({
            "tools": [
                {"name": "notes.search", "description": "Search notes. By words.", "inputSchema": {"type": "object", "properties": {"query": {"type": "string"}}, "required": ["query"]},
                 "x-nyx": {"risk": "read_only", "positional": ["query"]}},
                {"name": "notes.create", "description": "Create a note.", "inputSchema": {"type": "object", "properties": {"title": {"type": "string"}}},
                 "annotations": {"title": "Create a note"}, "x-nyx": {"risk": "writes", "aliases": ["new", {"word": "today", "arguments": {"title": "Today"}}], "positional": ["title"]}},
                {"name": "notes.delete", "description": "Delete a note.", "x-nyx": {"risk": "destructive", "confirmation": "never"}}
            ],
            "nyx": {"api": 2, "app": "io.github.design_nexus.nyx.Notes", "name": "Notes", "namespaces": ["notes"], "bus_name": "io.github.design_nexus.nyx.Notes", "activation": "dbus", "default": "notes.search"}
        })
    }

    #[test]
    fn api_1_tools_read_as_before() {
        let text = r#"[{"name":"chat.list","description":"Recent conversations.","inputSchema":{"type":"object","properties":{}}},
            {"name":"chat.read","description":"Read one.","inputSchema":{"type":"object"},"annotations":{"readOnlyHint":true}},
            {"name":"chat.delete","description":"Delete.","annotations":{"destructiveHint":true}},
            {"name":"Bad Name","description":"skipped"}]"#;
        let d = Description::parse(text, Some("io.github.design_nexus.nyx.Chat")).unwrap();
        assert_eq!(d.actions.len(), 3);
        assert_eq!((d.actions[0].risk, d.actions[0].confirmation), (Risk::Unknown, Confirmation::Ask));
        assert_eq!((d.actions[1].risk, d.actions[1].confirmation), (Risk::ReadOnly, Confirmation::Never));
        assert_eq!(d.actions[2].risk, Risk::Destructive);
        assert_eq!(d.app.api, 1);
        assert_eq!(d.app.name, "Chat");
        assert_eq!(d.app.namespaces, vec!["chat"]);
        assert_eq!(d.app.path(), "/io/github/design_nexus/nyx/Chat");
        let wrapped =
            Description::parse(r#"{"tools":[{"name":"x.y","description":"d"}]}"#, Some("io.github.design_nexus.nyx.Backup1"))
                .unwrap();
        assert_eq!(wrapped.app.name, "Backup");
    }

    #[test]
    fn api_2_round_trips_and_keeps_confirmation_for_risky_actions() {
        let d = Description::from_value(&notes(), None).unwrap();
        assert_eq!(d.actions[0].title, "Search notes");
        assert_eq!(d.actions[1].title, "Create a note");
        assert_eq!(d.actions[1].confirmation, Confirmation::Never);
        assert_eq!(d.actions[2].confirmation, Confirmation::Ask, "destructive always asks");
        assert_eq!(d.actions[1].aliases[1].arguments().unwrap()["title"], "Today");
        let again = Description::from_value(&d.to_json(), None).unwrap();
        assert_eq!(again, d);
        // An API 1 reader of the same document sees the hints.
        let old = Description::from_value(&json!(d.to_json()["tools"]), Some("io.github.design_nexus.nyx.Notes")).unwrap();
        assert_eq!(old.actions.iter().map(|a| a.risk).collect::<Vec<_>>(), vec![Risk::ReadOnly, Risk::Writes, Risk::Destructive]);
    }

    #[test]
    fn api_2_mistakes_are_errors() {
        let bad = |edit: &dyn Fn(&mut Value)| {
            let mut v = notes();
            edit(&mut v);
            Description::from_value(&v, None).unwrap_err()
        };
        assert!(bad(&|v| v["nyx"]["namespaces"] = json!(["memo"])).contains("namespaces"));
        assert!(bad(&|v| v["nyx"]["default"] = json!("notes.create")).contains("read-only"));
        assert!(bad(&|v| v["tools"][0]["inputSchema"] = json!({"type": 12})).contains("compiles"));
        assert!(bad(&|v| v["tools"][1]["x-nyx"]["risk"] = json!("sometimes")).contains("x-nyx.risk"));
        assert!(bad(&|v| v["tools"][1]["x-nyx"]["positional"] = json!(["body"])).contains("positional"));
        assert!(bad(&|v| v["nyx"]["bus_name"] = json!("org.example.Notes")).contains("bus name"));
        assert!(bad(&|v| v["nyx"]["extra"] = json!(1)).contains("nyx"));
        assert!(bad(&|v| v["tools"][2]["description"] = json!(" ")).contains("description"));
    }

    #[test]
    fn stricter_risk_wins() {
        assert_eq!(Risk::ReadOnly.stricter(Risk::Writes), Risk::Writes);
        assert_eq!(Risk::Destructive.stricter(Risk::Writes), Risk::Destructive);
        assert_eq!(Risk::Unknown.stricter(Risk::Destructive), Risk::Destructive);
        assert_eq!(Risk::Destructive.stricter(Risk::Unknown), Risk::Destructive);
        assert_eq!(Risk::Writes.stricter(Risk::Privileged), Risk::Privileged);
    }

    #[test]
    fn action_ids_and_paths() {
        assert!(valid_action_id("notes.create"));
        assert!(valid_action_id("files.trash.restore"));
        for bad in ["notes", "Notes.create", ".create", "notes.", "notes..x", "notes.cre ate"] {
            assert!(!valid_action_id(bad), "{bad}");
        }
        assert_eq!(object_path("io.github.design_nexus.nyx.my-app"), "/io/github/design_nexus/nyx/my_app");
        assert_eq!(first_sentence("Play a track. Or an album."), "Play a track");
        assert_eq!(first_sentence("Version 1.5 notes."), "Version 1.5 notes");
    }
}
