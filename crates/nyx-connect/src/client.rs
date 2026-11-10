//! Requests to fluxd: one JSON line out, the reply with the same `id` back. Events on
//! the same connection are skipped.

use crate::model::{Access, ClipEntry, Snapshot};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Where fluxd listens: `$FLUX_SOCKET`, else `$XDG_RUNTIME_DIR/flux/fluxd.sock`, else
/// `/run/user/<uid>/flux/fluxd.sock` (fluxd's own rule).
pub fn socket_path() -> PathBuf {
    if let Some(path) = std::env::var_os("FLUX_SOCKET").filter(|p| !p.is_empty()) {
        return path.into();
    }
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| PathBuf::from(format!("/run/user/{}", own_uid())))
        .join("flux/fluxd.sock")
}

pub(crate) fn own_uid() -> u32 {
    std::fs::metadata("/proc/self").map(|m| m.uid()).unwrap_or(u32::MAX)
}

/// What went wrong, for code to act on. [`Error::message`] is for people.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErrorKind {
    /// fluxd isn't running, or the connection to it dropped.
    Unavailable,
    /// The device has no link now.
    Offline,
    NotPaired,
    /// No paired device is connected and none was named.
    NoDevice,
    /// The name matches more than one device; use the ID.
    Ambiguous,
    NotFound,
    /// The device or this fluxd can't do it.
    Unsupported,
    /// The device's access to this is off.
    PermissionDenied,
    /// More than fluxd sends at once (256 KiB of text).
    TooLarge,
    /// No open pairing with that device or key.
    NoRequest,
    Timeout,
    BadRequest,
    /// An older fluxd that doesn't know the method.
    UnknownMethod,
    /// A reply that isn't what fluxd sends.
    BadReply,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    pub kind: ErrorKind,
    /// fluxd's code (`offline`, `not_paired`, ...), or Nyx's own for the first and last
    /// kinds.
    pub code: String,
    pub message: String,
}

