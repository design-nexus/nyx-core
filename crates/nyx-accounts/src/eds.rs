//! Evolution Data Server sources. Discovery and read-only contact fetch.
//!
//! Does not write sources, vCards, iCalendar, mail, or GOA tokens.

use super::{Error, map_gio_error, parse};
use glib::prelude::ToVariant;

pub const SOURCES_BUS: &str = "org.gnome.evolution.dataserver.Sources5";
pub const SOURCES_PATH: &str = "/org/gnome/evolution/dataserver/SourceManager";
pub const SOURCE_IFACE: &str = "org.gnome.evolution.dataserver.Source";
const OBJECT_MANAGER: &str = "org.freedesktop.DBus.ObjectManager";
const FACTORY_BUS: &str = "org.gnome.evolution.dataserver.AddressBook10";
const FACTORY_PATH: &str = "/org/gnome/evolution/dataserver/AddressBookFactory";
const FACTORY_IFACE: &str = "org.gnome.evolution.dataserver.AddressBookFactory";
const BOOK_IFACE: &str = "org.gnome.evolution.dataserver.AddressBook";
const BUILTIN_BOOK: &str = "system-address-book";
const BUILTIN_CALENDAR: &str = "system-calendar";
const BUILTIN_TASKS: &str = "system-task-list";
const BUILTIN_MAIL: &str = "local";
const ALL_QUERY: &str = r#"(contains "x-evolution-any-field" "")"#;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    AddressBook,
    Calendar,
    TaskList,
    MailAccount,
}

impl Kind {
    fn group(self) -> &'static str {
        match self {
            Self::AddressBook => "Address Book",
            Self::Calendar => "Calendar",
            Self::TaskList => "Task List",
            Self::MailAccount => "Mail Account",
        }
    }

    fn skip(self) -> &'static [&'static str] {
        match self {
            Self::AddressBook => &[BUILTIN_BOOK],
            Self::Calendar => &[BUILTIN_CALENDAR, "birthdays"],
            Self::TaskList => &[BUILTIN_TASKS],
            Self::MailAccount => &[BUILTIN_MAIL, "vfolder"],
        }
    }
}

/// An EDS source other than Evolution's built-in local one for that kind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
    pub uid: String,
    pub name: String,
    pub backend: String,
    pub enabled: bool,
    pub kind: Kind,
}

impl Source {
    pub fn label(&self) -> &str {
        if self.name.is_empty() { &self.uid } else { &self.name }
    }
}

/// An EDS address book other than Evolution's built-in local book.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AddressBook {
    pub uid: String,
    pub name: String,
    pub backend: String,
    pub enabled: bool,
}

impl AddressBook {
    pub fn label(&self) -> &str {
        if self.name.is_empty() { &self.uid } else { &self.name }
    }
}

impl From<Source> for AddressBook {
    fn from(s: Source) -> Self {
        Self { uid: s.uid, name: s.name, backend: s.backend, enabled: s.enabled }
    }
}

pub fn list_address_books() -> Result<Vec<AddressBook>, Error> {
    Ok(list_kind(Kind::AddressBook)?.into_iter().map(AddressBook::from).collect())
}

pub fn list_calendars() -> Result<Vec<Source>, Error> {
    list_kind(Kind::Calendar)
}

pub fn list_task_lists() -> Result<Vec<Source>, Error> {
    list_kind(Kind::TaskList)
}

pub fn list_mail_accounts() -> Result<Vec<Source>, Error> {
    list_kind(Kind::MailAccount)
}

fn list_kind(kind: Kind) -> Result<Vec<Source>, Error> {
    let objects = managed_objects()?;
    Ok(sources_from_managed_objects(&objects, kind))
}

fn managed_objects() -> Result<glib::Variant, Error> {
    let conn = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE).map_err(|e| map_gio_error(&e))?;
    let ty = glib::VariantTy::new("(a{oa{sa{sv}}})").expect("type");
    let reply = conn
        .call_sync(
            Some(SOURCES_BUS),
            SOURCES_PATH,
            OBJECT_MANAGER,
            "GetManagedObjects",
            None,
            Some(ty),
            gio::DBusCallFlags::NONE,
            8000,
            gio::Cancellable::NONE,
        )
        .map_err(|e| map_gio_error(&e))?;
    Ok(reply.try_child_value(0).unwrap_or(reply))
}

