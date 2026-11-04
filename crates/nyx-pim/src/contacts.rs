//! Raw vCards are retained without rewriting fields the UI does not understand.
use crate::{Error, Registry, registry::error};
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AddressBook {
    pub uid: String,
    pub name: String,
    pub backend: String,
    pub enabled: bool,
    pub writable: bool,
}
pub fn initialize(registry: &Registry) -> Result<(), Error> {
    registry.db.borrow().execute_batch("CREATE TABLE IF NOT EXISTS books(uid TEXT PRIMARY KEY,name TEXT NOT NULL,account TEXT,enabled INTEGER NOT NULL DEFAULT 1); CREATE TABLE IF NOT EXISTS contacts(book TEXT NOT NULL REFERENCES books(uid),uid TEXT NOT NULL,payload TEXT NOT NULL,etag TEXT,dirty INTEGER DEFAULT 0,deleted INTEGER DEFAULT 0,PRIMARY KEY(book,uid));
CREATE TABLE IF NOT EXISTS contact_resources(book TEXT,uid TEXT,remote_id TEXT,etag TEXT,PRIMARY KEY(book,uid));
CREATE TABLE IF NOT EXISTS contact_baselines(book TEXT,uid TEXT,payload TEXT,raw TEXT,PRIMARY KEY(book,uid));
CREATE TABLE IF NOT EXISTS contact_remote_versions(book TEXT,uid TEXT,raw TEXT,PRIMARY KEY(book,uid));
CREATE TABLE IF NOT EXISTS contact_history(id INTEGER PRIMARY KEY,created TEXT DEFAULT CURRENT_TIMESTAMP,book TEXT,uid TEXT,payload TEXT,deleted INTEGER);").map_err(error)?;
    let columns: Vec<String> = {
        let db = registry.db.borrow();
        let mut q = db.prepare("PRAGMA table_info(books)").map_err(error)?;
        q.query_map([], |r| r.get(1)).map_err(error)?.collect::<Result<_, _>>().map_err(error)?
    };
    for (name, definition) in
        [("backend", "TEXT NOT NULL DEFAULT 'native'"), ("endpoint", "TEXT"), ("writable", "INTEGER NOT NULL DEFAULT 1")]
    {
        if !columns.iter().any(|column| column == name) {
            registry.db.borrow().execute_batch(&format!("ALTER TABLE books ADD COLUMN {name} {definition}")).map_err(error)?;
        }
    }
    Ok(())
}
pub fn list_address_books() -> Result<Vec<AddressBook>, Error> {
    let registry = Registry::connect()?;
    initialize(&registry)?;
    let books: Vec<(AddressBook, Option<String>)> = {
        let db = registry.db.borrow();
        let mut query = db.prepare("SELECT uid,name,enabled,account,backend,(writable AND (account IS NULL OR backend<>'native')) FROM books ORDER BY name").map_err(error)?;
        query
            .query_map([], |r| {
                Ok((
                    AddressBook { uid: r.get(0)?, name: r.get(1)?, enabled: r.get(2)?, backend: r.get(4)?, writable: r.get(5)? },
                    r.get(3)?,
                ))
            })
            .map_err(error)?
            .map(|r| r.map_err(error))
            .collect::<Result<_, _>>()?
    };
    let accounts = if books.iter().any(|(_, account)| account.is_some()) {
        nyx_accounts_native::Client::connect().and_then(|c| c.list()).ok()
    } else {
        None
    };
    Ok(books
        .into_iter()
        .filter(|(book, account)| {
            book.enabled
                && account.as_ref().is_none_or(|id| {
                    accounts.as_ref().is_none_or(|accounts| {
                        accounts.iter().any(|a| &a.id == id && a.enabled.contains(&nyx_accounts_native::Service::Contacts))
                    })
                })
        })
        .map(|(book, _)| book)
        .collect())
}
pub fn list_vcards(uid: &str) -> Result<Vec<String>, Error> {
    let registry = Registry::connect()?;
    initialize(&registry)?;
    let db = registry.db.borrow();
    let mut query = db.prepare("SELECT payload FROM contacts WHERE book=?1 AND deleted=0 ORDER BY uid").map_err(error)?;
    query.query_map([uid], |r| r.get(0)).map_err(error)?.map(|r| r.map_err(error)).collect()
}
pub fn import_vcards(registry: &Registry, book: &str, name: &str, cards: &str) -> Result<usize, Error> {
    initialize(registry)?;
    let mut db = registry.db.borrow_mut();
    let tx = db.savepoint().map_err(error)?;
    tx.execute("INSERT INTO books(uid,name) VALUES(?1,?2) ON CONFLICT(uid) DO NOTHING", rusqlite::params![book, name])
        .map_err(error)?;
    let parsed = parse_cards(cards)?;
    let count = parsed.len();
    for (uid, payload) in parsed {
        tx.execute("INSERT INTO contacts(book,uid,payload) VALUES(?1,?2,?3) ON CONFLICT(book,uid) DO UPDATE SET payload=excluded.payload,deleted=0 WHERE contacts.dirty=0",rusqlite::params![book,uid,payload]).map_err(error)?;
    }
    tx.commit().map_err(error)?;
    Ok(count)
}
pub(crate) fn parse_cards(text: &str) -> Result<Vec<(String, String)>, Error> {
    let mut current = None;
    let mut result = Vec::new();
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if line.eq_ignore_ascii_case("BEGIN:VCARD") {
            if current.is_some() {
                return Err(error("Nested vCard; import refused"));
            }
            current = Some(vec![line.to_owned()]);
            continue;
        }
        if let Some(lines) = current.as_mut() {
            lines.push(line.to_owned());
            if line.eq_ignore_ascii_case("END:VCARD") {
                let lines = current.take().unwrap();
                let payload = lines.join("\r\n") + "\r\n";
                let uid = crate::contact_sync::uid_of(&payload)?;
                result.push((uid, payload));
            }
        }
    }
    if current.is_some() {
        return Err(error("Incomplete vCard; import refused"));
    }
    if result.is_empty() && !text.trim().is_empty() {
        return Err(error("Invalid vCard payload"));
    }
    Ok(result)
}
impl AddressBook {
    pub fn label(&self) -> &str {
        if self.name.is_empty() { &self.uid } else { &self.name }
    }
}
pub(crate) fn reconcile(registry: &Registry, book: &str, seen: &std::collections::HashSet<String>) -> Result<(), Error> {
    let known: Vec<String> = {
        let db = registry.db.borrow();
        let mut query = db.prepare("SELECT uid FROM contacts WHERE book=?1 AND dirty=0").map_err(error)?;
        query.query_map([book], |r| r.get(0)).map_err(error)?.map(|r| r.map_err(error)).collect::<Result<_, _>>()?
    };
    for uid in known {
        if !seen.contains(&uid) {
            registry
                .db
                .borrow()
                .execute("UPDATE contacts SET deleted=1 WHERE book=?1 AND uid=?2 AND dirty=0", rusqlite::params![book, uid])
                .map_err(error)?;
        }
    }
    Ok(())
}

