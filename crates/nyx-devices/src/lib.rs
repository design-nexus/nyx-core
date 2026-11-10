//! Remembered devices.
//!
//! A device the user configured (a monitor's scale, a mouse's speed, a headset's
//! equalizer, a drive's mount) keeps its preferences while it's unplugged, and gets them
//! back when it returns. Never-configured devices aren't remembered.
//!
//! The [`Index`] (`~/.config/nyx/devices.json`) knows each configured device: its
//! [`DeviceId`], a name to show, what the owning subsystem matches it by, and when it was
//! last seen. The preferences themselves stay with their owners (Settings' Hyprland rules,
//! Shell Core's per-screen settings, the dock's per-monitor section, WirePlumber rules,
//! the mount table), keyed by the same id, so there's no second copy of them. Forgetting
//! a device asks every [`Owner`] to drop its part.
//!
//! The subsystems restore preferences themselves on reconnection: Hyprland matches
//! `desc:` monitor rules and `hl.device` names, WirePlumber matches `node.name`, BlueZ
//! keeps pairings. This crate never applies anything.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const SCHEMA: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Category {
    Monitor,
    AudioOutput,
    AudioInput,
    Mouse,
    Keyboard,
    Touchpad,
    Bluetooth,
    Storage,
    /// A phone, tablet or computer paired through Flux (Nyx Connect), by Flux's device ID.
    Linked,
}

impl Category {
    pub fn prefix(self) -> &'static str {
        match self {
            Category::Monitor => "monitor",
            Category::AudioOutput => "audio-out",
            Category::AudioInput => "audio-in",
            Category::Mouse => "mouse",
            Category::Keyboard => "keyboard",
            Category::Touchpad => "touchpad",
            Category::Bluetooth => "bluetooth",
            Category::Storage => "storage",
            Category::Linked => "linked",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Category::Monitor => "Display",
            Category::AudioOutput => "Output",
            Category::AudioInput => "Input",
            Category::Mouse => "Mouse",
            Category::Keyboard => "Keyboard",
            Category::Touchpad => "Touchpad",
            Category::Bluetooth => "Bluetooth device",
            Category::Storage => "Drive",
            Category::Linked => "Linked device",
        }
    }
}

/// `category:key`. The key is what follows the device rather than the port: a monitor's
/// make, model and serial (`nyx_desktop::Monitor::stable_id`, which Shell Core and the dock
/// key their per-screen settings by), PipeWire's `node.name`, Hyprland's input device name,
/// a filesystem UUID, a hash of a Bluetooth address, or Flux's device ID.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DeviceId(pub String);

impl DeviceId {
    pub fn new(category: Category, key: &str) -> Self {
        DeviceId(format!("{}:{key}", category.prefix()))
    }

    /// A monitor's id: make, model and serial, or the connector when it reports none.
    /// The same as `nyx_desktop::Monitor::stable_id`.
    pub fn monitor(make: &str, model: &str, serial: &str, connector: &str) -> Self {
        let key = if make.trim().is_empty() && model.trim().is_empty() {
            connector.to_string()
        } else {
            format!("{}|{}|{}", make.trim(), model.trim(), serial.trim())
        };
        DeviceId::new(Category::Monitor, &key)
    }

    /// The part after the category: what owners key their preferences by.
    pub fn key(&self) -> &str {
        self.0.split_once(':').map(|(_, k)| k).unwrap_or(&self.0)
    }

    pub fn category(&self) -> Option<Category> {
        let prefix = self.0.split_once(':')?.0;
        [
            Category::Monitor,
            Category::AudioOutput,
            Category::AudioInput,
            Category::Mouse,
            Category::Keyboard,
            Category::Touchpad,
            Category::Bluetooth,
            Category::Storage,
            Category::Linked,
        ]
        .into_iter()
        .find(|c| c.prefix() == prefix)
    }
}

/// How a monitor's identity can be trusted: without a serial, two of the same model look
/// alike.
pub fn monitor_is_weak(serial: &str) -> bool {
    serial.trim().is_empty() || serial.trim() == "0"
}

