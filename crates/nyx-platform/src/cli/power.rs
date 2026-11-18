//! The screen (backlight, the laptop panel, scale), staying awake, Night Light, Do not
//! disturb, power profiles, the battery and sound.

use super::{Fail, Res, output, run};
use std::path::PathBuf;

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default()
}

fn state_home() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".local/state"))
}

fn hypr_json(what: &str) -> serde_json::Value {
    serde_json::from_str(&output("hyprctl", &[what, "-j"]).unwrap_or_default()).unwrap_or_default()
}

fn focused_monitor() -> Option<serde_json::Value> {
    hypr_json("monitors").as_array()?.iter().find(|m| m["focused"] == true).cloned()
}

fn is_internal(name: &str) -> bool {
    ["eDP-", "LVDS-", "DSI-"].iter().any(|p| name.starts_with(p))
}

/// `hw-display`: the screen's backlight device (a GPU's own before a firmware one).
pub fn hw_display() -> Res {
    match backlight() {
        Some(d) => {
            println!("{d}");
            Ok(())
        }
        None => Err(Fail::Error(String::new())),
    }
}

fn backlight() -> Option<String> {
    let mut names: Vec<String> = std::fs::read_dir("/sys/class/backlight")
        .ok()?
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n != "appletb_backlight")
        .collect();
    names.sort();
    let preferred = ["gmux_backlight", "amdgpu_bl", "intel_backlight", "nvidia_", "acpi_video"];
    preferred.iter().find_map(|p| names.iter().find(|n| n.starts_with(p)).cloned()).or_else(|| names.first().cloned())
}

fn backlight_percent(device: &str) -> Option<u32> {
    let line = output("brightnessctl", &["-d", device, "-m"])?;
    line.lines().next()?.split(',').nth(3)?.trim_end_matches('%').parse().ok()
}

/// `brightness-display [--no-osd] [--monitor NAME] [STEP|on|off]`: the laptop panel's
/// brightness (percent) without a step; `+5%`, `5%-`, `40%` set it; `off` and `on` turn
/// the screens off and on. Other screens' brightness isn't set here.
pub fn brightness(args: &[&str]) -> Res {
    let mut monitor = None;
    let mut rest = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match *a {
            "--no-osd" => {}
            "--monitor" => monitor = it.next().map(|m| m.to_string()),
            other => rest.push(other),
        }
    }
    let monitor = monitor.or_else(|| focused_monitor().and_then(|m| m["name"].as_str().map(str::to_string)));
    match rest.first().copied() {
        Some("off") => {
            let _ = run("hyprctl", &["dispatch", "hl.dsp.dpms({ action = \"disable\" })"]);
            return Ok(());
        }
        Some("on") => {
            let _ = run("hyprctl", &["dispatch", "hl.dsp.dpms({ action = \"enable\" })"]);
            return Ok(());
        }
        _ => {}
    }
    if monitor.as_deref().is_some_and(|m| !is_internal(m)) {
        return Err(Fail::Error(String::new()));
    }
    let device = backlight().ok_or_else(|| Fail::Error(String::new()))?;
    let current = backlight_percent(&device).ok_or_else(|| Fail::Error(String::new()))?;
    let Some(step) = rest.first() else {
        println!("{current}");
        return Ok(());
    };
    // Single steps near the bottom, so the dimmest settings can be reached.
    let target = match *step {
        "+5%" => format!("{}%", if current < 5 { current + 1 } else { current + 5 }.min(100)),
        "5%-" => format!("{}%", if current <= 5 { current.saturating_sub(1) } else { current - 5 }.max(1)),
        s => s.to_string(),
    };
    run("brightnessctl", &["-q", "-d", &device, "set", &target])?;
    Ok(())
}

/// Staying awake: the file says so, and the idle service (hypridle) is stopped meanwhile.
fn awake_file() -> PathBuf {
    state_home().join("nyx/indicators/stay-awake")
}

