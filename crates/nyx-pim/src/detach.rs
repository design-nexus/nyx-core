//! Preserve visible offline PIM copies before Settings disconnects an account.
use crate::{Calendar, Error, Kind, Registry, Source, contacts, registry::error};
impl Registry {
    /// Idempotent; copies only this preview's cache, never requests credentials or writes to the provider.
    pub fn preserve_account_cache(&self, account: &str) -> Result<usize, Error> {
        let _guard = self.sync_lock()?;
        contacts::initialize(self)?;
        self.db.borrow().execute_batch("SAVEPOINT detach_account").map_err(error)?;
        let result = (|| {
            let mut copied = 0;
            for kind in [Kind::Calendar, Kind::TaskList] {
                for (source, _) in self.all_sources(kind).into_iter().filter(|(s, _)| s.account.as_deref() == Some(account)) {
                    let uid = format!("detached:{}", source.uid);
                    if self.source(&uid, kind).is_none() {
                        self.add_source(&Source {
                            uid: uid.clone(),
                            name: format!("{} · Disconnected", source.label()),
                            kind,
                            color: source.color.clone(),
                            selected: source.selected,
                            backend: "native".into(),
                            account: None,
                            group: Some("Disconnected accounts".into()),
                            local: true,
                            writable: true,
                        })?;
                    }
                    let original = Calendar::open(self, &source.uid, kind, None)?;
                    let target = Calendar::open(self, &uid, kind, None)?;
                    let ids: Vec<String> = {
                        let db = self.db.borrow();
                        let mut q =
                            db.prepare("SELECT DISTINCT uid FROM objects WHERE source=?1 AND deleted=0").map_err(error)?;
                        q.query_map([&source.uid], |r| r.get(0)).map_err(error)?.collect::<Result<_, _>>().map_err(error)?
                    };
                    for id in ids {
                        target.import_all(&original.export(&id)?)?;
                        copied += 1;
                    }
                }
            }
            let books: Vec<(String, String)> = {
                let db = self.db.borrow();
                let mut q = db.prepare("SELECT uid,name FROM books WHERE account=?1").map_err(error)?;
                q.query_map([account], |r| Ok((r.get(0)?, r.get(1)?))).map_err(error)?.collect::<Result<_, _>>().map_err(error)?
            };
            for (book, name) in books {
                let cards: Vec<String> = {
                    let db = self.db.borrow();
                    let mut q = db.prepare("SELECT payload FROM contacts WHERE book=?1 AND deleted=0").map_err(error)?;
                    q.query_map([&book], |r| r.get(0)).map_err(error)?.collect::<Result<_, _>>().map_err(error)?
                };
                for card in cards {
                    copied +=
                        contacts::import_vcards(self, &format!("detached:{book}"), &format!("{name} · Disconnected"), &card)?;
                }
            }
            Ok(copied)
        })();
        if result.is_ok() {
            self.db.borrow().execute_batch("RELEASE detach_account").map_err(error)?;
        } else {
            let _ = self.db.borrow().execute_batch("ROLLBACK TO detach_account; RELEASE detach_account");
        }
        result
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn disconnect_preserves_pending_new_items_and_does_not_overwrite_edited_copies() {
        let r = Registry::open(std::env::temp_dir().join(format!("nyx-detach-{}", uuid::Uuid::new_v4())).join("pim.sqlite3"))
            .unwrap();
        r.add_source(&Source {
            uid: "remote".into(),
            name: "Calendar".into(),
            kind: Kind::Calendar,
            color: Some("#123456".into()),
            selected: true,
            backend: "google".into(),
            account: Some("account".into()),
            group: None,
            local: false,
            writable: true,
        })
        .unwrap();
        let calendar = Calendar::open(&r, "remote", Kind::Calendar, None).unwrap();
        calendar.import("BEGIN:VEVENT\r\nUID:new\r\nDTSTART:20261030T120000Z\r\nSUMMARY:Unsynced\r\nEND:VEVENT\r\n").unwrap();
        r.db.borrow().execute("UPDATE objects SET dirty=1 WHERE source='remote'", []).unwrap();
        r.configure_book("book", "People", "account", "dav", "https://dav.example.test/book/").unwrap();
        contacts::import_vcards(
            &r,
            "book",
            "People",
            "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:new\r\nFN:Unsynced contact\r\nEND:VCARD\r\n",
        )
        .unwrap();
        r.db.borrow().execute("UPDATE contacts SET dirty=1", []).unwrap();
        assert_eq!(r.preserve_account_cache("account").unwrap(), 2);
        assert!(r.source("detached:remote", Kind::Calendar).unwrap().local);
        r.db.borrow().execute("UPDATE contacts SET payload=replace(payload,'Unsynced contact','Edited copy'),dirty=1 WHERE book='detached:book'",[]).unwrap();
        assert_eq!(r.preserve_account_cache("account").unwrap(), 2);
        assert!(
            r.db.borrow()
                .query_row("SELECT payload FROM contacts WHERE book='detached:book'", [], |row| row.get::<_, String>(0))
                .unwrap()
                .contains("Edited copy")
        );
        assert_eq!(
            r.db.borrow()
                .query_row("SELECT count(*) FROM objects WHERE source='remote'", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            1
        );
    }
}
