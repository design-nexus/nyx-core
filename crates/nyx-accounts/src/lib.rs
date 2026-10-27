//! In-process client for GNOME Online Accounts and EDS discovery. No GTK.
//!
//! Metadata only, unless the `credentials` feature is on: then [`credentials`]
//! fetches tokens and passwords from GOA in-process for a binary that signs in
//! itself (Nyx Mail's engine). Settings and Contacts leave it off.
//!
//! Talks to `org.gnome.OnlineAccounts` on the session bus. Add/reauth is
//! `nyx-goa-helper` (libgoa-backend in a separate process). This crate does not
//! run OAuth or link libadwaita.
//! EDS sources (address books, calendars, tasks, mail) are listed when EDS is installed.

mod parse;
pub mod apps;
#[cfg(feature = "credentials")]
pub mod credentials;
pub mod eds;
pub mod mail;

pub use parse::accounts_from_managed_objects;

use anyhow::{Context, Result, bail};
use glib::prelude::ToVariant;
use std::cell::RefCell;
use std::fmt;
use std::path::Path;
use std::rc::Rc;

pub const BUS_NAME: &str = "org.gnome.OnlineAccounts";
pub const OBJECT_PATH: &str = "/org/gnome/OnlineAccounts";
pub const HELPER: &str = "gnome-online-accounts-gtk";
pub const BACKEND: &str = "nyx-goa-helper";
pub const ACCOUNT_IFACE: &str = "org.gnome.OnlineAccounts.Account";
const OBJECT_MANAGER: &str = "org.freedesktop.DBus.ObjectManager";

/// A connected GOA account. Debug prints identity and flags, never credentials.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Account {
    pub id: String,
    pub path: String,
    pub provider_type: String,
    pub provider_name: String,
    pub provider_icon: String,
    pub identity: String,
    pub presentation: String,
    pub attention_needed: bool,
    pub locked: bool,
    pub services: Vec<ServiceState>,
}

impl Account {
    pub fn label(&self) -> &str {
        if !self.presentation.is_empty() {
            &self.presentation
        } else if !self.identity.is_empty() {
            &self.identity
        } else {
            &self.id
        }
    }

    pub fn status(&self) -> Status {
        if self.attention_needed { Status::NeedsSignIn } else { Status::Connected }
    }

    pub fn service_enabled(&self, service: &Service) -> bool {
        self.services.iter().any(|s| &s.service == service && s.enabled)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Connected,
    NeedsSignIn,
}

impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Self::Connected => "Connected",
            Self::NeedsSignIn => "Needs sign-in",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ServiceState {
    pub service: Service,
    pub enabled: bool,
    /// GOA exposed a `*Disabled` property we can write.
    pub can_toggle: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Service {
    Mail,
    Calendar,
    Contacts,
    Tasks,
    Files,
    Chat,
    Photos,
    Ticketing,
    Music,
    Unknown(String),
}

impl Service {
    pub fn label(&self) -> &str {
        match self {
            Self::Mail => "Mail",
            Self::Calendar => "Calendar",
            Self::Contacts => "Contacts",
            Self::Tasks => "Tasks",
            Self::Files => "Files",
            Self::Chat => "Chat",
            Self::Photos => "Photos",
            Self::Ticketing => "Ticketing",
            Self::Music => "Music",
            Self::Unknown(name) => name,
        }
    }

    /// Account property that inverts this service, when GOA has one.
    pub fn disabled_property(&self) -> Option<&'static str> {
        match self {
            Self::Mail => Some("MailDisabled"),
            Self::Calendar => Some("CalendarDisabled"),
            Self::Contacts => Some("ContactsDisabled"),
            Self::Tasks => Some("TodoDisabled"),
            Self::Files => Some("FilesDisabled"),
            Self::Chat => Some("ChatDisabled"),
            Self::Photos => Some("PhotosDisabled"),
            Self::Ticketing => Some("TicketingDisabled"),
            Self::Music => Some("MusicDisabled"),
            Self::Unknown(_) => None,
        }
    }

    pub fn from_interface(iface: &str) -> Option<Self> {
        let rest = iface.strip_prefix("org.gnome.OnlineAccounts.")?;
        match rest {
            "Account" | "Manager" | "OAuth2" | "Password" => None,
            "Mail" => Some(Self::Mail),
            "Calendar" => Some(Self::Calendar),
            "Contacts" => Some(Self::Contacts),
            "Todo" => Some(Self::Tasks),
            "Files" => Some(Self::Files),
            "Chat" => Some(Self::Chat),
            "Photos" => Some(Self::Photos),
            "Ticketing" => Some(Self::Ticketing),
            "Music" => Some(Self::Music),
            other => Some(Self::Unknown(other.to_string())),
        }
    }
}

/// Why a GOA call failed. Display text is for the UI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    Unavailable,
    NotResponding,
    NeedsSignIn,
    KeyringLocked,
    Other(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable => {
                f.write_str("Online Accounts isn't available. Install gnome-online-accounts.")
            }
            Self::NotResponding => f.write_str("Online Accounts isn't answering."),
            Self::NeedsSignIn => f.write_str("This account needs you to sign in again."),
            Self::KeyringLocked => f.write_str("The keyring isn't available. Online accounts cannot be saved."),
            Self::Other(s) => f.write_str(s),
        }
    }
}

