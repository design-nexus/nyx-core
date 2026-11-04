//! D-Bus for sandboxed plugins: an `xdg-dbus-proxy` for one plugin, filtering the
//! session and system buses down to the names it may talk to. It stops when the
//! plugin does (`--fd`: it exits once we close our end).

use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

pub struct Proxy {
    child: Child,
    _keepalive: OwnedFd,
    pub session: Option<PathBuf>,
    pub system: Option<PathBuf>,
}

impl Drop for Proxy {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        for p in [&self.session, &self.system].into_iter().flatten() {
            let _ = std::fs::remove_file(p);
        }
    }
}

fn session_address() -> String {
    std::env::var("DBUS_SESSION_BUS_ADDRESS")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("unix:path={}", nyx_plugin::paths::runtime_dir().join("bus").display()))
}

/// Start a proxy for the names given. None when neither list has a name.
pub fn start(dir: &Path, session_talk: &[String], system_talk: &[String]) -> std::io::Result<Option<Proxy>> {
    if session_talk.is_empty() && system_talk.is_empty() {
        return Ok(None);
    }
    std::fs::create_dir_all(dir)?;
    let mut fds = [0; 2];
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    let (read_end, write_end) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
    let mut cmd = Command::new("xdg-dbus-proxy");
    cmd.arg("--fd=3");
    let mut session = None;
    let mut system = None;
    if !session_talk.is_empty() {
        let path = dir.join("session-bus");
        let _ = std::fs::remove_file(&path);
        cmd.arg(session_address()).arg(&path).arg("--filter");
        for n in session_talk {
            cmd.arg(format!("--talk={n}"));
        }
        session = Some(path);
    }
    if !system_talk.is_empty() {
        let path = dir.join("system-bus");
        let _ = std::fs::remove_file(&path);
        cmd.arg("unix:path=/run/dbus/system_bus_socket").arg(&path).arg("--filter");
        for n in system_talk {
            cmd.arg(format!("--talk={n}"));
        }
        system = Some(path);
    }
    // The proxy watches fd 3: our end (the read end) staying open keeps it running.
    let child_fd = read_end.as_raw_fd();
    unsafe {
        cmd.pre_exec(move || {
            if libc::dup2(child_fd, 3) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped());
    let mut child = cmd.spawn()?;
    drop(read_end);
    // Ready once its sockets exist.
    let deadline = Instant::now() + Duration::from_secs(3);
    let ready = || [&session, &system].into_iter().flatten().all(|p| p.exists());
    while !ready() {
        if let Ok(Some(status)) = child.try_wait() {
            let mut err = String::new();
            if let Some(mut e) = child.stderr.take() {
                let _ = e.read_to_string(&mut err);
            }
            return Err(std::io::Error::other(format!("xdg-dbus-proxy exited ({status}): {}", err.trim())));
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            return Err(std::io::Error::other("xdg-dbus-proxy didn't start"));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok(Some(Proxy { child, _keepalive: write_end, session, system }))
}

pub fn available() -> bool {
    nyx_plugin::discovery::on_path("xdg-dbus-proxy")
}
