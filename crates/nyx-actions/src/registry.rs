//! The registry: installed manifests and running apps (the contract's sections 4 to 6),
//! checked and routed. Linked into each frontend; it starts nothing until a call needs
//! an app, and then only that app (D-Bus activation).

use crate::catalog::{Catalog, State};
use crate::check::check_call;
use crate::error::{Error, ErrorKind};
use crate::spec::{Action, App, BUS_PREFIX, Description, INTERFACE};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

/// How long running apps' descriptions stay fresh.
const LIVE_TTL: Duration = Duration::from_secs(10);
const DESCRIBE_TIMEOUT: Duration = Duration::from_secs(2);
/// Describing an app that has to start first.
const START_TIMEOUT: Duration = Duration::from_secs(10);

pub struct Options {
    /// Folders of `<app-id>.json` manifests, in precedence order.
    pub dirs: Vec<PathBuf>,
    /// A bus address instead of the session bus (tests).
    pub bus_address: Option<String>,
    /// Whether to ask running apps (off: installed manifests only).
    pub live: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options { dirs: manifest_dirs(), bus_address: None, live: true }
    }
}

/// `nyx/actions` in `$XDG_DATA_HOME`, then in each of `$XDG_DATA_DIRS`.
pub fn manifest_dirs() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    let data_home = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home.join(".local/share"));
    let dirs =
        std::env::var("XDG_DATA_DIRS").ok().filter(|d| !d.is_empty()).unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    let mut out = vec![data_home.join("nyx/actions")];
    for d in dirs.split(':').map(PathBuf::from).filter(|p| p.is_absolute()) {
        let d = d.join("nyx/actions");
        if !out.contains(&d) {
            out.push(d);
        }
    }
    out
}

/// One call's standing.
#[derive(Clone, Debug, Default)]
pub struct InvokeOptions {
    /// A person agreed to this call in the frontend's UI.
    pub confirmed: bool,
    /// Whoever asked the frontend may run more than read-only actions (the person at
    /// the CLI or Glimpse; AI Core's trusted clients).
    pub trusted: bool,
    /// The program the frontend calls for, when that isn't itself (AI Core's client).
    pub on_behalf_of: Option<String>,
    /// `cli`, `glimpse`, `ai-core`…
    pub frontend: String,
}

struct Cached {
    stamp: (SystemTime, u64),
    parsed: Option<Description>,
}

#[derive(Default)]
struct Live {
    at: Option<Instant>,
    apps: Vec<Description>,
}

pub struct Registry {
    options: Options,
    files: Mutex<HashMap<PathBuf, Cached>>,
    warned: Mutex<HashSet<String>>,
    live: tokio::sync::Mutex<Live>,
    bus: tokio::sync::OnceCell<Option<zbus::Connection>>,
}

impl Registry {
    pub fn new(options: Options) -> Self {
        Registry {
            options,
            files: Mutex::default(),
            warned: Mutex::default(),
            live: tokio::sync::Mutex::default(),
            bus: tokio::sync::OnceCell::new(),
        }
    }

    /// Everything on offer now.
    pub async fn catalog(&self) -> Catalog {
        let manifests = self.manifests();
        let running = self.running(false, &manifests).await;
        self.merged(manifests, running)
    }

    /// Installed manifests only: reads files, asks no app.
    pub fn installed(&self) -> Catalog {
        self.merged(self.manifests(), Vec::new())
    }

    fn merged(&self, manifests: Vec<Description>, running: Vec<Description>) -> Catalog {
        let (catalog, warnings) = Catalog::merge(manifests, running, &missing);
        for w in warnings {
            self.warn(w);
        }
        catalog
    }

    fn warn(&self, message: String) {
        if self.warned.lock().expect("warned").insert(message.clone()) {
            tracing::warn!("{message}");
        }
    }

