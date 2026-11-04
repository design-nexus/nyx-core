//! BlueZ (system bus: `system_talk = ["org.bluez"]`).

use super::dbus::{self, Connection, prop_bool, prop_num, prop_str};
use std::collections::HashMap;
use std::time::Duration;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

const BLUEZ: &str = "org.bluez";

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Device {
    pub path: String,
    pub name: String,
    pub icon: Option<String>,
    pub battery: Option<u8>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Bluetooth {
    /// There's an adapter.
    pub available: bool,
    pub powered: bool,
    pub adapter: Option<String>,
    pub connected: Vec<Device>,
}

impl Bluetooth {
    pub fn icon(&self) -> &'static str {
        match (self.powered, self.connected.is_empty()) {
            (false, _) => "bluetooth-disabled-symbolic",
            (true, true) => "bluetooth-symbolic",
            (true, false) => "bluetooth-active-symbolic",
        }
    }
}

type Objects = HashMap<OwnedObjectPath, HashMap<String, HashMap<String, OwnedValue>>>;

pub fn snapshot(conn: &Connection) -> Bluetooth {
    let mut b = Bluetooth::default();
    let Ok(reply) = conn.call_method(Some(BLUEZ), "/", Some("org.freedesktop.DBus.ObjectManager"), "GetManagedObjects", &()) else {
        return b;
    };
    let objects: Objects = reply.body().deserialize().unwrap_or_default();
    let mut paths: Vec<&OwnedObjectPath> = objects.keys().collect();
    paths.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    for path in paths {
        let ifaces = &objects[path];
        if let Some(a) = ifaces.get("org.bluez.Adapter1")
            && b.adapter.is_none()
        {
            b.available = true;
            b.powered = prop_bool(a, "Powered").unwrap_or(false);
            b.adapter = Some(path.to_string());
        }
        if let Some(d) = ifaces.get("org.bluez.Device1")
            && prop_bool(d, "Connected").unwrap_or(false)
        {
            b.connected.push(Device {
                path: path.to_string(),
                name: prop_str(d, "Alias").or_else(|| prop_str(d, "Name")).unwrap_or_else(|| "Device".into()),
                icon: prop_str(d, "Icon"),
                battery: ifaces.get("org.bluez.Battery1").and_then(|bat| prop_num(bat, "Percentage")).map(|p| p as u8),
            });
        }
    }
    b
}

pub fn watch(f: impl Fn(Bluetooth) + Send + 'static) {
    std::thread::spawn(move || {
        let Ok(conn) = Connection::system() else {
            f(Bluetooth::default());
            return;
        };
        let c = conn.clone();
        let mut last = None;
        let trigger = super::refresher(Duration::from_secs(60), move || {
            let b = snapshot(&c);
            if last.as_ref() != Some(&b) {
                last = Some(b.clone());
                f(b);
            }
        });
        dbus::listen(&conn, Some(BLUEZ), None, trigger);
    });
}

pub fn set_powered(adapter: &str, on: bool) -> bool {
    let Ok(conn) = Connection::system() else { return false };
    dbus::set(&conn, BLUEZ, adapter, "org.bluez.Adapter1", "Powered", Value::from(on))
}

pub fn disconnect(device: &str) -> bool {
    let Ok(conn) = Connection::system() else { return false };
    conn.call_method(Some(BLUEZ), device, Some("org.bluez.Device1"), "Disconnect", &()).is_ok()
}
