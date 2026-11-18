//! The session's notifications and file chooser, over D-Bus (NyxOS has neither
//! notify-send nor zenity).

use std::collections::HashMap;
use std::path::PathBuf;
use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::{OwnedValue, Value};

/// A notification; `urgent` ones stay until dismissed.
pub fn notify(app: &str, summary: &str, body: &str, urgent: bool) {
    let Ok(conn) = Connection::session() else { return };
    let mut hints: HashMap<&str, Value> = HashMap::new();
    hints.insert("urgency", Value::U8(if urgent { 2 } else { 1 }));
    let _ = conn.call_method(
        Some("org.freedesktop.Notifications"),
        "/org/freedesktop/Notifications",
        Some("org.freedesktop.Notifications"),
        "Notify",
        &(app, 0u32, "", summary, body, Vec::<&str>::new(), hints, -1i32),
    );
}

/// `file:///a%20b` → `/a b`.
pub fn path_from_uri(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let bytes = rest.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(b) = u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?, 16)
        {
            out.push(b);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    use std::os::unix::ffi::OsStringExt;
    Some(PathBuf::from(std::ffi::OsString::from_vec(out)))
}

/// Files chosen in the desktop's file chooser (the portal): `None` when cancelled.
pub fn choose_files(title: &str) -> Result<Option<Vec<PathBuf>>, String> {
    let e = |e: zbus::Error| format!("the file chooser didn't open: {e}");
    let conn = Connection::session().map_err(e)?;
    let token = format!("nyx_platform_{}", std::process::id());
    let sender = conn.unique_name().map(|n| n.trim_start_matches(':').replace('.', "_")).unwrap_or_default();
    let path = format!("/org/freedesktop/portal/desktop/request/{sender}/{token}");
    // Listen before asking, so a quick answer isn't missed.
    let request =
        Proxy::new(&conn, "org.freedesktop.portal.Desktop", path.as_str(), "org.freedesktop.portal.Request").map_err(e)?;
    let mut responses = request.receive_signal("Response").map_err(e)?;
    let mut options: HashMap<&str, Value> = HashMap::new();
    options.insert("handle_token", Value::from(token.as_str()));
    options.insert("multiple", Value::from(true));
    conn.call_method(
        Some("org.freedesktop.portal.Desktop"),
        "/org/freedesktop/portal/desktop",
        Some("org.freedesktop.portal.FileChooser"),
        "OpenFile",
        &("", title, options),
    )
    .map_err(e)?;
    let msg = responses.next().ok_or("the file chooser closed without answering")?;
    let (code, results): (u32, HashMap<String, OwnedValue>) = msg.body().deserialize().map_err(|err| err.to_string())?;
    if code != 0 {
        return Ok(None);
    }
    let uris: Vec<String> = match results.get("uris") {
        Some(v) => Vec::<String>::try_from(v.try_clone().map_err(|err| err.to_string())?).map_err(|err| err.to_string())?,
        None => Vec::new(),
    };
    Ok(Some(uris.iter().filter_map(|u| path_from_uri(u)).collect()))
}

#[cfg(test)]
mod tests {
    #[test]
    fn uris_become_paths() {
        assert_eq!(super::path_from_uri("file:///home/k/a%20b.pdf").unwrap().to_str(), Some("/home/k/a b.pdf"));
        assert_eq!(super::path_from_uri("file:///x%").unwrap().to_str(), Some("/x%"));
        assert!(super::path_from_uri("https://x").is_none());
    }
}