impl Error {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        let kind = match code {
            "unavailable" => ErrorKind::Unavailable,
            "offline" => ErrorKind::Offline,
            "not_paired" => ErrorKind::NotPaired,
            "no_device" => ErrorKind::NoDevice,
            "ambiguous" => ErrorKind::Ambiguous,
            "not_found" => ErrorKind::NotFound,
            "not_supported" | "unsupported" => ErrorKind::Unsupported,
            "disabled" => ErrorKind::PermissionDenied,
            "too_large" => ErrorKind::TooLarge,
            "no_request" => ErrorKind::NoRequest,
            "timeout" => ErrorKind::Timeout,
            "bad_params" | "bad_setting" | "bad_address" => ErrorKind::BadRequest,
            "unknown_method" => ErrorKind::UnknownMethod,
            "bad_reply" => ErrorKind::BadReply,
            _ => ErrorKind::Other,
        };
        Error { kind, code: code.to_string(), message: message.into() }
    }

    pub fn unavailable(message: impl Into<String>) -> Self {
        Error::new("unavailable", message)
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

/// Connect to fluxd's socket, refusing one another user owns.
pub(crate) fn connect(socket: &Path) -> Result<UnixStream, Error> {
    match std::fs::metadata(socket) {
        Ok(m) if m.uid() != own_uid() => {
            return Err(Error::unavailable("Flux's socket belongs to another user"));
        }
        Ok(_) => {}
        Err(_) => return Err(Error::unavailable("Flux isn't running")),
    }
    UnixStream::connect(socket).map_err(|_| Error::unavailable("Flux isn't running"))
}

/// One connection, for several requests in a row.
pub struct Conn {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
    next: i64,
}

impl Conn {
    pub fn open(socket: &Path) -> Result<Self, Error> {
        let stream = connect(socket)?;
        let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
        let reader = stream.try_clone().map_err(|e| Error::unavailable(e.to_string()))?;
        let conn = Conn { reader: BufReader::new(reader), writer: stream, next: 1 };
        conn.wait_at_most(Duration::from_secs(10));
        Ok(conn)
    }

    /// How long a reply may take (10 s unless changed).
    pub fn wait_at_most(&self, timeout: Duration) {
        let _ = self.writer.set_read_timeout(Some(timeout));
    }

    pub fn call(&mut self, method: &str, params: Value) -> Result<Value, Error> {
        let id = self.next;
        self.next += 1;
        let line = format!("{}\n", json!({ "id": id, "method": method, "params": params }));
        let lost = |e: std::io::Error| match e.kind() {
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => Error::new("timeout", "Flux didn't answer in time"),
            _ => Error::unavailable(format!("Lost the connection to Flux: {e}")),
        };
        self.writer.write_all(line.as_bytes()).map_err(lost)?;
        loop {
            let mut reply = String::new();
            if self.reader.read_line(&mut reply).map_err(lost)? == 0 {
                return Err(Error::unavailable("Flux closed the connection"));
            }
            let Ok(msg) = serde_json::from_str::<Value>(&reply) else { continue };
            if msg.get("id").and_then(Value::as_i64) != Some(id) {
                continue;
            }
            if let Some(e) = msg.get("error") {
                return Err(Error::new(
                    e.get("code").and_then(Value::as_str).unwrap_or("error"),
                    e.get("message").and_then(Value::as_str).unwrap_or("Flux refused"),
                ));
            }
            return Ok(msg.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    pub fn state(&mut self) -> Result<Snapshot, Error> {
        parse_snapshot(self.call("state", json!({}))?)
    }
}

pub(crate) fn parse_snapshot(value: Value) -> Result<Snapshot, Error> {
    serde_json::from_value(value).map_err(|e| Error::new("bad_reply", format!("Flux answered oddly: {e}")))
}

/// What a pairing request or answer acted on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Paired {
    pub device: String,
    pub name: String,
    /// The verification key, 16 hex digits without spaces.
    pub key: String,
    pub fingerprint: String,
}

/// fluxd's methods, each on a short connection of its own.
#[derive(Debug, Clone)]
pub struct Client {
    socket: PathBuf,
    timeout: Duration,
}

impl Default for Client {
    fn default() -> Self {
        Client::new()
    }
}

impl Client {
    pub fn new() -> Self {
        Client::at(socket_path())
    }

    pub fn at(socket: impl Into<PathBuf>) -> Self {
        Client { socket: socket.into(), timeout: Duration::from_secs(10) }
    }

    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// Whether fluxd is listening.
    pub fn reachable(&self) -> bool {
        connect(&self.socket).is_ok()
    }

    pub fn call(&self, method: &str, params: Value) -> Result<Value, Error> {
        let mut conn = Conn::open(&self.socket)?;
        conn.wait_at_most(self.timeout);
        conn.call(method, params)
    }

    pub fn state(&self) -> Result<Snapshot, Error> {
        parse_snapshot(self.call("state", json!({}))?)
    }

    /// Broadcast this computer now, so nearby devices show up sooner.
    pub fn discover(&self) -> Result<(), Error> {
        self.call("discover", json!({})).map(drop)
    }

    pub fn pair_request(&self, device: &str) -> Result<Paired, Error> {
        self.pairing("pair.request", device, None)
    }

    /// Accept a request, or confirm a pairing the device accepted, for this `key` only.
    pub fn pair_accept(&self, device: &str, key: &str) -> Result<Paired, Error> {
        self.pairing("pair.accept", device, Some(key))
    }

    /// Reject a request, or cancel this computer's own.
    pub fn pair_reject(&self, device: &str, key: Option<&str>) -> Result<Paired, Error> {
        self.pairing("pair.reject", device, key)
    }

    /// Remove the device's trust here and on the device.
    pub fn unpair(&self, device: &str) -> Result<Paired, Error> {
        self.pairing("pair.unpair", device, None)
    }

    fn pairing(&self, method: &str, device: &str, key: Option<&str>) -> Result<Paired, Error> {
        let mut params = json!({ "device": device });
        if let Some(key) = key {
            params["key"] = json!(key);
        }
        let r = self.call(method, params)?;
        let s = |k: &str| r.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
        Ok(Paired { device: s("device"), name: s("name"), key: s("key"), fingerprint: s("fingerprint") })
    }

    pub fn ring(&self, device: &str) -> Result<(), Error> {
        self.call("ring", json!({ "device": device })).map(drop)
    }

    pub fn ping(&self, device: &str) -> Result<(), Error> {
        self.call("ping", json!({ "device": device })).map(drop)
    }

    /// Send text to the device's clipboard (needs the device's clipboard access).
    pub fn send_clipboard(&self, device: &str, text: &str) -> Result<(), Error> {
        self.call("clipboard.send", json!({ "device": device, "text": text })).map(drop)
    }

    /// The whole text of a history entry (the state holds only its first 1024 bytes).
    pub fn clip_text(&self, entry: &ClipEntry) -> Result<String, Error> {
        if !entry.truncated {
            return Ok(entry.text.clone());
        }
        let found = self.call("clipboard.search", json!({ "text": entry.text }))?;
        let list: Vec<ClipEntry> = serde_json::from_value(found).unwrap_or_default();
        list.into_iter()
            .find(|e| e.id == entry.id)
            .map(|e| e.text)
            .ok_or_else(|| Error::new("not_found", "Flux no longer has that clipboard entry"))
    }

    /// Queue files (absolute paths) for the device; returns the transfer IDs.
    pub fn share_files(&self, device: &str, paths: &[PathBuf]) -> Result<Vec<String>, Error> {
        let r = self.call("share.files", json!({ "device": device, "paths": paths }))?;
        Ok(r.get("transfers")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str().map(String::from))
            .collect())
    }

    pub fn share_text(&self, device: &str, text: &str) -> Result<(), Error> {
        self.call("share.text", json!({ "device": device, "text": text })).map(drop)
    }

    pub fn share_url(&self, device: &str, url: &str) -> Result<(), Error> {
        self.call("share.url", json!({ "device": device, "url": url })).map(drop)
    }

    pub fn cancel_transfer(&self, id: &str) -> Result<(), Error> {
        self.call("transfer.cancel", json!({ "id": id })).map(drop)
    }

    /// One of fluxd's settings (`autoClipboard`, `notifications`, `shareHome`,
    /// `downloadDir`, ...).
    pub fn set(&self, key: &str, value: Value) -> Result<(), Error> {
        self.call("settings.set", json!({ "key": key, "value": value })).map(drop)
    }

    /// The device's own choice for an access, or `None` to follow the global switch.
    pub fn set_access(&self, device: &str, access: Access, allowed: Option<bool>) -> Result<(), Error> {
        self.call("device.settings.set", json!({ "device": device, "key": access.key(), "value": allowed })).map(drop)
    }
}
