//! Touchpads, touchscreens, the battery, the focused monitor and Night Light, through
//! Hyprland (`hyprctl`), sysfs and the session's services.

use super::{Fail, Res, output, run};
use std::path::{Path, PathBuf};

/// Hyprland's devices (`hyprctl devices -j`), as JSON text.
fn devices() -> String {
    output("hyprctl", &["devices", "-j"]).unwrap_or_default()
}

/// The `name`s in one of `hyprctl devices -j`'s lists (`mice`, `touch`, `tablets`).
pub fn names_in(json: &str, list: &str) -> Vec<String> {
    let v: serde_json::Value = serde_json::from_str(json).unwrap_or_default();
    v[list].as_array().into_iter().flatten().filter_map(|d| d["name"].as_str().map(str::to_string)).collect()
}

/// Hyprland's name for a kernel input device name: lowercased, spaces and commas as `-`.
fn hypr_name(kernel: &str) -> String {
    kernel.to_lowercase().chars().map(|c| if c == ' ' || c == ',' || c == '\n' { '-' } else { c }).collect()
}

/// The touchpad: a mouse called touchpad or trackpad, else one udev says is a touchpad.
fn touchpad() -> Option<String> {
    let mice = names_in(&devices(), "mice");
    if let Some(m) = mice.iter().find(|m| {
        let l = m.to_lowercase();
        l.contains("touchpad") || l.contains("trackpad")
    }) {
        return Some(m.clone());
    }
    let entries = std::fs::read_dir("/sys/class/input").ok()?;
    for e in entries.flatten() {
        let path = e.path();
        if !path.file_name().is_some_and(|n| n.to_string_lossy().starts_with("event")) {
            continue;
        }
        let Ok(name) = std::fs::read_to_string(path.join("device/name")) else { continue };
        let name = hypr_name(name.trim());
        let Some(m) =
            mice.iter().find(|m| **m == name || m.strip_prefix(&format!("{name}-")).is_some_and(|n| n.parse::<u32>().is_ok()))
        else {
            continue;
        };
        let props = output("udevadm", &["info", "--query=property", &path.to_string_lossy()]).unwrap_or_default();
        if props.lines().any(|l| l == "ID_INPUT_TOUCHPAD=1") {
            return Some(m.clone());
        }
    }
    None
}

fn touchscreen() -> Option<String> {
    let d = devices();
    names_in(&d, "touch").into_iter().chain(names_in(&d, "tablets")).next()
}

/// `hw-touchpad`, `hw-touchscreen`: the device's Hyprland name, if there is one.
pub fn hw(kind: &str) -> Res {
    let device = if kind == "touchpad" { touchpad() } else { touchscreen() };
    match device {
        Some(d) => {
            println!("{d}");
            Ok(())
        }
        None => Err(Fail::Error(String::new())),
    }
}

/// Where a device switched off stays remembered (by name), so the toggle knows.
fn state_file(kind: &str) -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    home.join(format!(".local/state/nyx/toggles/{kind}-disabled-name"))
}

fn apply(device: &str, enabled: bool) -> Result<(), String> {
    // A name from a USB descriptor goes into a Lua string: backslashes and quotes escaped.
    let quoted = device.replace('\\', "\\\\").replace('"', "\\\"");
    run("hyprctl", &["eval", &format!("hl.device({{ name = \"{quoted}\", enabled = {enabled} }})")])
}

/// `toggle-touchpad`, `toggle-touchscreen` `[on|off|toggle|cycle]`.
pub fn toggle(kind: &str, args: &[&str]) -> Res {
    let action = args.first().copied().unwrap_or("toggle");
    let file = state_file(kind);
    let device = || -> Result<String, Fail> {
        let d = if kind == "touchpad" { touchpad() } else { touchscreen() };
        let d = d.ok_or_else(|| Fail::Error(format!("No {kind} device found")))?;
        if d.chars().any(char::is_control) {
            return Err(Fail::Error(format!("Invalid {kind} device name")));
        }
        Ok(d)
    };
    let on = || -> Res {
        let _ = std::fs::remove_file(&file);
        apply(&device()?, true)?;
        Ok(())
    };
    let off = || -> Res {
        let d = device()?;
        apply(&d, false)?;
        if let Some(dir) = file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        std::fs::write(&file, format!("{d}\n")).map_err(|e| e.to_string())?;
        Ok(())
    };
    match action {
        "on" => on(),
        "off" => off(),
        "toggle" if file.exists() => on(),
        "toggle" => off(),
        // Off and on again releases a button libinput still holds; a device switched off stays off.
        "cycle" => {
            if !file.exists() {
                let d = device()?;
                apply(&d, false)?;
                apply(&d, true)?;
            }
            Ok(())
        }
        _ => Err(Fail::Usage("[on|off|toggle|cycle]")),
    }
}

/// `battery-present`: exits 0 when the computer has a battery of its own (not a mouse's).
pub fn battery_present() -> Res {
    let root = std::env::var_os("NYX_POWER_SUPPLY_PATH").map(PathBuf::from).unwrap_or_else(|| "/sys/class/power_supply".into());
    let read = |p: &Path, f: &str| std::fs::read_to_string(p.join(f)).map(|s| s.trim().to_string()).unwrap_or_default();
    let found = std::fs::read_dir(root).ok().is_some_and(|d| {
        d.flatten().any(|e| {
            let p = e.path();
            read(&p, "present") == "1" && read(&p, "type") == "Battery" && read(&p, "scope") != "Device"
        })
    });
    if found { Ok(()) } else { Err(Fail::Error(String::new())) }
}

/// `hyprland-monitor-focused`: the focused monitor's name.
pub fn monitor_focused() -> Res {
    let v: serde_json::Value =
        serde_json::from_str(&output("hyprctl", &["monitors", "-j"]).unwrap_or_default()).unwrap_or_default();
    if let Some(name) = v.as_array().into_iter().flatten().find(|m| m["focused"] == true).and_then(|m| m["name"].as_str()) {
        println!("{name}");
    }
    Ok(())
}

/// `restart-hyprsunset`: Night Light's service, restarted.
pub fn restart_hyprsunset() -> Res {
    if run("systemctl", &["--user", "restart", "hyprsunset.service"]).is_ok() {
        return Ok(());
    }
    let _ = run("pkill", &["-x", "hyprsunset"]);
    std::process::Command::new("setsid")
        .args(["-f", "hyprsunset"])
        .spawn()
        .map_err(|e| format!("hyprsunset didn't start: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_lists_give_their_names() {
        let json = r#"{"mice": [{"address": "0x1", "name": "asus-keyboard-1"}, {"name": "asuf1209:00-2808:0219-touchpad"}],
            "keyboards": [{"name": "kb"}], "touch": [], "tablets": [{"name": "wacom-pen"}]}"#;
        assert_eq!(names_in(json, "mice"), ["asus-keyboard-1", "asuf1209:00-2808:0219-touchpad"]);
        assert!(names_in(json, "touch").is_empty());
        assert_eq!(names_in(json, "tablets"), ["wacom-pen"]);
        assert_eq!(hypr_name("ASUF1209:00 2808:0219 Touchpad"), "asuf1209:00-2808:0219-touchpad");
    }
}
