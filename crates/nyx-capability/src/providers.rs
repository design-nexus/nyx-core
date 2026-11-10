//! The built-in providers: programs, systemd units, D-Bus names, hardware, Shell Core,
//! Flux (Nyx Connect), the sound server and the fingerprint reader.
//!
//! Capability ids: `program:NAME`, `service:user:UNIT` / `service:system:UNIT` (without
//! `.service`), `bus:session:NAME` / `bus:system:NAME`, `hw:battery`, `hw:backlight`,
//! `hw:bluetooth`, `hw:keyboard-backlight`, `hw:touchpad`, `hw:touchscreen`, `hw:mouse`,
//! `hw:laptop-panel`, `hw:wifi`, `hw:camera`, `hw:fingerprint`, `daemon:shell-core`,
//! `daemon:flux`, `daemon:audio`.

use crate::sources::runtime_dir;
use crate::{Capability, Provider, Source, State};
use std::path::{Path, PathBuf};
use std::process::Command;

fn output(program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(program).args(args).stderr(std::process::Stdio::null()).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default()
}

/// Where programs are found: `PATH`, plus NixOS's profiles and `~/.local/bin`, which a
/// session started outside a login shell may not have on `PATH`.
pub fn program_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    let user = std::env::var("USER").unwrap_or_default();
    for extra in [
        home().join(".local/bin"),
        home().join(".nix-profile/bin"),
        PathBuf::from(format!("/etc/profiles/per-user/{user}/bin")),
        PathBuf::from("/run/current-system/sw/bin"),
    ] {
        if !dirs.contains(&extra) {
            dirs.push(extra);
        }
    }
    dirs.retain(|d| d.is_absolute());
    dirs
}

fn executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// Installed programs, by name.
pub struct Programs {
    names: Vec<String>,
}

impl Programs {
    pub fn new<S: Into<String>>(names: impl IntoIterator<Item = S>) -> Self {
        let mut names: Vec<String> = names.into_iter().map(Into::into).collect();
        names.sort();
        names.dedup();
        Programs { names }
    }

    pub fn find(name: &str) -> Option<PathBuf> {
        program_dirs().into_iter().map(|d| d.join(name)).find(|p| executable(p))
    }
}

impl Provider for Programs {
    fn name(&self) -> &str {
        "programs"
    }

    fn probe(&self) -> Vec<Capability> {
        let dirs = program_dirs();
        self.names
            .iter()
            .map(|n| {
                if dirs.iter().any(|d| executable(&d.join(n))) {
                    Capability::new(format!("program:{n}"), "programs", State::Available)
                } else {
                    Capability::new(format!("program:{n}"), "programs", State::NotInstalled)
                        .reason(format!("{n} isn't installed"))
                }
            })
            .collect()
    }

    fn sources(&self) -> Vec<Source> {
        let mut s: Vec<Source> = program_dirs().into_iter().map(Source::Folder).collect();
        // A NixOS switch replaces /run/current-system.
        if Path::new("/run/current-system").exists() {
            s.push(Source::Folder("/run".into()));
        }
        s
    }
}

/// systemd units, on the user's manager or the system's.
pub struct Services {
    units: Vec<(bool, String)>,
}

impl Services {
    /// `(system, unit)`: `unit` with or without `.service`.
    pub fn new<S: Into<String>>(units: impl IntoIterator<Item = (bool, S)>) -> Self {
        Services { units: units.into_iter().map(|(s, u)| (s, u.into())).collect() }
    }
}

fn unit_name(u: &str) -> String {
    if u.contains('.') { u.to_string() } else { format!("{u}.service") }
}

/// `systemctl show` blocks for several units, in order.
pub(crate) fn parse_show(text: &str) -> Vec<(String, String, String)> {
    text.split("\n\n")
        .filter(|b| !b.trim().is_empty())
        .map(|block| {
            let get = |k: &str| {
                block.lines().find_map(|l| l.strip_prefix(k).and_then(|r| r.strip_prefix('='))).unwrap_or("").to_string()
            };
            (get("Id"), get("LoadState"), get("ActiveState"))
        })
        .collect()
}

