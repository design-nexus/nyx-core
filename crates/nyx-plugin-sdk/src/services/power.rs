//! UPower's display device, the battery the bar shows (system bus:
//! `system_talk = ["org.freedesktop.UPower"]`), and the power mode
//! (`net.hadess.PowerProfiles`).

use super::dbus::{self, Connection, get_all, prop_bool, prop_num, prop_str};
use std::time::Duration;

const UPOWER: &str = "org.freedesktop.UPower";

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Battery {
    pub present: bool,
    pub percent: f64,
    pub charging: bool,
    pub full: bool,
    /// Seconds until empty or full.
    pub time_left: Option<i64>,
    pub profile: Option<String>,
}

impl Battery {
    pub fn icon(&self) -> String {
        if self.full {
            return "battery-level-100-charged-symbolic".into();
        }
        let level = ((self.percent / 10.0).round() as i64 * 10).clamp(0, 100);
        format!("battery-level-{level}{}-symbolic", if self.charging { "-charging" } else { "" })
    }
}

pub fn snapshot(conn: &Connection) -> Battery {
    let Some(p) = get_all(conn, UPOWER, "/org/freedesktop/UPower/devices/DisplayDevice", "org.freedesktop.UPower.Device") else {
        return Battery::default();
    };
    // State: 1 charging, 2 discharging, 4 fully charged, 5 pending charge.
    let state = prop_num(&p, "State").unwrap_or(0.0) as u32;
    // Type 2 is a battery; a desktop's display device is often Type 0 and not present.
    let present = prop_bool(&p, "IsPresent").unwrap_or(false) && prop_num(&p, "Type").unwrap_or(0.0) as u32 == 2;
    let time = match state {
        1 => prop_num(&p, "TimeToFull"),
        2 => prop_num(&p, "TimeToEmpty"),
        _ => None,
    }
    .map(|t| t as i64)
    .filter(|t| *t > 0);
    let profile = get_all(conn, "net.hadess.PowerProfiles", "/net/hadess/PowerProfiles", "net.hadess.PowerProfiles")
        .and_then(|pp| prop_str(&pp, "ActiveProfile"));
    Battery { present, percent: prop_num(&p, "Percentage").unwrap_or(0.0), charging: state == 1 || state == 5, full: state == 4, time_left: time, profile }
}

pub fn watch(f: impl Fn(Battery) + Send + 'static) {
    std::thread::spawn(move || {
        let Ok(conn) = Connection::system() else {
            f(Battery::default());
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
        dbus::listen(&conn, Some(UPOWER), None, trigger.clone());
        dbus::listen(&conn, Some("net.hadess.PowerProfiles"), None, trigger);
    });
}

/// `power-saver`, `balanced` or `performance`.
pub fn set_profile(profile: &str) -> bool {
    let Ok(conn) = Connection::system() else { return false };
    dbus::set(&conn, "net.hadess.PowerProfiles", "/net/hadess/PowerProfiles", "net.hadess.PowerProfiles", "ActiveProfile", profile.into())
}

/// "1 h 05 min", "12 min".
pub fn duration(secs: i64) -> String {
    let (h, m) = (secs / 3600, secs % 3600 / 60);
    if h > 0 { format!("{h} h {m:02} min") } else { format!("{m} min") }
}
