//! Nyx Connect: phones and other devices paired through upstream Flux, for Nyx.
//!
//! `fluxd` does discovery, pairing, transport and the phone features, and is the only
//! authority on trust (`~/.local/share/flux/devices.json`) and per-device access
//! (`device.settings`). This crate is the thin layer over its socket of JSON lines:
//!
//! - [`Client`] sends requests ([`Client::pair_request`], [`Client::share_files`],
//!   [`Client::set_access`], ...), with typed [`Error`]s.
//! - [`Watch`] follows fluxd on a thread of its own and hands each [`Snapshot`] on,
//!   reconnecting when fluxd restarts and saying when it's [`Event::Unavailable`].
//! - [`Snapshot`] normalizes devices: [`Device::status`], [`Device::features`],
//!   [`Snapshot::allowed`].
//! - [`Prefs`] is Nyx's own choice on top (whether Nyx Clipboard does the syncing), in
//!   NyxOS's configuration layers.
//! - [`remember`], [`forget`] keep paired devices in `nyx-devices`' remembered devices.
//!
//! There's no Nyx Connect daemon: each service that needs fluxd links this and opens its
//! own connection. See nyx/docs/desktop/CONNECT.md.

mod client;
mod model;
mod watch;

pub use client::{Client, Conn, Error, ErrorKind, Paired, socket_path};
pub use model::{
    Access, Battery, ClipEntry, Conversation, Device, Feature, PairStep, PhoneNotification, Settings, Snapshot, Status,
    ThisComputer, Transfer, group_key,
};
pub use watch::{Event, Watch};

use nyx_devices::{Category, DeviceId, Index, Owner};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The `nyx-devices` id of a Flux device.
pub fn device_id(flux_id: &str) -> DeviceId {
    DeviceId::new(Category::Linked, flux_id)
}

/// Note that the user configured this device (paired it, changed its access), so it's
/// listed under Remembered Devices while it's away.
pub fn remember(index: &mut Index, device: &Device) {
    index.remember(device_id(&device.id), Category::Linked, &device.name, "", None);
}

/// Refresh "last seen" for the connected paired devices. Returns whether to save.
pub fn seen(index: &mut Index, snapshot: &Snapshot) -> bool {
    let connected: Vec<(DeviceId, Option<String>)> =
        snapshot.paired().filter(|d| d.online).map(|d| (device_id(&d.id), None)).collect();
    index.seen(&connected)
}

/// Remembered linked devices fluxd no longer has paired (unpaired on the phone, or
/// fluxd's trust reset): shown so they can be forgotten or paired again.
pub fn orphans(index: &Index, snapshot: &Snapshot) -> Vec<(DeviceId, nyx_devices::Profile)> {
    index
        .remembered(Category::Linked, &[])
        .into_iter()
        .filter(|(id, _)| !snapshot.devices.iter().any(|d| d.paired && id.key() == d.id))
        .collect()
}

/// fluxd's per-device access choices, as an [`Owner`] of the device's preferences.
pub struct FluxAccess<'a> {
    pub client: &'a Client,
    pub snapshot: &'a Snapshot,
}

impl Owner for FluxAccess<'_> {
    fn name(&self) -> &str {
        "Flux's access settings"
    }

    fn holds(&self, id: &DeviceId) -> bool {
        self.snapshot.settings.device_rules.get(id.key()).is_some_and(|r| !r.is_empty())
    }

    fn forget(&self, id: &DeviceId) -> Result<(), String> {
        // fluxd changes access only for a paired device.
        if !self.snapshot.devices.iter().any(|d| d.id == id.key() && d.paired) {
            return Ok(());
        }
        for access in Access::ALL {
            if self.snapshot.settings.device_override(id.key(), access).is_some() {
                self.client.set_access(id.key(), access, None).map_err(|e| e.message)?;
            }
        }
        Ok(())
    }
}

/// Forget a device: drop its access choices, unpair it if it's still paired (so no
/// trust is left behind), and remove it from the remembered devices. Unpairing alone
/// ([`Client::unpair`]) keeps the device's choices for when it pairs again.
pub fn forget(client: &Client, snapshot: &Snapshot, index: &mut Index, flux_id: &str) -> Result<(), String> {
    let id = device_id(flux_id);
    let access = FluxAccess { client, snapshot };
    index.forget(&id, &[&access]).map_err(|e| e.join("; "))?;
    if snapshot.devices.iter().any(|d| d.id == flux_id && d.paired) {
        client.unpair(flux_id).map_err(|e| e.message)?;
    }
    Ok(())
}

