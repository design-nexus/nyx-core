//! A small control interface on the session bus, so other programs (the `nyx` command,
//! Glimpse, Nyx AI Core for the Assistant) can work an app directly instead of through
//! its window. Each app registers it on its own GApplication connection and object
//! path: `io.github.design_nexus.nyx.Control`, API 2 of the contract in nyx-actions
//! (`crates/nyx-actions/docs/CONTRACT.md`):
//!
//! - `Describe() -> s`: the app's description (an API 1 tool list is still accepted).
//! - `Call(s name, s arguments) -> s`: API 1, `{"ok": …}` or `{"error": "…"}`.
//! - `Invoke(s action, s arguments, s options) -> s`: API 2, with `confirmed`.
//! - `State(s topic) -> s`, `Watch(s topic) -> u`, `Unwatch(s topic)` and the
//!   `StateChanged(s topic, u rev, s patch)` signal, sent only to watchers.
//!
//! This helper is where the app's rules are enforced, before its handler runs: the
//! caller is identified by its program (`nyx-peer`); only Nyx programs may run more
//! than read-only actions; actions that ask first need the caller's confirmation;
//! arguments must match the action's schema.
//!
//! Only the primary instance registers it, so calls reach the open window (or the
//! headless service). Handlers run on the main thread. What they offer is kept narrow:
//! nothing closes unsaved work, and files are written only inside the home folder.

use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use nyx_actions::check::check_call;
use nyx_actions::{Description, Error, ErrorKind};
use serde_json::{Map, Value, json};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

pub const INTERFACE: &str = "io.github.design_nexus.nyx.Control";

const XML: &str = r#"<node>
  <interface name="io.github.design_nexus.nyx.Control">
    <method name="Describe"><arg type="s" name="tools" direction="out"/></method>
    <method name="Call">
      <arg type="s" name="name" direction="in"/>
      <arg type="s" name="arguments" direction="in"/>
      <arg type="s" name="result" direction="out"/>
    </method>
    <method name="Invoke">
      <arg type="s" name="action" direction="in"/>
      <arg type="s" name="arguments" direction="in"/>
      <arg type="s" name="options" direction="in"/>
      <arg type="s" name="result" direction="out"/>
    </method>
    <method name="State">
      <arg type="s" name="topic" direction="in"/>
      <arg type="s" name="state" direction="out"/>
    </method>
    <method name="Watch">
      <arg type="s" name="topic" direction="in"/>
      <arg type="u" name="rev" direction="out"/>
    </method>
    <method name="Unwatch"><arg type="s" name="topic" direction="in"/></method>
    <signal name="StateChanged">
      <arg type="s" name="topic"/>
      <arg type="u" name="rev"/>
      <arg type="s" name="patch"/>
    </signal>
  </interface>
</node>"#;

/// At most this often per topic; a burst sends its last state.
const PATCH_INTERVAL: Duration = Duration::from_millis(100);

/// The answer to one `Call`: give it the result once (now or from a later callback).
pub type Reply = Box<dyn FnOnce(Result<Value, String>)>;

/// [`Reply`] with the contract's error kinds.
pub type Answer = Box<dyn FnOnce(Result<Value, Error>)>;

/// Who called.
#[derive(Clone, Debug, PartialEq)]
pub struct Caller {
    /// The bus name it called from.
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
    /// A person agreed to it in the caller's UI.
    pub confirmed: bool,
    /// The program the caller works for (AI Core's client), when it says.
    pub on_behalf_of: Option<String>,
    /// `cli`, `glimpse`, `ai-core`…, when it says.
    pub frontend: Option<String>,
}

/// Register the interface for `app` (call from `connect_startup`). `tools` is what
/// `Describe` returns; `handler` answers `Call`.
pub fn serve(app: &gtk::Application, tools: Value, handler: impl Fn(&str, Value) -> Result<Value, String> + 'static) {
    serve_async(app, tools, move |name, args, reply| reply(handler(name, args)));
}

/// As `serve`, for handlers that answer later (after a database read, say).
pub fn serve_async(app: &gtk::Application, tools: Value, handler: impl Fn(&str, Value, Reply) + 'static) {
    serve_with(app, tools, move |request, answer| {
        handler(
            &request.action,
            request.arguments,
            Box::new(move |result| answer(result.map_err(|e| Error::new(ErrorKind::Failed, e)))),
        );
    });
}

