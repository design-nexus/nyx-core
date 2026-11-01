//! Raw vCards are retained without rewriting fields the UI does not understand.
use crate::{Error, Registry, registry::error};
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AddressBook {
    pub uid: String,
    pub name: String,
    pub backend: String,
    pub enabled: bool,
}
pub fn initialize(registry: &Registry) -> Result<(), Error> {
    registry.db.borrow().execute_batch("CREATE TABLE IF NOT EXISTS books(uid TEXT PRIMARY KEY,name TEXT NOT NULL,account TEXT,enabled INTEGER NOT NULL DEFAULT 1); CREATE TABLE IF NOT EXISTS contacts(book TEXT NOT NULL REFERENCES books(uid),uid TEXT NOT NULL,payload TEXT NOT NULL,etag TEXT,dirty INTEGER DEFAULT 0,deleted INTEGER DEFAULT 0,PRIMARY KEY(book,uid));").map_err(error)
}
pub fn list_address_books() -> Result<Vec<AddressBook>, Error> {
    let registry = Registry::connect()?;
    initialize(&registry)?;
    let books: Vec<(AddressBook, Option<String>)> = {
        let db = registry.db.borrow();
        let mut query = db.prepare("SELECT uid,name,enabled,account FROM books ORDER BY name").map_err(error)?;
        query
            .query_map([], |r| {
                Ok((AddressBook { uid: r.get(0)?, name: r.get(1)?, enabled: r.get(2)?, backend: "native".into() }, r.get(3)?))
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
    let tx = db.transaction().map_err(error)?;
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
                let unfolded = payload.replace("\r\n ", "").replace("\r\n\t", "");
                let uid = unfolded
                    .lines()
                    .filter_map(|s| s.split_once(':'))
                    .find(|(key, _)| key.eq_ignore_ascii_case("UID"))
                    .map(|(_, value)| value.to_owned())
                    .ok_or_else(|| error("A vCard without UID cannot be safely reconciled"))?;
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
