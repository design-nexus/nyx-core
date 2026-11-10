//! Typed words to a call, the same way in every frontend (the contract's section 8).
//! Deterministic: exact names, then unique prefixes; an ambiguous word is an error that
//! lists the choices, never a guess.

use crate::catalog::{Catalog, Entry, Listed};
use crate::error::{Error, ErrorKind};
use crate::spec::Action;
use serde_json::{Map, Value};

/// What the words name.
#[derive(Clone, Debug, PartialEq)]
pub enum Resolution {
    /// Only an app (no verb, and no default action for the words).
    App {
        app: String,
    },
    Call(Call),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Call {
    pub app: String,
    pub action: String,
    pub arguments: Map<String, Value>,
    /// Required arguments not given yet.
    pub missing: Vec<String>,
    /// The app's default action was used for free words.
    pub by_default: bool,
}

/// Words, with double or single quotes grouping them.
pub fn split_words(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quote: Option<char> = None;
    let mut any = false;
    for c in text.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => word.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                any = true;
            }
            (None, c) if c.is_whitespace() => {
                if any || !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                }
                any = false;
            }
            (None, c) => word.push(c),
        }
    }
    if any || !word.is_empty() {
        words.push(word);
    }
    words
}

/// The app the first word names: exact namespace, alias or name, then a unique prefix of
/// two letters or more.
pub fn find_app<'a>(catalog: &'a Catalog, word: &str) -> Result<&'a Entry, Error> {
    if let Some(entry) = catalog.app(word) {
        return Ok(entry);
    }
    let word = word.to_lowercase();
    let mut matches: Vec<(&Entry, &str)> = Vec::new();
    if word.chars().count() >= 2 {
        for e in &catalog.apps {
            if let Some(n) = e.app.namespaces.iter().find(|n| n.starts_with(&word)) {
                matches.push((e, n));
            } else if let Some(a) = e.app.aliases.iter().find(|a| a.to_lowercase().starts_with(&word)) {
                matches.push((e, a));
            }
        }
    }
    match matches.as_slice() {
        [(e, _)] => Ok(e),
        [] => Err(Error::new(
            ErrorKind::UnknownApp,
            format!(
                "no app answers to {word:?}{}",
                hint(&word, catalog.apps.iter().flat_map(|e| e.app.namespaces.iter().map(String::as_str)))
            ),
        )),
        many => Err(Error::new(ErrorKind::UnknownApp, format!("{word:?} could be {}", or_list(many.iter().map(|(_, n)| *n))))),
    }
}

/// Resolve `words` (as typed after `nyx`, or in Glimpse).
pub fn resolve(catalog: &Catalog, words: &[String]) -> Result<Resolution, Error> {
    let Some(first) = words.first() else {
        return Err(Error::new(ErrorKind::InvalidArguments, "name an app"));
    };
    let entry = find_app(catalog, first)?;
    let first_lower = first.to_lowercase();
    let namespace = entry
        .app
        .namespaces
        .iter()
        .find(|n| **n == first_lower || n.starts_with(&first_lower))
        .unwrap_or(&entry.app.namespaces[0])
        .clone();
    let rest = &words[1..];
    if rest.is_empty() {
        return Ok(Resolution::App { app: entry.app.app.clone() });
    }
    let actions: Vec<&Listed> = entry.actions.iter().filter(|l| l.action.namespace() == namespace).collect();
    match find_verb(&actions, rest)? {
        Some((listed, used, preset)) => {
            let mut arguments = preset.cloned().unwrap_or_default();
            bind(&listed.action, &rest[used..], &mut arguments)?;
            Ok(Resolution::Call(call(entry, &listed.action, arguments, false)))
        }
        None => {
            let default = entry.app.default.as_deref().and_then(|d| entry.actions.iter().find(|l| l.action.id == d));
            match default {
                Some(listed) if !rest[0].starts_with("--") || rest.len() > 1 => {
                    let mut arguments = Map::new();
                    bind(&listed.action, rest, &mut arguments)?;
                    Ok(Resolution::Call(call(entry, &listed.action, arguments, true)))
                }
                _ => {
                    let verbs = actions.iter().map(|l| l.action.verb());
                    Err(Error::new(
                        ErrorKind::UnknownAction,
                        format!("{} has no action {:?}{}", entry.app.name, rest[0], hint(&rest[0].to_lowercase(), verbs)),
                    ))
                }
            }
        }
    }
}