/// The interface with what API 2 adds: who called, whether they confirmed, typed
/// errors, and live state through the returned [`Service`].
pub fn serve_with(app: &gtk::Application, description: Value, handler: impl Fn(Request, Answer) + 'static) -> Option<Service> {
    let (Some(connection), Some(path), Some(id)) = (app.dbus_connection(), app.dbus_object_path(), app.application_id()) else {
        return None;
    };
    let own_dir = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf));
    match serve_on(&connection, &path, &id, description, own_dir, Some(app.clone().upcast()), handler) {
        Ok(service) => Some(service),
        Err(e) => {
            eprintln!("control interface: {e}");
            None
        }
    }
}

/// Print the app's description and exit when the program was started with
/// `--describe-actions` (how installers write the app's manifest). Call first in `main`.
pub fn describe_if_asked(description: &Value) {
    if !std::env::args().skip(1).any(|a| a == "--describe-actions") {
        return;
    }
    let text = match Description::from_value(description, Some("unknown")) {
        Ok(d) if d.app.api >= 2 => serde_json::to_string_pretty(&d.to_json()),
        Ok(_) => serde_json::to_string_pretty(description),
        Err(e) => {
            eprintln!("the app's description is invalid: {e}");
            std::process::exit(1);
        }
    };
    println!("{}", text.unwrap_or_default());
    std::process::exit(0);
}

/// The live-state side of a served interface.
#[derive(Clone)]
pub struct Service(Rc<Inner>);

struct Inner {
    connection: gio::DBusConnection,
    path: String,
    description: Description,
    text: String,
    own_dir: Option<PathBuf>,
    app: Option<gio::Application>,
    callers: RefCell<HashMap<String, Caller>>,
    topics: RefCell<HashMap<String, Topic>>,
    on_watch: RefCell<Option<WatchFn>>,
    owners: RefCell<Option<gio::SignalSubscription>>,
    /// Held while anything watches: a headless app stays up for a panel showing it.
    watched: RefCell<Option<gio::ApplicationHoldGuard>>,
}

type WatchFn = Rc<dyn Fn(&str, bool)>;

#[derive(Default)]
struct Topic {
    rev: u32,
    sent: Value,
    pending: Option<Value>,
    last: Option<Instant>,
    scheduled: bool,
    watchers: HashMap<String, u32>,
}

impl Service {
    /// `f(topic, watched)` when a topic gains its first watcher or loses its last, so
    /// the app works out a topic's state only while someone looks.
    pub fn on_watch(&self, f: impl Fn(&str, bool) + 'static) {
        *self.0.on_watch.borrow_mut() = Some(Rc::new(f));
    }

    pub fn watched(&self, topic: &str) -> bool {
        self.0.topics.borrow().get(topic).is_some_and(|t| !t.watchers.is_empty())
    }

    /// The topic's new state. Watchers get a JSON Merge Patch (so a `null` inside an
    /// object reads as a removed key).
    pub fn publish(&self, topic: &str, state: Value) {
        if !self.0.description.app.topics.iter().any(|t| t.name == topic) {
            eprintln!("control interface: {topic} isn't one of the app's topics");
            return;
        }
        let wait = {
            let mut topics = self.0.topics.borrow_mut();
            let t = topics.entry(topic.to_string()).or_default();
            t.pending = Some(state);
            match t.last.map(|l| l.elapsed()) {
                Some(e) if e < PATCH_INTERVAL => {
                    if t.scheduled {
                        return;
                    }
                    t.scheduled = true;
                    Some(PATCH_INTERVAL - e)
                }
                _ => None,
            }
        };
        match wait {
            None => flush(&self.0, topic),
            Some(wait) => {
                let weak = Rc::downgrade(&self.0);
                let topic = topic.to_string();
                glib::timeout_add_local_once(wait, move || {
                    if let Some(inner) = weak.upgrade() {
                        inner.topics.borrow_mut().entry(topic.clone()).or_default().scheduled = false;
                        flush(&inner, &topic);
                    }
                });
            }
        }
    }
}

fn flush(inner: &Inner, topic: &str) {
    let (rev, patch, watchers) = {
        let mut topics = inner.topics.borrow_mut();
        let t = topics.entry(topic.to_string()).or_default();
        let Some(new) = t.pending.take() else { return };
        if new == t.sent && t.rev > 0 {
            return;
        }
        let patch = merge_patch(&t.sent, &new);
        t.rev += 1;
        t.sent = new;
        t.last = Some(Instant::now());
        (t.rev, patch, t.watchers.keys().cloned().collect::<Vec<_>>())
    };
    let body = (topic, rev, patch.to_string()).to_variant();
    for w in watchers {
        let _ = inner.connection.emit_signal(Some(&w), &inner.path, INTERFACE, "StateChanged", Some(&body));
    }
}

