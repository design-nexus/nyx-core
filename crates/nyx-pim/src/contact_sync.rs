//! Application-owned contact writes. Raw provider data and local edits survive failed conversions.
use crate::{Error, Registry, Resolution, SyncIssue, contacts, registry::error};
use nyx_accounts_native::{
    Service,
    remote::{ApiWriteError, Remote},
};
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use vcard_parser::property::Property;

type Card = BTreeMap<String, Vec<Property>>;
fn parse(payload: &str) -> Result<Card, Error> {
    let mut parser = vcard_parser::VcardParser::new(std::io::Cursor::new(payload));
    let card = parser.next().ok_or_else(|| error("No vCard found"))?.map_err(error)?;
    if parser.next().is_some() {
        return Err(error("Save one contact at a time"));
    }
    let mut map = Card::new();
    for mut property in card.properties {
        property.name = property.name.rsplit('.').next().unwrap_or("").to_ascii_uppercase();
        map.entry(property.name.clone()).or_default().push(property);
    }
    if value(&map, "UID").trim().is_empty() {
        return Err(error("The contact has no stable identity"));
    }
    Ok(map)
}
fn unescape(text: &str) -> String {
    let mut chars = text.chars();
    let mut out = String::new();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            if let Some(ch) = chars.next() {
                out.push(if matches!(ch, 'n' | 'N') { '\n' } else { ch })
            }
        } else {
            out.push(ch)
        }
    }
    out
}
fn parts(text: &str) -> Vec<String> {
    let mut values = vec![String::new()];
    let mut escaped = false;
    for ch in text.chars() {
        if ch == ';' && !escaped {
            values.push(String::new())
        } else {
            values.last_mut().unwrap().push(ch)
        }
        escaped = ch == '\\' && !escaped;
    }
    values.iter().map(|s| unescape(s)).collect()
}
fn properties<'a>(card: &'a Card, key: &str) -> &'a [Property] {
    card.get(key).map(Vec::as_slice).unwrap_or(&[])
}
fn value(card: &Card, key: &str) -> String {
    properties(card, key).first().and_then(|p| p.value.as_deref()).map(unescape).unwrap_or_default()
}
fn label(p: &Property) -> String {
    p.params
        .as_ref()
        .into_iter()
        .flatten()
        .filter(|(k, _)| k.eq_ignore_ascii_case("TYPE"))
        .flat_map(|(_, v)| v)
        .map(|v| v.to_ascii_lowercase())
        .find(|v| !matches!(v.as_str(), "internet" | "voice" | "pref"))
        .unwrap_or_else(|| "other".into())
}
fn same(a: &Card, b: &Card, key: &str) -> bool {
    properties(a, key) == properties(b, key)
}
const GOOGLE_FIELDS: &str = "names,emailAddresses,phoneNumbers,organizations,biographies,birthdays,addresses,urls,metadata";
fn revision(raw: &Value, google: bool) -> String {
    if google {
        // Person etags vary with the requested representation. Only the CONTACT
        // source etag is Google's concurrency revision for contact edits.
        raw["metadata"]["sources"].as_array()
            .and_then(|sources| sources.iter().find(|s| s["type"] == "CONTACT"))
            .and_then(|s| s["etag"].as_str())
            .or_else(|| raw["etag"].as_str()).unwrap_or("").into()
    } else { raw["@odata.etag"].as_str().unwrap_or("").into() }
}

