use crate::Error;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::{cell::RefCell, os::unix::fs::PermissionsExt, path::PathBuf, rc::Rc};
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Kind {
    Calendar,
    TaskList,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    pub uid: String,
    pub name: String,
    pub kind: Kind,
    pub color: Option<String>,
    pub selected: bool,
    pub backend: String,
    pub account: Option<String>,
    pub group: Option<String>,
    pub local: bool,
    pub writable: bool,
}
impl Source {
    pub fn label(&self) -> &str {
        if self.name.is_empty() { &self.uid } else { &self.name }
    }
}
#[derive(Clone)]
pub struct Registry {
    pub(crate) db: Rc<RefCell<Connection>>,
    pub(crate) pending: Rc<std::cell::Cell<bool>>,
}
pub struct Watch {
    source: Option<glib::SourceId>,
}
impl Drop for Watch {
    fn drop(&mut self) {
        if let Some(s) = self.source.take() {
            s.remove();
        }
    }
}
pub(crate) fn error(e: impl std::fmt::Display) -> Error {
    Error::Failed(e.to_string())
}
impl Registry {
    pub fn connect() -> Result<Self, Error> {
        let base = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
            .ok_or_else(|| Error::Unavailable("Data directory unavailable".into()))?;
        Self::open(base.join("nyx-pim-preview/pim.sqlite3"))
    }
    pub fn open(path: PathBuf) -> Result<Self, Error> {
        let dir = path.parent().ok_or_else(|| error("Invalid database path"))?;
        std::fs::create_dir_all(dir).map_err(error)?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).map_err(error)?;
        let db = Connection::open(&path).map_err(error)?;
        db.busy_timeout(std::time::Duration::from_secs(5)).map_err(error)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).map_err(error)?;
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;
CREATE TABLE IF NOT EXISTS sources(uid TEXT PRIMARY KEY, data TEXT NOT NULL, enabled INTEGER NOT NULL DEFAULT 1);
CREATE TABLE IF NOT EXISTS objects(source TEXT NOT NULL REFERENCES sources(uid),uid TEXT NOT NULL,rid TEXT NOT NULL DEFAULT '',payload TEXT NOT NULL,etag TEXT,dirty INTEGER NOT NULL DEFAULT 0,deleted INTEGER NOT NULL DEFAULT 0,PRIMARY KEY(source,uid,rid));
CREATE TABLE IF NOT EXISTS api_endpoints(source TEXT PRIMARY KEY,endpoint TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS api_baselines(source TEXT,uid TEXT,rid TEXT,payload TEXT,PRIMARY KEY(source,uid,rid));
CREATE TABLE IF NOT EXISTS api_resources(source TEXT,uid TEXT,rid TEXT,remote_id TEXT,etag TEXT,PRIMARY KEY(source,uid,rid));
CREATE TABLE IF NOT EXISTS remote_conflicts(source TEXT,uid TEXT,payload TEXT,etag TEXT,PRIMARY KEY(source,uid));
CREATE TABLE IF NOT EXISTS timezones(source TEXT,tzid TEXT,payload TEXT,PRIMARY KEY(source,tzid));
CREATE TABLE IF NOT EXISTS raw_imports(source TEXT NOT NULL,payload TEXT NOT NULL,PRIMARY KEY(source,payload));
CREATE TABLE IF NOT EXISTS alarm_snapshots(source TEXT,uid TEXT,rid TEXT,due INTEGER,data TEXT,PRIMARY KEY(source,uid,rid,due));
CREATE TABLE IF NOT EXISTS alarms(source TEXT,uid TEXT,rid TEXT,due INTEGER,until INTEGER,dismissed INTEGER DEFAULT 0,PRIMARY KEY(source,uid,rid,due));").map_err(error)?;
        let _ = db.execute("ALTER TABLE sources ADD COLUMN healthy INTEGER NOT NULL DEFAULT 0", []);
        let registry = Self { db: Rc::new(RefCell::new(db)), pending: Default::default() };
        for (uid, name, kind) in [("system-calendar", "Personal", Kind::Calendar), ("system-task-list", "Tasks", Kind::TaskList)]
        {
            if registry.source(uid, kind).is_none() {
                registry.add_source(&Source {
                    uid: uid.into(),
                    name: name.into(),
                    kind,
                    color: None,
                    selected: true,
                    backend: "native".into(),
                    account: None,
                    group: None,
                    local: true,
                    writable: true,
                })?;
            }
        }
        Ok(registry)
    }
    pub fn add_source(&self, source: &Source) -> Result<(), Error> {
        let mut source = source.clone();
        if source.account.is_some() {
            if let Some(previous) = self.source(&source.uid, source.kind) {
                source.selected = previous.selected;
                source.color = previous.color;
            }
        }
        self.db
            .borrow()
            .execute(
                "INSERT INTO sources(uid,data) VALUES(?1,?2) ON CONFLICT(uid) DO UPDATE SET data=excluded.data",
                rusqlite::params![source.uid, serde_json::to_string(&source).map_err(error)?],
            )
            .map_err(error)?;
        Ok(())
    }
    pub fn all_sources(&self, kind: Kind) -> Vec<(Source, bool)> {
        let db = self.db.borrow();
        let Ok(mut query) = db.prepare("SELECT data,enabled FROM sources ORDER BY uid") else { return Vec::new() };
        let Ok(rows) = query.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, bool>(1)?))) else { return Vec::new() };
        rows.filter_map(Result::ok)
            .filter_map(|(s, e)| serde_json::from_str::<Source>(&s).ok().filter(|s| s.kind == kind).map(|s| (s, e)))
            .collect()
    }
    pub fn sources(&self, kind: Kind) -> Vec<Source> {
        let sources = self.all_sources(kind);
        let accounts = if sources.iter().any(|(s, _)| s.account.is_some()) {
            nyx_accounts_native::Client::connect().and_then(|c| c.list()).ok()
        } else {
            None
        };
        let service =
            if kind == Kind::Calendar { nyx_accounts_native::Service::Calendar } else { nyx_accounts_native::Service::Tasks };
        sources
            .into_iter()
            .filter(|(source, enabled)| {
                *enabled
                    && source.account.as_ref().is_none_or(|id| {
                        accounts
                            .as_ref()
                            .is_none_or(|accounts| accounts.iter().any(|a| &a.id == id && a.enabled.contains(&service)))
                    })
            })
            .map(|(s, _)| s)
            .collect()
    }
    pub fn source(&self, uid: &str, kind: Kind) -> Option<Source> {
        self.all_sources(kind).into_iter().find(|(s, _)| s.uid == uid).map(|(s, _)| s)
    }
    pub fn set_enabled(&self, uid: &str, enabled: bool) -> Result<(), Error> {
        self.db.borrow().execute("UPDATE sources SET enabled=?2 WHERE uid=?1", rusqlite::params![uid, enabled]).map_err(error)?;
        Ok(())
    }
    pub fn set_selected(&self, uid: &str, kind: Kind, selected: bool) -> Result<(), Error> {
        let mut s = self.source(uid, kind).ok_or_else(|| error("Source not found"))?;
        s.selected = selected;
        self.add_source(&s)
    }
    pub fn set_color(&self, uid: &str, kind: Kind, color: &str) -> Result<(), Error> {
        let mut s = self.source(uid, kind).ok_or_else(|| error("Source not found"))?;
        s.color = Some(color.into());
        self.add_source(&s)
    }
    pub fn watch(&self, on_change: impl Fn() + 'static) -> Watch {
        let db = self.db.clone();
        let mut previous = -1i64;
        let source = glib::timeout_add_local(std::time::Duration::from_secs(1), move || {
            let current = db.borrow().query_row("PRAGMA data_version", [], |r| r.get::<_, i64>(0)).unwrap_or(previous);
            let changes = db.borrow().total_changes() as i64;
            let version = current + changes;
            if previous != -1 && previous != version {
                on_change();
            }
            previous = version;
            glib::ControlFlow::Continue
        });
        Watch { source: Some(source) }
    }
}

impl Registry {
    pub(crate) fn queue_sync(&self, service: nyx_accounts_native::Service) {
        if self.pending.replace(true) {
            return;
        }
        let registry = self.clone();
        glib::timeout_add_local_once(std::time::Duration::from_secs(1), move || {
            registry.pending.set(false);
            let _ = registry.refresh_accounts(service);
        });
    }
    pub(crate) fn healthy(&self, uid: &str) -> bool {
        self.db.borrow().query_row("SELECT healthy FROM sources WHERE uid=?1", [uid], |r| r.get(0)).unwrap_or(false)
    }
    pub(crate) fn set_healthy(&self, uid: &str, healthy: bool) {
        let _ = self.db.borrow().execute("UPDATE sources SET healthy=?2 WHERE uid=?1", rusqlite::params![uid, healthy]);
    }
}