/// The RFC 7396 patch that turns `old` into `new`.
fn merge_patch(old: &Value, new: &Value) -> Value {
    match (old, new) {
        (Value::Object(o), Value::Object(n)) => {
            let mut patch = Map::new();
            for k in o.keys().filter(|k| !n.contains_key(*k)) {
                patch.insert(k.clone(), Value::Null);
            }
            for (k, v) in n {
                match o.get(k) {
                    Some(old) if old == v => {}
                    Some(old @ Value::Object(_)) if v.is_object() => {
                        patch.insert(k.clone(), merge_patch(old, v));
                    }
                    _ => {
                        patch.insert(k.clone(), v.clone());
                    }
                }
            }
            Value::Object(patch)
        }
        _ => new.clone(),
    }
}

/// The interface on any connection (an app's own, or a test's private bus). `own_dir`:
/// the folder whose programs are the app's fellow Nyx programs (its own).
pub fn serve_on(
    connection: &gio::DBusConnection,
    path: &str,
    bus_name: &str,
    description: Value,
    own_dir: Option<PathBuf>,
    app: Option<gio::Application>,
    handler: impl Fn(Request, Answer) + 'static,
) -> Result<Service, String> {
    let parsed = Description::from_value(&description, Some(bus_name))?;
    let info = gio::DBusNodeInfo::for_xml(XML)
        .ok()
        .and_then(|n| n.lookup_interface(INTERFACE))
        .ok_or("the interface XML doesn't parse")?;
    let inner = Rc::new(Inner {
        connection: connection.clone(),
        path: path.to_string(),
        description: parsed,
        text: description.to_string(),
        own_dir,
        app,
        callers: RefCell::default(),
        topics: RefCell::default(),
        on_watch: RefCell::default(),
        owners: RefCell::default(),
        watched: RefCell::default(),
    });
    // Callers that leave stop watching.
    if connection.unique_name().is_some() {
        let weak = Rc::downgrade(&inner);
        let sub = connection.subscribe_to_signal(
            Some("org.freedesktop.DBus"),
            Some("org.freedesktop.DBus"),
            Some("NameOwnerChanged"),
            Some("/org/freedesktop/DBus"),
            None,
            gio::DBusSignalFlags::NONE,
            move |signal| {
                let Some((name, _, new)) = signal.parameters.get::<(String, String, String)>() else { return };
                if new.is_empty()
                    && let Some(inner) = weak.upgrade()
                {
                    gone(&inner, &name);
                }
            },
        );
        *inner.owners.borrow_mut() = Some(sub);
    }
    let handler = Rc::new(handler);
    let weak = Rc::downgrade(&inner);
    connection
        .register_object(path, &info)
        .method_call(move |_, sender, _, _, method, params, invocation| {
            let Some(inner) = weak.upgrade() else { return };
            if method == "Describe" {
                invocation.return_value(Some(&(inner.text.clone(),).to_variant()));
                return;
            }
            let handler = handler.clone();
            let method = method.to_string();
            let sender = sender.map(str::to_string);
            identify(&inner, sender, move |inner, caller| dispatch(inner, &method, params, invocation, caller, &*handler));
        })
        .build()
        .map_err(|e| e.to_string())?;
    Ok(Service(inner))
}

fn hold_while_watched(inner: &Inner) {
    let any = inner.topics.borrow().values().any(|t| !t.watchers.is_empty());
    let mut held = inner.watched.borrow_mut();
    match (&inner.app, any, held.is_some()) {
        (Some(app), true, false) => *held = Some(app.hold()),
        (_, false, true) => *held = None,
        _ => {}
    }
}

fn gone(inner: &Inner, name: &str) {
    inner.callers.borrow_mut().remove(name);
    let emptied: Vec<String> = {
        let mut topics = inner.topics.borrow_mut();
        topics
            .iter_mut()
            .filter_map(|(topic, t)| (t.watchers.remove(name).is_some() && t.watchers.is_empty()).then(|| topic.clone()))
            .collect()
    };
    hold_while_watched(inner);
    let callback = inner.on_watch.borrow().clone();
    if let Some(f) = callback {
        for topic in emptied {
            f(&topic, false);
        }
    }
}