fn call(entry: &Entry, action: &Action, arguments: Map<String, Value>, by_default: bool) -> Call {
    let missing = action.required().into_iter().filter(|r| !arguments.contains_key(*r)).map(str::to_string).collect();
    Call { app: entry.app.app.clone(), action: action.id.clone(), arguments, missing, by_default }
}

type Verb<'a> = (&'a Listed, usize, Option<&'a Map<String, Value>>);

/// The action the next words name, how many words that took, and an alias's preset
/// arguments. `Ok(None)` when nothing matches (the default action may take the words).
fn find_verb<'a>(actions: &[&'a Listed], words: &[String]) -> Result<Option<Verb<'a>>, Error> {
    // A dotted verb may be typed with spaces: `files trash restore`.
    for take in (1..=words.len().min(3)).rev() {
        let verb = words[..take].join(".").to_lowercase();
        if let Some(l) = actions.iter().find(|l| l.action.verb() == verb) {
            return Ok(Some((l, take, None)));
        }
    }
    let word = words[0].to_lowercase();
    for l in actions {
        if let Some(alias) = l.action.aliases.iter().find(|a| a.word().to_lowercase() == word) {
            return Ok(Some((l, 1, alias.arguments())));
        }
    }
    if word.starts_with("--") || word.chars().count() < 2 {
        return Ok(None);
    }
    let prefixed: Vec<&&Listed> = actions.iter().filter(|l| l.action.verb().starts_with(&word)).collect();
    match prefixed.as_slice() {
        [l] => Ok(Some((l, 1, None))),
        [] => Ok(None),
        many => Err(Error::new(
            ErrorKind::UnknownAction,
            format!("{word:?} could be {}", or_list(many.iter().map(|l| l.action.id.as_str()))),
        )),
    }
}

/// Fill `arguments` from words: `--name value`, `--name=value`, `--flag`, then
/// positional arguments in order (the last string one takes the remaining words).
pub fn bind(action: &Action, words: &[String], arguments: &mut Map<String, Value>) -> Result<(), Error> {
    let invalid = |m: String| Error::new(ErrorKind::InvalidArguments, m);
    let mut loose: Vec<&str> = Vec::new();
    let mut i = 0;
    let mut options = true;
    while i < words.len() {
        let w = &words[i];
        i += 1;
        if options && w == "--" {
            options = false;
            continue;
        }
        let Some(flag) = w.strip_prefix("--").filter(|f| options && !f.is_empty()) else {
            loose.push(w);
            continue;
        };
        let (name, inline) = match flag.split_once('=') {
            Some((n, v)) => (n.replace('-', "_"), Some(v.to_string())),
            None => (flag.replace('-', "_"), None),
        };
        let schema = action.property(&name);
        if schema.is_none() && closed(action) {
            return Err(invalid(format!(
                "{} has no argument --{}{}",
                action.id,
                name.replace('_', "-"),
                hint(&name, properties(action))
            )));
        }
        let boolean = schema.is_some_and(|s| s.get("type").and_then(Value::as_str) == Some("boolean"));
        let text = match inline {
            Some(v) => v,
            None if boolean => "true".into(),
            None => {
                let Some(v) = words.get(i) else { return Err(invalid(format!("--{} needs a value", name.replace('_', "-")))) };
                i += 1;
                v.clone()
            }
        };
        let value = convert(&name, schema, &text)?;
        match (arguments.get_mut(&name), schema.and_then(|s| s.get("type")).and_then(Value::as_str)) {
            (Some(Value::Array(items)), Some("array")) => items.extend(value.as_array().cloned().unwrap_or_default()),
            _ => {
                arguments.insert(name, value);
            }
        }
    }
    let open: Vec<&String> = action.positional.iter().filter(|p| !arguments.contains_key(*p)).collect();
    for (n, name) in open.iter().enumerate() {
        if loose.is_empty() {
            break;
        }
        let schema = action.property(name);
        let last_text =
            n + 1 == open.len() && schema.and_then(|s| s.get("type")).and_then(Value::as_str).is_none_or(|t| t == "string");
        let text = if last_text { std::mem::take(&mut loose).join(" ") } else { loose.remove(0).to_string() };
        arguments.insert((*name).clone(), convert(name, schema, &text)?);
    }
    if !loose.is_empty() {
        return Err(invalid(format!("{} doesn't take {:?} here", action.id, loose.join(" "))));
    }
    Ok(())
}

