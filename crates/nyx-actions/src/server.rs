//! The control interface (API 2) for programs on zbus, such as daemons without a
//! GApplication, with the rules of `nyx_core::control`'s GTK helper: the caller is
//! known by its program (`nyx-peer`), only Nyx programs get past read-only actions,
//! actions that ask first need `confirmed`, and arguments must match the schema, all
//! before the handler runs. Topics go only to the callers watching them.

use crate::check::check_call;
use crate::error::{Error, ErrorKind};
use crate::patch;
use crate::spec::{Description, INTERFACE};
use futures_util::future::BoxFuture;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Who called.
#[derive(Clone, Debug, PartialEq)]
pub struct Caller {
    pub sender: Option<String>,
    /// The Nyx program it runs, if it runs one.
    pub program: Option<String>,
    pub trusted: bool,
}

/// One call that passed the checks.
#[derive(Clone, Debug)]
pub struct Request {
    pub action: String,
    pub arguments: Value,
    pub caller: Caller,
    pub confirmed: bool,
    pub on_behalf_of: Option<String>,
    pub frontend: Option<String>,
}

pub type Handler = Arc<dyn Fn(Request) -> BoxFuture<'static, Result<Value, Error>> + Send + Sync>;
type WatchFn = Arc<dyn Fn(&str, bool) + Send + Sync>;

#[derive(Default)]
struct Topic {
    rev: u32,
    sent: Value,
    watchers: HashMap<String, u32>,
}

struct Shared {
    conn: zbus::Connection,
    path: String,
    description: Description,
    text: String,
    own_dir: Option<PathBuf>,
    handler: Handler,
    callers: Mutex<HashMap<String, Caller>>,
    topics: Mutex<HashMap<String, Topic>>,
    on_watch: Mutex<Option<WatchFn>>,
}

/// A served interface: its live state and its callers.
#[derive(Clone)]
pub struct Server(Arc<Shared>);

impl Server {
    /// `f(topic, watched)` when a topic gains its first watcher or loses its last.
    pub fn on_watch(&self, f: impl Fn(&str, bool) + Send + Sync + 'static) {
        *self.0.on_watch.lock().expect("on_watch") = Some(Arc::new(f));
    }

    pub fn watched(&self, topic: &str) -> bool {
        self.0.topics.lock().expect("topics").get(topic).is_some_and(|t| !t.watchers.is_empty())
    }

    /// The topic's new state; its watchers get the merge patch from the last one.
    pub async fn publish(&self, topic: &str, state: Value) {
        let (rev, patch, watchers) = {
            let mut topics = self.0.topics.lock().expect("topics");
            let t = topics.entry(topic.to_string()).or_default();
            if t.rev > 0 && t.sent == state {
                return;
            }
            let patch = patch::diff(&t.sent, &state);
            t.rev += 1;
            t.sent = state;
            (t.rev, patch.to_string(), t.watchers.keys().cloned().collect::<Vec<_>>())
        };
        for w in watchers {
            let _ = self
                .0
                .conn
                .emit_signal(Some(w.as_str()), self.0.path.as_str(), INTERFACE, "StateChanged", &(topic, rev, patch.as_str()))
                .await;
        }
    }

    /// Who sent a message, for the program's other interfaces to check the same way.
    pub async fn caller(&self, header: &zbus::message::Header<'_>) -> Caller {
        identify(&self.0, header.sender().map(|s| s.to_string())).await
    }
}

/// Serve `description`'s actions at `path` on `conn`. `own_dir`: the folder whose
/// programs are this program's fellow Nyx programs (its own).
pub async fn serve(
    conn: &zbus::Connection,
    path: &str,
    bus_name: &str,
    description: Value,
    own_dir: Option<PathBuf>,
    handler: Handler,
) -> Result<Server, String> {
    let parsed = Description::from_value(&description, Some(bus_name))?;
    let shared = Arc::new(Shared {
        conn: conn.clone(),
        path: path.to_string(),
        description: parsed,
        text: description.to_string(),
        own_dir,
        handler,
        callers: Mutex::default(),
        topics: Mutex::default(),
        on_watch: Mutex::default(),
    });
    conn.object_server().at(path, Iface(shared.clone())).await.map_err(|e| e.to_string())?;
    // Callers that leave stop watching.
    let rule = "type='signal',sender='org.freedesktop.DBus',interface='org.freedesktop.DBus',member='NameOwnerChanged'";
    if let Ok(mut owners) = zbus::MessageStream::for_match_rule(rule, conn, Some(64)).await {
        let weak = Arc::downgrade(&shared);
        tokio::spawn(async move {
            use futures_util::StreamExt;
            while let Some(Ok(msg)) = owners.next().await {
                let Some(shared) = weak.upgrade() else { return };
                if let Ok((name, _, new)) = msg.body().deserialize::<(String, String, String)>()
                    && new.is_empty()
                {
                    gone(&shared, &name);
                }
            }
        });
    }
    Ok(Server(shared))
}

fn gone(shared: &Shared, name: &str) {
    shared.callers.lock().expect("callers").remove(name);
    let emptied: Vec<String> = {
        let mut topics = shared.topics.lock().expect("topics");
        topics
            .iter_mut()
            .filter_map(|(topic, t)| (t.watchers.remove(name).is_some() && t.watchers.is_empty()).then(|| topic.clone()))
            .collect()
    };
    let f = shared.on_watch.lock().expect("on_watch").clone();
    if let Some(f) = f {
        for topic in emptied {
            f(&topic, false);
        }
    }
}