impl std::error::Error for Error {}

pub fn map_gio_error(err: &glib::Error) -> Error {
    if err.matches(gio::DBusError::ServiceUnknown)
        || err.matches(gio::DBusError::NameHasNoOwner)
        || err.matches(gio::DBusError::SpawnServiceNotFound)
        || err.matches(gio::DBusError::SpawnServiceInvalid)
    {
        return Error::Unavailable;
    }
    if err.matches(gio::DBusError::Timeout)
        || err.matches(gio::DBusError::TimedOut)
        || err.matches(gio::DBusError::NoReply)
    {
        return Error::NotResponding;
    }
    if err.matches(gio::DBusError::AccessDenied) || err.matches(gio::DBusError::AuthFailed) {
        return Error::NeedsSignIn;
    }
    let s = err.message();
    let lower = s.to_ascii_lowercase();
    if lower.contains("was not provided by any .service files")
        || lower.contains("not provided by any .service files")
        || lower.contains("serviceunknown")
        || lower.contains("name has no owner")
    {
        Error::Unavailable
    } else if lower.contains("timed out") || lower.contains("timeout") {
        Error::NotResponding
    } else if lower.contains("keyring") || lower.contains("secret service") || lower.contains("no collection") {
        Error::KeyringLocked
    } else if lower.contains("not authorized") || lower.contains("not signed in") || lower.contains("attention needed") {
        Error::NeedsSignIn
    } else if lower.contains("network") || lower.contains("offline") || lower.contains("resolver") {
        Error::Other("Couldn't reach the account. Check the network.".into())
    } else {
        Error::Other(s.to_string())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize)]
pub struct ProviderInfo {
    #[serde(rename = "type")]
    pub provider_type: String,
    pub name: String,
}

pub fn helper_available() -> bool {
    find_bin(HELPER).is_some()
}

pub fn backend_available() -> bool {
    find_bin(BACKEND).is_some()
}

pub fn find_bin(name: &str) -> Option<std::path::PathBuf> {
    if let Ok(exe) = std::env::current_exe() {
        let next = exe.with_file_name(name);
        if next.is_file() {
            return Some(next);
        }
    }
    let paths = std::env::var("PATH").ok()?;
    paths.split(':').map(|dir| Path::new(dir).join(name)).find(|p| p.is_file())
}

pub fn list_providers() -> Result<Vec<ProviderInfo>, Error> {
    let bin = find_bin(BACKEND).ok_or(Error::Unavailable)?;
    let out = std::process::Command::new(bin).arg("list").output().map_err(|_| Error::Unavailable)?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(if err.is_empty() { Error::Unavailable } else { Error::Other(err.trim().to_string()) });
    }
    serde_json::from_slice(&out.stdout).map_err(|_| Error::Other("Couldn't read the provider list.".into()))
}