/// vCard text for every contact in the book. Empty query is rejected by EDS.
pub fn list_vcards(uid: &str) -> Result<Vec<String>, Error> {
    if uid.is_empty() || uid.contains('/') {
        return Err(Error::Other("That address book isn't here.".into()));
    }
    let conn = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE).map_err(|e| map_gio_error(&e))?;
    let open_ty = glib::VariantTy::new("(ss)").expect("type");
    let params = glib::Variant::tuple_from_iter([uid.to_variant()]);
    let opened = conn
        .call_sync(
            Some(FACTORY_BUS),
            FACTORY_PATH,
            FACTORY_IFACE,
            "OpenAddressBook",
            Some(&params),
            Some(open_ty),
            gio::DBusCallFlags::NONE,
            8000,
            gio::Cancellable::NONE,
        )
        .map_err(|e| map_gio_error(&e))?;
    let path = opened.try_child_value(0).and_then(|v| v.str().map(str::to_string)).unwrap_or_default();
    let bus = opened.try_child_value(1).and_then(|v| v.str().map(str::to_string)).unwrap_or_default();
    if path.is_empty() {
        return Err(Error::Other("Couldn't open this address book.".into()));
    }
    let dest = if bus.is_empty() { FACTORY_BUS } else { bus.as_str() };
    let _ = conn.call_sync(
        Some(dest),
        &path,
        BOOK_IFACE,
        "Open",
        None,
        None,
        gio::DBusCallFlags::NONE,
        8000,
        gio::Cancellable::NONE,
    );
    let list_ty = glib::VariantTy::new("(as)").expect("type");
    let query = glib::Variant::tuple_from_iter([ALL_QUERY.to_variant()]);
    let reply = conn
        .call_sync(
            Some(dest),
            &path,
            BOOK_IFACE,
            "GetContactList",
            Some(&query),
            Some(list_ty),
            gio::DBusCallFlags::NONE,
            15000,
            gio::Cancellable::NONE,
        )
        .map_err(|e| map_gio_error(&e))?;
    let arr = reply.try_child_value(0).unwrap_or(reply);
    let mut out = Vec::new();
    if arr.is_container() {
        for i in 0..arr.n_children() {
            if let Some(s) = arr.child_value(i).str() {
                out.push(s.to_string());
            }
        }
    }
    Ok(out)
}

pub fn books_from_managed_objects(objects: &glib::Variant) -> Vec<AddressBook> {
    sources_from_managed_objects(objects, Kind::AddressBook).into_iter().map(AddressBook::from).collect()
}

fn sources_from_managed_objects(objects: &glib::Variant, kind: Kind) -> Vec<Source> {
    let mut out = Vec::new();
    if !objects.is_container() {
        return out;
    }
    for i in 0..objects.n_children() {
        let child = objects.child_value(i);
        if child.n_children() < 2 {
            continue;
        }
        let ifaces = child.child_value(1);
        let Some(props) = parse::iface_props(&ifaces, SOURCE_IFACE) else { continue };
        let data = parse::prop_string(&props, "Data").unwrap_or_default();
        let uid = parse::prop_string(&props, "UID").unwrap_or_default();
        if let Some(src) = source_from_keyfile(&uid, &data, kind) {
            out.push(src);
        }
    }
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then(a.uid.cmp(&b.uid)));
    out
}

pub fn book_from_keyfile(uid: &str, data: &str) -> Option<AddressBook> {
    source_from_keyfile(uid, data, Kind::AddressBook).map(AddressBook::from)
}

fn source_from_keyfile(uid: &str, data: &str, kind: Kind) -> Option<Source> {
    if uid.is_empty() || uid.ends_with("-stub") || kind.skip().contains(&uid) {
        return None;
    }
    let groups = groups(data);
    if !groups.contains_key(kind.group()) {
        return None;
    }
    let src = groups.get("Data Source");
    let enabled = src.and_then(|m| m.get("Enabled")).map(|v| v.eq_ignore_ascii_case("true")).unwrap_or(true);
    if !enabled {
        return None;
    }
    let name = src.and_then(|m| m.get("DisplayName")).cloned().unwrap_or_default();
    let backend = groups.get(kind.group()).and_then(|m| m.get("BackendName")).cloned().unwrap_or_default();
    Some(Source { uid: uid.to_string(), name, backend, enabled: true, kind })
}

