//! Wi-Fi through NetworkManager: the band, the connection's details, restarting the
//! radios and signing in to a network that asks for it.

use super::{Fail, Res, nm_get, output, run};
use std::time::{Duration, Instant};

/// The band a frequency is on (`2412 MHz`, `5745.0` or `5745`).
pub fn band_for_freq(freq: &str) -> Option<&'static str> {
    let mhz: u32 = freq.trim().chars().take_while(char::is_ascii_digit).collect::<String>().parse().ok()?;
    match mhz {
        2400..2500 => Some("2.4"),
        4900..5925 => Some("5"),
        5925..7125 => Some("6"),
        _ => None,
    }
}

fn nm_band(band: &str) -> Option<&'static str> {
    match band {
        "2.4" => Some("bg"),
        "5" => Some("a"),
        "6" => Some("6GHz"),
        _ => None,
    }
}

fn band_from_nm(v: &str) -> &'static str {
    match v.trim() {
        "bg" => "2.4",
        "a" => "5",
        "6GHz" => "6",
        _ => "auto",
    }
}

/// The connected Wi-Fi device.
fn wifi_device() -> Option<String> {
    nm_get(&["DEVICE,TYPE,STATE", "device", "status"]).lines().find_map(|l| {
        let f: Vec<&str> = l.split(':').collect();
        (f.len() >= 3 && f[1] == "wifi" && f[2] == "connected").then(|| f[0].to_string())
    })
}

fn profile(device: &str) -> Option<String> {
    let p = nm_get(&["GENERAL.CONNECTION", "device", "show", device]).trim().to_string();
    (!p.is_empty()).then_some(p)
}

/// `nmcli -g IN-USE,FREQ,…,SSID dev wifi list` lines as (in use, fields before SSID, SSID):
/// the SSID last, so one with `:` in it comes back whole.
fn scan(device: &str, fields: &str, n: usize) -> Vec<(bool, Vec<String>, String)> {
    nm_get(&[&format!("IN-USE,{fields},SSID"), "dev", "wifi", "list", "ifname", device, "--rescan", "no"])
        .lines()
        .filter_map(|l| {
            let parts: Vec<&str> = l.splitn(n + 2, ':').collect();
            (parts.len() == n + 2)
                .then(|| (parts[0] == "*", parts[1..=n].iter().map(|s| s.to_string()).collect(), parts[n + 1].to_string()))
        })
        .collect()
}

/// The SSID and frequency in use, and every band that SSID answers on (with the one in
/// use always among them).
fn bands(device: &str) -> Option<(String, Option<&'static str>, Vec<&'static str>)> {
    let list = scan(device, "FREQ", 1);
    let (_, f, ssid) = list.iter().find(|(used, _, _)| *used)?;
    let current = band_for_freq(&f[0]);
    let mut available: Vec<&str> =
        list.iter().filter(|(_, _, s)| s == ssid).filter_map(|(_, f, _)| band_for_freq(&f[0])).collect();
    available.extend(current);
    available.sort_by(|a, b| a.parse::<f32>().unwrap_or(0.0).total_cmp(&b.parse::<f32>().unwrap_or(0.0)));
    available.dedup();
    Some((ssid.clone(), current, available))
}

/// `network-band [auto|2.4|5|6]`: without an argument, `band`, `available` and `selected`
/// lines (tab-separated); with one, sticks the connection to that band (or lets it roam).
pub fn band(args: &[&str]) -> Res {
    match args {
        [] => {
            let Some(device) = wifi_device() else { return Ok(()) };
            let Some((_, current, available)) = bands(&device) else { return Ok(()) };
            println!("band\t{}", current.unwrap_or(""));
            println!("available\t{}", available.join(" "));
            if let Some(p) = profile(&device) {
                println!("selected\t{}", band_from_nm(&nm_get(&["802-11-wireless.band", "connection", "show", &p])));
            }
            Ok(())
        }
        [target] if matches!(*target, "auto" | "2.4" | "5" | "6") => set_band(target),
        _ => Err(Fail::Usage("[auto|2.4|5|6]")),
    }
}

fn set_band(target: &str) -> Res {
    let device = wifi_device().ok_or("no connected Wi-Fi device.")?;
    let p = profile(&device).ok_or("no active Wi-Fi connection profile.")?;
    let desired = if target == "auto" {
        ""
    } else {
        let available = bands(&device).map(|b| b.2).unwrap_or_default();
        if !available.contains(&target) {
            return Err(format!("{target}GHz is not available on this network.").into());
        }
        nm_band(target).unwrap_or("")
    };
    let previous = nm_get(&["802-11-wireless.band", "connection", "show", &p]).trim().to_string();
    if previous == desired {
        return Ok(());
    }
    run("nmcli", &["connection", "modify", &p, "802-11-wireless.band", desired])?;
    // A band change takes effect on reconnecting. If it can't come back on that band, put
    // the old setting back rather than leave the computer offline.
    if run("nmcli", &["connection", "up", &p]).is_err() {
        let _ = run("nmcli", &["connection", "modify", &p, "802-11-wireless.band", &previous]);
        let _ = run("nmcli", &["connection", "up", &p]);
        return Err(format!("could not connect on {target}; reverted to previous band.").into());
    }
    Ok(())
}

/// The interface and gateway of the default route.
fn default_route() -> Option<(String, String)> {
    let text = output("ip", &["-4", "route", "show", "default"])?;
    let words: Vec<&str> = text.lines().next()?.split_whitespace().collect();
    let after = |k: &str| words.iter().position(|w| *w == k).and_then(|i| words.get(i + 1)).map(|s| s.to_string());
    Some((after("dev")?, after("via").unwrap_or_default()))
}