async fn identify(shared: &Shared, sender: Option<String>) -> Caller {
    let Some(name) = sender else { return Caller { sender: None, program: None, trusted: false } };
    if let Some(c) = shared.callers.lock().expect("callers").get(&name).cloned() {
        return c;
    }
    let pid = match (zbus::fdo::DBusProxy::new(&shared.conn).await, zbus::names::BusName::try_from(name.as_str())) {
        (Ok(proxy), Ok(bus)) => proxy.get_connection_unix_process_id(bus).await.ok(),
        _ => None,
    };
    let exe = pid.and_then(|p| std::fs::read_link(format!("/proc/{p}/exe")).ok());
    let program = exe.and_then(|exe| nyx_peer::program(&exe, shared.own_dir.as_deref().unwrap_or(Path::new("/nonexistent"))));
    let caller = Caller { sender: Some(name.clone()), trusted: program.is_some(), program };
    let mut callers = shared.callers.lock().expect("callers");
    if callers.len() > 256 {
        callers.clear();
    }
    callers.insert(name, caller.clone());
    caller
}

struct Iface(Arc<Shared>);

impl Iface {
    async fn run(&self, v2: bool, action: String, arguments: String, options: String, sender: Option<String>) -> String {
        let fail = |e: Error| if v2 { json!({"error": e}) } else { json!({"error": e.message}) }.to_string();
        let caller = identify(&self.0, sender).await;
        let arguments: Value = match serde_json::from_str(&arguments) {
            Ok(v) => v,
            Err(_) if !v2 => json!({}),
            Err(_) => return fail(Error::new(ErrorKind::InvalidArguments, "the arguments aren't JSON")),
        };
        let options: Value = serde_json::from_str(&options).unwrap_or(json!({}));
        // API 1 callers asked the person themselves when they're trusted.
        let confirmed = if v2 { options["confirmed"] == true } else { caller.trusted };
        let Some(spec) = self.0.description.action(&action) else {
            return fail(Error::new(ErrorKind::UnknownAction, format!("{action} isn't an action here")));
        };
        if let Err(e) = check_call(caller.trusted, spec, &arguments, confirmed) {
            return fail(e);
        }
        let text = |key: &str| options.get(key).and_then(Value::as_str).map(str::to_string);
        let request =
            Request { action, arguments, confirmed, on_behalf_of: text("on_behalf_of"), frontend: text("frontend"), caller };
        match (self.0.handler)(request).await {
            Ok(v) => json!({"ok": v}).to_string(),
            Err(e) => fail(e),
        }
    }

    async fn topic(&self, topic: &str, sender: Option<String>) -> zbus::fdo::Result<Caller> {
        let caller = identify(&self.0, sender).await;
        let Some(declared) = self.0.description.app.topics.iter().find(|t| t.name == topic) else {
            return Err(zbus::fdo::Error::InvalidArgs(format!("{topic} isn't one of this app's topics")));
        };
        if declared.sensitive && !caller.trusted {
            return Err(zbus::fdo::Error::AccessDenied(format!("only Nyx programs may read {topic}")));
        }
        Ok(caller)
    }

    fn changed(&self, topic: &str, on: bool) {
        let f = self.0.on_watch.lock().expect("on_watch").clone();
        if let Some(f) = f {
            f(topic, on);
        }
    }
}

#[zbus::interface(name = "io.github.design_nexus.nyx.Control")]
impl Iface {
    fn describe(&self) -> String {
        self.0.text.clone()
    }

    async fn call(&self, name: String, arguments: String, #[zbus(header)] header: zbus::message::Header<'_>) -> String {
        self.run(false, name, arguments, String::new(), header.sender().map(|s| s.to_string())).await
    }

    async fn invoke(
        &self,
        action: String,
        arguments: String,
        options: String,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> String {
        self.run(true, action, arguments, options, header.sender().map(|s| s.to_string())).await
    }

    async fn state(&self, topic: String, #[zbus(header)] header: zbus::message::Header<'_>) -> zbus::fdo::Result<String> {
        self.topic(&topic, header.sender().map(|s| s.to_string())).await?;
        let topics = self.0.topics.lock().expect("topics");
        let (rev, state) = topics.get(&topic).map(|t| (t.rev, t.sent.clone())).unwrap_or((0, Value::Null));
        Ok(json!({"rev": rev, "state": state}).to_string())
    }

    async fn watch(&self, topic: String, #[zbus(header)] header: zbus::message::Header<'_>) -> zbus::fdo::Result<u32> {
        let caller = self.topic(&topic, header.sender().map(|s| s.to_string())).await?;
        let (first, rev) = {
            let mut topics = self.0.topics.lock().expect("topics");
            let t = topics.entry(topic.clone()).or_default();
            let first = t.watchers.is_empty();
            *t.watchers.entry(caller.sender.unwrap_or_default()).or_default() += 1;
            (first, t.rev)
        };
        if first {
            self.changed(&topic, true);
        }
        Ok(rev)
    }

    async fn unwatch(&self, topic: String, #[zbus(header)] header: zbus::message::Header<'_>) -> zbus::fdo::Result<()> {
        let sender = header.sender().map(|s| s.to_string()).unwrap_or_default();
        let last = {
            let mut topics = self.0.topics.lock().expect("topics");
            let Some(t) = topics.get_mut(&topic) else { return Ok(()) };
            match t.watchers.get_mut(&sender) {
                Some(n) if *n > 1 => {
                    *n -= 1;
                    false
                }
                Some(_) => {
                    t.watchers.remove(&sender);
                    t.watchers.is_empty()
                }
                None => false,
            }
        };
        if last {
            self.changed(&topic, false);
        }
        Ok(())
    }

    #[zbus(signal)]
    async fn state_changed(
        emitter: &zbus::object_server::SignalEmitter<'_>,
        topic: &str,
        rev: u32,
        patch: &str,
    ) -> zbus::Result<()>;
}
