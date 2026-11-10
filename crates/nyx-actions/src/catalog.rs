//! What is on offer: installed apps' manifests merged with running apps' descriptions
//! (the contract's section 4). Pure, so frontends and tests can build one from any
//! source; the registry fills it from files and the session bus.

use crate::spec::{Action, Activation, App, Confirmation, Description};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// Whether an action can run now.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    /// The app runs.
    Ready,
    /// Installed; D-Bus starts it for the call.
    Startable,
    /// Installed; it answers only while open.
    NotRunning,
    /// Something it needs is missing.
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Manifest,
    Dbus,
    /// Offered over a connection to AI Core.
    Connection,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Listed {
    #[serde(flatten)]
    pub action: Action,
    pub available: bool,
    pub state: State,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub source: Source,
}

/// One app and what it offers.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub app: App,
    /// The control interface's API while the app runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub running: Option<u32>,
    pub actions: Vec<Listed>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Catalog {
    pub apps: Vec<Entry>,
}

impl Catalog {
    /// Merge `manifests` (in precedence order: the first file for an app wins) with the
    /// running apps' descriptions. `missing` says why a required capability isn't there
    /// (`None` when it is). Namespaces two apps claim stay with the first; the second
    /// loses them, which is reported in the warnings.
    pub fn merge(
        manifests: Vec<Description>,
        running: Vec<Description>,
        missing: &dyn Fn(&str) -> Option<String>,
    ) -> (Catalog, Vec<String>) {
        let mut entries: Vec<Entry> = Vec::new();
        let mut seen = HashSet::new();
        let mut used = vec![false; running.len()];
        for m in manifests {
            if !seen.insert(m.app.app.clone()) {
                continue;
            }
            let live = running.iter().position(|r| r.app.bus_name == m.app.bus_name || r.app.app == m.app.app);
            let entry = match live {
                Some(i) => {
                    used[i] = true;
                    let r = &running[i];
                    let actions = r
                        .actions
                        .iter()
                        .map(|a| {
                            let mut action = a.clone();
                            if let Some(installed) = m.action(&a.id) {
                                action.risk = a.risk.stricter(installed.risk);
                                if installed.asks() || action.risk.always_asks() {
                                    action.confirmation = Confirmation::Ask;
                                }
                            }
                            listed(action, State::Ready, None, Source::Dbus)
                        })
                        .collect();
                    let app = if r.app.api >= 2 { r.app.clone() } else { m.app.clone() };
                    Entry { app, running: Some(r.app.api), actions }
                }
                None => {
                    let (state, reason) = match m.app.activation {
                        Activation::Dbus => (State::Startable, None),
                        Activation::Running => (State::NotRunning, Some(format!("Open {} to use this", m.app.name))),
                    };
                    let actions = m.actions.iter().map(|a| listed(a.clone(), state, reason.clone(), Source::Manifest)).collect();
                    Entry { app: m.app, running: None, actions }
                }
            };
            entries.push(entry);
        }
        for (r, used) in running.into_iter().zip(used) {
            if !used && seen.insert(r.app.app.clone()) {
                let actions = r.actions.into_iter().map(|a| listed(a, State::Ready, None, Source::Dbus)).collect();
                entries.push(Entry { running: Some(r.app.api), app: r.app, actions });
            }
        }
        let mut warnings = Vec::new();
        let mut claimed: HashSet<String> = HashSet::new();
        for entry in &mut entries {
            let lost: Vec<String> = entry.app.namespaces.iter().filter(|n| claimed.contains(*n)).cloned().collect();
            for n in &lost {
                warnings.push(format!("{} also claims the namespace {n}; it stays with the first app", entry.app.app));
            }
            entry.app.namespaces.retain(|n| !lost.contains(n));
            entry.actions.retain(|l| !lost.iter().any(|n| n == l.action.namespace()));
            claimed.extend(entry.app.namespaces.iter().cloned());
            for l in &mut entry.actions {
                if let Some(reason) = l.action.requires.iter().find_map(|c| missing(c)) {
                    l.available = false;
                    l.state = State::Unavailable;
                    l.reason = Some(reason);
                }
            }
        }
        entries.retain(|e| !e.app.namespaces.is_empty());
        (Catalog { apps: entries }, warnings)
    }

    /// The app a word names exactly: a namespace, an alias or its name.
    pub fn app(&self, word: &str) -> Option<&Entry> {
        let word = word.to_lowercase();
        self.apps
            .iter()
            .find(|e| e.app.namespaces.contains(&word))
            .or_else(|| self.apps.iter().find(|e| e.app.aliases.iter().any(|a| a.to_lowercase() == word)))
            .or_else(|| self.apps.iter().find(|e| e.app.name.to_lowercase() == word))
    }