/// One ping's round trip, in milliseconds.
fn ping(host: &str) -> Option<f64> {
    if host.is_empty() {
        return None;
    }
    let text = output("ping", &["-c", "1", "-W", "1", "-n", host])?;
    let t = text.split("time=").nth(1)?;
    t.split_whitespace().next()?.parse().ok()
}

/// The signal in dBm, from the kernel's wireless table.
fn signal_dbm(iface: &str) -> Option<i32> {
    let table = std::fs::read_to_string("/proc/net/wireless").ok()?;
    let line = table.lines().find(|l| l.trim_start().starts_with(&format!("{iface}:")))?;
    let level = line.split_whitespace().nth(3)?;
    level.trim_end_matches('.').parse::<f64>().ok().map(|v| v as i32)
}

fn counter(iface: &str, name: &str) -> u64 {
    std::fs::read_to_string(format!("/sys/class/net/{iface}/statistics/{name}"))
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

/// `network-status [--verbose]`: `key<TAB>value` lines for the connection in use
/// (iface, type, ip, prefix, gateway, ssid, signal_dbm, freq, bitrate, pings, counters).
pub fn status(_args: &[&str]) -> Res {
    let Some((iface, gateway)) = default_route() else { return Ok(()) };
    let wifi = std::path::Path::new(&format!("/sys/class/net/{iface}/wireless")).exists();
    println!("iface\t{iface}");
    println!("type\t{}", if wifi { "wifi" } else { "ethernet" });
    if let Some(addr) = output("ip", &["-4", "-o", "addr", "show", "dev", &iface])
        .and_then(|t| t.split_whitespace().skip_while(|w| *w != "inet").nth(1).map(str::to_string))
    {
        let (ip, prefix) = addr.split_once('/').unwrap_or((&addr, ""));
        println!("ip\t{ip}");
        println!("prefix\t{prefix}");
    }
    println!("gateway\t{gateway}");
    if wifi && let Some((_, f, ssid)) = scan(&iface, "FREQ,RATE,SIGNAL", 3).into_iter().find(|(used, _, _)| *used) {
        println!("ssid\t{ssid}");
        let mhz: String = f[0].chars().take_while(char::is_ascii_digit).collect();
        println!("freq\t{mhz}");
        println!("bitrate\t{}", f[1].trim());
        let dbm = signal_dbm(&iface).or_else(|| f[2].trim().parse::<i32>().ok().map(|pct| pct / 2 - 100));
        if let Some(d) = dbm {
            println!("signal_dbm\t{d}");
        }
    }
    if let Some(ms) = ping(&gateway) {
        println!("router_ping_ms\t{ms}");
    }
    if let Some(ms) = ping("1.1.1.1") {
        println!("internet_ping_ms\t{ms}");
    }
    println!("rx_bytes\t{}", counter(&iface, "rx_bytes"));
    println!("tx_bytes\t{}", counter(&iface, "tx_bytes"));
    Ok(())
}

/// `restart-wifi`: unblocks the radio, turns networking and Wi-Fi on, and looks again.
pub fn restart_wifi() -> Res {
    println!("Unblocking wifi...\n");
    let _ = run("rfkill", &["unblock", "wifi"]);
    run("nmcli", &["networking", "on"])?;
    run("nmcli", &["radio", "wifi", "on"])?;
    let _ = run("nmcli", &["device", "wifi", "rescan"]);
    if let Some(t) = output("rfkill", &["list", "wifi"]) {
        print!("{t}");
    }
    Ok(())
}

/// `restart-bluetooth`: unblocks the radio and turns the adapter off and on.
pub fn restart_bluetooth() -> Res {
    println!("Unblocking bluetooth...\n");
    let _ = run("rfkill", &["unblock", "bluetooth"]);
    let _ = run("bluetoothctl", &["power", "off"]);
    std::thread::sleep(Duration::from_secs(1));
    run("bluetoothctl", &["power", "on"])?;
    Ok(())
}

fn online() -> bool {
    output("timeout", &["5", "nmcli", "networking", "connectivity", "check"]).is_some_and(|s| s.trim() == "full")
}

/// `network-captive-portal URL`: opens the network's sign-in page and waits (up to five
/// minutes) until the internet answers. One at a time.
pub fn captive_portal(args: &[&str]) -> Res {
    let [url] = args else { return Err(Fail::Usage("<url>")) };
    let dir = std::env::var_os("XDG_RUNTIME_DIR").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let lock = dir.join("nyx-network-captive-portal.lock");
    if let Ok(pid) = std::fs::read_to_string(&lock)
        && std::path::Path::new(&format!("/proc/{}", pid.trim())).exists()
    {
        return Ok(());
    }
    let _ = std::fs::write(&lock, std::process::id().to_string());
    let result = (|| {
        if online() {
            return Ok(());
        }
        std::process::Command::new("xdg-open").arg(url).spawn().map_err(|e| format!("can't open {url}: {e}"))?;
        let until = Instant::now() + Duration::from_secs(300);
        while Instant::now() < until {
            if online() {
                return Ok(());
            }
            std::thread::sleep(Duration::from_secs(2));
        }
        Err(Fail::Error("still not online after five minutes".into()))
    })();
    let _ = std::fs::remove_file(&lock);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frequencies_fall_in_bands() {
        assert_eq!(band_for_freq("2412 MHz"), Some("2.4"));
        assert_eq!(band_for_freq("5745.0"), Some("5"));
        assert_eq!(band_for_freq("6115"), Some("6"));
        assert_eq!(band_for_freq("60000"), None);
        assert_eq!(band_for_freq(""), None);
        assert_eq!(band_from_nm("bg"), "2.4");
        assert_eq!(band_from_nm(""), "auto");
    }
}
