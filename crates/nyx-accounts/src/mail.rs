//! Mail server settings from an account's GOA `Mail` interface. No secrets.

use super::parse::{prop_bool, prop_string};
use super::{Account, Client, Error};
use glib::prelude::ToVariant;

pub const MAIL_IFACE: &str = "org.gnome.OnlineAccounts.Mail";
pub const OAUTH2_IFACE: &str = "org.gnome.OnlineAccounts.OAuth2Based";
pub const PASSWORD_IFACE: &str = "org.gnome.OnlineAccounts.PasswordBased";

/// How a connection is secured.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tls {
    /// TLS from the first byte (IMAP 993, SMTP 465).
    Implicit,
    /// Plain connection upgraded with STARTTLS (IMAP 143, SMTP 587).
    StartTls,
    /// No encryption. Nyx Mail refuses these.
    None,
}

/// How the account signs in to its mail servers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MailAuth {
    /// XOAUTH2 with a token from GOA (`OAuth2Based.GetAccessToken`).
    OAuth2,
    /// A password from GOA (`PasswordBased.GetPassword`, ids `imap-password` and `smtp-password`).
    Password,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Server {
    pub host: String,
    pub port: u16,
    pub tls: Tls,
    pub user: String,
    /// GOA says to accept invalid certificates (set by the user in the GOA dialog).
    pub accept_invalid_certs: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MailSettings {
    pub address: String,
    /// The display name for From, if GOA has one.
    pub name: String,
    pub imap: Option<Server>,
    pub smtp: Option<Server>,
    pub auth: MailAuth,
}

impl Client {
    /// The account's mail settings, or `None` when it has no Mail interface
    /// (service switched off, or a provider such as Microsoft 365 that exposes
    /// no IMAP settings).
    pub fn mail_settings(&self, account: &Account) -> Result<Option<MailSettings>, Error> {
        let Some(props) = self.properties(&account.path, MAIL_IFACE)? else { return Ok(None) };
        let auth = if self.properties(&account.path, OAUTH2_IFACE)?.is_some() { MailAuth::OAuth2 } else { MailAuth::Password };
        Ok(settings_from(&props, auth))
    }

    /// `Properties.GetAll`, or `None` when the object has no such interface.
    pub(crate) fn properties(&self, path: &str, iface: &str) -> Result<Option<glib::Variant>, Error> {
        match self.call(path, "org.freedesktop.DBus.Properties", "GetAll", Some(&(iface,).to_variant()), Some("(a{sv})")) {
            Ok(reply) => Ok(reply.try_child_value(0)),
            Err(Error::Other(message)) if message.contains("UnknownInterface") || message.to_lowercase().contains("no such interface") => {
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }
}

/// Parses the `Mail` interface properties.
pub fn settings_from(props: &glib::Variant, auth: MailAuth) -> Option<MailSettings> {
    let address = prop_string(props, "EmailAddress").unwrap_or_default();
    let text = |key| prop_string(props, key).filter(|s| !s.is_empty());
    let flag = |key| prop_bool(props, key).unwrap_or(false);
    let imap = text("ImapHost").map(|host| {
        let tls = tls(flag("ImapUseSsl"), flag("ImapUseTls"));
        let (host, port) = split_port(&host, match tls {
            Tls::Implicit => 993,
            _ => 143,
        });
        Server {
            host,
            port,
            tls,
            user: text("ImapUserName").unwrap_or_else(|| address.clone()),
            accept_invalid_certs: flag("ImapAcceptSslErrors"),
        }
    });
    let smtp = text("SmtpHost").map(|host| {
        let tls = tls(flag("SmtpUseSsl") && !flag("SmtpUseTls"), flag("SmtpUseTls"));
        let (host, port) = split_port(&host, match tls {
            Tls::Implicit => 465,
            Tls::StartTls => 587,
            Tls::None => 25,
        });
        Server {
            host,
            port,
            tls,
            user: text("SmtpUserName").unwrap_or_else(|| address.clone()),
            accept_invalid_certs: flag("SmtpAcceptSslErrors"),
        }
    });
    if imap.is_none() && smtp.is_none() {
        return None;
    }
    // GOA marks SMTP XOAUTH2 explicitly; an OAuth2 account without it uses passwords.
    let auth = if auth == MailAuth::OAuth2 && smtp.is_some() && !flag("SmtpAuthXoauth2") { MailAuth::Password } else { auth };
    Some(MailSettings { name: text("Name").filter(|n| *n != address).unwrap_or_default(), address, imap, smtp, auth })
}

fn tls(ssl: bool, starttls: bool) -> Tls {
    if ssl {
        Tls::Implicit
    } else if starttls {
        Tls::StartTls
    } else {
        Tls::None
    }
}

/// "host:port" or "host"; IPv6 literals in brackets.
fn split_port(host: &str, default: u16) -> (String, u16) {
    if let Some(rest) = host.strip_prefix('[')
        && let Some((addr, tail)) = rest.split_once(']')
    {
        let port = tail.strip_prefix(':').and_then(|p| p.parse().ok()).unwrap_or(default);
        return (addr.to_string(), port);
    }
    match host.rsplit_once(':') {
        Some((h, p)) if !h.contains(':') => (h.to_string(), p.parse().unwrap_or(default)),
        _ => (host.to_string(), default),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glib::Variant;

    fn asv(pairs: &[(&str, Variant)]) -> Variant {
        let dict = glib::VariantDict::new(None);
        for (k, v) in pairs {
            dict.insert_value(k, v);
        }
        dict.to_variant()
    }

    #[test]
    fn google_uses_xoauth2_over_implicit_tls() {
        // As the live Google account reports it.
        let props = asv(&[
            ("EmailAddress", "a@gmail.com".to_variant()),
            ("Name", "a@gmail.com".to_variant()),
            ("ImapHost", "imap.gmail.com".to_variant()),
            ("ImapUserName", "a@gmail.com".to_variant()),
            ("ImapUseSsl", true.to_variant()),
            ("ImapUseTls", false.to_variant()),
            ("SmtpHost", "smtp.gmail.com".to_variant()),
            ("SmtpUseAuth", true.to_variant()),
            ("SmtpAuthXoauth2", true.to_variant()),
            ("SmtpUseSsl", true.to_variant()),
            ("SmtpUseTls", true.to_variant()),
        ]);
        let s = settings_from(&props, MailAuth::OAuth2).unwrap();
        assert_eq!(s.auth, MailAuth::OAuth2);
        assert_eq!(s.name, "");
        let imap = s.imap.unwrap();
        assert_eq!((imap.host.as_str(), imap.port, imap.tls), ("imap.gmail.com", 993, Tls::Implicit));
        // GOA sets both SSL and TLS for Gmail SMTP; that is STARTTLS on 587.
        let smtp = s.smtp.unwrap();
        assert_eq!((smtp.host.as_str(), smtp.port, smtp.tls), ("smtp.gmail.com", 587, Tls::StartTls));
    }

    #[test]
    fn imap_smtp_password_account_with_ports() {
        let props = asv(&[
            ("EmailAddress", "me@fastmail.com".to_variant()),
            ("Name", "Me".to_variant()),
            ("ImapHost", "imap.fastmail.com:993".to_variant()),
            ("ImapUseSsl", true.to_variant()),
            ("SmtpHost", "smtp.fastmail.com:465".to_variant()),
            ("SmtpUseSsl", true.to_variant()),
            ("SmtpUseTls", false.to_variant()),
            ("SmtpUserName", "me@fastmail.com".to_variant()),
        ]);
        let s = settings_from(&props, MailAuth::Password).unwrap();
        assert_eq!(s.name, "Me");
        assert_eq!(s.auth, MailAuth::Password);
        assert_eq!(s.imap.as_ref().unwrap().user, "me@fastmail.com");
        assert_eq!(s.smtp.as_ref().map(|x| (x.port, x.tls)), Some((465, Tls::Implicit)));
    }

    #[test]
    fn microsoft_graph_has_no_mail_servers() {
        let props = asv(&[
            ("EmailAddress", "x@outlook.com".to_variant()),
            ("ImapHost", "".to_variant()),
            ("SmtpHost", "".to_variant()),
        ]);
        assert_eq!(settings_from(&props, MailAuth::OAuth2), None);
    }

    #[test]
    fn ports_and_ipv6() {
        assert_eq!(split_port("mail.example:1993", 993), ("mail.example".into(), 1993));
        assert_eq!(split_port("[::1]:1143", 143), ("::1".into(), 1143));
        assert_eq!(split_port("::1", 143), ("::1".into(), 143));
    }
}