/// Nyx's choices about Flux, in `connect.toml`: NyxOS's defaults
/// (`/etc/nyxos/defaults`), the administrator's overrides (`/etc/nyxos/overrides`), then
/// the user's own file (`~/.config/nyx/connect.toml`), later layers winning.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    /// Nyx Clipboard syncs with the devices that have clipboard access, after its own
    /// filters (excluded apps, secrets, private mode). fluxd's own clipboard sync is off
    /// while this is on.
    pub clipboard_sync: bool,
    /// What fluxd's `autoClipboard` was before Nyx took over, to give it back.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flux_auto_clipboard: Option<bool>,
}

impl Prefs {
    pub fn user_path() -> PathBuf {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config"))
            .join("nyx/connect.toml")
    }

    pub fn layers() -> Vec<PathBuf> {
        vec![
            PathBuf::from("/etc/nyxos/defaults/connect.toml"),
            PathBuf::from("/etc/nyxos/overrides/connect.toml"),
            Prefs::user_path(),
        ]
    }

    pub fn load() -> Prefs {
        Prefs::load_from(&Prefs::layers())
    }

    pub fn load_from(layers: &[PathBuf]) -> Prefs {
        let mut merged = toml::Table::new();
        for path in layers {
            if let Some(table) = read_table(path) {
                merged.extend(table);
            }
        }
        toml::Value::Table(merged).try_into().unwrap_or_default()
    }

    /// Change one key in the user's own file (`None` removes it, so the layers below
    /// show through), leaving its other keys as they are.
    pub fn set_user(key: &str, value: Option<toml::Value>) -> std::io::Result<()> {
        Prefs::set_in(&Prefs::user_path(), key, value)
    }

    pub fn set_in(path: &Path, key: &str, value: Option<toml::Value>) -> std::io::Result<()> {
        let mut table = read_table(path).unwrap_or_default();
        match value {
            Some(v) => table.insert(key.to_string(), v),
            None => table.remove(key),
        };
        let dir = path.parent().unwrap_or(Path::new("."));
        std::fs::create_dir_all(dir)?;
        let tmp = dir.join(format!(".connect.toml.{}", std::process::id()));
        std::fs::write(&tmp, toml::to_string(&table).unwrap_or_default())?;
        std::fs::rename(tmp, path)
    }
}

fn read_table(path: &Path) -> Option<toml::Table> {
    std::fs::read_to_string(path).ok()?.parse::<toml::Table>().ok()
}

/// Turn Nyx Clipboard's sync on or off, handing fluxd's own sync over: off while Nyx
/// syncs, back to what it was when Nyx stops.
pub fn set_clipboard_sync(client: &Client, on: bool) -> Result<(), String> {
    let prefs = Prefs::load();
    let io = |e: std::io::Error| e.to_string();
    if on {
        let flux = client.state().map(|s| s.settings.auto_clipboard).map_err(|e| e.message)?;
        if prefs.flux_auto_clipboard.is_none() {
            Prefs::set_user("flux_auto_clipboard", Some(toml::Value::Boolean(flux))).map_err(io)?;
        }
        if flux {
            client.set("autoClipboard", serde_json::Value::Bool(false)).map_err(|e| e.message)?;
        }
    } else if let Some(before) = prefs.flux_auto_clipboard {
        client.set("autoClipboard", serde_json::Value::Bool(before)).map_err(|e| e.message)?;
        Prefs::set_user("flux_auto_clipboard", None).map_err(io)?;
    }
    Prefs::set_user("clipboard_sync", Some(toml::Value::Boolean(on))).map_err(io)
}

/// While Nyx Clipboard syncs, fluxd's own sync stays off (a reset `config.toml` or
/// another client could turn it back on). Returns whether it had to be turned off.
pub fn keep_flux_sync_off(client: &Client, snapshot: &Snapshot, prefs: &Prefs) -> bool {
    prefs.clipboard_sync
        && snapshot.settings.auto_clipboard
        && client.set("autoClipboard", serde_json::Value::Bool(false)).is_ok()
}

#[cfg(test)]
mod tests;
