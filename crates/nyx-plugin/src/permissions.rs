//! Hashing permissions (a grant is for exactly what was approved) and saying what they
//! mean in plain words.

use crate::manifest::Permissions;
use sha2::{Digest, Sha256};

/// The hash a grant stores. Any change to the permissions changes it, so an update
/// that asks for more stays off until it's approved again.
pub fn hash(p: &Permissions) -> String {
    let mut p = p.clone();
    p.read.sort();
    p.session_talk.sort();
    p.system_talk.sort();
    p.host_commands.sort();
    let json = serde_json::to_string(&p).unwrap_or_default();
    format!("sha256:{:x}", Sha256::digest(json.as_bytes()))
}

/// D-Bus services on the system bus that can change the machine, with what they can do.
const SYSTEM_SERVICES: &[(&str, &str)] = &[
    ("org.freedesktop.NetworkManager", "change network settings and connections"),
    ("org.bluez", "pair, connect and power Bluetooth devices"),
    ("org.freedesktop.UPower", "read battery and power details"),
    ("net.hadess.PowerProfiles", "change the power mode"),
    ("org.freedesktop.UPower.PowerProfiles", "change the power mode"),
    ("org.freedesktop.login1", "suspend, restart or shut down, and lock sessions"),
    ("org.freedesktop.systemd1", "start and stop system services"),
    ("org.freedesktop.timedate1", "change the time and time zone"),
    ("org.freedesktop.hostname1", "change the computer's name"),
    ("org.freedesktop.UDisks2", "mount, format and unlock disks"),
];

/// One line per permission, for the approval prompt and `nyx plugin info`.
pub fn describe(p: &Permissions) -> Vec<String> {
    let mut out = vec![];
    if p.network {
        out.push("Can use the network (send and receive data on the internet)".into());
    }
    for path in &p.read {
        out.push(format!("Can read {path}"));
    }
    for name in &p.session_talk {
        out.push(format!("Can talk to {name} on your session bus"));
    }
    for name in &p.system_talk {
        match SYSTEM_SERVICES.iter().find(|(s, _)| name == s || name.starts_with(&format!("{s}."))) {
            Some((_, what)) => out.push(format!("Can talk to {name} on the system bus: it can {what}")),
            None => out.push(format!("Can talk to {name} on the system bus")),
        }
    }
    for cmd in &p.host_commands {
        out.push(format!("Can ask the bar to run: {}", cmd.join(" ")));
    }
    if p.desktop {
        out.push("Can see your workspaces and window titles".into());
    }
    if p.windows {
        out.push("Can open its own windows on your screen".into());
    }
    if p.frames {
        out.push("Can draw animated pictures in its items".into());
    }
    if out.is_empty() {
        out.push("No special permissions: it can only show items and read its own folder".into());
    }
    out
}

/// System services that only report (reading them changes nothing).
const READ_ONLY: &[&str] = &["org.freedesktop.UPower"];

/// What a plugin could change on the computer through the system bus, one line each:
/// the known services' powers, and services Nyx doesn't know (assumed able to change
/// anything they offer).
pub fn system_changes(p: &Permissions) -> Vec<String> {
    let mut out = vec![];
    for name in &p.system_talk {
        match SYSTEM_SERVICES.iter().find(|(s, _)| name == s || name.starts_with(&format!("{s}."))) {
            Some((s, _)) if READ_ONLY.contains(s) => {}
            Some((_, what)) => out.push(format!("{what} ({name})")),
            None => out.push(format!("whatever {name} offers: Nyx doesn't know this system service")),
        }
    }
    out
}

/// Whether any permission can change the system, for a stronger warning.
pub fn changes_system(p: &Permissions) -> bool {
    !system_changes(p).is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hash_ignores_order_but_not_content() {
        let a = Permissions { session_talk: vec!["a".into(), "b".into()], ..Default::default() };
        let b = Permissions { session_talk: vec!["b".into(), "a".into()], ..Default::default() };
        let c = Permissions { session_talk: vec!["a".into(), "b".into()], network: true, ..Default::default() };
        assert_eq!(hash(&a), hash(&b));
        assert_ne!(hash(&a), hash(&c));
    }

    #[test]
    fn plain_words() {
        let p = Permissions {
            network: true,
            system_talk: vec!["org.freedesktop.NetworkManager".into()],
            host_commands: vec![vec!["nyx-weather".into(), "--toggle".into()]],
            ..Default::default()
        };
        let lines = describe(&p);
        assert!(lines[0].contains("network"));
        assert!(lines[1].contains("change network settings"));
        assert!(lines[2].ends_with("nyx-weather --toggle"));
        assert!(changes_system(&p));
        assert!(describe(&Permissions::default())[0].starts_with("No special"));
        let read_only = Permissions { system_talk: vec!["org.freedesktop.UPower".into()], ..Default::default() };
        assert!(!changes_system(&read_only), "reading the battery changes nothing");
        let unknown = Permissions { system_talk: vec!["com.example.Mystery".into()], ..Default::default() };
        assert!(system_changes(&unknown)[0].contains("doesn't know"));
        let login = Permissions { system_talk: vec!["org.freedesktop.login1".into()], ..Default::default() };
        assert!(system_changes(&login)[0].starts_with("suspend, restart or shut down"));
    }
}