/// `toggle-idle [toggle|stay-awake|allow-idle|status]`.
pub fn idle(args: &[&str]) -> Res {
    let file = awake_file();
    let set = |awake: bool| -> Res {
        if awake {
            if let Some(d) = file.parent() {
                let _ = std::fs::create_dir_all(d);
            }
            std::fs::write(&file, "").map_err(|e| e.to_string())?;
            let _ = run("systemctl", &["--user", "stop", "hypridle.service"]);
        } else {
            let _ = std::fs::remove_file(&file);
            let _ = run("systemctl", &["--user", "start", "hypridle.service"]);
        }
        println!("{}", if awake { "disabled" } else { "enabled" });
        Ok(())
    };
    match args.first().copied().unwrap_or("toggle") {
        "toggle" => set(!file.exists()),
        "stay-awake" | "awake" | "on" => set(true),
        "allow-idle" | "idle" | "off" => set(false),
        "status" | "--status" => {
            if file.exists() {
                println!(r#"{{"enabled":true,"class":"enabled","tooltip":"Allow Idle Lock & Screensaver"}}"#);
            } else {
                println!(r#"{{"enabled":false,"class":"disabled","tooltip":"Stay Awake"}}"#);
            }
            Ok(())
        }
        _ => Err(Fail::Usage("[toggle|stay-awake|allow-idle|status]")),
    }
}

const NIGHT: u32 = 4000;
const DAY: u32 = 6500;
const IDENTITY: u32 = 6000;

fn temperature() -> Option<u32> {
    let out = output("hyprctl", &["hyprsunset", "temperature"])?;
    out.split(|c: char| !c.is_ascii_digit()).find(|s| !s.is_empty())?.parse().ok()
}

/// `toggle-nightlight [--status]`: warm (4000 K) or back to normal.
pub fn nightlight(args: &[&str]) -> Res {
    if args.first() == Some(&"--status") {
        let t = temperature();
        let on = t.is_some_and(|t| t < IDENTITY);
        println!("{{\"enabled\":{on},\"temperature\":{}}}", t.map(|t| t.to_string()).unwrap_or_else(|| "null".into()));
        return Ok(());
    }
    if output("pgrep", &["-x", "hyprsunset"]).is_none() && run("systemctl", &["--user", "start", "hyprsunset.service"]).is_err() {
        std::process::Command::new("setsid")
            .args(["-f", "hyprsunset"])
            .spawn()
            .map_err(|e| format!("hyprsunset didn't start: {e}"))?;
    }
    let target = if temperature().is_some_and(|t| t < IDENTITY) { DAY } else { NIGHT };
    for _ in 0..10 {
        let _ = run("hyprctl", &["hyprsunset", "temperature", &target.to_string()]);
        if temperature() == Some(target) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    if target == DAY {
        let _ = run("hyprctl", &["hyprsunset", "identity"]);
    }
    Ok(())
}

/// `toggle-notification-silencing`: Do not disturb, through nyx-notifyd.
pub fn silence() -> Res {
    let conn = zbus::blocking::Connection::session().map_err(|e| e.to_string())?;
    let reply = conn
        .call_method(
            Some("io.github.design_nexus.nyx.Notifications"),
            "/io/github/design_nexus/nyx/Notifications",
            Some("io.github.design_nexus.nyx.Notifications1"),
            "ToggleDnd",
            &(),
        )
        .map_err(|e| format!("the notification service didn't answer: {e}"))?;
    let on: bool = reply.body().deserialize().unwrap_or(false);
    println!("{}", if on { "on" } else { "off" });
    Ok(())
}

/// `powerprofiles-set [autodetect|ac|battery] [power-saver|balanced|performance]`: the
/// profile for a power source, remembered for it (where NyxOS's power-profile service
/// reads it) and set now.
pub fn power_profile(args: &[&str]) -> Res {
    let source = match args.first().copied().unwrap_or("autodetect") {
        "autodetect" => {
            let on_battery = output(
                "busctl",
                &["get-property", "org.freedesktop.UPower", "/org/freedesktop/UPower", "org.freedesktop.UPower", "OnBattery"],
            )
            .is_some_and(|s| s.trim() == "b true");
            if on_battery { "battery" } else { "ac" }
        }
        s @ ("ac" | "battery") => s,
        _ => return Err(Fail::Usage("[autodetect|ac|battery] [power-saver|balanced|performance]")),
    };
    let profiles: Vec<String> = output("powerprofilesctl", &["list"])
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.trim().trim_start_matches('*').trim().strip_suffix(':').map(str::to_string))
        .collect();
    let dir = std::env::var_os("NYX_POWER_PROFILES_STATE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| state_home().join("nyx/power-profiles"));
    let file = dir.join(source);
    let requested = args.get(1).map(|s| s.to_string());
    if let Some(r) = &requested
        && !profiles.contains(r)
    {
        return Err(format!("Power profile is not available: {r}").into());
    }
    let mut profile = requested.clone().or_else(|| std::fs::read_to_string(&file).ok().map(|s| s.trim().to_string()));
    if !profile.as_ref().is_some_and(|p| profiles.contains(p)) {
        profile =
            Some(if source == "ac" && profiles.iter().any(|p| p == "performance") { "performance" } else { "balanced" }.into());
    }
    let profile = profile.unwrap_or_default();
    run("powerprofilesctl", &["set", &profile])?;
    if let Some(r) = requested {
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(&file, format!("{r}\n")).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// `upower --show-info` as `key<TAB>value` lines: state, time, percentage, rate, size.
pub fn shell_from_upower(info: &str) -> String {
    let get = |k: &str| {
        info.lines()
            .find_map(|l| l.trim().strip_prefix(k).map(|v| v.trim_start_matches(':').trim().to_string()))
            .unwrap_or_default()
    };
    let tight = |v: String| v.replace(' ', "");
    let time = [get("time to empty"), get("time to full")].into_iter().find(|t| !t.is_empty()).unwrap_or_default();
    [
        ("percentage", get("percentage")),
        ("state", get("state")),
        ("rate", tight(get("energy-rate"))),
        ("size", tight(get("energy-full"))),
        ("time", time),
    ]
    .iter()
    .map(|(k, v)| format!("{k}\t{v}"))
    .collect::<Vec<_>>()
    .join("\n")
}

/// `battery-status [--shell]`: the battery from UPower.
pub fn battery(args: &[&str]) -> Res {
    let devices = output("upower", &["--enumerate"]).unwrap_or_default();
    let bat = devices.lines().find(|l| l.contains("battery_BAT")).ok_or_else(|| Fail::Error(String::new()))?;
    let shell = shell_from_upower(&output("upower", &["--show-info", bat.trim()]).unwrap_or_default());
    if args.first() == Some(&"--shell") {
        println!("{shell}");
    } else {
        let v = |k: &str| shell.lines().find_map(|l| l.strip_prefix(&format!("{k}\t"))).unwrap_or("").to_string();
        println!("{} {}", v("percentage"), v("state"));
    }
    Ok(())
}

/// Where the laptop panel is remembered as switched off.
fn internal_off_file() -> PathBuf {
    state_home().join("nyx/toggles/internal-monitor-disabled")
}

/// `hyprland-monitor-internal on|off|toggle|recover`: the laptop's own screen, off only
/// while another screen is on.
pub fn internal(args: &[&str]) -> Res {
    let monitors = hypr_json("monitors");
    let all = serde_json::from_str::<serde_json::Value>(&output("hyprctl", &["monitors", "all", "-j"]).unwrap_or_default())
        .unwrap_or_default();
    let names = |v: &serde_json::Value| -> Vec<String> {
        v.as_array().into_iter().flatten().filter_map(|m| m["name"].as_str().map(str::to_string)).collect()
    };
    let internal = names(&all).into_iter().find(|n| is_internal(n));
    let external_on = names(&monitors).iter().any(|n| !is_internal(n) && !n.starts_with("HEADLESS"));
    let file = internal_off_file();
    let on = || -> Res {
        if file.exists() {
            let _ = std::fs::remove_file(&file);
            // The configuration's monitors again, the panel among them.
            run("hyprctl", &["reload"])?;
        }
        let _ = run("hyprctl", &["dispatch", "hl.dsp.dpms({ action = \"enable\" })"]);
        Ok(())
    };
    let off = || -> Res {
        let name = internal.clone().ok_or("This computer has no laptop screen.")?;
        if !name.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c)) {
            return Err("The laptop screen's name isn't safe to use.".into());
        }
        if !external_on {
            return Err("The laptop screen is the only screen on.".into());
        }
        run("hyprctl", &["eval", &format!("hl.monitor({{ output = \"{name}\", disabled = true }})")])?;
        if let Some(d) = file.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        std::fs::write(&file, &name).map_err(|e| e.to_string())?;
        Ok(())
    };
    match args.first().copied() {
        Some("on") => on(),
        Some("off") => off(),
        Some("toggle") if file.exists() => on(),
        Some("toggle") => off(),
        // Back on when no other screen is left.
        Some("recover") if !external_on && file.exists() => on(),
        Some("recover") => Ok(()),
        _ => Err(Fail::Usage("{on|off|toggle|recover}")),
    }
}

/// `hyprland-monitor-scaling [SCALE]`: the focused screen's scale, or set it (until the
/// next reload; Settings › Displays keeps it).
pub fn scaling(args: &[&str]) -> Res {
    let m = focused_monitor().ok_or("no focused screen")?;
    let Some(scale) = args.first() else {
        println!("{}", m["scale"].as_f64().unwrap_or(1.0));
        return Ok(());
    };
    let scale: f64 = scale.parse().map_err(|_| Fail::Usage("[SCALE]"))?;
    if !(0.25..=4.0).contains(&scale) {
        return Err(Fail::Usage("[SCALE between 0.25 and 4]"));
    }
    let name = m["name"].as_str().unwrap_or_default();
    let mode = format!(
        "{}x{}@{:.2}",
        m["width"].as_i64().unwrap_or(0),
        m["height"].as_i64().unwrap_or(0),
        m["refreshRate"].as_f64().unwrap_or(60.0)
    );
    run(
        "hyprctl",
        &["eval", &format!("hl.monitor({{ output = \"{name}\", mode = \"{mode}\", position = \"auto\", scale = {scale} }})")],
    )?;
    Ok(())
}

/// `restart-audio`: PipeWire and WirePlumber, restarted.
pub fn restart_audio() -> Res {
    run("systemctl", &["--user", "restart", "pipewire.service", "pipewire-pulse.service", "wireplumber.service"])?;
    println!("Restarted the sound services.");
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn upower_reads_as_lines() {
        let info = "    state:               discharging\n    energy-full:         52.3 Wh\n    energy-rate:         9.8 W\n    time to empty:       4.2 hours\n    percentage:          71%\n";
        let s = super::shell_from_upower(info);
        assert!(s.contains("percentage\t71%") && s.contains("state\tdischarging") && s.contains("rate\t9.8W"));
        assert!(s.contains("size\t52.3Wh") && s.contains("time\t4.2 hours"));
        assert!(super::is_internal("eDP-1") && !super::is_internal("DP-2"));
    }
}