/// Hyprland's `desc:` rule for a monitor: its description without the connector.
pub fn monitor_match(description: &str) -> String {
    let d = description.trim();
    let d = match d.rfind(" (") {
        Some(i) if d.ends_with(')') => &d[..i],
        _ => d,
    };
    format!("desc:{}", d.trim())
}

/// One remembered device.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    pub category: Category,
    /// What to call it (a monitor's make and model, a sink's description).
    pub name: String,
    /// What the owning subsystem matches it by (`desc:…`, a `node.name`, a device name);
    /// empty when the id itself is enough.
    #[serde(default, rename = "match")]
    pub matches: String,
    /// The port it was last on, for monitors.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connector: Option<String>,
    /// Seconds since 1970.
    pub last_seen: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct File {
    schema: u32,
    /// Mixed into Bluetooth addresses before hashing.
    #[serde(default)]
    salt: String,
    #[serde(default)]
    devices: BTreeMap<DeviceId, Profile>,
}

/// Something that keeps preferences for devices.
pub trait Owner {
    /// What to call it in an error ("Hyprland display rules", "the dock").
    fn name(&self) -> &str;
    /// Whether it has preferences for this device.
    fn holds(&self, id: &DeviceId) -> bool;
    /// Drop them.
    fn forget(&self, id: &DeviceId) -> Result<(), String>;
}

/// What's connected now, for [`Index::remembered`] and [`ambiguous`].
#[derive(Debug, Clone, PartialEq)]
pub struct Connected {
    pub id: DeviceId,
    /// Whether the identity could belong to another device too (no serial).
    pub weak: bool,
}

