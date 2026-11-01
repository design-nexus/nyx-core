//! Application-owned synchronization; never runs inside the accounts service.
use crate::registry::error;
use crate::*;
use nyx_accounts_native::{Client, model::Service, remote::Remote};
impl Registry {
    /// Explicit refresh from an application worker, with the application's service grant.
    pub fn refresh_accounts(&self, service: Service) -> Result<(), Error> {
        let _guard = self.sync_lock()?;
        let client = Client::connect().map_err(error)?;
        let mut failures = Vec::new();
        for account in client.list().map_err(error)?.into_iter().filter(|a| a.enabled.contains(&service)) {
            if let Err(failure) = self.refresh_account(&client, &account, service.clone()) {
                let _ = self
                    .db
                    .borrow()
                    .execute("UPDATE sources SET healthy=0 WHERE json_extract(data,'$.account')=?1", [&account.id]);
                failures.push(format!("{}: {failure}", account.label()));
            }
        }
        if !failures.is_empty() {
            return Err(error(failures.join("\n")));
        }
        Ok(())
    }
    fn refresh_account(&self, client: &Client, account: &nyx_accounts_native::Account, service: Service) -> Result<(), Error> {
        if matches!(account.provider.as_str(), "google" | "microsoft_personal") {
            let secret = client.credential(&account.id, service.clone()).map_err(error)?;
            let remote = Remote::new(account.clone(), secret).map_err(error)?;
            return self.sync_api(&remote, service.clone());
        }
        let contacts = service == Service::Contacts;
        let kind = if service == Service::Tasks { Kind::TaskList } else { Kind::Calendar };
        let key = if contacts { "carddav" } else { "caldav" };
        let endpoint = account
            .endpoints
            .get(key)
            .cloned()
            .ok_or_else(|| error(format!("Configure the {key} endpoint in Online Accounts")))?;
        let secret = client.credential(&account.id, service.clone()).map_err(error)?;
        let remote = Remote::new(account.clone(), secret).map_err(error)?;
        let collections = remote.dav_collections(&endpoint, contacts).map_err(error)?;
        for collection in collections {
            if !contacts
                && !collection.components.is_empty()
                && !collection.components.iter().any(|c| c == if kind == Kind::Calendar { "VEVENT" } else { "VTODO" })
            {
                continue;
            }

            let source_id = format!("{}:{}:{}", account.id, key, collection.url);
            let objects = remote.dav_list(&collection.url, contacts).map_err(error)?;
            if contacts {
                contacts::initialize(self)?;
                self.db
                    .borrow()
                    .execute(
                        "INSERT INTO books(uid,name,account) VALUES(?1,?2,?3) ON CONFLICT(uid) DO UPDATE SET name=excluded.name",
                        rusqlite::params![source_id, collection.name, account.id],
                    )
                    .map_err(error)?;
                let mut seen = std::collections::HashSet::new();
                for object in objects {
                    for (uid, _) in contacts::parse_cards(&object.payload)? {
                        seen.insert(uid);
                    }
                    contacts::import_vcards(self, &source_id, &collection.name, &object.payload)?;
                }
                contacts::reconcile(self, &source_id, &seen)?;
            } else {
                let source_id = format!("{source_id}:{kind:?}");
                self.add_source(&Source {
                    uid: source_id.clone(),
                    name: collection.name,
                    kind,
                    color: None,
                    selected: true,
                    backend: "dav".into(),
                    account: Some(account.id.clone()),
                    group: Some(account.label().to_owned()),
                    local: false,
                    writable: true,
                })?;
                self.set_healthy(&source_id, false);
                self.dav_merge(&source_id, kind, &remote, &collection.url, objects)?;
                self.set_healthy(&source_id, true);
            }
        }
        Ok(())
    }
}
impl Registry {
    fn dav_merge(
        &self,
        source: &str,
        kind: Kind,
        remote: &Remote,
        endpoint: &str,
        objects: Vec<nyx_accounts_native::remote::DavObject>,
    ) -> Result<(), Error> {
        self.db.borrow().execute_batch("CREATE TABLE IF NOT EXISTS remote_resources(source TEXT,uid TEXT,href TEXT,etag TEXT,PRIMARY KEY(source,uid)); CREATE TABLE IF NOT EXISTS remote_conflicts(source TEXT,uid TEXT,payload TEXT,etag TEXT,PRIMARY KEY(source,uid));").map_err(error)?;
        let calendar = Calendar::open(self, source, kind, None)?;
        let mut seen = std::collections::HashSet::new();
        for object in objects {
            let ids = calendar.import_all(&object.payload)?;
            for uid in ids {
                seen.insert(uid.clone());
                let dirty: bool = self
                    .db
                    .borrow()
                    .query_row(
                        "SELECT EXISTS(SELECT 1 FROM objects WHERE source=?1 AND uid=?2 AND dirty=1)",
                        rusqlite::params![source, uid],
                        |r| r.get(0),
                    )
                    .map_err(error)?;
                let previous: Option<String> = self
                    .db
                    .borrow()
                    .query_row(
                        "SELECT etag FROM remote_resources WHERE source=?1 AND uid=?2",
                        rusqlite::params![source, uid],
                        |r| r.get(0),
                    )
                    .ok();
                if dirty && previous.as_deref() != object.etag.as_deref() {
                    self.record_issue(
                        source,
                        &uid,
                        "*",
                        "dav",
                        &object.payload,
                        object.etag.as_deref().unwrap_or(""),
                        &object.href,
                        false,
                    )?;
                    return Err(error(
                        "An online item changed while you edited it. Both versions are retained; resolve the conflict before uploading.",
                    ));
                }
                self.db.borrow().execute("INSERT INTO remote_resources VALUES(?1,?2,?3,?4) ON CONFLICT(source,uid) DO UPDATE SET href=excluded.href,etag=excluded.etag",rusqlite::params![source,uid,object.href,object.etag]).map_err(error)?;
            }
        }
        let known: Vec<(String, String, Option<String>)> = {
            let db = self.db.borrow();
            let mut q = db.prepare("SELECT uid,href,etag FROM remote_resources WHERE source=?1").map_err(error)?;
            q.query_map([source], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).map_err(error)?.filter_map(Result::ok).collect()
        };
        for (uid, _, _) in &known {
            if !seen.contains(uid) {
                let dirty: bool = self
                    .db
                    .borrow()
                    .query_row(
                        "SELECT EXISTS(SELECT 1 FROM objects WHERE source=?1 AND uid=?2 AND dirty=1)",
                        rusqlite::params![source, uid],
                        |r| r.get(0),
                    )
                    .map_err(error)?;
                if dirty {
                    self.record_issue(source, uid, "*", "dav", "", "", "", true)?;
                    return Err(error("An edited item was removed remotely. Your local edit is retained."));
                }
                self.db
                    .borrow()
                    .execute("UPDATE objects SET deleted=1 WHERE source=?1 AND uid=?2", rusqlite::params![source, uid])
                    .map_err(error)?;
            }
        }
        let pending: Vec<String> = {
            let db = self.db.borrow();
            let mut q = db.prepare("SELECT DISTINCT uid FROM objects WHERE source=?1 AND dirty=1").map_err(error)?;
            q.query_map([source], |r| r.get(0)).map_err(error)?.filter_map(Result::ok).collect()
        };
        for uid in pending {
            let existing = known.iter().find(|(id, _, _)| id == &uid);
            let snapshot: Vec<(String, String, bool)> = {
                let db = self.db.borrow();
                let mut query = db
                    .prepare("SELECT rid,payload,deleted FROM objects WHERE source=?1 AND uid=?2 ORDER BY rid")
                    .map_err(error)?;
                query
                    .query_map(rusqlite::params![source, uid], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                    .map_err(error)?
                    .collect::<Result<_, _>>()
                    .map_err(error)?
            };
            if snapshot.iter().all(|(_, _, deleted)| *deleted) {
                if let Some((_, href, etag)) = existing {
                    remote.dav_delete(endpoint, href, etag.as_deref().unwrap_or("")).map_err(error)?;
                }
            } else {
                let payloads: Vec<String> =
                    snapshot.iter().filter(|(_, _, deleted)| !*deleted).map(|(_, payload, _)| payload.clone()).collect();
                let mut payload =
                    String::from("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Nyx//Online Accounts Preview//EN\r\n");
                let originals: Vec<String> = {
                    let db = self.db.borrow();
                    let mut q = db.prepare("SELECT payload FROM raw_imports WHERE source=?1").map_err(error)?;
                    q.query_map([source], |r| r.get(0)).map_err(error)?.filter_map(Result::ok).collect()
                };
                let mut zones = std::collections::BTreeSet::new();
                for original in originals {
                    let mut remainder = original.as_str();
                    while let Some(start) = remainder.find("BEGIN:VTIMEZONE") {
                        remainder = &remainder[start..];
                        let Some(end) = remainder.find("END:VTIMEZONE") else { break };
                        zones.insert(remainder[..end + 13].to_owned());
                        remainder = &remainder[end + 13..];
                    }
                }
                for zone in zones {
                    payload.push_str(&zone);
                    payload.push_str("\r\n");
                }
                for component in payloads {
                    payload.push_str(&component);
                }
                payload.push_str("END:VCALENDAR\r\n");
                let href = existing.map(|(_, href, _)| href.clone()).unwrap_or_else(|| format!("{}.ics", uuid::Uuid::new_v4()));
                let etag = if let Some((_, _, etag)) = existing {
                    Some(
                        etag.as_deref()
                            .filter(|s| !s.is_empty())
                            .ok_or_else(|| error("Provider did not supply a revision; safe update refused"))?,
                    )
                } else {
                    None
                };
                let revision = remote.dav_put(endpoint, &href, &payload, etag).map_err(error)?;
                self.db.borrow().execute("INSERT INTO remote_resources VALUES(?1,?2,?3,?4) ON CONFLICT(source,uid) DO UPDATE SET href=excluded.href,etag=excluded.etag",rusqlite::params![source,uid,href,revision]).map_err(error)?;
            }
            for (rid, payload, deleted) in snapshot {
                self.db
                    .borrow()
                    .execute(
                        "UPDATE objects SET dirty=0 WHERE source=?1 AND uid=?2 AND rid=?3 AND payload=?4 AND deleted=?5",
                        rusqlite::params![source, uid, rid, payload, deleted],
                    )
                    .map_err(error)?;
            }
        }
        Ok(())
    }
}