fn groups(data: &str) -> std::collections::BTreeMap<String, std::collections::BTreeMap<String, String>> {
    let mut out: std::collections::BTreeMap<String, std::collections::BTreeMap<String, String>> = std::collections::BTreeMap::new();
    let mut current = String::new();
    for line in data.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            current = name.to_string();
            out.entry(current.clone()).or_default();
            continue;
        }
        if current.is_empty() {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            out.entry(current.clone()).or_default().insert(k.trim().to_string(), v.trim().to_string());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_carddav_book() {
        let data = "\
[Data Source]\n\
DisplayName=Work\n\
Enabled=true\n\
Parent=google-stub\n\
\n\
[Address Book]\n\
BackendName=carddav\n";
        let b = book_from_keyfile("book-1", data).unwrap();
        assert_eq!(b.name, "Work");
        assert_eq!(b.backend, "carddav");
        assert!(b.enabled);
    }

    #[test]
    fn skips_builtin_and_disabled_and_other_kinds() {
        assert!(book_from_keyfile(BUILTIN_BOOK, "[Address Book]\nBackendName=file\n").is_none());
        assert!(book_from_keyfile("x", "[Data Source]\nEnabled=false\n[Address Book]\nBackendName=file\n").is_none());
        assert!(book_from_keyfile("cal", "[Data Source]\nDisplayName=Cal\n[Calendar]\nBackendName=caldav\n").is_none());
        assert!(source_from_keyfile(BUILTIN_CALENDAR, "[Calendar]\nBackendName=caldav\n", Kind::Calendar).is_none());
        assert!(source_from_keyfile(BUILTIN_TASKS, "[Task List]\nBackendName=caldav\n", Kind::TaskList).is_none());
        assert!(source_from_keyfile(BUILTIN_MAIL, "[Mail Account]\nBackendName=imapx\n", Kind::MailAccount).is_none());
        assert!(source_from_keyfile("birthdays", "[Calendar]\nBackendName=contacts\n", Kind::Calendar).is_none());
        assert!(source_from_keyfile("vfolder", "[Mail Account]\nBackendName=vfolder\n", Kind::MailAccount).is_none());
        assert!(source_from_keyfile("carddav-stub", "[Address Book]\nBackendName=carddav\n", Kind::AddressBook).is_none());
    }

    #[test]
    fn session_skips_builtins() {
        match list_address_books() {
            Ok(books) => {
                assert!(books.iter().all(|b| b.uid != BUILTIN_BOOK && !b.uid.ends_with("-stub")), "{books:?}");
            }
            Err(Error::Unavailable | Error::NotResponding) => {}
            Err(e) => panic!("{e}"),
        }
        match list_calendars() {
            Ok(list) => {
                assert!(list.iter().all(|s| s.uid != BUILTIN_CALENDAR && s.uid != "birthdays"), "{list:?}");
            }
            Err(Error::Unavailable | Error::NotResponding) => {}
            Err(e) => panic!("{e}"),
        }
        match list_mail_accounts() {
            Ok(list) => {
                assert!(list.iter().all(|s| s.uid != BUILTIN_MAIL && s.uid != "vfolder"), "{list:?}");
            }
            Err(Error::Unavailable | Error::NotResponding) => {}
            Err(e) => panic!("{e}"),
        }
    }

    #[test]
    fn parses_calendar_task_and_mail() {
        let cal = source_from_keyfile(
            "cal-1",
            "[Data Source]\nDisplayName=Work\nEnabled=true\n\n[Calendar]\nBackendName=caldav\n",
            Kind::Calendar,
        )
        .unwrap();
        assert_eq!(cal.name, "Work");
        assert_eq!(cal.backend, "caldav");
        assert_eq!(cal.kind, Kind::Calendar);

        let tasks = source_from_keyfile(
            "tasks-1",
            "[Data Source]\nDisplayName=Todos\n\n[Task List]\nBackendName=caldav\n",
            Kind::TaskList,
        )
        .unwrap();
        assert_eq!(tasks.name, "Todos");
        assert_eq!(tasks.kind, Kind::TaskList);

        let mail = source_from_keyfile(
            "mail-1",
            "[Data Source]\nDisplayName=Inbox\n\n[Mail Account]\nBackendName=imapx\n",
            Kind::MailAccount,
        )
        .unwrap();
        assert_eq!(mail.name, "Inbox");
        assert_eq!(mail.backend, "imapx");
        assert_eq!(mail.kind, Kind::MailAccount);
        assert!(source_from_keyfile("mail-1", "[Mail Account]\nBackendName=imapx\n", Kind::Calendar).is_none());
    }

    #[test]
    fn debug_has_no_secret_words() {
        let b = AddressBook { uid: "a".into(), name: "Work".into(), backend: "carddav".into(), enabled: true };
        let d = format!("{b:?}");
        for word in ["token", "password", "secret", "ya29"] {
            assert!(!d.to_ascii_lowercase().contains(word), "{d}");
        }
        let s = Source {
            uid: "c".into(),
            name: "Work".into(),
            backend: "caldav".into(),
            enabled: true,
            kind: Kind::Calendar,
        };
        let d = format!("{s:?}");
        for word in ["token", "password", "secret", "ya29"] {
            assert!(!d.to_ascii_lowercase().contains(word), "{d}");
        }
    }
}
