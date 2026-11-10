//! fluxd's `state`, as Nyx reads it. Every field is optional on the wire: an older or
//! newer fluxd that lacks one gets the default, never an error.

use serde::{Deserialize, Deserializer};
use std::collections::BTreeMap;

fn null_default<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

/// One `state` snapshot.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Snapshot {
    #[serde(rename = "self")]
    pub this: ThisComputer,
    #[serde(deserialize_with = "null_default")]
    pub devices: Vec<Device>,
    #[serde(deserialize_with = "null_default")]
    pub clipboard: Vec<ClipEntry>,
    #[serde(deserialize_with = "null_default")]
    pub transfers: Vec<Transfer>,
    pub settings: Settings,
}

impl Snapshot {
    pub fn device(&self, id: &str) -> Option<&Device> {
        self.devices.iter().find(|d| d.id == id)
    }

    /// Paired devices, connected first.
    pub fn paired(&self) -> impl Iterator<Item = &Device> {
        self.devices.iter().filter(|d| d.paired)
    }

    /// Whether `device` may use `access` (fluxd's per-device policy over its global
    /// switch, as fluxd itself decides).
    pub fn allowed(&self, device: &str, access: Access) -> bool {
        self.settings.global(access) && self.settings.device_override(device, access).unwrap_or(true)
    }
}

/// This computer, as fluxd announces it.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ThisComputer {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub tcp_port: u16,
    /// fluxd's version; empty from a fluxd before 0.8.
    pub version: String,
    pub pending_version: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Battery {
    pub charge: i32,
    pub charging: bool,
}

/// A device fluxd knows: paired, or connected and not paired.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Device {
    /// Flux's device ID (the CN of its certificate): stable across renames and networks.
    pub id: String,
    pub name: String,
    /// `phone`, `tablet`, `laptop`, `desktop`, `tv`.
    #[serde(rename = "type")]
    pub kind: String,
    pub ip: String,
    #[serde(deserialize_with = "null_default")]
    pub addresses: Vec<String>,
    pub paired: bool,
    pub online: bool,
    /// `none`, `paired`, `requested`, `confirm` or `incoming`.
    pub pair_state: String,
    /// The verification key of an open pairing (16 hex digits).
    pub pair_key: String,
    pub paired_at: String,
    /// Unix time of the last packet.
    pub last_seen: i64,
    pub battery: Option<Battery>,
    #[serde(deserialize_with = "null_default")]
    pub plugins: Vec<String>,
    #[serde(deserialize_with = "null_default")]
    pub notifications: Vec<PhoneNotification>,
    /// The latest message of each text conversation, once fluxd has asked.
    #[serde(deserialize_with = "null_default")]
    pub conversations: Vec<Conversation>,
    /// `android`, `ios`, `macos`, `fluxd`; empty for an app before 0.8.
    pub app: String,
    pub app_version: String,
    pub app_update: String,
    /// A Flux app from before 0.8, which can't connect to this fluxd.
    pub old_app: bool,
    /// 16 hex digits from the device's certificate; tells two devices with one name apart.
    pub fingerprint: String,
}

/// Where a device stands, normalized from `paired`, `online` and `pairState`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Connected, not paired.
    Discovered,
    /// A pairing is open.
    Pairing(PairStep),
    /// Paired and connected.
    Connected,
    /// Paired, not connected now.
    Disconnected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairStep {
    /// This computer asked; the device shows the key.
    Requested,
    /// The device accepted this computer's request; confirm the key here.
    Confirm,
    /// The device asks; compare the key and accept or reject.
    Incoming,
}

/// What a device can do, from its `plugins`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Feature {
    Battery,
    Clipboard,
    /// Takes files, text and links.
    Share,
    /// Sends its notifications.
    Notifications,
    /// Can be rung.
    Ring,
    Sms,
    /// Runs this computer's commands.
    Commands,
    /// Starts its camera or microphone when asked.
    Streams,
}

impl Feature {
    pub const ALL: [Feature; 8] = [
        Feature::Battery,
        Feature::Clipboard,
        Feature::Share,
        Feature::Notifications,
        Feature::Ring,
        Feature::Sms,
        Feature::Commands,
        Feature::Streams,
    ];

    pub fn plugin(self) -> &'static str {
        match self {
            Feature::Battery => "battery",
            Feature::Clipboard => "clipboard",
            Feature::Share => "share",
            Feature::Notifications => "notifications",
            Feature::Ring => "findmyphone",
            Feature::Sms => "sms",
            Feature::Commands => "runcommand",
            Feature::Streams => "streamrequest",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Feature::Battery => "Battery",
            Feature::Clipboard => "Clipboard",
            Feature::Share => "Files and links",
            Feature::Notifications => "Notifications",
            Feature::Ring => "Find my phone",
            Feature::Sms => "Text messages",
            Feature::Commands => "Commands",
            Feature::Streams => "Camera and microphone",
        }
    }
}

impl Device {
    pub fn status(&self) -> Status {
        match self.pair_state.as_str() {
            "requested" => Status::Pairing(PairStep::Requested),
            "confirm" => Status::Pairing(PairStep::Confirm),
            "incoming" => Status::Pairing(PairStep::Incoming),
            _ if self.paired && self.online => Status::Connected,
            _ if self.paired => Status::Disconnected,
            _ => Status::Discovered,
        }
    }

    pub fn has(&self, feature: Feature) -> bool {
        self.plugins.iter().any(|p| p == feature.plugin())
    }

    pub fn features(&self) -> Vec<Feature> {
        Feature::ALL.into_iter().filter(|f| self.has(*f)).collect()
    }