    /// The manifests, read again only when a file changed.
    fn manifests(&self) -> Vec<Description> {
        let mut out = Vec::new();
        let mut files = self.files.lock().expect("files");
        let mut present = HashSet::new();
        for dir in &self.options.dirs {
            let Ok(read) = std::fs::read_dir(dir) else { continue };
            let mut paths: Vec<PathBuf> =
                read.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "json")).collect();
            paths.sort();
            for path in paths {
                let Ok(meta) = std::fs::metadata(&path) else { continue };
                let stamp = (meta.modified().unwrap_or(SystemTime::UNIX_EPOCH), meta.len());
                present.insert(path.clone());
                if files.get(&path).is_none_or(|c| c.stamp != stamp) {
                    let parsed = match read_manifest(&path) {
                        Ok(d) => Some(d),
                        Err(e) => {
                            self.warn(format!("{}: {e}", path.display()));
                            None
                        }
                    };
                    files.insert(path.clone(), Cached { stamp, parsed });
                }
                if let Some(d) = &files[&path].parsed {
                    out.push(d.clone());
                }
            }
        }
        files.retain(|p, _| present.contains(p));
        out
    }

    async fn bus(&self) -> Option<zbus::Connection> {
        self.bus
            .get_or_init(|| async {
                let bus = match &self.options.bus_address {
                    Some(address) => match zbus::connection::Builder::address(address.as_str()) {
                        Ok(b) => b.build().await,
                        Err(e) => Err(e),
                    },
                    None => zbus::Connection::session().await,
                };
                bus.inspect_err(|e| tracing::info!(error = %e, "no session bus; only installed actions are listed")).ok()
            })
            .await
            .clone()
    }

    /// Running Nyx apps' descriptions, unless the last answer is still fresh.
    async fn running(&self, force: bool, manifests: &[Description]) -> Vec<Description> {
        if !self.options.live {
            return Vec::new();
        }
        let mut live = self.live.lock().await;
        if !force && live.at.is_some_and(|t| t.elapsed() < LIVE_TTL) {
            return live.apps.clone();
        }
        let Some(bus) = self.bus().await else { return Vec::new() };
        let names = match zbus::fdo::DBusProxy::new(&bus).await {
            Ok(proxy) => proxy.list_names().await.unwrap_or_default(),
            Err(_) => Vec::new(),
        };
        let names: Vec<String> = names.iter().map(|n| n.to_string()).filter(|n| n.starts_with(BUS_PREFIX)).collect();
        let answers = futures_util::future::join_all(names.iter().map(|name| {
            let path = manifests
                .iter()
                .find(|m| m.app.bus_name == *name)
                .map(|m| m.app.path())
                .unwrap_or_else(|| crate::spec::object_path(name));
            describe(&bus, name, path, DESCRIBE_TIMEOUT)
        }))
        .await;
        live.apps = answers.into_iter().filter_map(Result::ok).collect();
        live.at = Some(Instant::now());
        live.apps.clone()
    }

    /// Check and route one call.
    pub async fn invoke(&self, id: &str, arguments: Value, options: &InvokeOptions) -> Result<Value, Error> {
        let started = Instant::now();
        let mut catalog = self.catalog().await;
        // Not listed, or listed as closed: the app may have started since the last look.
        if catalog.find(id).is_none_or(|(_, l)| !l.available) {
            let manifests = self.manifests();
            let running = self.running(true, &manifests).await;
            catalog = self.merged(manifests, running);
        }
        let Some((entry, listed)) = catalog.find(id) else {
            let namespace = id.split('.').next().unwrap_or(id);
            return Err(match catalog.apps.iter().find(|e| e.app.namespaces.iter().any(|n| n == namespace)) {
                Some(e) => Error::new(ErrorKind::UnknownAction, format!("{} has no action {id}", e.app.name)),
                None => Error::new(ErrorKind::UnknownApp, format!("no app offers {id}")),
            });
        };
        if !listed.available {
            let kind = if listed.state == State::NotRunning { ErrorKind::NotRunning } else { ErrorKind::Unavailable };
            return Err(Error::new(kind, listed.reason.clone().unwrap_or_else(|| format!("{id} isn't available now"))));
        }
        let arguments = if arguments.is_null() { json!({}) } else { arguments };
        check_call(options.trusted, &listed.action, &arguments, options.confirmed)?;
        let (app, api, action) = (entry.app.clone(), entry.running, listed.action.clone());
        let result = match tokio::time::timeout(action.timeout(), self.route(&app, api, &action, &arguments, options)).await {
            Ok(r) => r,
            Err(_) => {
                Err(Error::new(ErrorKind::Timeout, format!("{} didn't answer within {} s", app.name, action.timeout().as_secs())))
            }
        };
        let outcome = match &result {
            Ok(_) => "ok",
            Err(e) => e.kind.as_str(),
        };
        tracing::info!(action = id, frontend = %options.frontend, owner = %app.app, outcome, ms = started.elapsed().as_millis() as u64, "action");
        result
    }

    async fn route(
        &self,
        app: &App,
        api: Option<u32>,
        action: &Action,
        arguments: &Value,
        options: &InvokeOptions,
    ) -> Result<Value, Error> {
        let bus = self.bus().await.ok_or_else(|| Error::new(ErrorKind::Unavailable, "there's no session bus"))?;
        let api = match api {
            Some(api) => api,
            // Not running: describing it starts it (D-Bus activation) and says which API it speaks.
            None => {
                let started = describe(&bus, &app.bus_name, app.path(), START_TIMEOUT).await?;
                let api = started.app.api;
                let mut live = self.live.lock().await;
                live.apps.retain(|d| d.app.bus_name != app.bus_name);
                live.apps.push(started);
                api
            }
        };
        let reply = if api >= 2 {
            let opts =
                json!({"confirmed": options.confirmed, "on_behalf_of": options.on_behalf_of, "frontend": options.frontend});
            call(&bus, app, "Invoke", &(action.id.as_str(), arguments.to_string(), opts.to_string())).await?
        } else {
            call(&bus, app, "Call", &(action.id.as_str(), arguments.to_string())).await?
        };
        parse_reply(&app.name, &reply)
    }

    /// A topic's current state (`{"rev", "state"}`), from an API 2 app.
    pub async fn state(&self, app_id: &str, topic: &str) -> Result<Value, Error> {
        let catalog = self.catalog().await;
        let entry = catalog
            .apps
            .iter()
            .find(|e| e.app.app == app_id)
            .ok_or_else(|| Error::new(ErrorKind::UnknownApp, format!("{app_id} isn't installed or running")))?;
        if !entry.app.topics.iter().any(|t| t.name == topic) {
            return Err(Error::new(ErrorKind::UnknownAction, format!("{} has no topic {topic}", entry.app.name)));
        }
        let bus = self.bus().await.ok_or_else(|| Error::new(ErrorKind::Unavailable, "there's no session bus"))?;
        let reply = tokio::time::timeout(START_TIMEOUT, call(&bus, &entry.app, "State", &(topic,)))
            .await
            .map_err(|_| Error::new(ErrorKind::Timeout, format!("{} didn't answer", entry.app.name)))??;
        serde_json::from_str(&reply)
            .map_err(|_| Error::new(ErrorKind::Failed, format!("{} answered with something that isn't JSON", entry.app.name)))
    }
}