pub(crate) fn unit_state(id: &str, load: &str, active: &str) -> Capability {
    let cap = |s| Capability::new(id, "services", s);
    match (load, active) {
        ("not-found", _) | ("", _) => cap(State::NotInstalled).reason("not installed"),
        ("masked", _) => cap(State::Unsupported).reason("turned off for this computer (masked)"),
        (_, "active" | "reloading" | "activating" | "refreshing") => cap(State::Available).operations(&["stop", "restart"]),
        (_, "failed") => cap(State::Inactive).reason("stopped after an error").operations(&["start"]),
        _ => cap(State::Inactive).reason("not running").operations(&["start"]),
    }
}

impl Provider for Services {
    fn name(&self) -> &str {
        "services"
    }

    fn probe(&self) -> Vec<Capability> {
        let mut out = Vec::new();
        for system in [false, true] {
            let units: Vec<&String> = self.units.iter().filter(|(s, _)| *s == system).map(|(_, u)| u).collect();
            if units.is_empty() {
                continue;
            }
            let names: Vec<String> = units.iter().map(|u| unit_name(u)).collect();
            let mut args = vec![if system { "--system" } else { "--user" }, "show", "-p", "Id,LoadState,ActiveState"];
            args.extend(names.iter().map(String::as_str));
            let blocks = output("systemctl", &args).map(|t| parse_show(&t)).unwrap_or_default();
            for (i, u) in units.iter().enumerate() {
                let id = format!("service:{}:{}", if system { "system" } else { "user" }, u.trim_end_matches(".service"));
                out.push(match blocks.get(i) {
                    Some((_, load, active)) => unit_state(&id, load, active),
                    None => Capability::new(id, "services", State::TemporarilyUnavailable).reason("systemd didn't answer"),
                });
            }
        }
        out
    }

    fn sources(&self) -> Vec<Source> {
        let mut s = Vec::new();
        if self.units.iter().any(|(sys, _)| !sys) {
            s.push(Source::Units { system: false });
        }
        if self.units.iter().any(|(sys, _)| *sys) {
            s.push(Source::Units { system: true });
        }
        s
    }
}

/// D-Bus services by well-known name: running, or startable on demand.
pub struct BusNames {
    names: Vec<(bool, String)>,
}

impl BusNames {
    /// `(system, name)`.
    pub fn new<S: Into<String>>(names: impl IntoIterator<Item = (bool, S)>) -> Self {
        BusNames { names: names.into_iter().map(|(s, n)| (s, n.into())).collect() }
    }
}

/// Folders holding `.service` files for D-Bus activation.
fn activation_dirs(system: bool) -> Vec<PathBuf> {
    let sub = if system { "dbus-1/system-services" } else { "dbus-1/services" };
    let mut data: Vec<PathBuf> = std::env::var_os("XDG_DATA_DIRS")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_else(|| vec!["/usr/local/share".into(), "/usr/share".into()]);
    data.push("/run/current-system/sw/share".into());
    if !system {
        data.insert(0, home().join(".local/share"));
    }
    let mut dirs: Vec<PathBuf> = data.into_iter().map(|d| d.join(sub)).collect();
    dirs.dedup();
    dirs
}

fn activatable(system: bool, name: &str) -> bool {
    activation_dirs(system).iter().any(|d| {
        std::fs::read_dir(d)
            .into_iter()
            .flatten()
            .flatten()
            .any(|e| std::fs::read_to_string(e.path()).is_ok_and(|t| t.lines().any(|l| l.trim() == format!("Name={name}"))))
    })
}

fn owned_names(system: bool) -> Option<Vec<String>> {
    let text = output("busctl", &[if system { "--system" } else { "--user" }, "list", "--acquired", "--json=short"])?;
    let list: Vec<serde_json::Value> = serde_json::from_str(&text).ok()?;
    Some(list.iter().filter_map(|e| e.get("name")?.as_str().map(str::to_string)).collect())
}

impl Provider for BusNames {
    fn name(&self) -> &str {
        "bus"
    }

    fn probe(&self) -> Vec<Capability> {
        let owned = [owned_names(false), owned_names(true)];
        self.names
            .iter()
            .map(|(system, name)| {
                let id = format!("bus:{}:{name}", if *system { "system" } else { "session" });
                let cap = |s| Capability::new(id.clone(), "bus", s);
                match &owned[*system as usize] {
                    None => cap(State::TemporarilyUnavailable).reason("the message bus didn't answer"),
                    Some(o) if o.iter().any(|n| n == name) => cap(State::Available),
                    Some(_) if activatable(*system, name) => cap(State::Available).operations(&["activate"]),
                    Some(_) => cap(State::NotInstalled).reason("its service isn't installed"),
                }
            })
            .collect()
    }

