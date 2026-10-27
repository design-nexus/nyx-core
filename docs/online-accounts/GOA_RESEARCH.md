# GOA research

Inspected 2026-10-20 on Omarchy (Arch) Hyprland. Packages queried with `pacman`; session bus with `busctl --user`. Ken installed GOA/EDS later the same day; live activation is recorded below.

## Packages on Arch extra

| Package | Version in extra | Installed here | Depends on GNOME Shell? |
| --- | --- | --- | --- |
| `gnome-online-accounts` | 3.58.1-1 | **yes** | no |
| `libgoa` (split from the same pkgbase) | 3.58.1-1 | **yes** | no |
| `gnome-online-accounts-gtk` | 3.50.11-1 | **yes** | no (XApp; depends on GOA + libadwaita) |
| `gnome-control-center` | 51.1-1 | no | **yes** (`gnome-shell`) |
| `evolution-data-server` | 3.62.1-1 | **yes** | no (pulls gtk3, webkit2gtk-4.1, libgoa) |
| `evolution-ews` | — | **no** | — |
| `gnome-keyring` | 1:51.1-1 | **yes**, running | no |
| `libsecret` | 0.21.8.2-1 | **yes** | no |
| `libadwaita` | 1:1.10.0-1 | yes (unused by Nyx) | no |
| `xdg-desktop-portal-hyprland` | 1.4.1-2 | yes | no |

`gnome-online-accounts` 3.58.1 depends on: dbus, dconf, gcr-4, glib2, gtk4, libadwaita, json-glib, krb5, librest, libsecret, libsoup3, libgoa. Optional: a browser for OAuth; `gvfs-goa` / `gvfs-onedrive` for files.

It **provides** `libgoa-backend-1.0.so=2-64`. Backend is in the same package as the daemon on Arch.

Do not install `gnome-control-center` or the `gnome` group.

## This session

```
XDG_CURRENT_DESKTOP=Hyprland
XDG_SESSION_TYPE=wayland
```

Present:

- `org.freedesktop.secrets` and `org.gnome.keyring` (`gnome-keyring-daemon.service` active)
- `~/.local/share/keyrings/Default_keyring.keyring`
- portals: `xdg-desktop-portal`, `-gtk`, `-hyprland`

After Ken installed the packages (same day):

- `busctl --user call org.gnome.OnlineAccounts … Peer.Ping` starts `/usr/lib/goa-daemon` under `systemd --user` (parent is user systemd, not gnome-shell; `pgrep gnome-shell` is empty)
- GOA tree is Manager only; `~/.config/goa-1.0/` exists and is empty (no accounts)
- `org.gnome.evolution.dataserver.Sources5` is `evolution-source-registry`; builtins only (`system-address-book`, `system-calendar`, `system-task-list`, `local`, `birthdays`, stubs)
- `xdg-mime query default x-scheme-handler/goa-oauth2` → `org.gnome.OnlineAccounts.OAuth2.desktop`
- Helper binary: `/usr/bin/gnome-online-accounts-gtk`

`goa-daemon` is D-Bus activated (`org.gnome.OnlineAccounts.service`). Confirmed on this Hyprland session.

### Package files

Confirmed on the installed packages (also unpacked earlier under `/tmp/goa-pkg`):

| File | Fact |
| --- | --- |
| `org.gnome.OnlineAccounts.service` | `Exec=/usr/lib/goa-daemon` |
| `org.gnome.Identity.service` | identity helper beside the daemon |
| `org.gnome.OnlineAccounts.OAuth2.desktop` | `Exec=/usr/lib/goa-oauth2-handler %u`; MimeType `goa-oauth2` plus Google client scheme `com.googleusercontent.apps.44438659992-7kgjeitenc16ssihbtdjbgguch7ju55s` |
| `gnome-online-accounts-gtk.desktop` | `Exec=gnome-online-accounts-gtk`; `Categories=Settings;` |
| Binaries | `goa-daemon`, `goa-identity-service`, `goa-oauth2-handler`, `libgoa-backend-1.0.so` |
| `goabackendenums.h` | Provider groups: branded, contacts, mail, ticketing, chat. Features include mail, calendar, contacts, files, photos, music, ticketing, chat. |