fn read_manifest(path: &Path) -> Result<Description, String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let d = Description::parse(&text, None)?;
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
    if d.app.app != stem {
        return Err(format!("describes {}, not {stem}", d.app.app));
    }
    Ok(d)
}

async fn describe(bus: &zbus::Connection, name: &str, path: String, timeout: Duration) -> Result<Description, Error> {
    let call = bus.call_method(Some(name), path.as_str(), Some(INTERFACE), "Describe", &());
    let reply = tokio::time::timeout(timeout, call)
        .await
        .map_err(|_| Error::new(ErrorKind::Timeout, format!("{name} didn't describe itself in time")))?
        .map_err(|e| dbus_error(name, e))?;
    let text: String =
        reply.body().deserialize().map_err(|e| Error::new(ErrorKind::Failed, format!("{name} answered oddly: {e}")))?;
    let mut d = Description::parse(&text, Some(name)).map_err(|e| Error::new(ErrorKind::Failed, format!("{name}: {e}")))?;
    // Calls go where it answered.
    d.app.bus_name = name.to_string();
    d.app.object_path = Some(path);
    Ok(d)
}

async fn call<B>(bus: &zbus::Connection, app: &App, method: &str, body: &B) -> Result<String, Error>
where
    B: serde::Serialize + zbus::zvariant::DynamicType,
{
    let path = app.path();
    let reply = bus
        .call_method(Some(app.bus_name.as_str()), path.as_str(), Some(INTERFACE), method, body)
        .await
        .map_err(|e| dbus_error(&app.name, e))?;
    reply.body().deserialize::<String>().map_err(|e| Error::new(ErrorKind::Failed, format!("{} answered oddly: {e}", app.name)))
}