    /// The key in the groups of four the apps show (`5EE6 825F 974E D59A`).
    pub fn key_groups(&self) -> String {
        group_key(&self.pair_key)
    }

    /// An icon name for the device's type.
    pub fn icon(&self) -> &'static str {
        match self.kind.as_str() {
            "phone" | "" => "phone-symbolic",
            "tablet" => "tablet-symbolic",
            "tv" => "video-display-symbolic",
            _ => "computer-symbolic",
        }
    }

    /// `Pixel 8 (A1B2…)` when another known device has the same name.
    pub fn display_name(&self, all: &[Device]) -> String {
        let twins = all.iter().filter(|d| d.name.eq_ignore_ascii_case(&self.name)).count();
        if twins > 1 && self.fingerprint.len() >= 4 {
            format!("{} ({}…)", self.name, &self.fingerprint[..4])
        } else {
            self.name.clone()
        }
    }
}

pub fn group_key(key: &str) -> String {
    let clean: Vec<char> = key.chars().filter(|c| !c.is_whitespace()).collect();
    clean.chunks(4).map(|c| c.iter().collect::<String>()).collect::<Vec<_>>().join(" ")
}

/// A notification the phone has now.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PhoneNotification {
    pub id: String,
    pub app: String,
    pub title: String,
    pub text: String,
    pub time: i64,
    pub reply_id: String,
    #[serde(deserialize_with = "null_default")]
    pub actions: Vec<String>,
    pub dismissable: bool,
}

/// A text conversation on the phone, by its latest message.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Conversation {
    pub thread: i64,
    /// The contact's name, or the address when the phone has no contact.
    pub name: String,
    pub address: String,
    #[serde(deserialize_with = "null_default")]
    pub addresses: Vec<String>,
}

/// One entry of fluxd's clipboard history.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ClipEntry {
    pub id: String,
    /// The text, or its first 1024 bytes when `truncated` (the full text comes from
    /// [`crate::Client::clip_text`]).
    pub text: String,
    pub truncated: bool,
    pub size: usize,
    /// The path of a copied image (PNG, JPEG, GIF or WebP); `text` is then empty.
    pub image: String,
    /// `in` from a device, `out` from this computer.
    pub dir: String,
    /// The device that sent an `in` entry.
    pub device: String,
    pub device_name: String,
    /// Empty for clipboard sync; `share`, `scan` or `link` otherwise.
    pub source: String,
    pub time: i64,
    pub pinned: bool,
}

impl ClipEntry {
    /// A copy on a device that clipboard sync brought here.
    pub fn is_incoming_copy(&self) -> bool {
        self.dir == "in" && self.source.is_empty() && !self.device.is_empty()
    }
}

/// A file going to or coming from a device.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Transfer {
    pub id: String,
    pub device: String,
    #[serde(rename = "deviceName")]
    pub device_name: String,
    pub name: String,
    /// Where an incoming file is saved, once known.
    pub path: String,
    pub size: i64,
    pub done: i64,
    /// `in` or `out`.
    pub dir: String,
    /// `waiting`, `queued`, `active`, `done`, `failed` or `canceled`.
    pub state: String,
    /// Bytes per second while active.
    pub rate: i64,
    #[serde(deserialize_with = "null_default")]
    pub error: String,
    pub time: i64,
}

impl Transfer {
    pub fn finished(&self) -> bool {
        matches!(self.state.as_str(), "done" | "failed" | "canceled")
    }

    pub fn incoming(&self) -> bool {
        self.dir == "in"
    }
}

/// What fluxd lets devices do: global switches, and per-device overrides.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Access {
    /// Clipboard sync and clips sent by hand.
    Clipboard,
    /// The device's notifications shown here.
    Notifications,
    /// Browse PC: the device reads this computer's home folder.
    ShareHome,
    RemoteInput,
    RemoteDesktop,
}

impl Access {
    pub const ALL: [Access; 5] =
        [Access::Clipboard, Access::Notifications, Access::ShareHome, Access::RemoteInput, Access::RemoteDesktop];

    /// The key of `device.settings.set` and of `deviceRules`.
    pub fn key(self) -> &'static str {
        match self {
            Access::Clipboard => "clipboard",
            Access::Notifications => "notifications",
            Access::ShareHome => "shareHome",
            Access::RemoteInput => "remoteInput",
            Access::RemoteDesktop => "remoteDesktop",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Access::Clipboard => "Clipboard",
            Access::Notifications => "Notifications",
            Access::ShareHome => "Browse this computer",
            Access::RemoteInput => "Control the pointer and keyboard",
            Access::RemoteDesktop => "Show this screen",
        }
    }
}

/// fluxd's `settings`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    /// fluxd syncs the clipboard itself (off while Nyx Clipboard does it).
    pub auto_clipboard: bool,
    pub notifications: bool,
    pub share_home: bool,
    pub remote_input: bool,
    pub remote_desktop: bool,
    pub download_dir: String,
    pub check_updates: bool,
    /// Device ID → access key → allowed.
    #[serde(deserialize_with = "null_default")]
    pub device_rules: BTreeMap<String, BTreeMap<String, bool>>,
}

impl Settings {
    /// The global switch behind an access. Clipboard access by hand needs no switch.
    pub fn global(&self, access: Access) -> bool {
        match access {
            Access::Clipboard => true,
            Access::Notifications => self.notifications,
            Access::ShareHome => self.share_home,
            Access::RemoteInput => self.remote_input,
            Access::RemoteDesktop => self.remote_desktop,
        }
    }

    /// The device's own choice, if it has one (otherwise it follows the switch).
    pub fn device_override(&self, device: &str, access: Access) -> Option<bool> {
        self.device_rules.get(device).and_then(|r| r.get(access.key())).copied()
    }
}