    fn sources(&self) -> Vec<Source> {
        let mut s: Vec<Source> =
            self.names.iter().map(|(system, name)| Source::BusName { system: *system, name: name.clone() }).collect();
        for system in [false, true] {
            if self.names.iter().any(|(sys, _)| *sys == system) {
                s.extend(activation_dirs(system).into_iter().map(Source::Folder));
            }
        }
        s
    }
}

/// Hardware, from sysfs and udev's database.
pub struct Hardware {
    pub sys: PathBuf,
    pub udev_data: PathBuf,
    pub dev: PathBuf,
}

impl Default for Hardware {
    fn default() -> Self {
        Hardware { sys: "/sys".into(), udev_data: "/run/udev/data".into(), dev: "/dev".into() }
    }
}

fn entries(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path()).collect()
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).map(|s| s.trim().to_string()).unwrap_or_default()
}

impl Hardware {
    fn battery(&self) -> bool {
        entries(&self.sys.join("class/power_supply"))
            .iter()
            .any(|p| read(&p.join("type")) == "Battery" && read(&p.join("scope")) != "Device")
    }

    /// Input devices udev tagged with `ID_INPUT_<KIND>=1`.
    fn input(&self, kind: &str) -> bool {
        let tag = format!("E:ID_INPUT_{kind}=1");
        entries(&self.udev_data).iter().any(|p| {
            p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("c13:"))
                && std::fs::read_to_string(p).is_ok_and(|t| t.lines().any(|l| l == tag))
        })
    }

    fn bluetooth(&self) -> Option<bool> {
        if entries(&self.sys.join("class/bluetooth")).is_empty() {
            return None;
        }
        let blocked = entries(&self.sys.join("class/rfkill"))
            .iter()
            .filter(|p| read(&p.join("type")) == "bluetooth")
            .any(|p| read(&p.join("soft")) == "1" || read(&p.join("hard")) == "1");
        Some(!blocked)
    }

    fn laptop_panel(&self) -> bool {
        entries(&self.sys.join("class/drm")).iter().any(|p| {
            let n = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            ["-eDP-", "-LVDS-", "-DSI-"].iter().any(|k| n.contains(k))
        })
    }

    fn camera(&self) -> Option<bool> {
        let video: Vec<PathBuf> = entries(&self.dev)
            .into_iter()
            .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("video")))
            .collect();
        if video.is_empty() {
            return None;
        }
        Some(video.iter().any(|p| std::fs::File::open(p).is_ok()))
    }
}

impl Provider for Hardware {
    fn name(&self) -> &str {
        "hardware"
    }

    fn probe(&self) -> Vec<Capability> {
        let hw = |id: &str, present: bool, missing: &str| {
            let c = Capability::new(format!("hw:{id}"), "hardware", if present { State::Available } else { State::Unsupported });
            if present { c } else { c.reason(missing) }
        };
        let mut out = vec![
            hw("battery", self.battery(), "This computer has no battery"),
            hw("backlight", !entries(&self.sys.join("class/backlight")).is_empty(), "No screen here has adjustable brightness"),
            hw(
                "keyboard-backlight",
                entries(&self.sys.join("class/leds")).iter().any(|p| p.to_string_lossy().contains("kbd_backlight")),
                "The keyboard has no backlight",
            ),
            hw("touchpad", self.input("TOUCHPAD"), "No touchpad is connected"),
            hw("touchscreen", self.input("TOUCHSCREEN"), "No touchscreen is connected"),
            hw("mouse", self.input("MOUSE"), "No mouse is connected"),
            hw("laptop-panel", self.laptop_panel(), "This computer has no built-in screen"),
            hw(
                "wifi",
                entries(&self.sys.join("class/net")).iter().any(|p| p.join("wireless").exists()),
                "This computer has no Wi-Fi",
            ),
        ];
        out.push(match self.bluetooth() {
            None => hw("bluetooth", false, "This computer has no Bluetooth"),
            Some(true) => hw("bluetooth", true, ""),
            Some(false) => Capability::new("hw:bluetooth", "hardware", State::Inactive)
                .reason("Bluetooth is turned off")
                .operations(&["turn-on"]),
        });
        out.push(match self.camera() {
            None => hw("camera", false, "No camera is connected"),
            Some(true) => hw("camera", true, ""),
            Some(false) => {
                Capability::new("hw:camera", "hardware", State::PermissionRestricted).reason("This account can't use the camera")
            }
        });
        out
    }