## Two APIs

### Public consumer (`libgoa-1.0`, GIR `Goa-1.0`)

Documented at <https://gnome.pages.gitlab.gnome.org/gnome-online-accounts/>.

`GoaClient`:

- `get_accounts()`
- signals `account-added`, `account-changed`, `account-removed`
- `get_manager()`

Bus name: `org.gnome.OnlineAccounts`.

This is what Evolution, GNOME Calendar, and third-party consumers use to **see** accounts and obtain credentials.

### Interactive backend (`libgoa-backend-1.0`)

`GoaProvider.add_account`, `refresh_account`, `show_account`. GTK4 + libadwaita since GOA 3.49. OAuth uses the system browser (WebKit was removed; `goa-oauth2-handler` + `goa-oauth2://` / Google client-id scheme).

Used by GNOME Settings and by **gnome-online-accounts-gtk**.

Debian/Ubuntu still recommend `gnome-control-center | gnome-online-accounts-gtk` as the UI for the daemon. Cinnamon dropped its in-process panel and launches the GTK helper for the same reason: GOA 3.50 moved the panel to GTK4/Adwaita.

### They are not interchangeable

`org.gnome.OnlineAccounts.Manager.AddAccount` **creates a local account record after credentials exist**:

```
AddAccount (
  IN provider s,
  IN identity s,
  IN presentation_identity s,
  IN credentials a{sv},
  IN details a{ss},
  OUT account_object_path o
)
```

It does not run OAuth. Passing empty credentials does not sign the user in.

`IsSupportedProvider(provider_type) → b` (since 3.36) is the public way to ask whether a type is built in. Provider **enumeration for a picker** still comes from `goa-backend` (`GoaProvider` list), not from the consumer GIR.

## Account D-Bus (`org.gnome.OnlineAccounts.Account`)

Useful properties (all from upstream docs):

| Property | Access | Use in Nyx |
| --- | --- | --- |
| `ProviderType` | read | google, webdav, `ms_graph` (Microsoft 365), exchange, imap_smtp, kerberos, … |
| `ProviderName` | read | UI |
| `ProviderIcon` | read | serialized GIcon |
| `Id` | read | stable key, never shown |
| `Identity` / `PresentationIdentity` | read | email / label |
| `AttentionNeeded` | read | expired / needs human |
| `IsLocked` | read | discourage remove |
| `IsTemporary` | readwrite | session-only |
| `MailDisabled`, `CalendarDisabled`, `ContactsDisabled`, `FilesDisabled`, `TicketingDisabled`, `ChatDisabled`, `PhotosDisabled`, `MusicDisabled`, `TodoDisabled`, … | readwrite | service switches. Deprecated ones: Documents, Maps, Printers, ReadLater, Todo |

Methods:

- `Remove()` — local only; “does not remove the account at the remote service”
- `EnsureCredentials() → expires_in` — error if cached credentials missing/invalid; sets `AttentionNeeded` on auth-server expiry

Service interfaces (`org.gnome.OnlineAccounts.Mail`, `.Calendar`, `.Contacts`, `.Files`, `.OAuth2`, `.Password`, …) appear and disappear when Disabled flags change. The UI must list **interfaces on the object**, not a hardcoded provider matrix.

`OAuth2.GetAccessToken` / `Password.GetPassword` return secrets to same-uid callers. Nyx Settings must not display them. A Nyx D-Bus API must not re-export them.

## Configuration