/// Ids shared by more than one connected weakly identified device: Settings mustn't apply
/// a profile to either, and asks the user which is which.
pub fn ambiguous(connected: &[Connected]) -> Vec<DeviceId> {
    let mut count: BTreeMap<&DeviceId, (usize, bool)> = BTreeMap::new();
    for c in connected {
        let e = count.entry(&c.id).or_insert((0, false));
        e.0 += 1;
        e.1 |= c.weak;
    }
    count.into_iter().filter(|(_, (n, weak))| *n > 1 && *weak).map(|(id, _)| id.clone()).collect()
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// The remembered devices.
#[derive(Debug, Clone)]
pub struct Index {
    path: PathBuf,
    file: File,
}

impl Index {
    pub fn standard_path() -> PathBuf {
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config"));
        config.join("nyx/devices.json")
    }

    pub fn load_standard() -> Self {
        Index::load(&Index::standard_path())
    }

    /// The index at `path`. A missing file is empty; one that can't be read as an index
    /// is kept beside it as `devices.json.broken-<time>` and the index starts empty.
    pub fn load(path: &Path) -> Self {
        let empty = || File { schema: SCHEMA, salt: String::new(), devices: BTreeMap::new() };
        let file = match std::fs::read_to_string(path) {
            Ok(text) => match serde_json::from_str::<File>(&text) {
                Ok(f) => f,
                Err(_) => {
                    let _ = std::fs::rename(path, path.with_file_name(format!("devices.json.broken-{}", now())));
                    empty()
                }
            },
            Err(_) => empty(),
        };
        Index { path: path.to_path_buf(), file }
    }

    pub fn save(&self) -> std::io::Result<()> {
        let dir = self.path.parent().unwrap_or(Path::new("."));
        std::fs::create_dir_all(dir)?;
        let tmp = dir.join(format!(".devices.json.{}", std::process::id()));
        std::fs::write(&tmp, serde_json::to_string_pretty(&self.file).unwrap_or_default())?;
        std::fs::rename(tmp, &self.path)
    }

    pub fn get(&self, id: &DeviceId) -> Option<&Profile> {
        self.file.devices.get(id)
    }

    pub fn devices(&self) -> impl Iterator<Item = (&DeviceId, &Profile)> {
        self.file.devices.iter()
    }

    /// Remember a device the user just configured (or refresh one already remembered).
    pub fn remember(&mut self, id: DeviceId, category: Category, name: &str, matches: &str, connector: Option<&str>) {
        let p = self.file.devices.entry(id).or_insert_with(|| Profile {
            category,
            name: name.to_string(),
            matches: matches.to_string(),
            connector: None,
            last_seen: 0,
        });
        p.name = name.to_string();
        if !matches.is_empty() {
            p.matches = matches.to_string();
        }
        if connector.is_some() {
            p.connector = connector.map(str::to_string);
        }
        p.last_seen = now();
    }

    /// Note that these remembered devices are connected now. Returns whether anything
    /// changed (so the caller saves).
    pub fn seen(&mut self, connected: &[(DeviceId, Option<String>)]) -> bool {
        let t = now();
        let mut changed = false;
        for (id, connector) in connected {
            if let Some(p) = self.file.devices.get_mut(id) {
                // Once a minute is often enough to say "last seen".
                if t.saturating_sub(p.last_seen) > 60 || (connector.is_some() && p.connector != *connector) {
                    p.last_seen = t;
                    if connector.is_some() {
                        p.connector = connector.clone();
                    }
                    changed = true;
                }
            }
        }
        changed
    }

    /// Remembered devices of a category that aren't connected now, most recently seen
    /// first.
    pub fn remembered(&self, category: Category, connected: &[DeviceId]) -> Vec<(DeviceId, Profile)> {
        let mut out: Vec<(DeviceId, Profile)> = self
            .file
            .devices
            .iter()
            .filter(|(id, p)| p.category == category && !connected.contains(id))
            .map(|(id, p)| (id.clone(), p.clone()))
            .collect();
        out.sort_by_key(|p| std::cmp::Reverse(p.1.last_seen));
        out
    }

    /// Drop a device's preferences from every owner that has them, then the device. An
    /// owner that fails leaves the device remembered (so nothing is half forgotten
    /// silently) and its error is returned.
    pub fn forget(&mut self, id: &DeviceId, owners: &[&dyn Owner]) -> Result<(), Vec<String>> {
        let mut errors = Vec::new();
        for o in owners {
            if o.holds(id)
                && let Err(e) = o.forget(id)
            {
                errors.push(format!("{}: {e}", o.name()));
            }
        }
        if !errors.is_empty() {
            return Err(errors);
        }
        self.file.devices.remove(id);
        Ok(())
    }

    /// The id for a Bluetooth address: hashed with this index's own salt, so the file
    /// doesn't hold the address itself.
    pub fn bluetooth_id(&mut self, address: &str) -> DeviceId {
        if self.file.salt.is_empty() {
            self.file.salt = salt();
        }
        DeviceId::new(
            Category::Bluetooth,
            &format!("{:016x}", fnv1a(&format!("{}{}", self.file.salt, address.to_ascii_uppercase()))),
        )
    }
}

fn fnv1a(s: &str) -> u64 {
    s.bytes().fold(0xcbf29ce484222325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3))
}