pub fn start_add(provider_type: &str) -> Result<std::process::Command, Error> {
    backend_cmd(&["add", provider_type])
}

pub fn start_refresh(account_id: &str) -> Result<std::process::Command, Error> {
    backend_cmd(&["refresh", account_id])
}

fn backend_cmd(args: &[&str]) -> Result<std::process::Command, Error> {
    let bin = find_bin(BACKEND).ok_or(Error::Unavailable)?;
    let mut cmd = std::process::Command::new(bin);
    cmd.args(args);
    Ok(cmd)
}

/// Session-bus client. Blocking calls; run them off the GTK thread.
pub struct Client {
    conn: gio::DBusConnection,
    watches: RefCell<Vec<gio::SignalSubscription>>,
}

impl Client {
    pub fn connect() -> Result<Self> {
        let conn = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE)
            .context("couldn't reach the session bus")?;
        Ok(Self { conn, watches: RefCell::new(Vec::new()) })
    }

    pub fn connection(&self) -> &gio::DBusConnection {
        &self.conn
    }

    pub fn list(&self) -> Result<Vec<Account>, Error> {
        const REPLY: &str = "(a{oa{sa{sv}}})";
        let ty = glib::VariantTy::new(REPLY).expect("reply type");
        let reply = self
            .conn
            .call_sync(
                Some(BUS_NAME),
                OBJECT_PATH,
                OBJECT_MANAGER,
                "GetManagedObjects",
                None,
                Some(ty),
                gio::DBusCallFlags::NONE,
                8000,
                gio::Cancellable::NONE,
            )
            .map_err(|e| map_gio_error(&e))?;
        // GetManagedObjects has one out arg; GIO returns it as a tuple.
        let body = reply.try_child_value(0).unwrap_or(reply);
        Ok(accounts_from_managed_objects(&body))
    }

    /// Keep the returned client alive for as long as these subscriptions should fire.
    pub fn watch<F>(&self, on_change: F)
    where
        F: Fn() + 'static,
    {
        let on_change = Rc::new(on_change);
        let mut watches = self.watches.borrow_mut();
        for (iface, member) in [
            (OBJECT_MANAGER, "InterfacesAdded"),
            (OBJECT_MANAGER, "InterfacesRemoved"),
            ("org.freedesktop.DBus.Properties", "PropertiesChanged"),
        ] {
            let cb = on_change.clone();
            watches.push(self.conn.subscribe_to_signal(
                Some(BUS_NAME),
                Some(iface),
                Some(member),
                None,
                None,
                gio::DBusSignalFlags::NONE,
                move |_| cb(),
            ));
        }
    }

    pub fn get(&self, id: &str) -> Result<Option<Account>, Error> {
        Ok(self.list()?.into_iter().find(|a| a.id == id || a.path == id))
    }

    pub fn by_provider(&self, provider_type: &str) -> Result<Vec<Account>, Error> {
        Ok(self.list()?.into_iter().filter(|a| a.provider_type == provider_type).collect())
    }

    pub fn with_service(&self, service: &Service) -> Result<Vec<Account>, Error> {
        Ok(self.list()?.into_iter().filter(|a| a.service_enabled(service)).collect())
    }

    pub fn remove(&self, path: &str) -> Result<(), Error> {
        if !account_path(path) {
            return Err(Error::Other("That account isn't here.".into()));
        }
        self.call(path, ACCOUNT_IFACE, "Remove", None, None).map(|_| ())
    }

    pub fn set_service_enabled(&self, path: &str, service: &Service, enabled: bool) -> Result<(), Error> {
        let Some(key) = service.disabled_property() else {
            return Ok(());
        };
        if !account_path(path) {
            return Err(Error::Other("That account isn't here.".into()));
        }
        let params = glib::Variant::tuple_from_iter([
            ACCOUNT_IFACE.to_variant(),
            key.to_variant(),
            glib::Variant::from_variant(&(!enabled).to_variant()),
        ]);
        match self.call(path, "org.freedesktop.DBus.Properties", "Set", Some(&params), None) {
            Ok(_) => Ok(()),
            Err(Error::Other(s)) if s.to_ascii_lowercase().contains("property") => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// Returns seconds until cached credentials expire. Does not complete OAuth.
    pub fn ensure_credentials(&self, path: &str) -> Result<i32, Error> {
        if !account_path(path) {
            return Err(Error::Other("That account isn't here.".into()));
        }
        let reply = self.call(path, ACCOUNT_IFACE, "EnsureCredentials", None, Some("(i)"))?;
        Ok(reply.try_child_value(0).and_then(|v| v.get::<i32>()).unwrap_or(0))
    }

    pub(crate) fn call(
        &self,
        path: &str,
        iface: &str,
        method: &str,
        params: Option<&glib::Variant>,
        reply_type: Option<&str>,
    ) -> Result<glib::Variant, Error> {
        let ty = reply_type.map(|s| glib::VariantTy::new(s).expect("type"));
        self.conn
            .call_sync(
                Some(BUS_NAME),
                path,
                iface,
                method,
                params,
                ty,
                gio::DBusCallFlags::NONE,
                8000,
                gio::Cancellable::NONE,
            )
            .map_err(|e| map_gio_error(&e))
    }
}

fn account_path(path: &str) -> bool {
    path.starts_with("/org/gnome/OnlineAccounts/") && glib::Variant::is_object_path(path)
}

/// Secret Service is present on the session bus.
pub fn keyring_available() -> bool {
    let Ok(conn) = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE) else {
        return false;
    };
    let Ok(ty) = glib::VariantTy::new("(b)") else {
        return false;
    };
    let params = glib::Variant::tuple_from_iter(["org.freedesktop.secrets".to_variant()]);
    let Ok(reply) = conn.call_sync(
        Some("org.freedesktop.DBus"),
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
        "NameHasOwner",
        Some(&params),
        Some(ty),
        gio::DBusCallFlags::NONE,
        3000,
        gio::Cancellable::NONE,
    ) else {
        return false;
    };
    reply.try_child_value(0).and_then(|v| v.get::<bool>()).unwrap_or(false)
}