/// Who `sender` is, by the program its process runs.
fn identify(inner: &Rc<Inner>, sender: Option<String>, then: impl FnOnce(&Rc<Inner>, Caller) + 'static) {
    let Some(name) = sender else {
        return then(inner, Caller { sender: None, program: None, trusted: false });
    };
    if let Some(c) = inner.callers.borrow().get(&name).cloned() {
        return then(inner, c);
    }
    let weak: Weak<Inner> = Rc::downgrade(inner);
    let asked = name.clone();
    inner.connection.call(
        Some("org.freedesktop.DBus"),
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
        "GetConnectionUnixProcessID",
        Some(&(asked,).to_variant()),
        glib::VariantTy::new("(u)").ok(),
        gio::DBusCallFlags::NONE,
        2000,
        gio::Cancellable::NONE,
        move |reply| {
            let Some(inner) = weak.upgrade() else { return };
            let pid = reply.ok().and_then(|v| v.get::<(u32,)>()).map(|(p,)| p);
            let exe = pid.and_then(|p| std::fs::read_link(format!("/proc/{p}/exe")).ok());
            let program =
                exe.and_then(|exe| nyx_peer::program(&exe, inner.own_dir.as_deref().unwrap_or(Path::new("/nonexistent"))));
            let caller = Caller { sender: Some(name.clone()), trusted: program.is_some(), program };
            let mut callers = inner.callers.borrow_mut();
            if callers.len() > 256 {
                callers.clear();
            }
            callers.insert(name, caller.clone());
            drop(callers);
            then(&inner, caller);
        },
    );
}

fn dispatch(
    inner: &Rc<Inner>,
    method: &str,
    params: glib::Variant,
    invocation: gio::DBusMethodInvocation,
    caller: Caller,
    handler: &dyn Fn(Request, Answer),
) {
    let reply = |text: String| invocation.clone().return_value(Some(&(text,).to_variant()));
    match method {
        "Call" | "Invoke" => {
            let v2 = method == "Invoke";
            let (action, arguments, options) = if v2 {
                params.get::<(String, String, String)>().unwrap_or_default()
            } else {
                let (a, b) = params.get::<(String, String)>().unwrap_or_default();
                (a, b, String::new())
            };
            let fail = |e: Error| if v2 { json!({"error": e}) } else { json!({"error": e.message}) }.to_string();
            let arguments: Value = match serde_json::from_str(&arguments) {
                Ok(v) => v,
                Err(_) if !v2 => json!({}),
                Err(_) => return reply(fail(Error::new(ErrorKind::InvalidArguments, "the arguments aren't JSON"))),
            };
            let options: Value = serde_json::from_str(&options).unwrap_or(json!({}));
            // API 1 callers asked the person themselves when they're trusted.
            let confirmed = if v2 { options["confirmed"] == true } else { caller.trusted };
            let Some(spec) = inner.description.action(&action) else {
                return reply(fail(Error::new(ErrorKind::UnknownAction, format!("{action} isn't an action here"))));
            };
            if let Err(e) = check_call(caller.trusted, spec, &arguments, confirmed) {
                return reply(fail(e));
            }
            let text = |key: &str| options.get(key).and_then(Value::as_str).map(str::to_string);
            let request =
                Request { action, arguments, confirmed, on_behalf_of: text("on_behalf_of"), frontend: text("frontend"), caller };
            let hold = inner.app.as_ref().map(|a| a.hold());
            handler(
                request,
                Box::new(move |result| {
                    let _hold = hold;
                    let body = match result {
                        Ok(v) => json!({ "ok": v }),
                        Err(e) if v2 => json!({ "error": e }),
                        Err(e) => json!({ "error": e.message }),
                    };
                    invocation.return_value(Some(&(body.to_string(),).to_variant()));
                }),
            );
        }
        "State" | "Watch" | "Unwatch" => {
            let (topic,) = params.get::<(String,)>().unwrap_or_default();
            let Some(declared) = inner.description.app.topics.iter().find(|t| t.name == topic) else {
                return invocation.return_dbus_error(
                    "org.freedesktop.DBus.Error.InvalidArgs",
                    &format!("{topic} isn't one of this app's topics"),
                );
            };
            if declared.sensitive && !caller.trusted {
                return invocation.return_dbus_error(
                    "org.freedesktop.DBus.Error.AccessDenied",
                    &format!("only Nyx programs may read {topic}"),
                );
            }
            let mut first = false;
            let mut last = false;
            let rev = {
                let mut topics = inner.topics.borrow_mut();
                let t = topics.entry(topic.clone()).or_default();
                let sender = caller.sender.clone().unwrap_or_default();
                match method {
                    "Watch" => {
                        first = t.watchers.is_empty();
                        *t.watchers.entry(sender).or_default() += 1;
                    }
                    "Unwatch" => {
                        if let Some(n) = t.watchers.get_mut(&sender) {
                            *n -= 1;
                            if *n == 0 {
                                t.watchers.remove(&sender);
                                last = t.watchers.is_empty();
                            }
                        }
                    }
                    _ => {}
                }
                t.rev
            };
            hold_while_watched(inner);
            let callback = inner.on_watch.borrow().clone();
            if let Some(f) = callback.filter(|_| first || last) {
                f(&topic, first);
            }
            match method {
                "State" => {
                    let topics = inner.topics.borrow();
                    let t = &topics[&topic];
                    let state = t.pending.clone().unwrap_or_else(|| t.sent.clone());
                    reply(json!({"rev": t.rev, "state": state}).to_string());
                }
                "Watch" => invocation.return_value(Some(&(rev,).to_variant())),
                _ => invocation.return_value(None),
            }
        }
        _ => invocation.return_dbus_error("org.freedesktop.DBus.Error.UnknownMethod", method),
    }
}

