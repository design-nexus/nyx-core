//! The bubblewrap sandbox for third-party plugins (Glimpse's model). The plugin sees
//! the system's programs and libraries read-only, its own folder at `/plugin`
//! (read-only), its data folder at `/storage`, a private `/tmp`, and only what its
//! approved permissions add: the network, extra read-only paths, D-Bus through a
//! filtering proxy, the frame socket, the Wayland socket. No home folder, no Hyprland
//! sockets, no capabilities, its own session (no terminal injection).

use nyx_plugin::manifest::Permissions;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

pub const PLUGIN_DIR: &str = "/plugin";
pub const STORAGE_DIR: &str = "/storage";

/// Sockets made for this run, to bind in.
#[derive(Debug, Clone, Default)]
pub struct Sockets {
    pub session_bus: Option<PathBuf>,
    pub system_bus: Option<PathBuf>,
    pub frames: Option<PathBuf>,
}

/// Where the runtime folder is inside the sandbox.
pub fn inner_runtime_dir() -> PathBuf {
    PathBuf::from(format!("/run/user/{}", unsafe { libc::getuid() }))
}

pub fn inner_frames_socket() -> PathBuf {
    inner_runtime_dir().join("nyx-frames.sock")
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

fn expand(p: &str) -> PathBuf {
    match p.strip_prefix("~/") {
        Some(rest) => home().join(rest),
        None => PathBuf::from(p),
    }
}

/// Whether `bwrap` works here (it may be missing, or user namespaces may be off).
pub fn available() -> bool {
    std::process::Command::new("bwrap")
        .args(["--unshare-all", "--die-with-parent", "--ro-bind", "/", "/", "--", "true"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// The bwrap arguments, ending in `--`; the program goes after them. The sandbox's
/// environment is `env` alone (`--clearenv`), while `systemd-run` outside it keeps the
/// host's (it needs the user bus).
pub fn args(plugin_dir: &Path, storage: &Path, perms: &Permissions, sockets: &Sockets, env: &[(String, String)]) -> Vec<OsString> {
    let mut a: Vec<OsString> = vec![];

    push(&mut a, &["--unshare-all", "--die-with-parent", "--new-session", "--cap-drop", "ALL"]);
    if perms.network {
        push(&mut a, &["--share-net"]);
    }
    let nix = Path::new("/nix/store").is_dir();
    if nix {
        push(&mut a, &["--ro-bind", "/nix", "/nix", "--ro-bind-try", "/usr", "/usr", "--ro-bind-try", "/bin", "/bin"]);
    } else {
        push(&mut a, &[
            "--ro-bind", "/usr", "/usr", "--symlink", "usr/bin", "/bin", "--symlink", "usr/bin", "/sbin", "--symlink",
            "usr/lib", "/lib", "--symlink", "usr/lib", "/lib64",
        ]);
    }
    push(&mut a, &["--proc", "/proc", "--dev", "/dev", "--tmpfs", "/tmp", "--tmpfs", "/run"]);
    if nix {
        push(&mut a, &["--ro-bind-try", "/run/current-system", "/run/current-system"]);
        push(&mut a, &["--ro-bind-try", "/etc/static", "/etc/static"]);
        let profile = format!("/etc/profiles/per-user/{}", std::env::var("USER").unwrap_or_default());
        a.extend(["--ro-bind-try".into(), OsString::from(&profile), OsString::from(&profile)]);
    }
    let mut etc = vec![
        "ssl", "ca-certificates", "pki", "fonts", "localtime", "zoneinfo", "nsswitch.conf", "passwd", "group",
        "os-release", "ld.so.cache", "ld.so.conf", "ld.so.conf.d", "alternatives", "mime.types", "xdg",
    ];
    if perms.network {
        etc.extend(["resolv.conf", "hosts", "gai.conf"]);
    }
    for f in etc {
        let p = format!("/etc/{f}");
        a.extend(["--ro-bind-try".into(), OsString::from(&p), OsString::from(&p)]);
    }
    let canonical = plugin_dir.canonicalize().unwrap_or_else(|_| plugin_dir.to_path_buf());
    a.extend(["--ro-bind".into(), canonical.into_os_string(), PLUGIN_DIR.into()]);
    a.extend(["--bind".into(), storage.as_os_str().to_owned(), STORAGE_DIR.into()]);
    for p in &perms.read {
        let p = expand(p);
        a.extend(["--ro-bind-try".into(), p.as_os_str().to_owned(), p.into_os_string()]);
    }
    let runtime = inner_runtime_dir();
    a.extend(["--dir".into(), runtime.as_os_str().to_owned()]);
    if let Some(s) = &sockets.session_bus {
        a.extend(["--bind".into(), s.as_os_str().to_owned(), runtime.join("bus").into_os_string()]);
    }
    if let Some(s) = &sockets.system_bus {
        a.extend(["--dir".into(), "/run/dbus".into()]);
        a.extend(["--bind".into(), s.as_os_str().to_owned(), "/run/dbus/system_bus_socket".into()]);
    }
    if let Some(s) = &sockets.frames {
        a.extend(["--bind".into(), s.as_os_str().to_owned(), inner_frames_socket().into_os_string()]);
    }
    if perms.windows
        && let Some(display) = std::env::var_os("WAYLAND_DISPLAY")
    {
        let outer = nyx_plugin::paths::runtime_dir().join(&display);
        a.extend(["--ro-bind-try".into(), outer.into_os_string(), runtime.join(&display).into_os_string()]);
        push(&mut a, &["--dev-bind-try", "/dev/dri", "/dev/dri"]);
    }
    push(&mut a, &["--clearenv"]);
    for (k, v) in env {
        a.extend(["--setenv".into(), OsString::from(k), OsString::from(v)]);
    }
    a.extend(["--chdir".into(), PLUGIN_DIR.into(), "--".into()]);
    a
}

/// The environment inside the sandbox: nothing from the host but the basics.
pub fn env(perms: &Permissions, sockets: &Sockets, id: &str, kind: &str) -> Vec<(String, String)> {
    let runtime = inner_runtime_dir();
    let path = if Path::new("/nix/store").is_dir() {
        let user = std::env::var("USER").unwrap_or_default();
        format!("/run/current-system/sw/bin:/etc/profiles/per-user/{user}/bin:/usr/bin:/bin")
    } else {
        "/usr/local/bin:/usr/bin:/bin".to_string()
    };
    let mut env = vec![
        ("PATH".into(), path),
        ("HOME".into(), STORAGE_DIR.into()),
        ("NYX_PLUGIN_STORAGE".into(), STORAGE_DIR.into()),
        ("NYX_PLUGIN_DIR".into(), PLUGIN_DIR.into()),
        ("NYX_PLUGIN_ID".into(), id.into()),
        ("NYX_PLUGIN_KIND".into(), kind.into()),
        ("XDG_RUNTIME_DIR".into(), runtime.to_string_lossy().into()),
        ("TMPDIR".into(), "/tmp".into()),
    ];
    for var in ["LANG", "LC_ALL", "LC_MESSAGES", "LC_TIME", "LC_NUMERIC", "TZ"] {
        if let Ok(v) = std::env::var(var) {
            env.push((var.into(), v));
        }
    }
    if sockets.session_bus.is_some() {
        env.push(("DBUS_SESSION_BUS_ADDRESS".into(), format!("unix:path={}", runtime.join("bus").display())));
    }
    if sockets.system_bus.is_some() {
        env.push(("DBUS_SYSTEM_BUS_ADDRESS".into(), "unix:path=/run/dbus/system_bus_socket".into()));
    }
    if perms.windows
        && let Ok(d) = std::env::var("WAYLAND_DISPLAY")
    {
        env.push(("WAYLAND_DISPLAY".into(), d));
    }
    env
}

/// The program to run inside the sandbox: a file in the plugin's folder becomes
/// `/plugin/…`; anything else is a command on the sandbox's PATH.
pub fn inner_exec(plugin_dir: &Path, exec: &[String]) -> Vec<String> {
    let mut out = exec.to_vec();
    if let Some(first) = out.first_mut() {
        let rel = first.trim_start_matches("./");
        if plugin_dir.join(rel).is_file() {
            *first = format!("{PLUGIN_DIR}/{rel}");
        }
    }
    out
}

/// The program outside a sandbox (trusted plugins).
pub fn outer_exec(plugin_dir: &Path, exec: &[String]) -> Vec<OsString> {
    let mut out: Vec<OsString> = exec.iter().map(OsString::from).collect();
    if let Some(first) = exec.first() {
        let rel = first.trim_start_matches("./");
        let local = plugin_dir.join(rel);
        if local.is_file() {
            out[0] = local.into_os_string();
        }
    }
    out
}

fn push(a: &mut Vec<OsString>, items: &[&str]) {
    a.extend(items.iter().map(OsString::from));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_what_was_granted() {
        let t = tempfile::tempdir().unwrap();
        let none = args(t.path(), t.path(), &Permissions::default(), &Sockets::default(), &[]);
        let s: Vec<String> = none.iter().map(|a| a.to_string_lossy().into()).collect();
        assert!(s.contains(&"--unshare-all".to_string()));
        assert!(!s.contains(&"--share-net".to_string()));
        assert!(!s.iter().any(|a| a.contains("resolv.conf")));
        assert!(!s.iter().any(|a| a.ends_with("/bus")));
        assert_eq!(s.last().map(String::as_str), Some("--"));
        let net = Permissions { network: true, read: vec!["~/Music".into()], ..Default::default() };
        let sockets = Sockets { session_bus: Some(t.path().join("bus")), ..Default::default() };
        let s: Vec<String> = args(t.path(), t.path(), &net, &sockets, &[]).iter().map(|a| a.to_string_lossy().into()).collect();
        assert!(s.contains(&"--share-net".to_string()));
        assert!(s.iter().any(|a| a.ends_with("/Music")));
        assert!(s.iter().any(|a| a.ends_with("/bus")));
        let env = env(&net, &sockets, "acme.x", "bar");
        assert!(env.iter().any(|(k, _)| k == "DBUS_SESSION_BUS_ADDRESS"));
        assert!(!env.iter().any(|(k, _)| k == "WAYLAND_DISPLAY"));
    }

    #[test]
    fn programs_in_the_folder() {
        let t = tempfile::tempdir().unwrap();
        std::fs::write(t.path().join("run.sh"), "").unwrap();
        assert_eq!(inner_exec(t.path(), &["./run.sh".into(), "x".into()]), vec!["/plugin/run.sh", "x"]);
        assert_eq!(inner_exec(t.path(), &["python3".into(), "main.py".into()]), vec!["python3", "main.py"]);
        assert_eq!(outer_exec(t.path(), &["run.sh".into()])[0], t.path().join("run.sh").into_os_string());
    }
}