    fn sources(&self) -> Vec<Source> {
        ["power_supply", "backlight", "leds", "input", "bluetooth", "rfkill", "drm", "net", "video4linux"]
            .into_iter()
            .map(|s| Source::Udev(s.into()))
            .collect()
    }
}

/// Nyx Shell Core: its socket answering.
pub struct ShellCore;

impl ShellCore {
    pub fn socket() -> PathBuf {
        std::env::var_os("NYX_SHELL_SOCKET").map(PathBuf::from).unwrap_or_else(|| runtime_dir().join("nyx-shell/core.sock"))
    }
}

impl Provider for ShellCore {
    fn name(&self) -> &str {
        "shell-core"
    }

    fn probe(&self) -> Vec<Capability> {
        let sock = ShellCore::socket();
        let cap = |s| Capability::new("daemon:shell-core", "shell-core", s);
        let c = if std::os::unix::net::UnixStream::connect(&sock).is_ok() {
            cap(State::Available)
        } else if sock.exists() {
            cap(State::TemporarilyUnavailable).reason("Shell Core isn't answering")
        } else if Programs::find("nyx-shell-core").is_some() {
            cap(State::Inactive).reason("Shell Core isn't running").operations(&["start"])
        } else {
            cap(State::NotInstalled).reason("Shell Core isn't installed")
        };
        vec![c]
    }

    fn sources(&self) -> Vec<Source> {
        ShellCore::socket().parent().map(|d| vec![Source::Folder(d.to_path_buf())]).unwrap_or_default()
    }
}

/// Flux's fluxd, Nyx Connect's backend: its socket answering.
pub struct Connect;

impl Connect {
    /// fluxd's rule: `$FLUX_SOCKET`, else the runtime folder's `flux/fluxd.sock`.
    pub fn socket() -> PathBuf {
        std::env::var_os("FLUX_SOCKET")
            .filter(|p| !p.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| runtime_dir().join("flux/fluxd.sock"))
    }
}

impl Provider for Connect {
    fn name(&self) -> &str {
        "connect"
    }

    fn probe(&self) -> Vec<Capability> {
        let sock = Connect::socket();
        let cap = |s| Capability::new("daemon:flux", "connect", s).sections(&["devices"]);
        let c = if std::os::unix::net::UnixStream::connect(&sock).is_ok() {
            cap(State::Available)
        } else if Programs::find("fluxd").is_some() {
            cap(State::Inactive).reason("Flux isn't running").operations(&["start"])
        } else {
            cap(State::NotInstalled).reason("Flux isn't installed")
        };
        vec![c]
    }

    fn sources(&self) -> Vec<Source> {
        let mut s: Vec<Source> = program_dirs().into_iter().map(Source::Folder).collect();
        s.push(Source::Units { system: false });
        if let Some(dir) = Connect::socket().parent() {
            s.push(Source::Folder(dir.to_path_buf()));
        }
        s
    }
}

/// The sound server (PipeWire, through its PulseAudio interface).
pub struct Audio;

impl Provider for Audio {
    fn name(&self) -> &str {
        "audio"
    }

    fn probe(&self) -> Vec<Capability> {
        let cap = |s| Capability::new("daemon:audio", "audio", s);
        let c = if Programs::find("pactl").is_none() {
            cap(State::NotInstalled).reason("pactl isn't installed")
        } else if output("pactl", &["info"]).is_some() {
            cap(State::Available)
        } else {
            cap(State::TemporarilyUnavailable).reason("The sound server isn't running")
        };
        vec![c]
    }

    fn sources(&self) -> Vec<Source> {
        vec![Source::Lines { cmd: vec!["pactl".into(), "subscribe".into()], matching: vec!["server".into(), "card".into()] }]
    }
}

/// A fingerprint reader, through fprintd.
pub struct Fingerprint;

impl Provider for Fingerprint {
    fn name(&self) -> &str {
        "fingerprint"
    }