/// Queue one edited card, rejecting an editor whose raw snapshot is stale.
pub fn save_vcard(book: &str, payload: &str, expected: Option<&str>) -> Result<(), Error> {
    crate::contact_sync::queue_save(&Registry::connect()?, book, payload, expected)
}
/// Queue explicit deletions against the exact cards reviewed by the user.
pub fn delete_vcards(book: &str, expected: &[(String, String)]) -> Result<(), Error> {
    crate::contact_sync::queue_delete(&Registry::connect()?, book, expected)
}

/// Keep raw properties the existing editor did not change.
pub fn merge_editor_changes(raw: &str, before: &str, after: &str) -> Result<String, Error> {
    crate::contact_sync::merge_editor(raw, before, after)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn incoming_contact_updates_keep_unknown_fields() {
        let registry =
            Registry::open(std::env::temp_dir().join(format!("nyx-contact-fixture-{}/pim.db", uuid::Uuid::new_v4()))).unwrap();
        import_vcards(&registry, "fixture", "Fixture", "BEGIN:VCARD\r\nUID:fixture\r\nFN:Before\r\nEND:VCARD\r\n").unwrap();
        import_vcards(
            &registry,
            "fixture",
            "Fixture",
            "begin:vcard\r\nuid:fixture\r\nFN:After\r\nX-CUSTOM:preserved\r\nend:vcard\r\n",
        )
        .unwrap();
        let payload: String =
            registry.db.borrow().query_row("SELECT payload FROM contacts WHERE book='fixture'", [], |r| r.get(0)).unwrap();
        assert!(payload.contains("FN:After"));
        assert!(payload.contains("X-CUSTOM:preserved"));
    }
}
