//! Discovery for later Nyx apps. Identity and source lists only.
//!
//! Does not fetch iCalendar, mail, or files. Does not copy GOA tokens into rclone,
//! nyx-sync, or Nyx config.

use crate::{eds, Account, Client, Error, Service};

/// GOA accounts with Calendar enabled. Events themselves come from EDS later.
pub fn calendar_accounts(client: &Client) -> Result<Vec<Account>, Error> {
    client.with_service(&Service::Calendar)
}

pub fn calendar_sources() -> Result<Vec<eds::Source>, Error> {
    eds::list_calendars()
}

/// GOA accounts with Tasks enabled. VTODO lists come from EDS later.
pub fn reminder_accounts(client: &Client) -> Result<Vec<Account>, Error> {
    client.with_service(&Service::Tasks)
}

pub fn reminder_sources() -> Result<Vec<eds::Source>, Error> {
    eds::list_task_lists()
}

/// GOA accounts with Mail enabled. Camel talks to EDS when Mail is built.
pub fn mail_accounts(client: &Client) -> Result<Vec<Account>, Error> {
    client.with_service(&Service::Mail)
}

pub fn mail_sources() -> Result<Vec<eds::Source>, Error> {
    eds::list_mail_accounts()
}

/// GOA Files identities. rclone stays for remotes it already owns.
pub fn drive_accounts(client: &Client) -> Result<Vec<Account>, Error> {
    client.with_service(&Service::Files)
}

/// Nextcloud / WebDAV identities. Notes stay local Markdown; nyx-sync keeps secret-tool.
pub fn notes_accounts(client: &Client) -> Result<Vec<Account>, Error> {
    Ok(client.list()?.into_iter().filter(|a| is_notes_provider(&a.provider_type)).collect())
}

pub fn is_notes_provider(provider_type: &str) -> bool {
    provider_type.eq_ignore_ascii_case("webdav") || provider_type.eq_ignore_ascii_case("owncloud")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_provider_is_webdav() {
        assert!(is_notes_provider("webdav"));
        assert!(is_notes_provider("ownCloud"));
        assert!(!is_notes_provider("google"));
        assert!(!is_notes_provider("ms365"));
        assert!(!is_notes_provider("ms_graph"));
    }
}
