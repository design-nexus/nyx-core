//! NetworkManager (system bus: `system_talk = ["org.freedesktop.NetworkManager"]`).

use super::dbus::{self, Connection, Props, get_all, prop_bool, prop_num, prop_str, prop_strs};
use std::time::Duration;

const NM: &str = "org.freedesktop.NetworkManager";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Kind {
    #[default]
    None,
    Wifi,
    Ethernet,
    Other,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Network {
    pub kind: Kind,
    /// The connection's name (for Wi-Fi, usually the network's).
    pub name: Option<String>,
    /// Wi-Fi signal, 0–100.
    pub strength: Option<u8>,
    pub wifi_enabled: bool,
    pub wifi_available: bool,
    pub vpn: Option<String>,
    /// NetworkManager says the internet is reachable.
    pub online: bool,
    /// NetworkManager isn't running.
    pub missing: bool,
}

impl Network {
    pub fn icon(&self) -> &'static str {
        match self.kind {
            _ if self.missing => "network-offline-symbolic",
            Kind::Wifi => match self.strength.unwrap_or(0) {
                75.. => "network-wireless-signal-excellent-symbolic",
                50..=74 => "network-wireless-signal-good-symbolic",
                25..=49 => "network-wireless-signal-ok-symbolic",
                _ => "network-wireless-signal-weak-symbolic",
            },
            Kind::Ethernet => "network-wired-symbolic",
            Kind::Other => "network-transmit-receive-symbolic",
            Kind::None if self.wifi_available && !self.wifi_enabled => "network-wireless-disabled-symbolic",
            Kind::None => "network-offline-symbolic",
        }
    }
}

fn props(conn: &Connection, path: &str, iface: &str) -> Option<Props> {
    get_all(conn, NM, path, &format!("{NM}.{iface}"))
}

pub fn snapshot(conn: &Connection) -> Network {
    let Some(nm) = get_all(conn, NM, "/org/freedesktop/NetworkManager", NM) else {
        return Network { missing: true, ..Default::default() };
    };
    let mut n = Network {
        wifi_enabled: prop_bool(&nm, "WirelessEnabled").unwrap_or(false),
        online: prop_num(&nm, "Connectivity").unwrap_or(0.0) as u32 == 4 || prop_num(&nm, "State").unwrap_or(0.0) as u32 == 70,
        ..Default::default()
    };
    if let Ok(reply) = conn.call_method(Some(NM), "/org/freedesktop/NetworkManager", Some(NM), "GetDevices", &()) {
        let devices: Vec<zbus::zvariant::OwnedObjectPath> = reply.body().deserialize().unwrap_or_default();
        n.wifi_available = devices.iter().any(|d| props(conn, d.as_str(), "Device").and_then(|p| prop_num(&p, "DeviceType")) == Some(2.0));
    }
    for ac in prop_strs(&nm, "ActiveConnections") {
        if let Some(a) = props(conn, &ac, "Connection.Active") {
            let t = prop_str(&a, "Type").unwrap_or_default();
            if t == "vpn" || t == "wireguard" {
                n.vpn = prop_str(&a, "Id");
            }
        }
    }
    let primary = prop_str(&nm, "PrimaryConnection").unwrap_or_default();
    if primary.len() > 1
        && let Some(a) = props(conn, &primary, "Connection.Active")
    {
        n.name = prop_str(&a, "Id");
        let t = prop_str(&a, "Type").unwrap_or_default();
        n.kind = match t.as_str() {
            "802-11-wireless" => Kind::Wifi,
            "802-3-ethernet" => Kind::Ethernet,
            "vpn" | "wireguard" => {
                // A VPN as the primary connection: show what carries it.
                Kind::Other
            }
            _ => Kind::Other,
        };
        if n.kind == Kind::Wifi
            && let Some(dev) = prop_strs(&a, "Devices").first()
            && let Some(w) = props(conn, dev, "Device.Wireless")
            && let Some(ap) = prop_str(&w, "ActiveAccessPoint")
            && ap.len() > 1
            && let Some(app) = props(conn, &ap, "AccessPoint")
        {
            n.strength = prop_num(&app, "Strength").map(|s| s as u8);
        }
    }
    n
}

/// Call `f` with the state now and whenever it changes.
pub fn watch(f: impl Fn(Network) + Send + 'static) {
    std::thread::spawn(move || {
        let Ok(conn) = Connection::system() else {
            f(Network { missing: true, ..Default::default() });
            return;
        };
        let c = conn.clone();
        let mut last = None;
        let trigger = super::refresher(Duration::from_secs(30), move || {
            let n = snapshot(&c);
            if last.as_ref() != Some(&n) {
                last = Some(n.clone());
                f(n);
            }
        });
        dbus::listen(&conn, Some(NM), None, trigger);
    });
}

pub fn set_wifi(on: bool) {
    crate::spawn(&["nmcli", "radio", "wifi", if on { "on" } else { "off" }]);
}
