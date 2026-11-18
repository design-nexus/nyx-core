//! Taildrop: sending files to a machine on the tailnet, and receiving them into
//! Downloads with a notification.

use super::desktop::{choose_files, notify};
use super::{Fail, Res};
use std::path::{Path, PathBuf};
use std::process::Command;

/// `tailscale-send MACHINE [FILE…]`: without files, the file chooser asks for them.
pub fn send(args: &[&str]) -> Res {
    let Some((machine, files)) = args.split_first() else { return Err(Fail::Usage("<machine> [file...]")) };
    // Spoken of by its short name, so a MagicDNS name doesn't fill every message.
    let name = machine.split('.').next().unwrap_or(machine);
    let mut files: Vec<PathBuf> = files.iter().map(PathBuf::from).collect();
    if files.is_empty() {
        match choose_files(&format!("Send to {name}")) {
            Ok(Some(chosen)) if !chosen.is_empty() => files = chosen,
            Ok(_) => return Ok(()),
            Err(e) => {
                notify("Taildrop", &format!("Couldn't send to {name}"), &e, true);
                return Err(e.into());
            }
        }
    }
    let what = if files.len() == 1 {
        files[0].file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    } else {
        format!("{} files", files.len())
    };
    let out = Command::new("tailscale")
        .args(["file", "cp", "--update-interval=0", "--"])
        .args(&files)
        .arg(format!("{machine}:"))
        .output()
        .map_err(|e| format!("tailscale didn't run: {e}"))?;
    if out.status.success() {
        notify("Taildrop", &format!("Sent to {name}"), &what, false);
        Ok(())
    } else {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        let why = if err.is_empty() { "The transfer failed.".to_string() } else { err };
        notify("Taildrop", &format!("Couldn't send to {name}"), &why, true);
        Err(why.into())
    }
}

fn downloads() -> PathBuf {
    if let Some(d) = std::env::var_os("XDG_DOWNLOAD_DIR") {
        return PathBuf::from(d);
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    // xdg-user-dirs' file: XDG_DOWNLOAD_DIR="$HOME/Downloads".
    let dirs = std::fs::read_to_string(home.join(".config/user-dirs.dirs")).unwrap_or_default();
    dirs.lines()
        .find_map(|l| l.strip_prefix("XDG_DOWNLOAD_DIR="))
        .map(|v| PathBuf::from(v.trim_matches('"').replace("$HOME", &home.to_string_lossy())))
        .unwrap_or_else(|| home.join("Downloads"))
}

/// Moves a received file from the staging folder to `dir` under a free name
/// (`notes.pdf`, `notes-1.pdf`, …).
pub fn claim(staged: &Path, dir: &Path) -> Option<PathBuf> {
    let name = staged.file_name()?.to_string_lossy().into_owned();
    let (base, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name.as_str(), ""),
    };
    for i in 0..1000 {
        let candidate = if i == 0 { dir.join(&name) } else { dir.join(format!("{base}-{i}{ext}")) };
        if std::fs::hard_link(staged, &candidate).is_ok() {
            let _ = std::fs::remove_file(staged);
            return Some(candidate);
        }
        if !candidate.exists() {
            return None;
        }
    }
    None
}

fn deliver(staging: &Path, dir: &Path) {
    let Ok(entries) = std::fs::read_dir(staging) else { return };
    for e in entries.flatten() {
        if let Some(target) = claim(&e.path(), dir) {
            let name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let home = std::env::var("HOME").unwrap_or_default();
            let shown = dir.to_string_lossy().replacen(&home, "~", 1);
            notify("Taildrop", &format!("Received {name}"), &format!("Saved to {shown}"), true);
        }
    }
}

/// `tailscale-receive [--once] [DIR]`: waits for files sent to this computer and saves
/// them to DIR (Downloads), announcing each.
pub fn receive(args: &[&str]) -> Res {
    let once = args.first() == Some(&"--once");
    let rest = if once { &args[1..] } else { args };
    let dir = rest.first().map(PathBuf::from).unwrap_or_else(downloads);
    let staging = dir.join(".nyx-taildrop");
    std::fs::create_dir_all(&staging).map_err(|e| format!("can't make {}: {e}", staging.display()))?;
    deliver(&staging, &dir);
    loop {
        let ok = Command::new("tailscale")
            .args(["file", "get", "--wait", "--conflict=rename"])
            .arg(&staging)
            .status()
            .is_ok_and(|s| s.success());
        if !ok {
            if once {
                return Err("tailscale couldn't receive files.".into());
            }
            std::thread::sleep(std::time::Duration::from_secs(10));
            continue;
        }
        deliver(&staging, &dir);
        if once {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_received_file_takes_a_free_name() {
        let dir = std::env::temp_dir().join(format!("nyx-platform-claim-{}", std::process::id()));
        let staging = dir.join(".stage");
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::write(dir.join("notes.pdf"), "old").unwrap();
        std::fs::write(staging.join("notes.pdf"), "new").unwrap();
        let got = super::claim(&staging.join("notes.pdf"), &dir).unwrap();
        assert_eq!(got.file_name().unwrap(), "notes-1.pdf");
        assert!(!staging.join("notes.pdf").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
