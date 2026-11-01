//! User-requested recovery always creates an offline copy, never a provider write.
use crate::{Calendar, Error, Kind, Registry, Source, contacts, registry::error};
use rusqlite::params;
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HistorySummary {
    pub key: String,
    pub created: String,
    pub source: String,
    pub title: String,
}
#[derive(Clone, Debug)]
pub struct HistoryDetail {
    pub summary: HistorySummary,
    pub local: String,
    pub remote: String,
}
#[derive(Clone, Copy)]
pub enum HistoryVersion {
    Local,
    Provider,
}
impl Registry {
    pub fn history(&self) -> Result<Vec<HistorySummary>, Error> {
        self.issues_schema()?;
        let rows: Vec<(String, String, String, String)> = {
            let db = self.db.borrow();
            let mut q=db.prepare("SELECT 'resolution:'||id AS key,created,source,decision FROM resolution_history UNION ALL SELECT 'contact:'||id,created,book,'Previous contact version' FROM contact_history ORDER BY created DESC,key DESC LIMIT 200").map_err(error)?;
            q.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
                .map_err(error)?
                .collect::<Result<_, _>>()
                .map_err(error)?
        };
        Ok(rows
            .into_iter()
            .map(|(key, created, source, title)| HistorySummary {
                key,
                created,
                source: self.history_source_label(&source),
                title: match title.as_str() {
                    "KeepLocal" => "Kept local changes".into(),
                    "UseRemote" => "Accepted provider version".into(),
                    "KeepOffline" => "Preserved an offline copy".into(),
                    "RetryCreation" => "Confirmed a creation retry".into(),
                    _ => title,
                },
            })
            .collect())
    }
    fn history_source_label(&self, source: &str) -> String {
        self.source(source, Kind::Calendar)
            .or_else(|| self.source(source, Kind::TaskList))
            .map(|s| s.label().to_owned())
            .or_else(|| self.db.borrow().query_row("SELECT name FROM books WHERE uid=?1", [source], |r| r.get(0)).ok())
            .unwrap_or_else(|| source.into())
    }
    fn history_raw(&self, key: &str) -> Result<(String, String, String, bool, String, String), Error> {
        self.issues_schema()?;
        let (kind, id) = key.split_once(':').ok_or_else(|| error("Invalid history entry"))?;
        let id = id.parse::<i64>().map_err(|_| error("Invalid history entry"))?;
        match kind {
            "resolution" => self
                .db
                .borrow()
                .query_row("SELECT source,local,remote,1,created,decision FROM resolution_history WHERE id=?1", [id], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?))
                })
                .map_err(error),
            "contact" => self
                .db
                .borrow()
                .query_row(
                    "SELECT book,payload,'',0,created,'Previous contact version' FROM contact_history WHERE id=?1",
                    [id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
                )
                .map_err(error),
            _ => Err(error("Unknown history entry type")),
        }
    }
    pub fn history_detail(&self, key: &str) -> Result<HistoryDetail, Error> {
        let (source, local, remote, encoded, created, title) = self.history_raw(key)?;
        let local = if encoded {
            serde_json::from_str::<Vec<(String, String, bool)>>(&local)
                .map_err(error)?
                .into_iter()
                .map(|(_, payload, deleted)| format!("{}{}", if deleted { "Deletion was queued\n" } else { "" }, payload))
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            local
        };
        Ok(HistoryDetail {
            summary: HistorySummary { key: key.into(), created, source: self.history_source_label(&source), title },
            local,
            remote,
        })
    }
    pub fn recover_history(&self, key: &str, version: HistoryVersion) -> Result<String, Error> {
        let _guard = self.sync_lock()?;
        let (source, local, remote, encoded, _, _) = self.history_raw(key)?;
        let payloads = match version {
            HistoryVersion::Provider => vec![remote],
            HistoryVersion::Local => {
                if encoded {
                    serde_json::from_str::<Vec<(String, String, bool)>>(&local)
                        .map_err(error)?
                        .into_iter()
                        .map(|(_, p, _)| p)
                        .collect()
                } else {
                    vec![local]
                }
            }
        };
        let payloads: Vec<_> = payloads.into_iter().filter(|p| !p.trim().is_empty()).collect();
        if payloads.is_empty() {
            return Err(error("This archive has no recoverable version"));
        }
        let name = format!("Recovered · {}", self.history_source_label(&source));
        let uid = format!("offline:{}", uuid::Uuid::new_v4());
        self.db.borrow().execute_batch("SAVEPOINT recover_history").map_err(error)?;
        let result = (|| {
            let card = payloads[0].trim_start().starts_with("BEGIN:VCARD");
            if card {
                for payload in payloads {
                    if contacts::import_vcards(self, &uid, &name, &payload)? == 0 {
                        return Err(error("The archive contains no recoverable contacts"));
                    }
                }
            } else {
                let kind = self
                    .source(&source, Kind::Calendar)
                    .or_else(|| self.source(&source, Kind::TaskList))
                    .map(|s| s.kind)
                    .unwrap_or_else(|| if payloads[0].contains("BEGIN:VTODO") { Kind::TaskList } else { Kind::Calendar });
                self.add_source(&Source {
                    uid: uid.clone(),
                    name: name.clone(),
                    kind,
                    color: None,
                    selected: true,
                    backend: "native".into(),
                    account: None,
                    group: None,
                    local: true,
                    writable: true,
                })?;
                self.db
                    .borrow()
                    .execute(
                        "INSERT INTO timezones(source,tzid,payload) SELECT ?1,tzid,payload FROM timezones WHERE source=?2",
                        params![uid, source],
                    )
                    .map_err(error)?;
                let calendar = Calendar::open(self, &uid, kind, None)?;
                for payload in payloads {
                    if calendar.import_all(&payload)?.is_empty() {
                        return Err(error("The archive contains no recoverable calendar items"));
                    }
                }
            }
            Ok(uid)
        })();
        if result.is_ok() {
            self.db.borrow().execute_batch("RELEASE recover_history").map_err(error)?;
        } else {
            let _ = self.db.borrow().execute_batch("ROLLBACK TO recover_history; RELEASE recover_history");
        }
        result
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn archive_recovery_creates_offline_copy_and_keeps_original() {
        let r = Registry::open(std::env::temp_dir().join(format!("nyx-history-{}", uuid::Uuid::new_v4())).join("pim.sqlite3"))
            .unwrap();
        r.issues_schema().unwrap();
        let payload = "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:person\r\nFN:Archived\r\nEND:VCARD\r\n";
        contacts::import_vcards(&r, "original", "Original", payload).unwrap();
        r.db.borrow()
            .execute("INSERT INTO contact_history(book,uid,payload,deleted) VALUES('original','person',?1,0)", [payload])
            .unwrap();
        let entry = r.history().unwrap().remove(0);
        assert!(r.history_detail(&entry.key).unwrap().local.contains("Archived"));
        let id = r.recover_history(&entry.key, HistoryVersion::Local).unwrap();
        assert!(id.starts_with("offline:"));
        assert_eq!(
            r.db.borrow().query_row("SELECT count(*) FROM contacts WHERE deleted=0", [], |row| row.get::<_, i64>(0)).unwrap(),
            2
        );
        assert!(r.recover_history(&entry.key, HistoryVersion::Provider).is_err());
    }
}