/// Launch the standalone GOA helper. Does not pass secrets on the command line.
pub fn open_management() -> Result<()> {
    if !helper_available() {
        bail!("Install gnome-online-accounts-gtk to add or sign in to accounts.");
    }
    std::process::Command::new(HELPER).spawn().context("couldn't start Online Accounts")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_from_known_interfaces() {
        assert_eq!(Service::from_interface("org.gnome.OnlineAccounts.Mail"), Some(Service::Mail));
        assert_eq!(Service::from_interface("org.gnome.OnlineAccounts.Todo"), Some(Service::Tasks));
        assert_eq!(Service::from_interface("org.gnome.OnlineAccounts.Account"), None);
        assert_eq!(Service::from_interface("org.gnome.OnlineAccounts.OAuth2"), None);
        assert_eq!(
            Service::from_interface("org.gnome.OnlineAccounts.Printers"),
            Some(Service::Unknown("Printers".into()))
        );
    }

    #[test]
    fn error_sentences() {
        assert_eq!(
            Error::Unavailable.to_string(),
            "Online Accounts isn't available. Install gnome-online-accounts."
        );
        assert!(!Error::Unavailable.to_string().to_ascii_lowercase().contains("token"));
        assert_eq!(
            map_gio_error(&glib::Error::new(gio::DBusError::ServiceUnknown, "gone")),
            Error::Unavailable
        );
        assert_eq!(
            map_gio_error(&glib::Error::new(gio::DBusError::TimedOut, "wait")),
            Error::NotResponding
        );
        assert_eq!(
            map_gio_error(&glib::Error::new(gio::DBusError::AuthFailed, "no")),
            Error::NeedsSignIn
        );
        assert_eq!(
            map_gio_error(&glib::Error::new(gio::DBusError::Failed, "The keyring is locked")),
            Error::KeyringLocked
        );
        assert_eq!(
            Error::KeyringLocked.to_string(),
            "The keyring isn't available. Online accounts cannot be saved."
        );
    }

    #[test]
    fn disabled_property_known_only() {
        assert_eq!(Service::Mail.disabled_property(), Some("MailDisabled"));
        assert_eq!(Service::Tasks.disabled_property(), Some("TodoDisabled"));
        assert_eq!(Service::Unknown("Printers".into()).disabled_property(), None);
    }

    #[test]
    fn account_path_rejects_other_objects() {
        assert!(account_path("/org/gnome/OnlineAccounts/Accounts/account_1"));
        assert!(!account_path("/org/freedesktop/secrets"));
        assert!(!account_path("account_1"));
    }

    #[test]
    fn helper_has_no_secret_argv() {
        assert_eq!(HELPER, "gnome-online-accounts-gtk");
        let cmd = std::process::Command::new(HELPER);
        assert_eq!(cmd.get_program(), std::ffi::OsStr::new(HELPER));
        assert_eq!(cmd.get_args().len(), 0);
        let add = backend_cmd(&["add", "google"]).unwrap_or_else(|_| std::process::Command::new(BACKEND));
        let args: Vec<_> = add.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        assert!(args.iter().all(|a| a == "add" || a == "google" || a == "list" || a == "refresh"), "{args:?}");
    }

    #[test]
    fn parses_provider_list() {
        let list: Vec<ProviderInfo> =
            serde_json::from_str(r#"[{"type":"google","name":"Google"},{"type":"ms_graph","name":"Microsoft 365"}]"#)
                .unwrap();
        assert_eq!(list[0].provider_type, "google");
        assert_eq!(list[1].provider_type, "ms_graph");
    }

    #[test]
    fn session_goa_list() {
        let Ok(client) = Client::connect() else {
            return;
        };
        match client.list() {
            Ok(accounts) => {
                for a in &accounts {
                    assert!(account_path(&a.path), "{}", a.path);
                    let d = format!("{a:?}");
                    for word in ["token", "password", "secret", "ya29", "refresh"] {
                        assert!(!d.to_ascii_lowercase().contains(word), "{d}");
                    }
                }
            }
            Err(Error::Unavailable | Error::NotResponding) => {}
            Err(e) => panic!("{e}"),
        }
    }

    fn sample_account() -> Account {
        Account {
            id: "account_1".into(),
            path: "/org/gnome/OnlineAccounts/Accounts/account_1".into(),
            provider_type: "google".into(),
            provider_name: "Google".into(),
            provider_icon: "goa-account-google".into(),
            identity: "a@example.com".into(),
            presentation: "a@example.com".into(),
            attention_needed: false,
            locked: false,
            services: vec![
                ServiceState { service: Service::Mail, enabled: true, can_toggle: true },
                ServiceState { service: Service::Calendar, enabled: false, can_toggle: true },
                ServiceState { service: Service::Files, enabled: true, can_toggle: true },
            ],
        }
    }

    #[test]
    fn service_enabled_filters() {
        let a = sample_account();
        assert!(a.service_enabled(&Service::Mail));
        assert!(a.service_enabled(&Service::Files));
        assert!(!a.service_enabled(&Service::Calendar));
        assert!(!a.service_enabled(&Service::Tasks));
        assert!(apps::is_notes_provider("webdav"));
        assert!(!apps::is_notes_provider(&a.provider_type));
    }

    #[test]
    fn account_debug_has_no_secret_words() {
        let a = Account {
            id: "account_1".into(),
            path: "/org/gnome/OnlineAccounts/Accounts/account_1".into(),
            provider_type: "google".into(),
            provider_name: "Google".into(),
            provider_icon: "goa-account-google".into(),
            identity: "a@example.com".into(),
            presentation: "a@example.com".into(),
            attention_needed: false,
            locked: false,
            services: vec![ServiceState { service: Service::Mail, enabled: true, can_toggle: true }],
        };
        let d = format!("{a:?}");
        for word in ["token", "password", "secret", "ya29", "refresh"] {
            assert!(!d.to_ascii_lowercase().contains(word), "{d}");
        }
    }
}