/// Text to the schema's type. Unknown or string types stay text.
fn convert(name: &str, schema: Option<&Value>, text: &str) -> Result<Value, Error> {
    let invalid = |m: String| Error::new(ErrorKind::InvalidArguments, m);
    let Some(schema) = schema else { return Ok(Value::String(text.into())) };
    if let Some(choices) = schema.get("enum").and_then(Value::as_array) {
        return choices
            .iter()
            .find(|c| c.as_str().is_some_and(|c| c.eq_ignore_ascii_case(text)) || *c == text)
            .cloned()
            .ok_or_else(|| {
                invalid(format!(
                    "{name} is one of {}",
                    or_list(
                        choices
                            .iter()
                            .map(|c| c.as_str().map(str::to_string).unwrap_or_else(|| c.to_string()))
                            .collect::<Vec<_>>()
                            .iter()
                            .map(String::as_str)
                    )
                ))
            });
    }
    let kind = schema.get("type").and_then(Value::as_str).unwrap_or("string");
    Ok(match kind {
        "integer" => Value::from(text.parse::<i64>().map_err(|_| invalid(format!("{name} is a whole number")))?),
        "number" => serde_json::Number::from_f64(text.parse::<f64>().map_err(|_| invalid(format!("{name} is a number")))?)
            .map(Value::Number)
            .ok_or_else(|| invalid(format!("{name} is a number")))?,
        "boolean" => Value::Bool(match text.to_lowercase().as_str() {
            "true" | "yes" | "on" | "1" => true,
            "false" | "no" | "off" | "0" => false,
            _ => return Err(invalid(format!("{name} is yes or no"))),
        }),
        "array" => {
            let item = schema.get("items");
            Value::Array(
                text.split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(|s| convert(name, item, s))
                    .collect::<Result<_, _>>()?,
            )
        }
        "object" => serde_json::from_str(text).map_err(|_| invalid(format!("{name} is a JSON object")))?,
        _ => Value::String(text.into()),
    })
}

fn closed(action: &Action) -> bool {
    action.input_schema.get("additionalProperties") == Some(&Value::Bool(false))
}

fn properties(action: &Action) -> impl Iterator<Item = &str> {
    action.input_schema.get("properties").and_then(Value::as_object).into_iter().flat_map(|p| p.keys().map(String::as_str))
}

/// "; did you mean …?" for close names (a prefix either way, or two edits).
fn hint<'a>(word: &str, names: impl Iterator<Item = &'a str>) -> String {
    let close: Vec<&str> = names.filter(|n| n.starts_with(word) || word.starts_with(*n) || edits(word, n) <= 2).take(3).collect();
    if close.is_empty() { String::new() } else { format!("; did you mean {}?", or_list(close.into_iter())) }
}

fn or_list<'a>(items: impl Iterator<Item = &'a str>) -> String {
    let items: Vec<&str> = items.collect();
    match items.as_slice() {
        [] => String::new(),
        [one] => one.to_string(),
        [rest @ .., last] => format!("{} or {last}", rest.join(", ")),
    }
}

