# Nyx Online Accounts

Centralized online account management for the Nyx suite on Omarchy/Hyprland, using **GNOME Online Accounts (GOA)** and **Evolution Data Server (EDS)**.

This document is the implementation plan. Architecture, GOA research, EDS, providers, security, and tests live beside it in this directory.

**Phase 0 (this document set) is research only.** No Settings UI, no Contacts migration, no Mail app, no `nyx-accountsd`.

This plan **replaces** `/home/ken/Projects/MAIL_PLAN.md` Phase 0. That file proposed a Nyx-owned SQLite daemon (`nyx-accountsd`) for connected accounts. GOA already is that session service. Mail later consumes GOA/EDS.

---

## Decision

The smallest maintainable implementation:

1. **GOA** owns identities, OAuth, passwords, and per-service enable flags.
2. **EDS** owns mail/calendar/contacts/tasks sync for GOA-backed sources.
3. **Nyx Settings** shows a native Online Accounts page (list, status, service toggles, remove).
4. **Add and reauthenticate** launch `gnome-online-accounts-gtk` (Arch extra, no GNOME Shell).
5. **`crates/nyx-accounts`** is an in-process Rust client (gio D-Bus). No GTK. No Nyx D-Bus that exports tokens.
6. **No Nyx accounts daemon.** Do not copy tokens into Nyx config or a second keyring schema.

Do not promise a fully native Nyx OAuth dialog until `libgoa-backend` is wrapped without violating `STYLE.md`. That is not Phase 1–2.

---

## Current tree

| Fact | Where |
| --- | --- |
| GTK4 0.11 `v4_18`, no libadwaita, no WebKit | `STYLE.md`, app `Cargo.toml` files |
| Settings pages are `sections::Section` | `apps/settings/src/sections/mod.rs` |
| Settings → Users (`id: accounts`) is OS users | `sections/accounts.rs` |
| Contacts is local SQLite only | `apps/contacts`, `~/.local/share/nyx-contacts/contacts.db` |
| Nextcloud secrets today are per-app `secret-tool` | `crates/nyx-sync/src/credentials.rs` |
| Shared crates | `nyx-core`, `nyx-theme`, `nyx-ui`, `nyx-icons` |

Online Accounts is a **new** Settings page. It does not reuse Users.

Inspected 2026-10-20 on this Omarchy/Hyprland session. See [GOA_RESEARCH.md](GOA_RESEARCH.md) for package and D-Bus facts.

---

## Phases

### Phase 0 — Research (this work)

- [x] Inspect Settings and Contacts
- [x] Read `STYLE.md`
- [x] Identify Settings architecture
- [x] Inspect Contacts storage
- [x] Inventory GOA/EDS packages on Arch/Omarchy
- [x] Compare public GOA consumer API vs backend add-account API
- [x] Document EDS and Mail decision
- [x] Install GOA on this machine and confirm `goa-daemon` D-Bus activation (Ken, 2026-10-20)
- [x] Live Google add through the helper (Hyprland, 2026-10-20)
- [x] Live Microsoft 365 add through the helper (`ProviderType=ms_graph`, 2026-10-20)
- [ ] Live Nextcloud — skipped; no account to test

Google and Microsoft are connected. Nextcloud is not. Revoke / remove / second-account / session-restart still open.

### Phase 1 — Account discovery (read-only)

- [x] `crates/nyx-accounts`: connect, list, provider, services present, `AttentionNeeded`, change signals, missing-GOA errors (gio 0.22, no GTK)
- [x] Settings section `online-accounts` (group Accounts, after Users): rows for connected accounts
- [x] Banner when `gnome-online-accounts` is missing
- [x] `cargo test -p nyx-accounts` and `cargo check -p nyx-settings` (2026-10-20)
- No add/remove yet (Phase 2)

### Phase 2 — Account management

- [x] Remove via `GoaAccount.Remove` with copy that remote data is kept
- [x] Service switches via `*Disabled` properties GOA actually exposes
- [x] Add / Sign in again: native provider picker in Settings; `nyx-goa-helper` runs libgoa-backend (GNOME OAuth IDs, system browser). `gnome-online-accounts-gtk` is fallback.
- [x] Detail page, AttentionNeeded, keyring/network errors
- [x] Dynamic providers; do not hardcode a fixed list in the UI
- Live add/remove still needs throwaway logins (Ken)

### Phase 3 — Shared layer + Contacts

- [x] Contacts keeps `contacts.db` (no merge, no delete on GOA remove)
- [x] When EDS is installed, list EDS address books as extra sidebar sources (`nyx-accounts::eds`)
- [x] Local book stays default (`All contacts`); EDS books are read-only here
- Live EDS/GOA books still need a connected account (Ken)

### Phase 4 — Productivity interfaces only

Prepare crate APIs for Calendar, Reminders, Mail, Drive, Notes. Do not build those apps in this work.

- [x] EDS `Kind` / `Source` listing: calendars, task lists, mail accounts (skip builtins)
- [x] Contacts `AddressBook` / `list_address_books` / `list_vcards` unchanged
- [x] `Client::by_provider` / `with_service`
- [x] `apps` discovery: Calendar, Reminders, Mail, Drive (GOA Files), Notes (webdav/owncloud identity)
- [x] Drive does not copy GOA tokens to rclone; Notes does not copy tokens to nyx-sync
- [x] `cargo test -p nyx-accounts` and clippy `-D warnings` (2026-10-20)
- No Calendar / Reminders / Mail / Drive / Notes GTK apps

### Phase 5 — Hardening

Missing packages, expired auth, multi-account, session restart, security review, tests where a private bus is feasible.

- [x] Session: `goa-daemon` D-Bus-activates under systemd --user, no GNOME Shell (2026-10-20)
- [x] Session: EDS `Sources5` lists builtins only; crate skips `system-*`, `local`, `birthdays`, `vfolder`, `*-stub`
- [x] Session: empty GOA `GetManagedObjects` (Manager only); `Client::list` returns no accounts
- [x] Security: crate has no `access_token` / `refresh_token` / `client_secret` literals; helper argv is the binary name only
- [x] `cargo test -p nyx-accounts` includes session-bus list tests that tolerate missing GOA
- [ ] Live expired auth / multi-account / session restart (needs throwaway logins)
- [x] B3 helper window on Hyprland (`org.x.GnomeOnlineAccountsGtk`)

---

## Out of scope until asked

- Nyx-branded OAuth app registrations (consent still says GNOME)
- `nyx-accountsd`
- Migrating Git, News, Theater, Pulse, Passwords onto GOA
- rclone using GOA tokens
- Making Notes depend on Nextcloud
- Installing GNOME Shell or GNOME Control Center
- Editing Contacts data on disk

---

## Checks (later phases)

```sh
cargo test -p nyx-accounts
cargo clippy -p nyx-accounts --all-targets -- -D warnings
# Settings still builds with no GOA installed:
cargo test -p nyx-settings
```