fn typed(raw: &Value, key: &str, item: &str, val: &str) -> Value {
    let mut out =
        raw[key].as_array().and_then(|a| a.iter().find(|v| v[item].as_str() == Some(val))).cloned().unwrap_or_else(|| json!({}));
    if let Some(object) = out.as_object_mut() {
        for key in ["metadata", "formattedType", "canonicalForm", "formattedValue"] {
            object.remove(key);
        }
    }
    out
}
/// Build only supported changed fields. Unsupported edits fail before they can replace provider data.
fn body(payload: &str, baseline: Option<&str>, raw: &Value, google: bool) -> Result<(Value, Vec<String>), Error> {
    let card = parse(payload)?;
    let old = baseline.map(parse).transpose()?.unwrap_or_default();
    let mut out = json!({});
    let mut mask = Vec::new();
    for key in card.keys().chain(old.keys()) {
        if !matches!(
            key.as_str(),
            "UID" | "VERSION" | "FN" | "N" | "EMAIL" | "TEL" | "ADR" | "ORG" | "TITLE" | "BDAY" | "NOTE" | "URL"
        ) && !same(&card, &old, key)
        {
            return Err(error(format!(
                "Editing {key} is not supported by this provider. Keep the original value or use a DAV/local book."
            )));
        }
    }
    let mut put = |key: &str, v: Value| {
        out[key] = v;
        mask.push(key.to_owned());
    };
    if !same(&card, &old, "N") || !same(&card, &old, "FN") {
        let n = properties(&card, "N").first().and_then(|p| p.value.as_deref()).map(parts).unwrap_or_default();
        let field = |i: usize| n.get(i).cloned().unwrap_or_default();
        if google {
            put(
                "names",
                json!([{"familyName":field(0),"givenName":field(1),"middleName":field(2),"honorificPrefix":field(3),"honorificSuffix":field(4),"unstructuredName":value(&card,"FN")}]),
            )
        } else {
            for (key, val) in [
                ("surname", field(0)),
                ("givenName", field(1)),
                ("middleName", field(2)),
                ("title", field(3)),
                ("generation", field(4)),
                ("displayName", value(&card, "FN")),
            ] {
                put(key, json!(val));
            }
        }
    }
    for (property, gkey, mkey) in
        [("EMAIL", "emailAddresses", "emailAddresses"), ("TEL", "phoneNumbers", ""), ("URL", "urls", "businessHomePage")]
    {
        if same(&card, &old, property) {
            continue;
        }
        let fields = properties(&card, property);
        if google {
            put(
                gkey,
                Value::Array(
                    fields
                        .iter()
                        .map(|p| {
                            let val = unescape(p.value.as_deref().unwrap_or(""));
                            let mut v = typed(raw, gkey, "value", &val);
                            v["value"] = json!(val);
                            v["type"] = json!(label(p));
                            v
                        })
                        .collect(),
                ),
            );
        } else if property == "EMAIL" {
            put(
                mkey,
                Value::Array(
                    fields
                        .iter()
                        .map(|p| {
                            let val = unescape(p.value.as_deref().unwrap_or(""));
                            let mut v = typed(raw, mkey, "address", &val);
                            v["address"] = json!(val);
                            v
                        })
                        .collect(),
                ),
            );
        } else if property == "URL" {
            if fields.len() > 1 {
                return Err(error("Microsoft contacts support one website here; keep additional websites unchanged"));
            }
            put(mkey, json!(value(&card, property)));
        } else {
            let mut work = Vec::new();
            let mut home = Vec::new();
            let mut mobile = None;
            for p in fields {
                let val = unescape(p.value.as_deref().unwrap_or(""));
                match label(p).as_str() {
                    "work" => work.push(val),
                    "home" => home.push(val),
                    "cell" | "mobile" => {
                        if mobile.replace(val).is_some() {
                            return Err(error("Microsoft contacts support one mobile number"));
                        }
                    }
                    _ => return Err(error("Choose work, home or mobile for Microsoft phone numbers")),
                }
            }
            put("businessPhones", json!(work));
            put("homePhones", json!(home));
            put("mobilePhone", mobile.map(Value::String).unwrap_or(Value::Null));
        }
    }
    if !same(&card, &old, "ORG") || !same(&card, &old, "TITLE") {
        if google {
            let mut organizations = raw["organizations"].as_array().cloned().unwrap_or_default();
            if organizations.is_empty() {
                organizations.push(json!({}));
            }
            for organization in &mut organizations {
                if let Some(o) = organization.as_object_mut() {
                    o.remove("metadata");
                    o.remove("formattedType");
                }
            }
            let company = properties(&card, "ORG")
                .first()
                .and_then(|p| p.value.as_deref())
                .map(parts)
                .and_then(|p| p.into_iter().next())
                .unwrap_or_default();
            organizations[0]["name"] = json!(company);
            organizations[0]["title"] = json!(value(&card, "TITLE"));
            put("organizations", json!(organizations));
        } else {
            put("companyName", json!(value(&card, "ORG")));
            put("jobTitle", json!(value(&card, "TITLE")));
        }
    }
    if !same(&card, &old, "NOTE") {
        if google {
            put(
                "biographies",
                if value(&card, "NOTE").is_empty() {
                    json!([])
                } else {
                    json!([{"value":value(&card,"NOTE"),"contentType":"TEXT_PLAIN"}])
                },
            );
        } else {
            put("personalNotes", json!(value(&card, "NOTE")));
        }
    }
    if !same(&card, &old, "BDAY") {
        let date = value(&card, "BDAY");
        if date.is_empty() {
            put(if google { "birthdays" } else { "birthday" }, if google { json!([]) } else { Value::Null });
        } else {
            let date = if date.starts_with("--") {
                if !google {
                    return Err(error("Microsoft birthdays need a year; keep a yearless birthday in a DAV/local book"));
                }
                format!("2000{}", &date[1..])
            } else {
                date.clone()
            };
            let parsed =
                chrono::NaiveDate::parse_from_str(&date, "%Y-%m-%d").map_err(|_| error("Use a supported ISO birthday"))?;
            use chrono::Datelike;
            if google {
                let mut d = json!({"month":parsed.month(),"day":parsed.day()});
                if !value(&card, "BDAY").starts_with("--") {
                    d["year"] = json!(parsed.year());
                }
                put("birthdays", json!([{"date":d}]));
            } else {
                put("birthday", json!(format!("{parsed}T00:00:00Z")));
            }
        }
    }
    if !same(&card, &old, "ADR") {
        let mut addresses = Vec::new();
        let mut graph = BTreeMap::new();
        for p in properties(&card, "ADR") {
            let a = parts(p.value.as_deref().unwrap_or(""));
            let get = |i: usize| a.get(i).cloned().unwrap_or_default();
            if google {
                addresses.push(json!({"poBox":get(0),"extendedAddress":get(1),"streetAddress":get(2),"city":get(3),"region":get(4),"postalCode":get(5),"country":get(6),"type":label(p)}));
            } else {
                if !get(0).is_empty() || !get(1).is_empty() {
                    return Err(error("Microsoft addresses cannot represent separate PO box/extended fields here"));
                }
                let key = match label(p).as_str() {
                    "work" => "businessAddress",
                    "home" => "homeAddress",
                    _ => "otherAddress",
                };
                if graph
                    .insert(
                        key,
                        json!({"street":get(2),"city":get(3),"state":get(4),"postalCode":get(5),"countryOrRegion":get(6)}),
                    )
                    .is_some()
                {
                    return Err(error("Microsoft contacts support one address per type"));
                }
            }
        }
        if google {
            put("addresses", json!(addresses));
        } else {
            for key in ["businessAddress", "homeAddress", "otherAddress"] {
                put(
                    key,
                    graph
                        .remove(key)
                        .unwrap_or_else(|| json!({"street":"","city":"","state":"","postalCode":"","countryOrRegion":""})),
                );
            }
        }
    }
    if google && !mask.is_empty() && baseline.is_some() {
        let sources = raw["metadata"]["sources"]
            .as_array()
            .map(|s| s.iter().filter(|s| s["type"] == "CONTACT").cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        if sources.is_empty() || sources.iter().any(|s| s["etag"].as_str().is_none_or(str::is_empty)) {
            return Err(error("Sync the contact before editing so its Google source revision is known"));
        }
        out["metadata"] = json!({"sources":sources});
    }
    // Graph may recalculate displayName when a name changes; retain the selected display name.
    if !google && !mask.is_empty() {
        out["displayName"] = json!(value(&card, "FN"));
    }
    Ok((out, mask))
}

pub(crate) fn queue_save(registry: &Registry, book: &str, payload: &str, expected: Option<&str>) -> Result<(), Error> {
    contacts::initialize(registry)?;
    let card = parse(payload)?;
    let uid = value(&card, "UID");
    let (backend, writable, account): (String, bool, Option<String>) = registry
        .db
        .borrow()
        .query_row("SELECT backend,writable,account FROM books WHERE uid=?1 AND enabled=1", [book], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .map_err(error)?;
    if !writable || (account.is_some() && backend == "native") {
        return Err(error("This address book is read-only or needs discovery first"));
    }
    if matches!(backend.as_str(), "google" | "microsoft_personal") {
        let baseline: Option<(String, String)> = registry
            .db
            .borrow()
            .query_row("SELECT payload,raw FROM contact_baselines WHERE book=?1 AND uid=?2", params![book, uid], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional()
            .map_err(error)?;
        let raw = baseline
            .as_ref()
            .map(|(_, raw)| serde_json::from_str::<Value>(raw))
            .transpose()
            .map_err(error)?
            .unwrap_or(Value::Null);
        body(payload, baseline.as_ref().map(|(p, _)| p.as_str()), &raw, backend == "google")?;
    }
    let mut db = registry.db.borrow_mut();
    let tx = db.transaction().map_err(error)?;
    let current: Option<(String, bool)> = tx
        .query_row("SELECT payload,deleted FROM contacts WHERE book=?1 AND uid=?2", params![book, uid], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .optional()
        .map_err(error)?;
    if current.as_ref().map(|(p, _)| p.as_str()) != expected || current.as_ref().is_some_and(|(_, deleted)| *deleted) {
        return Err(error("The contact changed while you edited it. Keep this editor open and review the latest version."));
    }
    if let Some((previous, deleted)) = current {
        tx.execute(
            "INSERT INTO contact_history(book,uid,payload,deleted) VALUES(?1,?2,?3,?4)",
            params![book, uid, previous, deleted],
        )
        .map_err(error)?;
    }
    tx.execute("INSERT INTO contacts(book,uid,payload,dirty,deleted) VALUES(?1,?2,?3,?4,0) ON CONFLICT(book,uid) DO UPDATE SET payload=excluded.payload,dirty=excluded.dirty,deleted=0",params![book,uid,payload,true]).map_err(error)?;
    tx.commit().map_err(error)
}
pub(crate) fn queue_delete(registry: &Registry, book: &str, expected: &[(String, String)]) -> Result<(), Error> {
    contacts::initialize(registry)?;
    let mut db = registry.db.borrow_mut();
    let tx = db.transaction().map_err(error)?;
    let (writable, account, backend): (bool, Option<String>, String) = tx
        .query_row("SELECT writable,account,backend FROM books WHERE uid=?1 AND enabled=1", [book], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .map_err(error)?;
    if !writable || (account.is_some() && backend == "native") {
        return Err(error("This address book is read-only"));
    }
    for (uid, payload) in expected {
        let current: Option<(String, bool)> = tx
            .query_row("SELECT payload,deleted FROM contacts WHERE book=?1 AND uid=?2", params![book, uid], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional()
            .map_err(error)?;
        if current != Some((payload.clone(), false)) {
            return Err(error("A selected contact changed. Refresh before deleting."));
        }
        tx.execute("INSERT INTO contact_history(book,uid,payload,deleted) VALUES(?1,?2,?3,0)", params![book, uid, payload])
            .map_err(error)?;
        tx.execute("UPDATE contacts SET deleted=1,dirty=?3 WHERE book=?1 AND uid=?2", params![book, uid, true]).map_err(error)?;
    }
    tx.commit().map_err(error)
}
impl Registry {
    pub(crate) fn configure_book(
        &self,
        book: &str,
        name: &str,
        account: &str,
        backend: &str,
        endpoint: &str,
    ) -> Result<(), Error> {
        contacts::initialize(self)?;
        self.db.borrow().execute("INSERT INTO books(uid,name,account,backend,endpoint,writable) VALUES(?1,?2,?3,?4,?5,1) ON CONFLICT(uid) DO UPDATE SET name=excluded.name,account=excluded.account,backend=excluded.backend,endpoint=excluded.endpoint,writable=1",params![book,name,account,backend,endpoint]).map_err(error)?;
        Ok(())
    }
    fn contact_resource(&self, book: &str, uid: &str) -> Result<Option<(String, String)>, Error> {
        self.db
            .borrow()
            .query_row("SELECT remote_id,etag FROM contact_resources WHERE book=?1 AND uid=?2", params![book, uid], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional()
            .map_err(error)
    }
    fn contact_revision(&self, book: &str, uid: &str, id: &str, etag: &str, payload: &str, raw: &Value) -> Result<(), Error> {
        self.db.borrow().execute("INSERT INTO contact_resources VALUES(?1,?2,?3,?4) ON CONFLICT(book,uid) DO UPDATE SET remote_id=excluded.remote_id,etag=excluded.etag",params![book,uid,id,etag]).map_err(error)?;
        self.db.borrow().execute("INSERT INTO contact_baselines VALUES(?1,?2,?3,?4) ON CONFLICT(book,uid) DO UPDATE SET payload=excluded.payload,raw=excluded.raw",params![book,uid,payload,raw.to_string()]).map_err(error)?;
        Ok(())
    }
    fn merge_contact(
        &self,
        book: &str,
        uid: &str,
        id: &str,
        etag: &str,
        payload: &str,
        raw: &Value,
        dav: bool,
    ) -> Result<(), Error> {
        let snapshot = self.write_snapshot()?;
        self.db
            .borrow()
            .execute(
                "INSERT INTO contact_remote_versions VALUES(?1,?2,?3) ON CONFLICT(book,uid) DO UPDATE SET raw=excluded.raw",
                params![book, uid, raw.to_string()],
            )
            .map_err(error)?;
        let local: Option<(String, bool)> = self
            .db
            .borrow()
            .query_row("SELECT payload,dirty FROM contacts WHERE book=?1 AND uid=?2", params![book, uid], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional()
            .map_err(error)?;
        let previous = self.contact_resource(book, uid)?;
        // Older preview caches stored the Person etag. Compare the retained raw
        // baseline's source revision so upgrading does not invent conflicts.
        let baseline_revision = if !dav && raw.get("resourceName").is_some() {
            let baseline: Option<String> = self.db.borrow().query_row(
                "SELECT raw FROM contact_baselines WHERE book=?1 AND uid=?2", params![book,uid], |r|r.get(0)
            ).optional().map_err(error)?;
            baseline.as_deref().and_then(|s|serde_json::from_str::<Value>(s).ok()).map(|v|revision(&v,true))
        } else { None };
        let previous_revision = baseline_revision.as_deref().or_else(||previous.as_ref().map(|(_,e)|e.as_str()));
        if let Some((local, true)) = &local {
            if previous_revision != Some(etag)
                && !(previous.is_none() && parse(local)? == parse(payload)?)
            {
                self.record_issue(book, uid, "", if dav { "contact-dav" } else { "contact-api" }, payload, etag, id, false)?;
                snapshot.commit()?;
                return Err(error(
                    "A contact changed on the provider while you edited it. Review both versions in Settings → Online Accounts.",
                ));
            }
        }
        contacts::import_vcards(self, book, book, payload)?;
        self.contact_revision(book, uid, id, etag, payload, raw)?;
        snapshot.commit()
    }
    fn reconcile_contacts(&self, book: &str, seen: &std::collections::HashSet<String>, dav: bool) -> Result<(), Error> {
        let snapshot = self.write_snapshot()?;
        let resources: Vec<(String, String)> = {
            let db = self.db.borrow();
            let mut q = db.prepare("SELECT uid,remote_id FROM contact_resources WHERE book=?1").map_err(error)?;
            q.query_map([book], |r| Ok((r.get(0)?, r.get(1)?))).map_err(error)?.collect::<Result<_, _>>().map_err(error)?
        };
        for (uid, id) in resources {
            if !seen.contains(&uid) {
                let (dirty, deleted): (bool, bool) = self
                    .db
                    .borrow()
                    .query_row("SELECT dirty,deleted FROM contacts WHERE book=?1 AND uid=?2", params![book, uid], |r| {
                        Ok((r.get(0)?, r.get(1)?))
                    })
                    .map_err(error)?;
                if dirty && !deleted {
                    self.record_issue(book, &uid, "", if dav { "contact-dav" } else { "contact-api" }, "", "", &id, true)?;
                    snapshot.commit()?;
                    return Err(error(
                        "An edited contact was removed on the provider. Your local version is retained for review.",
                    ));
                }
                self.db
                    .borrow()
                    .execute("UPDATE contacts SET deleted=1,dirty=0 WHERE book=?1 AND uid=?2", params![book, uid])
                    .map_err(error)?;
                self.db
                    .borrow()
                    .execute("DELETE FROM contact_resources WHERE book=?1 AND uid=?2", params![book, uid])
                    .map_err(error)?;
            }
        }
        // Upgrade older read-only caches that did not yet have resource associations.
        contacts::reconcile(self, book, seen)?;
        snapshot.commit()
    }
    pub(crate) fn sync_contacts_dav(
        &self,
        remote: &Remote,
        book: &str,
        name: &str,
        endpoint: &str,
        objects: Vec<nyx_accounts_native::remote::DavObject>,
    ) -> Result<(), Error> {
        self.configure_book(book, name, &remote.account.id, "dav", endpoint)?;
        let mut seen = std::collections::HashSet::new();
        for object in objects {
            let cards = contacts::parse_cards(&object.payload)?;
            if cards.len() != 1 {
                return Err(error("A DAV contact resource must contain one vCard"));
            }
            let (uid, payload) = cards.into_iter().next().unwrap();
            seen.insert(uid.clone());
            self.merge_contact(book, &uid, &object.href, object.etag.as_deref().unwrap_or(""), &payload, &Value::Null, true)?;
        }
        self.reconcile_contacts(book, &seen, true)?;
        self.upload_contacts(remote, book, endpoint, true)
    }
    pub(crate) fn sync_contacts_api(&self, remote: &Remote) -> Result<(), Error> {
        let google = remote.account.provider == "google";
        let book = format!("{}:api", remote.account.id);
        let endpoint = if google {
            "https://people.googleapis.com/v1/people:createContact"
        } else {
            "https://graph.microsoft.com/v1.0/me/contacts"
        };
        self.sync_contact_book_api(
            remote,
            &book,
            remote.account.label(),
            endpoint,
            remote.list(Service::Contacts).map_err(error)?,
        )?;
        if !google {
            let mut pending = vec!["https://graph.microsoft.com/v1.0/me/contactFolders".to_owned()];
            let mut visited = std::collections::HashSet::new();
            while let Some(folder_endpoint) = pending.pop() {
                for folder in crate::api::pages(remote, &folder_endpoint, "value")? {
                    let id = folder["id"]
                        .as_str()
                        .filter(|id| !id.is_empty())
                        .ok_or_else(|| error("Contact folder has no identity"))?;
                    if !visited.insert(id.to_owned()) {
                        continue;
                    }
                    if visited.len() > 1000 {
                        return Err(error("Contact folder discovery exceeded limit"));
                    }
                    let mut base = url::Url::parse("https://graph.microsoft.com/v1.0/me/contactFolders/").unwrap();
                    base.path_segments_mut().unwrap().pop_if_empty().push(id);
                    let mut contacts = base.clone();
                    contacts.path_segments_mut().unwrap().push("contacts");
                    let name = folder["displayName"].as_str().unwrap_or("Contacts");
                    self.sync_contact_book_api(
                        remote,
                        &format!("{}:contacts:{id}", remote.account.id),
                        &format!("{} · {name}", remote.account.label()),
                        contacts.as_str(),
                        crate::api::pages(remote, contacts.as_str(), "value")?,
                    )?;
                    base.path_segments_mut().unwrap().push("childFolders");
                    pending.push(base.to_string());
                }
            }
        }
        Ok(())
    }
    fn sync_contact_book_api(
        &self,
        remote: &Remote,
        book: &str,
        name: &str,
        endpoint: &str,
        mut values: Vec<Value>,
    ) -> Result<(), Error> {
        let google = remote.account.provider == "google";
        self.configure_book(book, name, &remote.account.id, &remote.account.provider, endpoint)?;
        let known: Vec<String> = {
            let db = self.db.borrow();
            let mut q = db.prepare("SELECT remote_id FROM contact_resources WHERE book=?1").map_err(error)?;
            q.query_map([book], |r| r.get(0)).map_err(error)?.collect::<Result<_, _>>().map_err(error)?
        };
        // Provider lists can lag recent writes. Confirm individual resources before treating absence as deletion.
        for id in known {
            if values.iter().any(|value| value[if google { "resourceName" } else { "id" }].as_str() == Some(&id)) {
                continue;
            }
            let target = contact_target(endpoint, google, Some(&id), None)?;
            if let Some(value) = remote.json_if_present(target.as_str()).map_err(error)? {
                values.push(value);
            }
        }
        let mut seen = std::collections::HashSet::new();
        for raw in values {
            if raw["metadata"]["deleted"].as_bool() == Some(true) {
                continue;
            }
            let id = raw[if google { "resourceName" } else { "id" }]
                .as_str()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| error("Contact response has no identifier"))?;
            let mut payload = crate::api::card(&raw, google)
                .ok_or_else(|| error("Contact response cannot be represented; cached cards are retained"))?;
            let alias: Option<String> = self
                .db
                .borrow()
                .query_row("SELECT uid FROM contact_resources WHERE book=?1 AND remote_id=?2", params![book, id], |r| r.get(0))
                .optional()
                .map_err(error)?;
            if let Some(alias) = alias {
                payload = replace_uid(&payload, &alias);
            }
            let uid = value(&parse(&payload)?, "UID");
            seen.insert(uid.clone());
            self.merge_contact(book, &uid, id, &revision(&raw, google), &payload, &raw, false)?;
        }
        self.reconcile_contacts(book, &seen, false)?;
        self.upload_contacts(remote, book, endpoint, false)
    }
    fn upload_contacts(&self, remote: &Remote, book: &str, endpoint: &str, dav: bool) -> Result<(), Error> {
        self.issues_schema()?;
        let google = remote.account.provider == "google";
        let pending: Vec<(String, String, bool)> = {
            let db = self.db.borrow();
            let mut q =
                db.prepare("SELECT uid,payload,deleted FROM contacts WHERE book=?1 AND dirty=1 ORDER BY uid").map_err(error)?;
            q.query_map([book], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                .map_err(error)?
                .collect::<Result<_, _>>()
                .map_err(error)?
        };
        for (uid, payload, deleted) in pending {
            let existing = self.contact_resource(book, &uid)?;
            let uncertain: bool = self
                .db
                .borrow()
                .query_row("SELECT EXISTS(SELECT 1 FROM uncertain_creates WHERE source=?1 AND uid=?2)", params![book, uid], |r| {
                    r.get(0)
                })
                .map_err(error)?;
            if uncertain && existing.is_none() {
                return Err(error(
                    "Contact creation may have succeeded. Check the provider before retrying in Settings → Online Accounts.",
                ));
            }
            let etag = existing.as_ref().map(|(_, e)| e.as_str());
            if etag == Some("") {
                return Err(error("Sync a known contact revision before updating or deleting it"));
            }
            let baseline: Option<(String, String)> = self
                .db
                .borrow()
                .query_row("SELECT payload,raw FROM contact_baselines WHERE book=?1 AND uid=?2", params![book, uid], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })
                .optional()
                .map_err(error)?;
            let raw = baseline
                .as_ref()
                .map(|(_, r)| serde_json::from_str::<Value>(r))
                .transpose()
                .map_err(error)?
                .unwrap_or(Value::Null);
            let (new_id, new_etag, response) = if dav {
                let href = existing
                    .as_ref()
                    .map(|(id, _)| id.clone())
                    .unwrap_or_else(|| format!("{}.vcf", crate::api_write::stable_id(book, &uid, "")));
                if deleted {
                    if existing.is_some() {
                        remote.dav_delete(endpoint, &href, etag.unwrap()).map_err(error)?;
                    }
                    (href, String::new(), Value::Null)
                } else {
                    let revision = remote.dav_put(endpoint, &href, &payload, etag).map_err(error)?;
                    (href, revision, Value::Null)
                }
            } else {
                let mut target = contact_target(
                    endpoint,
                    google,
                    existing.as_ref().map(|(id, _)| id.as_str()),
                    Some(if deleted { "deleteContact" } else { "updateContact" }),
                )?;
                if deleted {
                    if existing.is_some() {
                        if google {
                            let check = contact_target(endpoint, true, existing.as_ref().map(|(id, _)| id.as_str()), None)?;
                            let latest = remote.json(check.as_str()).map_err(error)?;
                            if revision(&latest, true) != etag.unwrap() {
                                return Err(error("The contact changed before deletion. Refresh and review it first."));
                            }
                        }
                        remote.json_write(target.as_str(), reqwest::Method::DELETE, &Value::Null, if google { None } else { etag }).map_err(error)?;
                    }
                    (String::new(), String::new(), Value::Null)
                } else {
                    let (body, mask) = body(&payload, baseline.as_ref().map(|(p, _)| p.as_str()), &raw, google)?;
                    if mask.is_empty() && existing.is_some() {
                        self.db
                            .borrow()
                            .execute(
                                "UPDATE contacts SET dirty=0 WHERE book=?1 AND uid=?2 AND payload=?3 AND deleted=0",
                                params![book, uid, payload],
                            )
                            .map_err(error)?;
                        continue;
                    }
                    if google {
                        target.query_pairs_mut().append_pair("personFields", GOOGLE_FIELDS);
                        if existing.is_some() {
                            target.query_pairs_mut().append_pair("updatePersonFields", &mask.join(","));
                        }
                    }
                    if existing.is_none() {
                        self.db
                            .borrow()
                            .execute("INSERT INTO uncertain_creates VALUES(?1,?2)", params![book, uid])
                            .map_err(error)?;
                    }
                    let response = match remote.json_write(
                        target.as_str(),
                        if existing.is_some() { reqwest::Method::PATCH } else { reqwest::Method::POST },
                        &body,
                        if google { None } else { etag },
                    ) {
                        Ok(value) => value,
                        Err(failure) => {
                            if existing.is_none()
                                && failure
                                    .downcast_ref::<ApiWriteError>()
                                    .is_some_and(|e| (400..500).contains(&e.status) && e.status != 408)
                            {
                                self.db
                                    .borrow()
                                    .execute("DELETE FROM uncertain_creates WHERE source=?1 AND uid=?2", params![book, uid])
                                    .map_err(error)?;
                            }
                            return Err(error(failure));
                        }
                    };
                    let id = response[if google { "resourceName" } else { "id" }]
                        .as_str()
                        .or_else(|| existing.as_ref().map(|(id, _)| id.as_str()))
                        .ok_or_else(|| error("Contact creation response has no identifier; check the provider before retrying"))?
                        .to_owned();
                    let etag = revision(&response, google);
                    (id, etag, response)
                }
            };
            self.db.borrow().execute_batch("SAVEPOINT contact_uploaded").map_err(error)?;
            let saved = (|| {
                if !deleted {
                    self.contact_revision(book, &uid, &new_id, &new_etag, &payload, &response)?;
                } else {
                    self.db
                        .borrow()
                        .execute("DELETE FROM contact_resources WHERE book=?1 AND uid=?2", params![book, uid])
                        .map_err(error)?;
                }
                self.db
                    .borrow()
                    .execute("DELETE FROM uncertain_creates WHERE source=?1 AND uid=?2", params![book, uid])
                    .map_err(error)?;
                self.db
                    .borrow()
                    .execute(
                        "UPDATE contacts SET dirty=0 WHERE book=?1 AND uid=?2 AND payload=?3 AND deleted=?4",
                        params![book, uid, payload, deleted],
                    )
                    .map_err(error)?;
                Ok(())
            })();
            if saved.is_ok() {
                self.db.borrow().execute_batch("RELEASE contact_uploaded").map_err(error)?;
            } else {
                let _ = self.db.borrow().execute_batch("ROLLBACK TO contact_uploaded; RELEASE contact_uploaded");
            }
            saved?;
        }
        Ok(())
    }
}
fn replace_uid(payload: &str, uid: &str) -> String {
    payload
        .lines()
        .map(|line| {
            if line.split(':').next().is_some_and(|k| k.eq_ignore_ascii_case("UID")) {
                format!(
                    "UID:{}",
                    uid.replace('\\', "\\\\").replace(';', "\\;").replace(',', "\\,").replace('\n', "\\n").replace('\r', "")
                )
            } else {
                line.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\r\n")
        + "\r\n"
}
impl Registry {
    pub(crate) fn resolve_contact_issue(&self, issue: &SyncIssue, resolution: Resolution) -> Result<(), Error> {
        let uncertain = issue.kind == "contact-uncertain";
        if uncertain && !matches!(resolution, Resolution::KeepOffline | Resolution::RetryCreation) {
            return Err(error("Check the provider first, then retry explicitly or preserve an offline copy"));
        }
        if matches!(resolution, Resolution::RetryCreation) && !uncertain {
            return Err(error("This is not an uncertain creation"));
        }
        let (name, writable): (String, bool) = self
            .db
            .borrow()
            .query_row("SELECT name,writable FROM books WHERE uid=?1", [&issue.source], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(error)?;
        if matches!(resolution, Resolution::KeepLocal) && (!writable || issue.remote_deleted) {
            return Err(error("Keep an offline copy instead of replacing a removed or read-only contact"));
        }
        if matches!(resolution, Resolution::KeepOffline) {
            let book = format!("offline:{}", uuid::Uuid::new_v4());
            let rows: Vec<(String, String, bool)> = serde_json::from_str(&issue.local).map_err(error)?;
            for (_, payload, deleted) in rows {
                if !deleted {
                    contacts::import_vcards(self, &book, &format!("{name} · Local copy"), &payload)?;
                }
            }
        }
        if !matches!(resolution, Resolution::RetryCreation) {
            if matches!(resolution, Resolution::KeepLocal) || (!issue.remote_deleted && !issue.remote.is_empty()) {
                let (etag, id): (String, String) = self
                    .db
                    .borrow()
                    .query_row(
                        "SELECT etag,remote_id FROM sync_issues WHERE source=?1 AND uid=?2 AND rid=''",
                        params![issue.source, issue.uid],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .map_err(error)?;
                if matches!(resolution, Resolution::KeepLocal) && etag.is_empty() {
                    return Err(error("The provider supplied no safe contact revision"));
                }
                let raw: Option<String> = self
                    .db
                    .borrow()
                    .query_row(
                        "SELECT raw FROM contact_remote_versions WHERE book=?1 AND uid=?2",
                        params![issue.source, issue.uid],
                        |r| r.get(0),
                    )
                    .optional()
                    .map_err(error)?;
                let raw = raw.as_deref().map(serde_json::from_str::<Value>).transpose().map_err(error)?.unwrap_or(Value::Null);
                self.contact_revision(&issue.source, &issue.uid, &id, &etag, &issue.remote, &raw)?;
            }
            if !matches!(resolution, Resolution::KeepLocal) {
                self.db
                    .borrow()
                    .execute("DELETE FROM contacts WHERE book=?1 AND uid=?2", params![issue.source, issue.uid])
                    .map_err(error)?;
                if !issue.remote_deleted && !issue.remote.is_empty() {
                    contacts::import_vcards(self, &issue.source, &name, &issue.remote)?;
                } else {
                    self.db
                        .borrow()
                        .execute("DELETE FROM contact_resources WHERE book=?1 AND uid=?2", params![issue.source, issue.uid])
                        .map_err(error)?;
                }
            }
        }
        self.db
            .borrow()
            .execute("DELETE FROM sync_issues WHERE source=?1 AND uid=?2", params![issue.source, issue.uid])
            .map_err(error)?;
        self.db
            .borrow()
            .execute("DELETE FROM uncertain_creates WHERE source=?1 AND uid=?2", params![issue.source, issue.uid])
            .map_err(error)?;
        Ok(())
    }
}

pub(crate) fn uid_of(payload: &str) -> Result<String, Error> {
    let card = parse(payload)?;
    let id = value(&card, "UID");
    if properties(&card, "UID").len() != 1 || id.chars().any(char::is_control) {
        return Err(error("A vCard requires one valid UID"));
    }
    Ok(id)
}
fn chunks(payload: &str) -> Vec<String> {
    let mut result: Vec<String> = Vec::new();
    for line in payload.split_inclusive('\n') {
        if (line.starts_with(' ') || line.starts_with('\t')) && !result.is_empty() {
            result.last_mut().unwrap().push_str(line)
        } else {
            result.push(line.into())
        }
    }
    result
}
fn property_name(chunk: &str) -> String {
    chunk
        .split(':')
        .next()
        .unwrap_or("")
        .split(';')
        .next()
        .unwrap_or("")
        .rsplit('.')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_uppercase()
}
/// Apply only editor-changed properties to a raw card, retaining original parameters, groups and folding elsewhere.
pub(crate) fn merge_editor(raw: &str, before: &str, after: &str) -> Result<String, Error> {
    if uid_of(raw)? != uid_of(after)? {
        return Err(error("A contact identity cannot be changed"));
    }
    let old = parse(before)?;
    let new = parse(after)?;
    let changed: std::collections::BTreeSet<String> = old
        .keys()
        .chain(new.keys())
        .filter(|key| !matches!(key.as_str(), "UID" | "VERSION") && !same(&old, &new, key))
        .cloned()
        .collect();
    let version4 = value(&parse(raw)?, "VERSION") == "4.0";
    let mut output = String::new();
    for chunk in chunks(raw) {
        let key = property_name(&chunk);
        if key == "END" {
            for addition in chunks(after) {
                let key = property_name(&addition);
                if changed.contains(&key) {
                    if version4 && matches!(key.as_str(), "PHOTO" | "TEL") {
                        let unfolded = addition.replace("\r\n ", "").replace("\r\n\t", "");
                        let Some((header, value)) = unfolded.trim_end().split_once(':') else {
                            return Err(error("Invalid edited contact property"));
                        };
                        if key == "PHOTO" && header.to_ascii_uppercase().contains("ENCODING=B") {
                            let mime = if header.to_ascii_uppercase().contains("TYPE=PNG") { "image/png" } else { "image/jpeg" };
                            output.push_str(&format!("PHOTO:data:{mime};base64,{value}\r\n"));
                        } else if key == "TEL" && !value.starts_with("tel:") {
                            output.push_str(&format!("{header};VALUE=text:{value}\r\n"));
                        } else {
                            output.push_str(&addition);
                        }
                    } else {
                        output.push_str(&addition);
                    }
                }
            }
            output.push_str(&chunk);
        } else if !changed.contains(&key) {
            output.push_str(&chunk);
        }
    }
    parse(&output)?;
    Ok(output)
}

fn contact_target(endpoint: &str, google: bool, id: Option<&str>, action: Option<&str>) -> Result<url::Url, Error> {
    let mut target = url::Url::parse(endpoint).map_err(error)?;
    if let Some(id) = id {
        if google {
            let person = id
                .strip_prefix("people/")
                .filter(|s| !s.is_empty() && !s.contains('/'))
                .ok_or_else(|| error("Invalid Google contact identity"))?;
            target = url::Url::parse("https://people.googleapis.com/v1/people/").unwrap();
            target.path_segments_mut().unwrap().pop_if_empty().push(&match action {
                Some(action) => format!("{person}:{action}"),
                None => person.to_owned(),
            });
            if action.is_none() {
                target
                    .query_pairs_mut()
                    .append_pair("personFields", GOOGLE_FIELDS)
                    .append_pair("sources", "READ_SOURCE_TYPE_CONTACT");
            }
        } else {
            target.path_segments_mut().map_err(|_| error("Invalid contact endpoint"))?.push(id);
        }
    }
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn registry() -> Registry {
        Registry::open(std::env::temp_dir().join(format!("nyx-contact-write-{}", uuid::Uuid::new_v4())).join("pim.sqlite3"))
            .unwrap()
    }
    fn card(name: &str) -> String {
        format!("BEGIN:VCARD\r\nVERSION:3.0\r\nUID:person\r\nFN:{name}\r\nN:{name};;;;\r\nNOTE:Before\r\nEND:VCARD\r\n")
    }
    fn google() -> Value {
        json!({"etag":"person-1","metadata":{"sources":[{"type":"CONTACT","id":"1","etag":"source-1"}]},"organizations":[{"name":"Company","department":"Keep","title":"Old"},{"name":"Second organization"}]})
    }
    #[test]
    fn google_contact_revision_ignores_person_projection_changes() {
        let first=json!({"etag":"person-a","metadata":{"sources":[{"type":"CONTACT","etag":"contact-1"},{"type":"PROFILE","etag":"profile-1"}]}});
        let mut projected=first.clone(); projected["etag"]=json!("person-b");
        projected["metadata"]["sources"][1]["etag"]=json!("profile-2");
        assert_eq!(revision(&first,true),revision(&projected,true));
        projected["metadata"]["sources"][0]["etag"]=json!("contact-2");
        assert_ne!(revision(&first,true),revision(&projected,true));
    }
    #[test]
    fn api_updates_omit_unchanged_fields_and_preserve_provider_metadata() {
        let before = card("Person");
        let after = before.replace("NOTE:Before", "NOTE:After");
        let (graph, mask) = body(&after, Some(&before), &json!({"unknown":"retained"}), false).unwrap();
        assert_eq!(mask, vec!["personalNotes"]);
        assert_eq!(graph, json!({"personalNotes":"After","displayName":"Person"}));
        let (google, mask) = body(&after, Some(&before), &google(), true).unwrap();
        assert_eq!(mask, vec!["biographies"]);
        assert_eq!(google["metadata"]["sources"][0]["etag"], "source-1");
        assert!(google.get("names").is_none());
    }
    #[test]
    fn editing_organization_keeps_other_organizations_and_department() {
        let before = card("Person").replace("END:VCARD", "ORG:Company\r\nTITLE:Old\r\nEND:VCARD");
        let after = before.replace("TITLE:Old", "TITLE:New");
        let (body, _) = body(&after, Some(&before), &google(), true).unwrap();
        assert_eq!(body["organizations"][0]["department"], "Keep");
        assert_eq!(body["organizations"][1]["name"], "Second organization");
        assert_eq!(body["organizations"][0]["title"], "New");
    }
    #[test]
    fn unsupported_fields_and_unversioned_google_edits_fail_before_queueing() {
        let before = card("Person");
        let after = before.replace("END:VCARD", "PHOTO;ENCODING=b:ZmFrZQ==\r\nEND:VCARD");
        assert!(body(&after, Some(&before), &google(), true).is_err());
        assert!(body(&before.replace("Before", "After"), Some(&before), &Value::Null, true).is_err());
        let two = before.replace("END:VCARD", "TEL;TYPE=CELL:1\r\nTEL;TYPE=CELL:2\r\nEND:VCARD");
        assert!(body(&two, None, &Value::Null, false).is_err());
    }
    #[test]
    fn raw_merge_keeps_unedited_parameters_unknown_fields_and_folding() {
        let raw = card("Person").replace(
            "NOTE:Before",
            "item1.EMAIL;TYPE=HOME;PREF=1:person@example.test\r\nX-UNKNOWN:line\r\n continued\r\nNOTE:Before",
        );
        let before =
            card("Person").replace("END:VCARD", "EMAIL;TYPE=home:person@example.test\r\nX-UNKNOWN:linecontinued\r\nEND:VCARD");
        let after = before.replace("NOTE:Before", "NOTE:After");
        let merged = merge_editor(&raw, &before, &after).unwrap();
        assert!(merged.contains("item1.EMAIL;TYPE=HOME;PREF=1:"));
        assert!(merged.contains("X-UNKNOWN:line\r\n continued"));
        assert!(merged.contains("NOTE:After"));
        assert!(!merged.contains("NOTE:Before"));
        assert_eq!(uid_of(&raw.replace("UID:person", "UID:one\\,two")).unwrap(), "one,two");
    }
    #[test]
    fn queued_edits_and_deletes_reject_stale_snapshots_and_keep_history() {
        let r = registry();
        let original = card("Original");
        contacts::import_vcards(&r, "book", "Local", &original).unwrap();
        let edited = original.replace("Original", "Edited");
        queue_save(&r, "book", &edited, Some(&original)).unwrap();
        contacts::import_vcards(&r, "book", "Local", &original).unwrap();
        assert_eq!(
            r.db.borrow()
                .query_row("SELECT payload FROM contacts WHERE book='book' AND uid='person'", [], |row| row.get::<_, String>(0))
                .unwrap(),
            edited
        );
        assert!(queue_save(&r, "book", &original, Some(&original)).is_err());
        assert!(queue_delete(&r, "book", &[("person".into(), original)]).is_err());
        queue_delete(&r, "book", &[("person".into(), edited)]).unwrap();
        assert_eq!(r.db.borrow().query_row("SELECT count(*) FROM contact_history", [], |row| row.get::<_, i64>(0)).unwrap(), 2);
    }
    #[test]
    fn contact_conflicts_resolve_against_latest_revision_and_keep_both_versions() {
        let r = registry();
        r.configure_book("book", "Online", "account", "dav", "https://dav.example.test/book/").unwrap();
        let original = card("Original");
        r.merge_contact("book", "person", "person.vcf", "v1", &original, &Value::Null, true).unwrap();
        let local = original.replace("Original", "Local");
        queue_save(&r, "book", &local, Some(&original)).unwrap();
        let remote = original.replace("Original", "Remote");
        assert!(r.merge_contact("book", "person", "person.vcf", "v2", &remote, &Value::Null, true).is_err());
        let issue = r.sync_issues().unwrap().remove(0);
        assert_eq!(issue.kind, "contact-dav");
        r.resolve_sync_issue(&issue, Resolution::KeepLocal).unwrap();
        assert_eq!(r.contact_resource("book", "person").unwrap().unwrap().1, "v2");
        assert!(r.sync_issues().unwrap().is_empty());
        assert_eq!(
            r.db.borrow().query_row("SELECT payload FROM contacts WHERE book='book'", [], |row| row.get::<_, String>(0)).unwrap(),
            local
        );
    }
    #[test]
    fn uncertain_contact_creates_have_explicit_retry_and_offline_recovery() {
        let r = registry();
        contacts::import_vcards(&r, "book", "Local", &card("Person")).unwrap();
        r.issues_schema().unwrap();
        r.db.borrow().execute("INSERT INTO uncertain_creates VALUES('book','person')", []).unwrap();
        let issue = r.sync_issues().unwrap().remove(0);
        assert_eq!(issue.kind, "contact-uncertain");
        assert!(r.resolve_sync_issue(&issue, Resolution::UseRemote).is_err());
        r.resolve_sync_issue(&issue, Resolution::KeepOffline).unwrap();
        assert!(r.sync_issues().unwrap().is_empty());
        assert_eq!(
            r.db.borrow()
                .query_row("SELECT count(*) FROM contacts WHERE book LIKE 'offline:%' AND deleted=0", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }
    #[test]
    fn remote_contact_deletion_retains_dirty_copy_for_resolution() {
        let r = registry();
        r.configure_book("book", "Online", "account", "dav", "https://dav.example.test/book/").unwrap();
        let original = card("Person");
        r.merge_contact("book", "person", "person.vcf", "v1", &original, &Value::Null, true).unwrap();
        queue_save(&r, "book", &original.replace("Before", "After"), Some(&original)).unwrap();
        assert!(r.reconcile_contacts("book", &Default::default(), true).is_err());
        let issue = r.sync_issues().unwrap().remove(0);
        assert!(issue.remote_deleted);
        r.resolve_sync_issue(&issue, Resolution::KeepOffline).unwrap();
        assert!(r.contact_resource("book", "person").unwrap().is_none());
    }
    #[test]
    fn contact_endpoints_encode_ids_and_keep_folder_routes() {
        let target = contact_target(
            "https://graph.microsoft.com/v1.0/me/contactFolders/folder/contacts",
            false,
            Some("an/id"),
            Some("updateContact"),
        )
        .unwrap();
        assert!(target.as_str().ends_with("contacts/an%2Fid"));
        let google = contact_target(
            "https://people.googleapis.com/v1/people:createContact",
            true,
            Some("people/123"),
            Some("updateContact"),
        )
        .unwrap();
        assert_eq!(google.as_str(), "https://people.googleapis.com/v1/people/123:updateContact");
        assert!(contact_target(google.as_str(), true, Some("people/../../other"), None).is_err());
    }
}
