//! Switching a component between its GTK interface and its Quickshell one, for
//! `nyx-<app> --ui qml|gtk`, on a desktop where the user's systemd manages the units.
//!
//! QML on: the GTK unit stops and is disabled; the backend units start and are enabled;
//! the component's QML folder is linked into `$XDG_DATA_HOME/nyx-shell/components/<app>`,
//! where the shared Quickshell runtime (`nyx-shell-qs.service`) picks it up live.
//! GTK on: the reverse; the runtime stops when no component is left in it.
//!
//! On NyxOS the system configuration decides, so nothing here runs there (see
//! [`managed_by_system`]).

use std::path::{Path, PathBuf};
use std::process::Command;

/// One component's pieces.
pub struct Component<'a> {
    /// `nyx-dock`: the link's name and the data folder holding its QML.
    pub app: &'a str,
    /// The GTK interface's unit (`nyx-dock.service`).
    pub gtk_unit: &'a str,
    /// The units the QML interface needs besides the runtime (its backend).
    pub backend_units: &'a [&'a str],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ui {
    Gtk,
    Qml,
}

impl Ui {
    pub fn parse(s: &str) -> Option<Ui> {
        match s {
            "gtk" => Some(Ui::Gtk),
            "qml" | "quickshell" => Some(Ui::Qml),
            _ => None,
        }
    }
}

pub const RUNTIME_UNIT: &str = "nyx-shell-qs.service";

fn data_home() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/share"))
}

/// Where the runtime looks for components.
pub fn components_dir() -> PathBuf {
    data_home().join("nyx-shell/components")
}

/// Where a component's own QML is installed (`~/.local/share/nyx-dock/qml`).
pub fn qml_dir(app: &str) -> PathBuf {
    data_home().join(app).join("qml")
}

/// NixOS: units and files come from the system configuration.
pub fn managed_by_system() -> bool {
    Path::new("/etc/NIXOS").exists()
}

/// Which interface is on now.
pub fn current(c: &Component) -> Ui {
    if components_dir().join(c.app).exists() { Ui::Qml } else { Ui::Gtk }
}

fn systemctl(args: &[&str]) -> Result<(), String> {
    let out = Command::new("systemctl").arg("--user").args(args).output().map_err(|e| format!("systemctl: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!("systemctl --user {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim()))
    }
}

fn unit_exists(unit: &str) -> bool {
    Command::new("systemctl")
        .args(["--user", "cat", unit])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Turn the component's QML or GTK interface on. Returns what was done, for the user.
pub fn set(c: &Component, ui: Ui) -> Result<String, String> {
    if managed_by_system() {
        return Err("this system's configuration chooses the interface (NyxOS)".into());
    }
    let link = components_dir().join(c.app);
    match ui {
        Ui::Qml => {
            let target = qml_dir(c.app);
            if !target.join("Main.qml").is_file() {
                return Err(format!(
                    "the Quickshell interface isn't installed ({} has no Main.qml); run install.sh",
                    target.display()
                ));
            }
            for u in c.backend_units.iter().chain([&RUNTIME_UNIT]) {
                if !unit_exists(u) {
                    return Err(format!("{u} isn't installed; run install.sh"));
                }
            }
            std::fs::create_dir_all(components_dir()).map_err(|e| e.to_string())?;
            let _ = systemctl(&["disable", "--now", c.gtk_unit]);
            for u in c.backend_units {
                systemctl(&["enable", "--now", u])?;
            }
            let _ = std::fs::remove_file(&link);
            std::os::unix::fs::symlink(&target, &link).map_err(|e| format!("{}: {e}", link.display()))?;
            systemctl(&["enable", "--now", RUNTIME_UNIT])?;
            Ok(format!("{}: the Quickshell interface is on (nyx-shell-qs)", c.app))
        }
        Ui::Gtk => {
            let _ = std::fs::remove_file(&link);
            for u in c.backend_units {
                let _ = systemctl(&["disable", "--now", u]);
            }
            systemctl(&["enable", "--now", c.gtk_unit])?;
            let empty = std::fs::read_dir(components_dir()).map(|mut d| d.next().is_none()).unwrap_or(true);
            if empty {
                let _ = systemctl(&["disable", "--now", RUNTIME_UNIT]);
            }
            Ok(format!("{}: the GTK interface is on", c.app))
        }
    }
}

/// Forward a command line request (`--toggle`, `--show`, …) to a running backend.
/// `None` when no backend answers (the GTK interface handles it then).
pub fn forward(app: &str, op: &str, body: serde_json::Value) -> Option<Result<serde_json::Value, crate::CallError>> {
    let path = crate::socket_path(app);
    if !path.exists() {
        return None;
    }
    match crate::call(&path, op, body) {
        Err(crate::CallError::Unavailable(_)) => None,
        r => Some(r),
    }
}
