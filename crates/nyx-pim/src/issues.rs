//! Explicit conflict resolution. Neither version is discarded: every decision is archived.
use crate::{Calendar, Error, Kind, Registry, Source, registry::error};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{fs::File, os::fd::AsRawFd};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncIssue {
    pub source: String,
    pub uid: String,
    pub rid: String,
    pub kind: String,
    pub source_label: String,
    pub local: String,
    pub remote: String,
    pub remote_deleted: bool,
    pub revision: String,
}
#[derive(Clone, Copy, Debug)]
pub enum Resolution {
    KeepLocal,
    UseRemote,
    KeepOffline,
    RetryCreation,
}

impl Registry {
    pub(crate) fn sync_lock(&self) -> Result<File, Error> {
        let path = self.db.borrow().path().map(std::path::PathBuf::from).ok_or_else(|| error("Store path unavailable"))?;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path.with_extension("sync.lock"))
            .map_err(error)?;
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(error("Another application is synchronizing. Try again when it finishes."));
        }
        Ok(file)
    }
    pub(crate) fn issues_schema(&self) -> Result<(), Error> {
        self.db.borrow().execute_batch("CREATE TABLE IF NOT EXISTS sync_issues(source TEXT,uid TEXT,rid TEXT,kind TEXT,payload TEXT,etag TEXT,remote_id TEXT,deleted INTEGER,PRIMARY KEY(source,uid,rid));
CREATE TABLE IF NOT EXISTS remote_resources(source TEXT,uid TEXT,href TEXT,etag TEXT,PRIMARY KEY(source,uid));
CREATE TABLE IF NOT EXISTS uncertain_creates(source TEXT,uid TEXT,PRIMARY KEY(source,uid));
CREATE TABLE IF NOT EXISTS resolution_history(id INTEGER PRIMARY KEY,created TEXT DEFAULT CURRENT_TIMESTAMP,source TEXT,uid TEXT,rid TEXT,decision TEXT,local TEXT,remote TEXT);").map_err(error)
    }
    pub(crate) fn record_issue(
        &self,
        source: &str,
        uid: &str,
        rid: &str,
        kind: &str,
        payload: &str,
        etag: &str,
        remote_id: &str,
        deleted: bool,
    ) -> Result<(), Error> {
        self.issues_schema()?;
        self.db.borrow().execute("INSERT INTO sync_issues VALUES(?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(source,uid,rid) DO UPDATE SET kind=excluded.kind,payload=excluded.payload,etag=excluded.etag,remote_id=excluded.remote_id,deleted=excluded.deleted",params![source,uid,rid,kind,payload,etag,remote_id,deleted]).map_err(error)?;
        Ok(())
    }
    pub fn sync_issues(&self) -> Result<Vec<SyncIssue>, Error> {
        self.issues_schema()?;
        let records: Vec<(String, String, String, String, String, String, String, bool)> = {
            let db = self.db.borrow();
            let mut q=db.prepare("SELECT source,uid,rid,kind,payload,etag,remote_id,deleted FROM sync_issues UNION ALL SELECT source,uid,'','uncertain','','','',0 FROM uncertain_creates WHERE NOT EXISTS(SELECT 1 FROM sync_issues s WHERE s.source=uncertain_creates.source AND s.uid=uncertain_creates.uid) ORDER BY source,uid,rid").map_err(error)?;
            q.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?)))
                .map_err(error)?
                .collect::<Result<_, _>>()
                .map_err(error)?
        };
        records
            .into_iter()
            .map(|(source, uid, rid, kind, remote, etag, remote_id, remote_deleted)| {
                let local = self.issue_local(&source, &uid, &rid, &kind)?;
                let source_label = self
                    .source(&source, Kind::Calendar)
                    .or_else(|| self.source(&source, Kind::TaskList))
                    .map(|s| s.label().to_owned())
                    .unwrap_or_else(|| source.clone());
                let revision = format!(
                    "{:x}",
                    Sha256::digest(
                        serde_json::to_vec(&(&source, &uid, &rid, &kind, &local, &remote, &etag, &remote_id, remote_deleted))
                            .map_err(error)?
                    )
                );
                Ok(SyncIssue { source, uid, rid, kind, source_label, local, remote, remote_deleted, revision })
            })
            .collect()
    }
    fn issue_local(&self, source: &str, uid: &str, rid: &str, kind: &str) -> Result<String, Error> {
        let db = self.db.borrow();
        let mut q = db
            .prepare("SELECT rid,payload,deleted FROM objects WHERE source=?1 AND uid=?2 AND (?3 OR rid=?4) ORDER BY rid")
            .map_err(error)?;
        let rows: Vec<(String, String, bool)> = q
            .query_map(params![source, uid, kind == "dav" || rid == "*", rid], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .map_err(error)?
            .collect::<Result<_, _>>()
            .map_err(error)?;
        serde_json::to_string_pretty(&rows).map_err(error)
    }
    /// Fails if either version changed since the dialog was displayed. Network writes happen later in the owning application.
    pub fn resolve_sync_issue(&self, issue: &SyncIssue, resolution: Resolution) -> Result<(), Error> {
        let _guard = self.sync_lock()?;
        self.db.borrow().execute_batch("SAVEPOINT resolve_issue").map_err(error)?;
        let result = (|| {
            let current = self
                .sync_issues()?
                .into_iter()
                .find(|i| i.source == issue.source && i.uid == issue.uid && i.rid == issue.rid)
                .ok_or_else(|| error("This issue has already been resolved"))?;
            if current.revision != issue.revision {
                return Err(error("The item changed. Refresh the issue before deciding."));
            }
            if matches!(resolution, Resolution::RetryCreation) && issue.kind != "uncertain" {
                return Err(error("This item is not an uncertain creation"));
            }
            if matches!(resolution, Resolution::KeepLocal) && (issue.kind == "uncertain" || issue.remote_deleted) {
                return Err(error("Keep a local copy instead of overwriting a removed or uncertain item"));
            }
            self.db
                .borrow()
                .execute(
                    "INSERT INTO resolution_history(source,uid,rid,decision,local,remote) VALUES(?1,?2,?3,?4,?5,?6)",
                    params![issue.source, issue.uid, issue.rid, format!("{resolution:?}"), issue.local, issue.remote],
                )
                .map_err(error)?;
            let source = self
                .source(&issue.source, Kind::Calendar)
                .or_else(|| self.source(&issue.source, Kind::TaskList))
                .ok_or_else(|| error("Source no longer exists"))?;
            if matches!(resolution, Resolution::KeepLocal) && !source.writable {
                return Err(error("This source is read-only; keep an offline copy instead"));
            }
            let whole = issue.kind == "dav" || issue.rid == "*";
            if matches!(resolution, Resolution::KeepOffline) {
                let local = Source {
                    uid: format!("offline:{}", uuid::Uuid::new_v4()),
                    name: format!("{} · Local copy", source.label()),
                    kind: source.kind,
                    color: source.color.clone(),
                    selected: true,
                    backend: "native".into(),
                    account: None,
                    group: None,
                    local: true,
                    writable: true,
                };
                self.add_source(&local)?;
                self.db.borrow().execute("INSERT OR IGNORE INTO timezones(source,tzid,payload) SELECT ?1,tzid,payload FROM timezones WHERE source=?2",params![local.uid,issue.source]).map_err(error)?;
                self.db
                    .borrow()
                    .execute(
                        "INSERT OR IGNORE INTO raw_imports(source,payload) SELECT ?1,payload FROM raw_imports WHERE source=?2",
                        params![local.uid, issue.source],
                    )
                    .map_err(error)?;
                let calendar = Calendar::open(self, &local.uid, local.kind, None)?;
                let rows: Vec<(String, String, bool)> = serde_json::from_str(&issue.local).map_err(error)?;
                for (_, payload, deleted) in rows {
                    if !deleted {
                        calendar.import_all(&payload)?;
                    }
                }
            }
            if matches!(resolution, Resolution::KeepLocal) {
                let (etag, id): (String, String) = self
                    .db
                    .borrow()
                    .query_row(
                        "SELECT etag,remote_id FROM sync_issues WHERE source=?1 AND uid=?2 AND rid=?3",
                        params![issue.source, issue.uid, issue.rid],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .map_err(error)?;
                if etag.is_empty() {
                    return Err(error("The provider did not supply a safe revision"));
                }
                if issue.kind == "dav" {
                    self.db.borrow().execute("INSERT INTO remote_resources VALUES(?1,?2,?3,?4) ON CONFLICT(source,uid) DO UPDATE SET href=excluded.href,etag=excluded.etag",params![issue.source,issue.uid,id,etag]).map_err(error)?;
                } else {
                    self.save_api_resource(&issue.source, &issue.uid, &issue.rid, &id, &etag)?;
                    self.db.borrow().execute("INSERT INTO api_baselines VALUES(?1,?2,?3,?4) ON CONFLICT(source,uid,rid) DO UPDATE SET payload=excluded.payload",params![issue.source,issue.uid,issue.rid,issue.remote]).map_err(error)?;
                }
            } else if !matches!(resolution, Resolution::RetryCreation) {
                if issue.kind == "uncertain" && !matches!(resolution, Resolution::KeepOffline) {
                    return Err(error("Check the provider, then retry explicitly or keep an offline copy"));
                }
                self.db
                    .borrow()
                    .execute(
                        "DELETE FROM objects WHERE source=?1 AND uid=?2 AND (?3 OR rid=?4)",
                        params![issue.source, issue.uid, whole, issue.rid],
                    )
                    .map_err(error)?;
                if !issue.remote_deleted && !issue.remote.is_empty() {
                    Calendar::open(self, &issue.source, source.kind, None)?.import_all(&issue.remote)?;
                    if issue.kind != "dav" {
                        let (etag, id): (String, String) = self
                            .db
                            .borrow()
                            .query_row(
                                "SELECT etag,remote_id FROM sync_issues WHERE source=?1 AND uid=?2 AND rid=?3",
                                params![issue.source, issue.uid, issue.rid],
                                |r| Ok((r.get(0)?, r.get(1)?)),
                            )
                            .map_err(error)?;
                        self.save_api_resource(&issue.source, &issue.uid, &issue.rid, &id, &etag)?;
                        self.db.borrow().execute("INSERT INTO api_baselines VALUES(?1,?2,?3,?4) ON CONFLICT(source,uid,rid) DO UPDATE SET payload=excluded.payload",params![issue.source,issue.uid,issue.rid,issue.remote]).map_err(error)?;
                    }
                }
                if issue.remote_deleted || issue.kind == "uncertain" {
                    self.db
                        .borrow()
                        .execute(
                            "DELETE FROM api_resources WHERE source=?1 AND uid=?2 AND (?3 OR rid=?4)",
                            params![issue.source, issue.uid, whole, issue.rid],
                        )
                        .map_err(error)?;
                    self.db
                        .borrow()
                        .execute(
                            "DELETE FROM api_baselines WHERE source=?1 AND uid=?2 AND (?3 OR rid=?4)",
                            params![issue.source, issue.uid, whole, issue.rid],
                        )
                        .map_err(error)?;
                    if issue.kind == "dav" {
                        self.db
                            .borrow()
                            .execute("DELETE FROM remote_resources WHERE source=?1 AND uid=?2", params![issue.source, issue.uid])
                            .map_err(error)?;
                    }
                }
            }
            self.db
                .borrow()
                .execute(
                    "DELETE FROM sync_issues WHERE source=?1 AND uid=?2 AND rid=?3",
                    params![issue.source, issue.uid, issue.rid],
                )
                .map_err(error)?;
            self.db
                .borrow()
                .execute("DELETE FROM remote_conflicts WHERE source=?1 AND uid=?2", params![issue.source, issue.uid])
                .map_err(error)?;
            self.db
                .borrow()
                .execute("DELETE FROM uncertain_creates WHERE source=?1 AND uid=?2", params![issue.source, issue.uid])
                .map_err(error)?;
            Ok(())
        })();
        if result.is_ok() {
            self.db.borrow().execute_batch("RELEASE resolve_issue").map_err(error)?;
        } else {
            let _ = self.db.borrow().execute_batch("ROLLBACK TO resolve_issue; RELEASE resolve_issue");
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn setup() -> (Registry, Calendar) {
        let path = std::env::temp_dir().join(format!("nyx-resolve-{}", uuid::Uuid::new_v4())).join("pim.sqlite3");
        let registry = Registry::open(path).unwrap();
        let calendar = Calendar::open(&registry, "system-calendar", Kind::Calendar, None).unwrap();
        calendar.import("BEGIN:VEVENT\r\nUID:item\r\nDTSTART:20261030T120000Z\r\nSUMMARY:Local\r\nEND:VEVENT\r\n").unwrap();
        registry.db.borrow().execute("UPDATE objects SET dirty=1", []).unwrap();
        registry
            .record_issue(
                "system-calendar",
                "item",
                "",
                "api",
                "BEGIN:VEVENT\r\nUID:item\r\nDTSTART:20261030T120000Z\r\nSUMMARY:Remote\r\nEND:VEVENT\r\n",
                "revision-2",
                "remote-id",
                false,
            )
            .unwrap();
        (registry, calendar)
    }
    #[test]
    fn keep_local_rebases_revision_and_archives_both_versions() {
        let (r, c) = setup();
        let issue = r.sync_issues().unwrap().remove(0);
        r.resolve_sync_issue(&issue, Resolution::KeepLocal).unwrap();
        assert_eq!(c.get("item", None).unwrap().summary, "Local");
        assert_eq!(r.api_resource("system-calendar", "item", "").unwrap(), ("remote-id".into(), "revision-2".into()));
        assert!(r.sync_issues().unwrap().is_empty());
        assert_eq!(
            r.db.borrow()
                .query_row(
                    "SELECT count(*) FROM resolution_history WHERE local LIKE '%Local%' AND remote LIKE '%Remote%'",
                    [],
                    |x| x.get::<_, i64>(0)
                )
                .unwrap(),
            1
        );
    }
    #[test]
    fn remote_choice_and_stale_dialog_are_safe() {
        let (r, c) = setup();
        let issue = r.sync_issues().unwrap().remove(0);
        r.db.borrow().execute("UPDATE objects SET payload=replace(payload,'Local','New local')", []).unwrap();
        assert!(r.resolve_sync_issue(&issue, Resolution::UseRemote).is_err());
        let current = r.sync_issues().unwrap().remove(0);
        r.resolve_sync_issue(&current, Resolution::UseRemote).unwrap();
        assert_eq!(c.get("item", None).unwrap().summary, "Remote");
        assert_eq!(r.db.borrow().query_row("SELECT count(*) FROM objects WHERE dirty=1", [], |x| x.get::<_, i64>(0)).unwrap(), 0);
    }
    #[test]
    fn uncertain_creation_requires_explicit_retry_or_offline_copy() {
        let (r, _) = setup();
        r.db.borrow().execute("DELETE FROM sync_issues", []).unwrap();
        r.db.borrow().execute("INSERT INTO uncertain_creates VALUES('system-calendar','item')", []).unwrap();
        let issue = r.sync_issues().unwrap().remove(0);
        assert!(r.resolve_sync_issue(&issue, Resolution::KeepLocal).is_err());
        r.resolve_sync_issue(&issue, Resolution::KeepOffline).unwrap();
        assert!(r.sync_issues().unwrap().is_empty());
        let source = r.all_sources(Kind::Calendar).into_iter().find(|(s, _)| s.uid.starts_with("offline:")).unwrap().0;
        assert_eq!(Calendar::open(&r, &source.uid, source.kind, None).unwrap().get("item", None).unwrap().summary, "Local");
        assert_eq!(
            r.db.borrow()
                .query_row("SELECT count(*) FROM objects WHERE source='system-calendar'", [], |x| x.get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    #[test]
    fn other_connections_cannot_resolve_while_syncing() {
        let (r, _) = setup();
        let path = std::path::PathBuf::from(r.db.borrow().path().unwrap());
        let other = Registry::open(path).unwrap();
        let guard = r.sync_lock().unwrap();
        assert!(other.resolve_sync_issue(&other.sync_issues().unwrap()[0], Resolution::UseRemote).is_err());
        drop(guard);
        other.resolve_sync_issue(&other.sync_issues().unwrap()[0], Resolution::UseRemote).unwrap();
    }
    #[test]
    fn dav_offline_choice_keeps_local_and_imports_remote() {
        let (r, c) = setup();
        r.db.borrow().execute("DELETE FROM sync_issues", []).unwrap();
        r.record_issue("system-calendar","item","*","dav","BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:item\r\nDTSTART:20261030T120000Z\r\nSUMMARY:Remote DAV\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n","dav-2","/calendar/item.ics",false).unwrap();
        let issue = r.sync_issues().unwrap().remove(0);
        r.resolve_sync_issue(&issue, Resolution::KeepOffline).unwrap();
        assert_eq!(c.get("item", None).unwrap().summary, "Remote DAV");
        let offline = r.all_sources(Kind::Calendar).into_iter().find(|(s, _)| s.uid.starts_with("offline:")).unwrap().0;
        assert_eq!(Calendar::open(&r, &offline.uid, offline.kind, None).unwrap().get("item", None).unwrap().summary, "Local");
    }
    #[test]
    fn explicit_retry_unblocks_only_the_selected_creation() {
        let (r, _) = setup();
        r.db.borrow().execute("DELETE FROM sync_issues", []).unwrap();
        r.db.borrow().execute("INSERT INTO uncertain_creates VALUES('system-calendar','item')", []).unwrap();
        r.db.borrow().execute("INSERT INTO uncertain_creates VALUES('system-calendar','another')", []).unwrap();
        let issue = r.sync_issues().unwrap().into_iter().find(|i| i.uid == "item").unwrap();
        r.resolve_sync_issue(&issue, Resolution::RetryCreation).unwrap();
        assert_eq!(r.sync_issues().unwrap().len(), 1);
        assert_eq!(r.sync_issues().unwrap()[0].uid, "another");
        assert_eq!(
            r.db.borrow().query_row("SELECT dirty FROM objects WHERE uid='item'", [], |row| row.get::<_, bool>(0)).unwrap(),
            true
        );
    }
    #[test]
    fn accepting_cancelled_instance_does_not_restore_the_occurrence() {
        let (r, c) = setup();
        r.db.borrow().execute("DELETE FROM sync_issues", []).unwrap();
        r.db.borrow().execute("UPDATE objects SET dirty=0", []).unwrap();
        c.import("BEGIN:VEVENT\r\nUID:item\r\nDTSTART:20261030T120000Z\r\nRRULE:FREQ=DAILY;COUNT=3\r\nSUMMARY:Series\r\nEND:VEVENT\r\n").unwrap();
        let cancelled = "BEGIN:VEVENT\r\nUID:item\r\nRECURRENCE-ID:20261031T120000Z\r\nDTSTART:20261031T120000Z\r\nSTATUS:CANCELLED\r\nEND:VEVENT\r\n";
        r.record_issue("system-calendar", "item", "20261031T120000Z", "api", cancelled, "rev-3", "instance", false).unwrap();
        let issue = r.sync_issues().unwrap().remove(0);
        r.resolve_sync_issue(&issue, Resolution::UseRemote).unwrap();
        let from = chrono::DateTime::parse_from_rfc3339("2026-10-30T00:00:00Z").unwrap().timestamp();
        assert_eq!(c.instances(from, from + 3 * 86400).len(), 2);
    }
}
