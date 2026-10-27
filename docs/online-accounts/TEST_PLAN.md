# Test plan

Automated tests cannot complete a real Google OAuth login. Split: crate tests without GOA, session tests on Omarchy, live provider tests with throwaway accounts.

## A. Crate tests (`nyx-accounts`, Phase 1+)

Run on a private D-Bus or with a fake `org.gnome.OnlineAccounts` object manager.

- Connect fails with the missing-service sentence when the name is absent.
- List maps ProviderType, PresentationIdentity, AttentionNeeded, and present interfaces.
- Unknown provider type still lists (no hardcoded allowlist).
- `set_service_enabled` writes `MailDisabled` and does nothing if the property is missing.
- `remove` calls `Remove` on the account path.
- Change subscription fires on InterfacesAdded/PropertiesChanged.
- Debug format of account types contains no `ya29.` / `refresh_token` substrings.
- `open_management` looks up `gnome-online-accounts-gtk` and errors if missing.

```sh
cargo test -p nyx-accounts
cargo clippy -p nyx-accounts --all-targets -- -D warnings
```

Settings must still build and test with GOA uninstalled:

```sh
cargo test -p nyx-settings
```

Contacts tests keep using a scratch `XDG_DATA_HOME` so `contacts.db` is never the real book.

## B. Session tests (Omarchy, after package install)

Packages: `gnome-online-accounts`, `gnome-online-accounts-gtk`. Not `gnome-shell`.

| # | Check | Pass |
| --- | --- | --- |
| B1 | `busctl --user call org.gnome.OnlineAccounts /org/gnome/OnlineAccounts org.freedesktop.DBus.Peer Ping` starts `goa-daemon` | yes, 2026-10-20 |
| B2 | `goa-daemon` has no gnome-shell parent | yes (user systemd; no gnome-shell process) |
| B3 | `gnome-online-accounts-gtk` opens on Hyprland | yes, 2026-10-20 (`org.x.GnomeOnlineAccountsGtk`) |
| B4 | `xdg-mime query default x-scheme-handler/goa-oauth2` (or the Google client scheme) points at the handler | yes, `org.gnome.OnlineAccounts.OAuth2.desktop` |
| B5 | Secret Service still `org.freedesktop.secrets` | yes, `gnome-keyring-daemon` |
| B6 | After logout/login (or `systemctl --user restart gnome-keyring-daemon` only if safe), accounts.conf persists | Google + Microsoft present; session restart not re-run |
| B7 | With GOA uninstalled, Settings page (once built) shows the missing-package banner and does not crash | code path present; not uninstalled to re-check |
| B8 | Helper missing: Add button explains the package name | code path present; helper is installed |

## C. Live providers (Ken + throwaway accounts)

One Google, one Microsoft 365, one Nextcloud. Record strings in [PROVIDER_COMPATIBILITY.md](PROVIDER_COMPATIBILITY.md).

For each:

1. Add via the helper from a terminal (Phase 0) / from Settings Add (Phase 2).
2. Account appears on the bus (`gdbus introspect` / `GoaClient` list).
3. Presentation identity is the email or server user, not a token.
4. Service interfaces match what the user enabled.
5. Disable Calendar (if present); interface disappears; EDS calendar (if EDS installed) stops listing it.
6. Sign out at the provider or corrupt credentials; `AttentionNeeded` becomes true; UI says sign in, not connected.
7. Sign in again via helper; AttentionNeeded clears.
8. Remove; remote mailbox/calendar still exists in the provider web UI; local Contacts DB row count unchanged.
9. Add a second account of the same provider; both ids distinct.
10. Restart Hyprland session; both still listed.

Microsoft: if mail/calendar interfaces are absent without `evolution-ews`, that is expected; record it.

Nextcloud: if discovery fails, the helper error must be visible; do not write a half account and call it connected.

## D. EDS (optional package)

- Registry running after a GOA Google account with Contacts enabled → at least one address book source with `ESourceGoa` account id matching.
- Contacts app (Phase 3): local book still loads; EDS book appears as another sidebar source; turning off the app does not wipe `contacts.db`.
- Removing GOA account drops EDS collection; local book remains.

## E. Security

- `journalctl --user -u gnome-keyring-daemon` / Nyx logs during add-account: no access token.
- `grep -R 'ya29\|1//' ~/.config/nyx ~/.config/nyx-settings` empty of GOA tokens.
- `accounts.conf` has no access/refresh tokens (Google: Provider/Identity; Microsoft: plus OAuth2 URIs/client id, empty ClientSecret).

## F. What Phase 0 can run now

Packages are installed (2026-10-20). Session checks B1, B2, B4, B5 passed. B3 (helper window) and C (live providers) still need Ken to add throwaway accounts. Do not spawn the helper or Settings from this session unattended.