    pub fn find(&self, id: &str) -> Option<(&Entry, &Listed)> {
        self.apps.iter().find_map(|e| e.actions.iter().find(|l| l.action.id == id).map(|l| (e, l)))
    }

    /// Whether some app owns this namespace.
    pub fn owns(&self, namespace: &str) -> bool {
        self.apps.iter().any(|e| e.app.namespaces.iter().any(|n| n == namespace))
    }

    pub fn actions(&self) -> impl Iterator<Item = (&Entry, &Listed)> {
        self.apps.iter().flat_map(|e| e.actions.iter().map(move |l| (e, l)))
    }
}

fn listed(action: Action, state: State, reason: Option<String>, source: Source) -> Listed {
    let available = matches!(state, State::Ready | State::Startable);
    Listed { action, available, state, reason, source }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::spec::Risk;
    use serde_json::{Value, json};

    pub fn description(app: &str, namespace: &str, activation: &str, tools: Value) -> Description {
        Description::from_value(
            &json!({"tools": tools, "nyx": {"api": 2, "app": format!("io.github.design_nexus.nyx.{app}"), "name": app, "namespaces": [namespace],
                "bus_name": format!("io.github.design_nexus.nyx.{app}"), "activation": activation}}),
            None,
        )
        .unwrap()
    }

    fn tool(id: &str, risk: &str) -> Value {
        json!({"name": id, "description": "Something.", "x-nyx": {"risk": risk}})
    }

    #[test]
    fn closed_apps_are_startable_or_need_opening() {
        let music = description("Music", "music", "dbus", json!([tool("music.status", "read_only")]));
        let notes = description("Notes", "notes", "running", json!([tool("notes.search", "read_only")]));
        let (c, warnings) = Catalog::merge(vec![music, notes], vec![], &|_| None);
        assert!(warnings.is_empty());
        let (_, status) = c.find("music.status").unwrap();
        assert_eq!((status.state, status.available, status.source), (State::Startable, true, Source::Manifest));
        let (_, search) = c.find("notes.search").unwrap();
        assert_eq!((search.state, search.available), (State::NotRunning, false));
        assert_eq!(search.reason.as_deref(), Some("Open Notes to use this"));
    }

    #[test]
    fn running_apps_list_their_own_tools_with_the_stricter_risk() {
        let installed =
            description("Music", "music", "dbus", json!([tool("music.control", "destructive"), tool("music.old", "read_only")]));
        let live =
            description("Music", "music", "dbus", json!([tool("music.control", "writes"), tool("music.volume", "writes")]));
        let legacy =
            Description::parse(r#"[{"name":"chat.list","description":"Recent."}]"#, Some("io.github.design_nexus.nyx.Chat"))
                .unwrap();
        let (c, _) = Catalog::merge(vec![installed], vec![live, legacy], &|_| None);
        let (entry, control) = c.find("music.control").unwrap();
        assert_eq!(entry.running, Some(2));
        assert_eq!(
            (control.action.risk, control.action.confirmation, control.state),
            (Risk::Destructive, Confirmation::Ask, State::Ready)
        );
        assert!(c.find("music.volume").is_some());
        assert!(c.find("music.old").is_none(), "the running build's list wins");
        let (chat, list) = c.find("chat.list").unwrap();
        assert_eq!((chat.running, list.action.risk), (Some(1), Risk::Unknown));
        assert_eq!(c.app("Chat").unwrap().app.app, "io.github.design_nexus.nyx.Chat");
    }

    #[test]
    fn namespaces_stay_with_the_first_app_and_requirements_are_checked() {
        let first = description("Notes", "notes", "dbus", json!([tool("notes.search", "read_only")]));
        let mut second = description("Jotter", "notes", "dbus", json!([tool("notes.create", "writes")]));
        second.app.app = "io.github.design_nexus.nyx.Jotter".into();
        let mut docker = description("Containers", "containers", "dbus", json!([tool("containers.list", "read_only")]));
        docker.actions[0].requires = vec!["program:docker".into()];
        let (c, warnings) = Catalog::merge(vec![first, second, docker], vec![], &|cap| {
            (cap == "program:docker").then(|| "Docker isn't installed".to_string())
        });
        assert_eq!(warnings.len(), 1);
        assert!(c.find("notes.create").is_none());
        assert_eq!(c.apps.len(), 2);
        let (_, list) = c.find("containers.list").unwrap();
        assert_eq!(
            (list.available, list.state, list.reason.as_deref()),
            (false, State::Unavailable, Some("Docker isn't installed"))
        );
    }
}