fn dbus_error(who: &str, e: zbus::Error) -> Error {
    if let zbus::Error::MethodError(name, message, _) = &e {
        let detail = message.clone().unwrap_or_default();
        return match name.as_str() {
            "org.freedesktop.DBus.Error.ServiceUnknown" | "org.freedesktop.DBus.Error.NameHasNoOwner" => {
                Error::new(ErrorKind::Unavailable, format!("{who} isn't running and couldn't be started"))
            }
            "org.freedesktop.DBus.Error.Spawn.ChildExited"
            | "org.freedesktop.DBus.Error.Spawn.ExecFailed"
            | "org.freedesktop.DBus.Error.Spawn.ChildSignaled" => {
                Error::new(ErrorKind::Unavailable, format!("{who} couldn't be started: {detail}"))
            }
            "org.freedesktop.DBus.Error.AccessDenied" => {
                Error::new(ErrorKind::PermissionDenied, if detail.is_empty() { format!("{who} refused") } else { detail })
            }
            "org.freedesktop.DBus.Error.NoReply" | "org.freedesktop.DBus.Error.Timeout" => {
                Error::new(ErrorKind::Timeout, format!("{who} didn't answer"))
            }
            _ => Error::new(ErrorKind::Failed, format!("{who}: {detail}")),
        };
    }
    Error::new(ErrorKind::Unavailable, format!("{who} didn't answer: {e}"))
}

/// `{"ok": …}`, or `{"error": {"kind", "message"}}` (API 2) or `{"error": "…"}` (API 1).
fn parse_reply(who: &str, text: &str) -> Result<Value, Error> {
    let value: Value = serde_json::from_str(text)
        .map_err(|_| Error::new(ErrorKind::Failed, format!("{who} answered with something that isn't JSON")))?;
    match (value.get("ok"), value.get("error")) {
        (_, Some(Value::Object(e))) => Err(Error::new(
            e.get("kind").and_then(Value::as_str).map(ErrorKind::parse).unwrap_or(ErrorKind::Failed),
            e.get("message").and_then(Value::as_str).unwrap_or("it failed").to_string(),
        )),
        (_, Some(e)) => Err(Error::new(ErrorKind::Failed, e.as_str().map(str::to_string).unwrap_or_else(|| e.to_string()))),
        (Some(ok), None) => Ok(ok.clone()),
        (None, None) => Ok(Value::Null),
    }
}

/// Why a required capability isn't there, for the kinds checked here (`program:NAME`
/// on `PATH`). Other kinds are taken as present until a probe exists for them.
fn missing(capability: &str) -> Option<String> {
    let program = capability.strip_prefix("program:")?;
    let found = std::env::var_os("PATH").is_some_and(|path| std::env::split_paths(&path).any(|d| d.join(program).is_file()));
    (!found).then(|| format!("{program} isn't installed"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replies_of_both_apis() {
        assert_eq!(parse_reply("Notes", r#"{"ok": {"id": "a"}}"#).unwrap()["id"], "a");
        let e = parse_reply("Notes", r#"{"error": {"kind": "permission_denied", "message": "locked"}}"#).unwrap_err();
        assert_eq!((e.kind, e.message.as_str()), (ErrorKind::PermissionDenied, "locked"));
        let e = parse_reply("Notes", r#"{"error": "no vault"}"#).unwrap_err();
        assert_eq!((e.kind, e.message.as_str()), (ErrorKind::Failed, "no vault"));
        assert_eq!(parse_reply("Notes", "{}").unwrap(), Value::Null);
        assert!(parse_reply("Notes", "nope").is_err());
    }

    #[test]
    fn manifest_folders_follow_xdg() {
        let dirs = manifest_dirs();
        assert!(dirs[0].ends_with("nyx/actions"));
        assert!(dirs.len() >= 2);
    }

    #[test]
    fn missing_programs() {
        assert!(missing("program:sh").is_none());
        assert_eq!(missing("program:nyx-no-such-program").as_deref(), Some("nyx-no-such-program isn't installed"));
        assert!(missing("service:user:anything").is_none());
    }
}