    fn probe(&self) -> Vec<Capability> {
        let cap = |s| Capability::new("hw:fingerprint", "fingerprint", s);
        let reply = Command::new("busctl")
            .args([
                "--system",
                "call",
                "net.reactivated.Fprint",
                "/net/reactivated/Fprint/Manager",
                "net.reactivated.Fprint.Manager",
                "GetDevices",
            ])
            .output();
        let c = match reply {
            Ok(o) if o.status.success() => {
                // `ao N path…`
                let n: usize =
                    String::from_utf8_lossy(&o.stdout).split_whitespace().nth(1).and_then(|n| n.parse().ok()).unwrap_or(0);
                if n > 0 {
                    cap(State::Available)
                } else {
                    cap(State::Unsupported).reason("No supported fingerprint reader is connected")
                }
            }
            Ok(o)
                if String::from_utf8_lossy(&o.stderr).contains("not provided by any .service")
                    || String::from_utf8_lossy(&o.stderr).contains("ServiceUnknown") =>
            {
                cap(State::NotInstalled).reason("Fingerprint support (fprintd) isn't installed")
            }
            Ok(_) => cap(State::TemporarilyUnavailable).reason("The fingerprint service didn't answer"),
            Err(_) => cap(State::TemporarilyUnavailable).reason("busctl isn't available"),
        };
        vec![c]
    }

    fn sources(&self) -> Vec<Source> {
        vec![Source::Udev("usb".into())]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn systemctl_show_blocks_become_states() {
        let text = "Id=nyx-dock.service\nLoadState=loaded\nActiveState=active\n\nId=nope.service\nLoadState=not-found\nActiveState=inactive\n\nId=x.service\nLoadState=loaded\nActiveState=failed\n";
        let b = parse_show(text);
        assert_eq!(b.len(), 3);
        assert_eq!(unit_state("a", &b[0].1, &b[0].2).state, State::Available);
        assert_eq!(unit_state("b", &b[1].1, &b[1].2).state, State::NotInstalled);
        let failed = unit_state("c", &b[2].1, &b[2].2);
        assert_eq!(failed.state, State::Inactive);
        assert_eq!(failed.operations, vec!["start"]);
    }

    #[test]
    fn programs_are_found_in_their_folders() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let bin = d.path().join("nyx-dock");
        std::fs::write(&bin, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(d.path().join("not-executable"), "").unwrap();
        assert!(executable(&bin));
        assert!(!executable(&d.path().join("not-executable")));
        assert!(!executable(d.path()));
    }

    #[test]
    fn hardware_from_a_fake_sysfs() {
        let d = tempfile::tempdir().unwrap();
        let sys = d.path().join("sys");
        let mk = |p: &str, v: &str| {
            let f = sys.join(p);
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(f, v).unwrap();
        };
        mk("class/power_supply/BAT1/type", "Battery\n");
        mk("class/power_supply/hidpp_battery_0/type", "Battery\n");
        mk("class/power_supply/hidpp_battery_0/scope", "Device\n");
        mk("class/bluetooth/hci0/x", "");
        mk("class/rfkill/rfkill0/type", "bluetooth\n");
        mk("class/rfkill/rfkill0/soft", "1\n");
        mk("class/rfkill/rfkill0/hard", "0\n");
        std::fs::create_dir_all(sys.join("class/drm/card0-eDP-2")).unwrap();
        let udev = d.path().join("udev");
        std::fs::create_dir_all(&udev).unwrap();
        std::fs::write(udev.join("c13:74"), "E:ID_INPUT=1\nE:ID_INPUT_TOUCHPAD=1\n").unwrap();
        let hw = Hardware { sys, udev_data: udev, dev: d.path().join("dev") };
        let caps = hw.probe();
        let state = |id: &str| caps.iter().find(|c| c.id == id).unwrap().state;
        assert_eq!(state("hw:battery"), State::Available);
        assert_eq!(state("hw:bluetooth"), State::Inactive);
        assert_eq!(state("hw:touchpad"), State::Available);
        assert_eq!(state("hw:touchscreen"), State::Unsupported);
        assert_eq!(state("hw:laptop-panel"), State::Available);
        assert_eq!(state("hw:backlight"), State::Unsupported);
        assert_eq!(state("hw:camera"), State::Unsupported);
    }
}