fn edits(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        let mut prev = row[0];
        row[0] = i;
        for j in 1..=b.len() {
            let keep = row[j];
            row[j] = (row[j] + 1).min(row[j - 1] + 1).min(prev + usize::from(a[i - 1] != b[j - 1]));
            prev = keep;
        }
    }
    row[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::tests::description;
    use serde_json::json;

    fn catalog() -> Catalog {
        let notes = description(
            "Notes",
            "notes",
            "dbus",
            json!([
                {"name": "notes.search", "description": "Search.", "inputSchema": {"type": "object", "properties": {"query": {"type": "string"}, "limit": {"type": "integer"}}, "required": ["query"], "additionalProperties": false}, "x-nyx": {"risk": "read_only", "positional": ["query"]}},
                {"name": "notes.create", "description": "Create.", "inputSchema": {"type": "object", "properties": {"title": {"type": "string"}, "pinned": {"type": "boolean"}}, "required": ["title"], "additionalProperties": false}, "x-nyx": {"risk": "writes", "positional": ["title"], "aliases": ["new"]}},
                {"name": "notes.clip", "description": "Clip.", "x-nyx": {"risk": "writes"}},
                {"name": "notes.clear", "description": "Clear.", "x-nyx": {"risk": "writes"}},
                {"name": "notes.trash.restore", "description": "Restore.", "x-nyx": {"risk": "writes"}}
            ]),
        );
        let mut notes = notes;
        notes.app.default = Some("notes.search".into());
        notes.app.aliases = vec!["note".into()];
        let calendar = description(
            "Calendar",
            "calendar",
            "dbus",
            json!([{"name": "calendar.list", "description": "Events.", "inputSchema": {"type": "object", "properties": {"range": {"enum": ["today", "week"]}}},
                "x-nyx": {"risk": "read_only", "aliases": [{"word": "today", "arguments": {"range": "today"}}], "positional": ["range"]}}]),
        );
        let music = description(
            "Music",
            "music",
            "dbus",
            json!([{"name": "music.play", "description": "Play.", "x-nyx": {"risk": "writes"}}]),
        );
        Catalog::merge(vec![notes, calendar, music], vec![], &|_| None).0
    }

    fn words(text: &str) -> Vec<String> {
        split_words(text)
    }

    fn resolved(text: &str) -> Call {
        match resolve(&catalog(), &words(text)).unwrap() {
            Resolution::Call(c) => c,
            other => panic!("{text}: {other:?}"),
        }
    }

    #[test]
    fn quotes_group_words() {
        assert_eq!(split_words(r#"notes create "Project ideas" ''"#), vec!["notes", "create", "Project ideas", ""]);
        assert_eq!(split_words("  a  b "), vec!["a", "b"]);
    }

    #[test]
    fn verbs_positional_and_flags() {
        let c = resolved("notes create Project ideas --pinned");
        assert_eq!(
            (c.action.as_str(), c.arguments["title"].as_str(), c.arguments["pinned"].as_bool()),
            ("notes.create", Some("Project ideas"), Some(true))
        );
        assert!(c.missing.is_empty());
        let c = resolved("note new --title=Plans");
        assert_eq!((c.action.as_str(), c.arguments["title"].as_str()), ("notes.create", Some("Plans")));
        let c = resolved("notes search rust --limit 5");
        assert_eq!(c.arguments["limit"], 5);
        assert_eq!(resolved("notes trash restore").action, "notes.trash.restore");
        assert_eq!(resolved("no cr x").action, "notes.create", "unique prefixes");
        let c = resolved("notes create");
        assert_eq!(c.missing, vec!["title"]);
    }

    #[test]
    fn aliases_preset_arguments_and_defaults_take_free_words() {
        let c = resolved("calendar today");
        assert_eq!((c.action.as_str(), c.arguments["range"].as_str()), ("calendar.list", Some("today")));
        let c = resolved("calendar list WEEK");
        assert_eq!(c.arguments["range"], "week", "enums match case-insensitively");
        let c = resolved("notes architecture review");
        assert_eq!(
            (c.action.as_str(), c.arguments["query"].as_str(), c.by_default),
            ("notes.search", Some("architecture review"), true)
        );
        assert_eq!(
            resolve(&catalog(), &words("music")).unwrap(),
            Resolution::App { app: "io.github.design_nexus.nyx.Music".into() }
        );
    }

    #[test]
    fn mistakes_name_the_choices() {
        let c = resolved("notes c x");
        assert_eq!((c.action.as_str(), c.arguments["query"].as_str()), ("notes.search", Some("c x")), "one letter is no prefix");
        let e = resolve(&catalog(), &words("notes cl")).unwrap_err();
        assert!(e.message.contains("notes.clip or notes.clear"), "{}", e.message);
        let e = resolve(&catalog(), &words("music jazz")).unwrap_err();
        assert_eq!(e.kind, ErrorKind::UnknownAction);
        let e = resolve(&catalog(), &words("notse")).unwrap_err();
        assert_eq!(e.kind, ErrorKind::UnknownApp);
        assert!(e.message.contains("did you mean notes"), "{}", e.message);
        let e = resolve(&catalog(), &words("notes create --colour red")).unwrap_err();
        assert!(e.message.contains("--colour"), "{}", e.message);
        let e = resolve(&catalog(), &words("calendar list month")).unwrap_err();
        assert!(e.message.contains("today or week"), "{}", e.message);
        let e = resolve(&catalog(), &words("notes search a --limit lots")).unwrap_err();
        assert!(e.message.contains("whole number"));
    }
}
