# Provider compatibility

Matrix from GOA 3.58 / EDS 3.62, plus live Google and Microsoft 365 on this machine (2026-10-20). Nextcloud was not tested (no account).

UI rule: never show a service chip or switch unless that GOA object actually has the interface (or the matching `*Disabled` property). This table is a planning aid, not a hardcoded widget list.

## Matrix

| Provider | Typical `ProviderType` | Mail | Calendar | Contacts | Tasks | Files | Auth | Extra package |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| Google | `google` | Nyx Mail (IMAP/SMTP, XOAUTH2) | EDS CalDAV | EDS CardDAV | EDS VTODO | `gvfs-google`; Files may be **disabled** in GOA 3.58 | OAuth2 + PKCE, browser | EDS |
| Microsoft 365 | `ms_graph` (icon `goa-account-ms365`) | GOA Mail iface; **EDS needs evolution-ews** | GOA Calendar iface; EDS needs ews | GOA Contacts iface; EDS needs ews | no Todo iface | `Files` iface present (`gvfs-onedrive`) | OAuth2; GNOME client id in `accounts.conf` | EDS + ews |
| Nextcloud | `webdav` | no (add IMAP/SMTP separately) | CalDAV | CardDAV | CalDAV VTODO | `gvfs-goa` WebDAV | app password / server login | EDS for DAV |
| Exchange (EWS) | `exchange` | evolution-ews | evolution-ews | evolution-ews | | | password / autodiscover | EDS + ews |
| IMAP and SMTP | `imap_smtp` | Nyx Mail (password) | | | | | password | EDS |
| Kerberos / Fedora | `kerberos` / `fedora` | | | | | | ticket | |
| ownCloud (legacy) | migrated to `webdav` | | | | | | | |

Priorities for Nyx: Google, Microsoft, Nextcloud. Other GOA providers appear if the daemon loads them.

## Live tests

| Check | Google | Microsoft 365 | Nextcloud |
| --- | --- | --- | --- |
| Helper opens without GNOME Shell | yes (workspace 2, `org.x.GnomeOnlineAccountsGtk`) | same helper | skipped |
| Browser OAuth / login completes | yes | yes | skipped (no account) |
| `ProviderType` string observed | `google` | `ms_graph` | skipped |
| Interfaces on the object | Account, Calendar, Contacts, Mail, OAuth2Based | Account, Calendar, Contacts, Files, Mail, OAuth2Based | skipped |
| `AttentionNeeded` after revoke | | | |
| `Remove()` leaves remote data | | | |
| Second account same provider | | | |
| EDS sources appear | yes: CardDAV book, CalDAV calendars, imapx, smtp, gtasks | **none** (no `evolution-ews`) | skipped |
| Session restart still listed | | | |

Packages: GOA 3.58.1-1, helper 3.50.11-1, EDS 3.62.1-1. `evolution-ews` is not installed. Identities are presentation emails, not tokens. Google still publishes `FilesDisabled=false` with **no** `Files` interface; Microsoft **does** expose `Files`. `nyx-accounts` only shows a Files switch when the interface is present (or already disabled). Microsoft `accounts.conf` also stores GNOME OAuth2 authorize/token URIs and client id; `OAuth2ClientSecret` is empty. Nyx does not read that file.

## Provider notes

### Google

- Scopes are whatever `GoaOAuth2Provider.get_scope()` requests in GOA. Nyx does not add scopes.
- Contacts: CardDAV (`www.googleapis.com`), not the retired GData API.
- Files: 3.58.0 changelog includes “Disable google provider Files feature” in some builds. If `org.gnome.OnlineAccounts.Files` is missing, Drive is rclone/GVFS-from-elsewhere, not GOA.

### Microsoft 365

- Live `ProviderType` is `ms_graph`, not `ms365`. `ProviderName` is “Microsoft 365”; icon `goa-account-ms365`.
- Files/OneDrive: `org.gnome.OnlineAccounts.Files` is present. Drive can list this identity; do not copy the token to rclone.
- Mail, calendar, contacts interfaces are on the GOA object, but EDS created **no** collection without `evolution-ews`. Contacts/Calendar/Mail sync for this account will stay empty until that package is installed.
- No `Todo` interface on the live object.
- Custom Azure client ID is a GOA feature; Nyx should not collect client IDs. The helper used GNOME’s id (stored by goa-daemon in `accounts.conf`).

### Nextcloud / WebDAV

- Replaced the old ownCloud-specific provider.
- Fastmail and mailbox.org also use WebDAV; app passwords can be DAV-only, so advertised services may fail later. User turns extras off in account details.
- Discovery can fail (`Cannot find WebDAV endpoint`) even when the password is right. Show that error; do not mark connected.
- Mail is not this provider. User adds IMAP/SMTP (or a Google/M365 account) for mail.

### IMAP/SMTP

- Password accounts. Useful for Fastmail mail alongside a DAV account.
- No OAuth on this provider (no XOAUTH2 in GOA IMAP as of the research notes).

## Nyx-specific vs GOA

| Setting | Where |
| --- | --- |
| Mail/Calendar/Contacts/Files enabled | GOA `*Disabled` |
| “Use this Nextcloud folder for News sync” | News / `nyx-sync` prefs |
| rclone remote | Drive |
| Local Contacts book | Contacts `contacts.db` |
| Vaultwarden / Plex / Telegram | those apps; not GOA |

Do not present app prefs as GOA capabilities.