fn salt() -> String {
    use std::io::Read;
    let mut bytes = [0u8; 16];
    if std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut bytes)).is_err() {
        bytes = now().to_le_bytes().repeat(2).try_into().unwrap_or([7; 16]);
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn monitor(make: &str, model: &str, serial: &str, name: &str) -> nyx_desktop::Monitor {
        nyx_desktop::Monitor {
            id: 0,
            name: name.into(),
            description: format!("{make} {model} {serial} ({name})"),
            make: make.into(),
            model: model.into(),
            serial: serial.into(),
            width: 0,
            height: 0,
            x: 0,
            y: 0,
            scale: 1.0,
            transform: 0,
            refresh_rate: 60.0,
            focused: false,
            disabled: false,
            active_workspace: 1,
            special_workspace: None,
            reserved: [0; 4],
        }
    }

    #[test]
    fn a_monitor_keeps_its_id_on_another_port() {
        let a = monitor("Dell Inc.", "U2723QE", "ABC123", "DP-1");
        let b = monitor("Dell Inc.", "U2723QE", "ABC123", "HDMI-A-1");
        let id = |m: &nyx_desktop::Monitor| DeviceId::monitor(&m.make, &m.model, &m.serial, &m.name);
        assert_eq!(id(&a), id(&b));
        for m in [&a, &monitor("Samsung", "X", "", "eDP-1"), &monitor("", "", "", "HEADLESS-2")] {
            assert_eq!(id(m).key(), m.stable_id(), "the same id Shell Core and the dock use");
        }
        assert_eq!(id(&a).category(), Some(Category::Monitor));
        assert_eq!(monitor_match(&a.description), "desc:Dell Inc. U2723QE ABC123");
        assert!(!monitor_is_weak(&a.serial));
        assert!(monitor_is_weak(""));
    }

    #[test]
    fn two_identical_monitors_without_a_serial_are_ambiguous() {
        let id = DeviceId::new(Category::Monitor, "Acme|M24|");
        let other = DeviceId::new(Category::Monitor, "Dell|U27|S1");
        let c = vec![
            Connected { id: id.clone(), weak: true },
            Connected { id: id.clone(), weak: true },
            Connected { id: other.clone(), weak: false },
        ];
        assert_eq!(ambiguous(&c), vec![id]);
        let two_mice = vec![Connected { id: other.clone(), weak: false }, Connected { id: other, weak: false }];
        assert!(ambiguous(&two_mice).is_empty(), "two of the same mouse share their preferences by design");
    }

    struct Rules {
        held: RefCell<Vec<DeviceId>>,
        fail: bool,
    }

    impl Owner for Rules {
        fn name(&self) -> &str {
            "rules"
        }
        fn holds(&self, id: &DeviceId) -> bool {
            self.held.borrow().contains(id)
        }
        fn forget(&self, id: &DeviceId) -> Result<(), String> {
            if self.fail {
                return Err("read-only".into());
            }
            self.held.borrow_mut().retain(|h| h != id);
            Ok(())
        }
    }

    #[test]
    fn remember_see_list_and_forget() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("devices.json");
        let mut ix = Index::load(&path);
        let dell = DeviceId::new(Category::Monitor, "Dell|U27|S1");
        let panel = DeviceId::new(Category::Monitor, "Samsung|ATNA|");
        ix.remember(dell.clone(), Category::Monitor, "Dell U27", "desc:Dell U27 S1", Some("DP-1"));
        ix.remember(panel.clone(), Category::Monitor, "Built-in display", "desc:Samsung ATNA", Some("eDP-1"));
        ix.save().unwrap();
        let mut ix = Index::load(&path);
        assert_eq!(ix.remembered(Category::Monitor, std::slice::from_ref(&panel)).len(), 1);
        assert!(ix.remembered(Category::Mouse, &[]).is_empty());
        assert!(ix.seen(&[(dell.clone(), Some("HDMI-A-1".into()))]));
        assert_eq!(ix.get(&dell).unwrap().connector.as_deref(), Some("HDMI-A-1"));

        let rules = Rules { held: RefCell::new(vec![dell.clone()]), fail: true };
        assert!(ix.forget(&dell, &[&rules]).is_err());
        assert!(ix.get(&dell).is_some(), "not half forgotten");
        let rules = Rules { held: RefCell::new(vec![dell.clone()]), fail: false };
        ix.forget(&dell, &[&rules]).unwrap();
        assert!(ix.get(&dell).is_none());
        assert!(rules.held.borrow().is_empty());
    }

    #[test]
    fn bluetooth_addresses_are_not_stored() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("devices.json");
        let mut ix = Index::load(&path);
        let id = ix.bluetooth_id("aa:bb:cc:dd:ee:ff");
        assert_eq!(id, ix.bluetooth_id("AA:BB:CC:DD:EE:FF"));
        ix.remember(id, Category::Bluetooth, "Headphones", "", None);
        ix.save().unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.to_ascii_uppercase().contains("AA:BB:CC"));
    }

    #[test]
    fn a_broken_index_is_kept_aside() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("devices.json");
        std::fs::write(&path, "{ not json").unwrap();
        let ix = Index::load(&path);
        assert_eq!(ix.devices().count(), 0);
        assert!(
            std::fs::read_dir(d.path())
                .unwrap()
                .flatten()
                .any(|e| e.file_name().to_string_lossy().starts_with("devices.json.broken-"))
        );
    }
}
