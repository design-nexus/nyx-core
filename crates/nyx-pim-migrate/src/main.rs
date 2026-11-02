//! Explicit, read-only EDS export and separate native snapshot import.
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{io::Write, os::unix::fs::OpenOptionsExt};
#[derive(Serialize, Deserialize)]
struct Bundle {
    version: u32,
    sources: Vec<Source>,
    books: Vec<Book>,
    warnings: Vec<String>,
    #[serde(default)]
    accounts: Vec<nyx_accounts_native::Account>,
}
#[derive(Serialize, Deserialize)]
struct Source {
    uid: String,
    name: String,
    tasks: bool,
    color: Option<String>,
    selected: bool,
    payloads: Vec<String>,
}
#[derive(Serialize, Deserialize)]
struct Book {
    uid: String,
    name: String,
    cards: Vec<String>,
}
fn export() -> Result<Bundle> {
    let registry = nyx_eds::Registry::connect()?;
    let mut bundle = Bundle {
        version: 1,
        accounts: Vec::new(),
        sources: Vec::new(),
        books: Vec::new(),
        warnings: vec![
            "Export is a snapshot. EDS pending uploads cannot be certified by this tool; leave the original stores intact."
                .into(),
        ],
    };
    if let Ok(client) = nyx_accounts::Client::connect() {
        for legacy in client.list().unwrap_or_default() {
            let provider = match legacy.provider_type.as_str() {
                "google" => "google",
                "ms_graph" => "microsoft_personal",
                "owncloud" => "nextcloud",
                "imap_smtp" => "imap_smtp",
                "webdav" => "webdav",
                _ => {
                    bundle.warnings.push(format!("Account {} uses an unsupported legacy provider", legacy.id));
                    continue;
                }
            };
            let mut enabled = std::collections::BTreeSet::new();
            for service in legacy.services.iter().filter(|s| s.enabled) {
                use nyx_accounts::Service as Old;
                use nyx_accounts_native::Service as New;
                if let Some(service) = match service.service {
                    Old::Mail => Some(New::Mail),
                    Old::Calendar => Some(New::Calendar),
                    Old::Contacts => Some(New::Contacts),
                    Old::Tasks => Some(New::Tasks),
                    Old::Files => Some(New::Files),
                    _ => None,
                } {
                    enabled.insert(service);
                }
            }
            let mut endpoints = std::collections::BTreeMap::new();
            if let Ok(Some(mail)) = client.mail_settings(&legacy) {
                for (kind, server) in [("imap", mail.imap), ("smtp", mail.smtp)] {
                    if let Some(server) = server {
                        let scheme = match (kind, server.tls) {
                            ("imap", nyx_accounts::mail::Tls::Implicit) => "imaps",
                            ("imap", nyx_accounts::mail::Tls::StartTls) => "imap+starttls",
                            ("smtp", nyx_accounts::mail::Tls::Implicit) => "smtps",
                            ("smtp", nyx_accounts::mail::Tls::StartTls) => "smtp+starttls",
                            _ => continue,
                        };
                        endpoints.insert(kind.into(), format!("{scheme}://{}:{}", server.host, server.port));
                    }
                }
            }
            use glib::variant::ToVariant;
            for (key, interface) in [("caldav", "Calendar"), ("carddav", "Contacts"), ("webdav", "Files")] {
                if let Ok(reply) = client.connection().call_sync(
                    Some(nyx_accounts::BUS_NAME),
                    &legacy.path,
                    "org.freedesktop.DBus.Properties",
                    "Get",
                    Some(&(format!("org.gnome.OnlineAccounts.{interface}"), "Uri").to_variant()),
                    None,
                    gio::DBusCallFlags::NONE,
                    8000,
                    gio::Cancellable::NONE,
                ) {
                    if let Some(uri) =
                        reply.try_child_value(0).and_then(|v| v.as_variant()).and_then(|v| v.str().map(str::to_owned))
                    {
                        let uri = uri.strip_prefix("davs:").map(|s| format!("https:{s}")).unwrap_or(uri);
                        if let Ok(mut uri) = url::Url::parse(&uri) {
                            let _ = uri.set_username("");
                            let _ = uri.set_password(None);
                            if uri.scheme() == "https" && uri.query().is_none() {
                                endpoints.insert(key.into(), uri.into());
                            }
                        }
                    }
                }
            }
            bundle.accounts.push(nyx_accounts_native::Account {
                id: String::new(),
                provider: provider.into(),
                identity: legacy.identity.clone(),
                label: legacy.label().into(),
                enabled,
                status: "needs_sign_in".into(),
                client_id: None,
                endpoints,
                legacy_id: Some(legacy.id),
                error: None,
                provider_user_id: None,
            });
        }
    }
    for kind in [nyx_eds::Kind::Calendar, nyx_eds::Kind::TaskList] {
        for (source, _) in registry.all_sources(kind) {
            let calendar = match nyx_eds::Calendar::open(&registry, &source.uid, kind, None) {
                Ok(c) => c,
                Err(_) => {
                    bundle.warnings.push(format!("Could not open source {}", source.uid));
                    continue;
                }
            };
            let mut payloads = Vec::new();
            let mut exported = std::collections::HashSet::new();
            for item in calendar.items("#t")? {
                if exported.insert(item.uid.clone()) {
                    payloads.push(calendar.export(&item.uid)?);
                }
            }
            bundle.sources.push(Source {
                uid: source.uid,
                name: source.name,
                tasks: kind == nyx_eds::Kind::TaskList,
                color: source.color,
                selected: source.selected,
                payloads,
            });
        }
    }
    let mut books: Vec<_> = nyx_accounts::eds::list_address_books()?.into_iter().map(|b| (b.uid, b.name)).collect();
    books.push(("system-address-book".into(), "Personal".into()));
    for (uid, name) in books {
        match nyx_accounts::eds::list_vcards(&uid) {
            Ok(cards) => bundle.books.push(Book { uid, name, cards }),
            Err(_) => bundle.warnings.push(format!("Could not export address book {uid}")),
        }
    }
    Ok(bundle)
}
fn import(bundle: Bundle) -> Result<()> {
    anyhow::ensure!(bundle.version == 1, "Unsupported export version");
    let registry = nyx_pim::Registry::connect()?;
    if !bundle.accounts.is_empty() {
        let client = nyx_accounts_native::Client::connect()?;
        let existing = client.list()?;
        for account in bundle.accounts {
            if !existing.iter().any(|a| {
                (account.legacy_id.is_some() && a.legacy_id == account.legacy_id)
                    || (a.provider == account.provider && a.identity.eq_ignore_ascii_case(&account.identity))
            }) {
                client.register(&account)?;
            }
        }
    }

    for source in bundle.sources {
        let kind = if source.tasks { nyx_pim::Kind::TaskList } else { nyx_pim::Kind::Calendar };
        // Distinct source IDs keep imported snapshots separate from new accounts.
        let uid = format!("import:{}", source.uid);
        if registry.source(&uid, kind).is_none() {
            registry.add_source(&nyx_pim::Source {
                uid: uid.clone(),
                name: format!("{} (imported)", source.name),
                kind,
                color: source.color,
                selected: source.selected,
                backend: "native".into(),
                account: None,
                group: Some("Imported snapshot".into()),
                local: true,
                writable: true,
            })?;
        }
        let calendar = nyx_pim::Calendar::open(&registry, &uid, kind, None)?;
        for payload in source.payloads {
            calendar.import_all(&payload)?;
        }
    }
    for book in bundle.books {
        nyx_pim::contacts::import_vcards(
            &registry,
            &format!("import:{}", book.uid),
            &format!("{} (imported)", book.name),
            &book.cards.join("\r\n"),
        )?;
    }
    for warning in bundle.warnings {
        eprintln!("Migration note: {warning}");
    }
    Ok(())
}
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let path = args.get(1).context("Usage: nyx-pim-migrate export|import FILE")?;
    match args.first().map(String::as_str) {
        Some("export") => {
            let bundle = export()?;
            let mut file = std::fs::OpenOptions::new().create_new(true).write(true).mode(0o600).open(path)?;
            file.write_all(&serde_json::to_vec_pretty(&bundle)?)?;
            file.sync_all()?;
            println!(
                "Exported {} sources and {} books; {} warnings",
                bundle.sources.len(),
                bundle.books.len(),
                bundle.warnings.len()
            );
        }
        Some("import") => {
            let bytes = std::fs::read(path)?;
            import(serde_json::from_slice(&bytes)?)?;
            println!("Imported snapshot; original stores were not modified");
        }
        _ => anyhow::bail!("Usage: nyx-pim-migrate export|import FILE"),
    }
    Ok(())
}