- User accounts: `$XDG_CONFIG_HOME/goa-1.0/accounts.conf` (private format; third parties must not parse it). No passwords or refresh tokens. Live `ms_graph` entries also have GNOME OAuth2 authorize/token URIs and client id (`OAuth2ClientSecret` empty).
- Secrets: gnome-keyring; format private; third parties must not read/modify/delete those items directly.
- System: `/etc/goa.conf` and historically dconf `whitelisted-providers`. Default is all providers enabled.

## OAuth on Hyprland

Since GOA 3.49/3.50:

1. Backend opens the default browser.
2. Redirect hits `goa-oauth2://…` or a Google client-id URI scheme.
3. `org.gnome.OnlineAccounts.OAuth2.desktop` runs `goa-oauth2-handler`.

Risks on a non-GNOME session:

- Handler `.desktop` with `NoDisplay=true` still needs to be a valid `x-scheme-handler`.
- Default browser must exist (Firefox/Chromium on Omarchy is enough; Epiphany is only an optional dep).
- Keyring must be unlocked (already true for this session once the user has signed in).
- Portal OpenURI should work via `xdg-desktop-portal-gtk` / Hyprland.

**Unverified until install.** Known issues in the XApp tracker: keyring collection missing, browser not opening. Nyx must surface those as errors, not as “connected”.

## Third-party policy

GNOME’s GOA goals: accounts are for the **core GNOME system and apps**. Third-party consumers should talk to GOA maintainers. Applications using GOA use **GNOME’s OAuth client IDs**. Forking those IDs into a Nyx OAuth crate can get GNOME’s Google/Microsoft access revoked.

Acceptable:

- Consume accounts through the public D-Bus API
- Drive add/reauth through `libgoa-backend` (the helper)

Not acceptable:

- Copy `goagoogleprovider.c` / client secrets into Nyx
- Parse `accounts.conf` or keyring items

## Rust bindings

| Option | Verdict |
| --- | --- |
| crates.io `goa` 0.0.3 (`libgoa-rs`) | Stale; do not use |
| gtk-rs official crates | No Goa crate |
| GIR-generate `goa` | Possible; extra maintenance |
| gio 0.22 against ObjectManager | **Chosen** (Phase 1 `nyx-accounts`) |
| zbus/tokio | Extra runtime; skip |
| Bind `goa-backend` | Only if Nyx embeds add-account; pulls GTK4 + libadwaita |

## gnome-online-accounts-gtk

- Upstream: <https://github.com/xapp-project/gnome-online-accounts-gtk> (Linux Mint packaging URL on Arch)
- License: GPL-3.0-or-later
- GTK4 + libadwaita + libgoa-backend
- Designed for Cinnamon and other non-GNOME desktops
- Arch extra 3.50.11 vs GOA 3.58.1: version skew exists; Cinnamon/Arch already ship this pair. Confirm after install that add-account still runs.

This is the supported standalone entry point. It will look like Adwaita, not Nyx. That is an accepted Phase 2 limitation.

## Compatibility with Nyx Settings

| Constraint | Impact |
| --- | --- |
| No libadwaita in Nyx apps | Settings must not link `goa-backend`. Helper is a separate process. |
| No WebKit in Nyx apps | Fine; GOA dropped WebKit. |
| STYLE.md | List/detail native; add/reauth window is the helper. |
| gtk4 0.11 | D-Bus client has no GTK requirement. |

## Limitations (Phase 0)

1. Add-account is not a public one-shot D-Bus login.
2. Native Nyx OAuth is not promised.
3. GOA not installed here; D-Bus activation untested.
4. Live Google / Microsoft / Nextcloud not tested.
5. Google Files may be disabled in GOA 3.58 (`Disable google provider Files feature` in 3.58.0 notes). UI must hide Files when the interface is absent.
6. Microsoft 365 mail/calendar/contacts need EDS + `evolution-ews`, not GOA alone.
7. Nextcloud is the WebDAV provider; IMAP is a separate GOA account if the user wants mail.