/// A string argument, or an error naming it.
pub fn text(args: &Value, key: &str) -> Result<String, String> {
    args[key].as_str().map(str::to_string).ok_or_else(|| format!("{key} is required"))
}

/// A path inside the home folder: `~/x`, a path relative to home, or an absolute one under
/// it. Symbolic links are resolved first, so they can't lead out.
pub fn home_path(input: &str) -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME").map(PathBuf::from).ok_or("no home folder")?;
    inside(&home, input)
}

fn inside(home: &Path, input: &str) -> Result<PathBuf, String> {
    let input = input.trim();
    if input.is_empty() {
        return Err("path is empty".into());
    }
    let raw = match input.strip_prefix("~/") {
        Some(rest) => home.join(rest),
        None if input == "~" => home.to_path_buf(),
        None => home.join(input),
    };
    // The deepest folder that exists, resolved; the rest must be plain names.
    let mut existing = raw.clone();
    let mut rest = Vec::new();
    while !existing.exists() {
        let Some(name) = existing.file_name().map(|n| n.to_os_string()) else { return Err("not a usable path".into()) };
        rest.push(name);
        existing = existing.parent().ok_or("not a usable path")?.to_path_buf();
    }
    let mut resolved = existing.canonicalize().map_err(|e| e.to_string())?;
    for name in rest.into_iter().rev() {
        if name == ".." || name == "." {
            return Err("path can't use . or ..".into());
        }
        resolved.push(name);
    }
    let home = home.canonicalize().map_err(|e| e.to_string())?;
    if !resolved.starts_with(&home) || resolved == home {
        return Err("only paths inside the home folder".into());
    }
    Ok(resolved)
}

/// Where to save: inside home, its folder existing, and an existing file only with
/// `overwrite: true`.
pub fn save_target(args: &Value) -> Result<PathBuf, String> {
    let path = home_path(&text(args, "path")?)?;
    if path.is_dir() {
        return Err("that's a folder".into());
    }
    if !path.parent().is_some_and(Path::is_dir) {
        return Err("its folder doesn't exist".into());
    }
    if path.exists() && args["overwrite"] != true {
        return Err("a file of that name exists; pass overwrite: true to replace it".into());
    }
    Ok(path)
}

/// Run `f` once the main loop is idle (after a tab or window has appeared).
pub fn later(f: impl FnOnce() + 'static) {
    glib::idle_add_local_once(f);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_stay_inside_home() {
        let home = std::env::temp_dir().join(format!("nyx-control-{}", std::process::id()));
        std::fs::create_dir_all(home.join("docs")).unwrap();
        let canon = home.canonicalize().unwrap();
        assert_eq!(inside(&home, "~/docs/a.txt").unwrap(), canon.join("docs/a.txt"));
        assert_eq!(inside(&home, "docs/new/b.txt").unwrap(), canon.join("docs/new/b.txt"));
        assert!(inside(&home, "/etc/passwd").is_err());
        assert!(inside(&home, "~/../x").is_err());
        assert!(inside(&home, "~").is_err());
        assert!(inside(&home, "").is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/tmp", home.join("out")).unwrap();
            assert!(inside(&home, "~/out/x.txt").is_err());
        }
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn merge_patches() {
        let old = json!({"title": "A", "art": "x", "volume": {"level": 0.5, "muted": false}});
        let new = json!({"title": "B", "volume": {"level": 0.5, "muted": true}, "queue": [1]});
        assert_eq!(merge_patch(&old, &new), json!({"title": "B", "art": null, "volume": {"muted": true}, "queue": [1]}));
        assert_eq!(merge_patch(&Value::Null, &new), new);
    }
}
