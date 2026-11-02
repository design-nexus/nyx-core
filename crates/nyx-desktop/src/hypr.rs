//! Finding the running Hyprland and making requests on its command socket.

use anyhow::{Context, Result, bail};
use serde::de::DeserializeOwned;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How long a request may take before it's given up on.
const TIMEOUT: Duration = Duration::from_secs(3);

/// One Hyprland instance's folder: `$XDG_RUNTIME_DIR/hypr/<signature>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Instance {
    pub dir: PathBuf,
}

impl Instance {
    /// The command socket (`.socket.sock`).
    pub fn command_socket(&self) -> PathBuf {
        self.dir.join(".socket.sock")
    }

    /// The event socket (`.socket2.sock`).
    pub fn event_socket(&self) -> PathBuf {
        self.dir.join(".socket2.sock")
    }

    /// The running instance. `HYPRLAND_INSTANCE_SIGNATURE` names it, unless it is stale
    /// (Hyprland restarted since this process started, or a systemd service didn't
    /// inherit it): then the instance whose event socket was created last.
    pub fn find() -> Result<Self> {
        let runtime = std::env::var_os("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR isn't set")?;
        let base = PathBuf::from(runtime).join("hypr");
        let signature = std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE");
        Self::find_in(&base, signature.as_deref().map(Path::new))
    }

    /// As [`Instance::find`], in `base` (for tests).
    pub fn find_in(base: &Path, signature: Option<&Path>) -> Result<Self> {
        if let Some(sig) = signature {
            let named = Self { dir: base.join(sig) };
            if live(&named.event_socket()) {
                return Ok(named);
            }
        }
        let newest = std::fs::read_dir(base)
            .with_context(|| format!("no Hyprland folder at {}", base.display()))?
            .flatten()
            .map(|e| Self { dir: e.path() })
            .filter(|i| live(&i.event_socket()))
            .max_by_key(|i| std::fs::metadata(i.event_socket()).and_then(|m| m.modified()).ok());
        newest.context("Hyprland isn't running")
    }

    /// Send `command` (for example `j/monitors`) and return the whole reply.
    pub fn request(&self, command: &str) -> Result<String> {
        let mut stream = UnixStream::connect(self.command_socket()).context("connect to Hyprland")?;
        stream.set_read_timeout(Some(TIMEOUT))?;
        stream.set_write_timeout(Some(TIMEOUT))?;
        stream.write_all(command.as_bytes())?;
        let mut out = Vec::new();
        stream.read_to_end(&mut out).context("Hyprland didn't answer")?;
        Ok(String::from_utf8_lossy(&out).into_owned())
    }

    /// `j/<what>` decoded as `T`.
    pub fn json<T: DeserializeOwned>(&self, what: &str) -> Result<T> {
        let text = self.request(&format!("j/{what}"))?;
        serde_json::from_str(&text).with_context(|| format!("unexpected Hyprland reply to {what}: {}", short(&text)))
    }

    /// Run a dispatcher (`dispatch focuswindow address:0x…`). Hyprland answers `ok`.
    pub fn dispatch(&self, dispatcher: &str) -> Result<()> {
        let reply = self.request(&format!("dispatch {dispatcher}"))?;
        if reply.trim() != "ok" {
            bail!("Hyprland refused `{dispatcher}`: {}", short(&reply));
        }
        Ok(())
    }
}

/// A socket path that exists and is a socket. A crashed Hyprland can leave its folder.
fn live(path: &Path) -> bool {
    use std::os::unix::fs::FileTypeExt;
    std::fs::metadata(path).is_ok_and(|m| m.file_type().is_socket())
}

fn short(text: &str) -> &str {
    let end = text.char_indices().nth(120).map_or(text.len(), |(i, _)| i);
    &text[..end]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    fn instance(base: &Path, sig: &str) -> UnixListener {
        let dir = base.join(sig);
        std::fs::create_dir_all(&dir).unwrap();
        UnixListener::bind(dir.join(".socket2.sock")).unwrap()
    }

    #[test]
    fn named_instance_wins_when_live() {
        let tmp = tempdir("named");
        let _a = instance(&tmp, "a");
        std::thread::sleep(Duration::from_millis(20));
        let _b = instance(&tmp, "b");
        let found = Instance::find_in(&tmp, Some(Path::new("a"))).unwrap();
        assert_eq!(found.dir, tmp.join("a"));
    }

    #[test]
    fn stale_signature_falls_back_to_newest() {
        let tmp = tempdir("stale");
        std::fs::create_dir_all(tmp.join("gone")).unwrap();
        let _a = instance(&tmp, "a");
        std::thread::sleep(Duration::from_millis(20));
        let _b = instance(&tmp, "b");
        let found = Instance::find_in(&tmp, Some(Path::new("gone"))).unwrap();
        assert_eq!(found.dir, tmp.join("b"));
    }

    #[test]
    fn no_instance_is_an_error() {
        let tmp = tempdir("none");
        std::fs::create_dir_all(tmp.join("leftover")).unwrap();
        assert!(Instance::find_in(&tmp, None).is_err());
    }

    pub(crate) fn tempdir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nyx-desktop-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
