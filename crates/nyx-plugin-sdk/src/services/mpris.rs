//! Media players over MPRIS (session bus: `session_talk = ["org.mpris.MediaPlayer2.*"]`).

use super::dbus::{self, Connection, dict_of, get_all, num_of, prop_bool, prop_num, prop_str, str_of, strs_of};
use std::time::Duration;

const PATH: &str = "/org/mpris/MediaPlayer2";
const PLAYER: &str = "org.mpris.MediaPlayer2.Player";

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Player {
    /// Its bus name.
    pub bus: String,
    pub identity: String,
    pub playing: bool,
    pub title: String,
    pub artist: String,
    pub album: String,
    /// `file://` or `https://`.
    pub art: Option<String>,
    pub track: Option<String>,
    /// Microseconds.
    pub length: Option<i64>,
    pub position: Option<i64>,
    pub can_next: bool,
    pub can_previous: bool,
    pub can_seek: bool,
}

pub fn players(conn: &Connection) -> Vec<Player> {
    let Ok(reply) = conn.call_method(Some("org.freedesktop.DBus"), "/org/freedesktop/DBus", Some("org.freedesktop.DBus"), "ListNames", &()) else {
        return vec![];
    };
    let names: Vec<String> = reply.body().deserialize().unwrap_or_default();
    let roots: Vec<(String, Option<std::collections::HashMap<String, zbus::zvariant::OwnedValue>>)> = names
        .into_iter()
        .filter(|n| n.starts_with("org.mpris.MediaPlayer2."))
        .map(|bus| {
            let root = get_all(conn, &bus, PATH, "org.mpris.MediaPlayer2");
            (bus, root)
        })
        .collect();
    let entries: Vec<String> = roots.iter().filter_map(|(_, r)| r.as_ref().and_then(|r| prop_str(r, "DesktopEntry"))).collect();
    let mut out = vec![];
    for (bus, root) in roots {
        if webkit_duplicate(&bus, &entries) {
            continue;
        }
        let Some(p) = get_all(conn, &bus, PATH, PLAYER) else { continue };
        let meta = p.get("Metadata").map(|m| dict_of(m)).unwrap_or_default();
        let identity = root
            .as_ref()
            .and_then(|i| prop_str(i, "Identity"))
            .unwrap_or_else(|| bus.trim_start_matches("org.mpris.MediaPlayer2.").to_string());
        let artist = meta.get("xesam:artist").map(|a| strs_of(a).join(", ")).filter(|a| !a.is_empty()).or_else(|| meta.get("xesam:artist").and_then(|a| str_of(a))).unwrap_or_default();
        out.push(Player {
            identity,
            playing: prop_str(&p, "PlaybackStatus").as_deref() == Some("Playing"),
            title: meta.get("xesam:title").and_then(|t| str_of(t)).unwrap_or_default(),
            artist,
            album: meta.get("xesam:album").and_then(|t| str_of(t)).unwrap_or_default(),
            art: meta.get("mpris:artUrl").and_then(|t| str_of(t)).filter(|u| !u.is_empty()),
            track: meta.get("mpris:trackid").and_then(|t| str_of(t)),
            length: meta.get("mpris:length").and_then(|t| num_of(t)).map(|l| l as i64),
            position: prop_num(&p, "Position").map(|v| v as i64),
            can_next: prop_bool(&p, "CanGoNext").unwrap_or(false),
            can_previous: prop_bool(&p, "CanGoPrevious").unwrap_or(false),
            can_seek: prop_bool(&p, "CanSeek").unwrap_or(false),
            bus,
        });
    }
    out
}

/// Whether `bus` is WebKit's player for a web page inside an app that has a player of its
/// own: WebKitGTK publishes a page's media as `org.mpris.MediaPlayer2.<app ID>.Sandboxed.
/// instance-N`, and an app like Lyra publishes its own with that ID as its `DesktopEntry`.
pub fn webkit_duplicate(bus: &str, desktop_entries: &[String]) -> bool {
    bus.strip_prefix("org.mpris.MediaPlayer2.")
        .and_then(|rest| rest.split_once(".Sandboxed.instance-"))
        .is_some_and(|(app, _)| desktop_entries.iter().any(|e| e.trim_end_matches(".desktop") == app))
}

/// The player to show: one that's playing, else the first with a title.
pub fn current(conn: &Connection) -> Option<Player> {
    let all = players(conn);
    all.iter().find(|p| p.playing).or_else(|| all.iter().find(|p| !p.title.is_empty())).cloned()
}

/// Call `f` with the current player (or None) whenever it changes; positions are
/// refreshed every `position_every` while something plays.
pub fn watch(position_every: Duration, f: impl Fn(Option<Player>) + Send + 'static) {
    std::thread::spawn(move || {
        let Ok(conn) = Connection::session() else {
            f(None);
            return;
        };
        let c = conn.clone();
        let mut last: Option<Option<Player>> = None;
        let trigger = super::refresher(position_every, move || {
            let p = current(&c);
            if last.as_ref() != Some(&p) {
                last = Some(p.clone());
                f(p);
            }
        });
        dbus::listen(&conn, None, Some(PATH), trigger.clone());
        dbus::listen(&conn, Some("org.freedesktop.DBus"), None, trigger);
    });
}

fn call(bus: &str, method: &str) {
    if let Ok(conn) = Connection::session() {
        let _ = conn.call_method(Some(bus), PATH, Some(PLAYER), method, &());
    }
}

pub fn play_pause(bus: &str) {
    call(bus, "PlayPause");
}

pub fn next(bus: &str) {
    call(bus, "Next");
}

pub fn previous(bus: &str) {
    call(bus, "Previous");
}

/// Seek to `position` microseconds in `track`.
pub fn set_position(bus: &str, track: &str, position: i64) {
    if let (Ok(conn), Ok(track)) = (Connection::session(), zbus::zvariant::ObjectPath::try_from(track)) {
        let _ = conn.call_method(Some(bus), PATH, Some(PLAYER), "SetPosition", &(track, position));
    }
}

#[cfg(test)]
mod tests {
    use super::webkit_duplicate;

    #[test]
    fn webkit_players_of_apps_with_their_own_are_dropped() {
        let entries = vec!["io.github.design_nexus.nyx.Lyra".to_string(), "firefox".to_string()];
        assert!(webkit_duplicate("org.mpris.MediaPlayer2.io.github.design_nexus.nyx.Lyra.Sandboxed.instance-7113", &entries));
        assert!(!webkit_duplicate("org.mpris.MediaPlayer2.lyra", &entries), "the app's own player stays");
        assert!(!webkit_duplicate("org.mpris.MediaPlayer2.org.gnome.Epiphany.Sandboxed.instance-1", &entries), "a browser's page with no player of its own stays");
        assert!(!webkit_duplicate("org.mpris.MediaPlayer2.firefox.instance_1_31071", &entries));
    }
}
