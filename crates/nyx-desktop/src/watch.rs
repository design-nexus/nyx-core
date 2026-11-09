//! A thread that holds Hyprland's event socket and keeps a [`Desktop`] current.
//!
//! It subscribes first and reads the snapshot second, so nothing that happens in
//! between is missed. Events that need data they don't carry (a window opened: where
//! did it land?) are batched: the socket is drained of everything Hyprland has already
//! written, then `j/clients` or `j/workspaces` is read once for the lot. When Hyprland
//! goes away (restart, logout) the thread reports `connected: false`, finds the new
//! instance, reconnects and reads everything again.
//!
//! Idle cost: a thread blocked in `read`. No timers, no polling.

use crate::event::Event;
use crate::hypr::Instance;
use crate::model::{Changes, Desktop};
use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The state after an event (or a batch of them) and what changed.
#[derive(Debug, Clone)]
pub struct Update {
    pub desktop: Arc<Desktop>,
    pub changes: Changes,
}

/// Stops the thread when dropped.
pub struct Watcher {
    shared: Arc<Shared>,
}

#[derive(Default)]
struct Shared {
    stop: AtomicBool,
    stream: Mutex<Option<UnixStream>>,
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        if let Some(s) = self.shared.stream.lock().unwrap().take() {
            let _ = s.shutdown(std::net::Shutdown::Both);
        }
    }
}

/// Start watching. `on_update` runs on the watcher's thread: first with the full
/// snapshot, then after every change. Use [`watch_local`] to get updates on the GTK
/// main loop instead.
pub fn watch(on_update: impl FnMut(Update) + Send + 'static) -> Watcher {
    let shared = Arc::new(Shared::default());
    let thread_shared = shared.clone();
    std::thread::Builder::new()
        .name("nyx-desktop-hyprland".into())
        .spawn(move || run(&thread_shared, Instance::find, on_update))
        .expect("spawn the Hyprland watcher");
    Watcher { shared }
}

const BACKOFF_START: Duration = Duration::from_millis(100);
const BACKOFF_MAX: Duration = Duration::from_secs(3);

fn run(shared: &Shared, find: impl Fn() -> anyhow::Result<Instance>, mut on_update: impl FnMut(Update)) {
    let mut desktop = Desktop::default();
    let mut backoff = BACKOFF_START;
    let mut was_connected = false;
    while !shared.stop.load(Ordering::SeqCst) {
        let connected = find().and_then(|i| {
            let stream = UnixStream::connect(i.event_socket())?;
            Ok((i, stream))
        });
        let (instance, stream) = match connected {
            Ok(c) => c,
            Err(_) => {
                if was_connected {
                    desktop.connected = false;
                    on_update(Update { desktop: Arc::new(desktop.clone()), changes: Changes::ALL });
                    was_connected = false;
                }
                std::thread::sleep(backoff);
                backoff = (backoff * 2).min(BACKOFF_MAX);
                continue;
            }
        };
        if let Ok(clone) = stream.try_clone() {
            *shared.stream.lock().unwrap() = Some(clone);
        }
        if shared.stop.load(Ordering::SeqCst) {
            break;
        }
        match Desktop::snapshot(&instance) {
            Ok(d) => desktop = d,
            Err(_) => {
                std::thread::sleep(backoff);
                backoff = (backoff * 2).min(BACKOFF_MAX);
                continue;
            }
        }
        backoff = BACKOFF_START;
        was_connected = true;
        on_update(Update { desktop: Arc::new(desktop.clone()), changes: Changes::ALL });
        read_events(&instance, stream, &mut desktop, &mut on_update);
        // The socket closed: Hyprland went away or we're stopping.
        *shared.stream.lock().unwrap() = None;
        if !shared.stop.load(Ordering::SeqCst) {
            desktop.connected = false;
            on_update(Update { desktop: Arc::new(desktop.clone()), changes: Changes::ALL });
            was_connected = false;
        }
    }
}

fn read_events(instance: &Instance, stream: UnixStream, desktop: &mut Desktop, on_update: &mut impl FnMut(Update)) {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    let mut changes = Changes::default();
    let (mut need_windows, mut need_layout) = (false, false);
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        let event = Event::parse(line.trim_end_matches('\n'));
        changes.merge(desktop.apply(&event));
        need_windows |= event.needs_windows();
        need_layout |= event.needs_layout();
        // More already read? Apply it before asking Hyprland for anything.
        if !reader.buffer().is_empty() {
            continue;
        }
        if need_layout && desktop.refresh_layout(instance).is_ok() {
            changes.merge(Changes { monitors: true, workspaces: true, active: true, ..Default::default() });
        }
        if (need_windows || need_layout) && desktop.refresh_windows(instance).is_ok() {
            changes.merge(Changes { windows: true, focus: true, ..Default::default() });
        }
        if changes.any() {
            on_update(Update { desktop: Arc::new(desktop.clone()), changes });
        }
        changes = Changes::default();
        need_windows = false;
        need_layout = false;
    }
}

/// As [`watch`], with updates delivered on the default GLib main context (the GTK main
/// thread). Updates that queue up while the main loop is busy are folded into one.
#[cfg(feature = "glib")]
pub fn watch_local(on_update: impl Fn(&Update) + 'static) -> Watcher {
    let (tx, rx) = async_channel::unbounded::<Update>();
    let watcher = watch(move |u| {
        let _ = tx.send_blocking(u);
    });
    glib::MainContext::default().spawn_local(async move {
        while let Ok(mut update) = rx.recv().await {
            while let Ok(next) = rx.try_recv() {
                let mut changes = update.changes;
                changes.merge(next.changes);
                update = Update { desktop: next.desktop, changes };
            }
            on_update(&update);
        }
    });
    watcher
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::os::unix::net::UnixListener;
    use std::path::PathBuf;
    use std::sync::mpsc;

    /// A pretend Hyprland: answers `j/…` from the test fixtures and hands the test its
    /// event connection.
    struct Fake {
        dir: PathBuf,
        events: mpsc::Receiver<UnixStream>,
    }

    impl Fake {
        fn start(dir: PathBuf, clients: &'static str) -> Fake {
            std::fs::create_dir_all(&dir).unwrap();
            let commands = UnixListener::bind(dir.join(".socket.sock")).unwrap();
            let events = UnixListener::bind(dir.join(".socket2.sock")).unwrap();
            std::thread::spawn(move || {
                for mut s in commands.incoming().flatten() {
                    let mut buf = [0u8; 256];
                    let n = s.read(&mut buf).unwrap_or(0);
                    let reply = match std::str::from_utf8(&buf[..n]).unwrap_or("") {
                        "j/monitors all" => include_str!("../tests/data/monitors.json"),
                        "j/workspaces" => include_str!("../tests/data/workspaces.json"),
                        "j/clients" => clients,
                        "j/activewindow" => r#"{"address": "0x55ef7100a730"}"#,
                        "j/layers" => r#"{"eDP-2": {"levels": {"3": [{"namespace": "nyx-control-center"}]}}}"#,
                        _ => "unknown request",
                    };
                    let _ = s.write_all(reply.as_bytes());
                }
            });
            let (tx, rx) = mpsc::channel();
            std::thread::spawn(move || {
                for s in events.incoming().flatten() {
                    if tx.send(s).is_err() {
                        break;
                    }
                }
            });
            Fake { dir, events: rx }
        }

        fn instance(&self) -> Instance {
            Instance { dir: self.dir.clone() }
        }

        fn shut(&self) {
            let _ = std::fs::remove_file(self.dir.join(".socket.sock"));
            let _ = std::fs::remove_file(self.dir.join(".socket2.sock"));
        }
    }

    fn next(rx: &mpsc::Receiver<Update>) -> Update {
        rx.recv_timeout(Duration::from_secs(5)).expect("an update")
    }

    #[test]
    fn follows_events_and_reconnects() {
        let base = std::env::temp_dir().join(format!("nyx-desktop-watch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let first = Fake::start(base.join("first"), include_str!("../tests/data/clients.json"));
        let current = Arc::new(Mutex::new(first.instance()));

        let (tx, rx) = mpsc::channel();
        let shared = Arc::new(Shared::default());
        let (s, c) = (shared.clone(), current.clone());
        std::thread::spawn(move || {
            run(
                &s,
                move || Ok(c.lock().unwrap().clone()),
                move |u| {
                    let _ = tx.send(u);
                },
            )
        });

        // Subscribed, then the snapshot.
        let mut events = first.events.recv_timeout(Duration::from_secs(5)).unwrap();
        let u = next(&rx);
        assert!(u.desktop.connected);
        assert_eq!(u.changes, Changes::ALL);
        assert_eq!(u.desktop.windows.len(), 3);
        assert!(u.desktop.layer_open("nyx-control-center"));

        // A panel hides: its layer closes, nothing else is read.
        events.write_all(b"closelayer>>nyx-control-center\n").unwrap();
        let u = next(&rx);
        assert!(u.changes.layers && !u.changes.windows);
        assert!(!u.desktop.layer_open("nyx-control-center"));

        // Two events in one write arrive as one update.
        events.write_all(b"workspace>>2\nworkspacev2>>2,2\nactivewindowv2>>55ef72c2b830\n").unwrap();
        let u = next(&rx);
        assert!(u.changes.active && u.changes.focus && !u.changes.windows);
        assert_eq!(u.desktop.monitor("eDP-2").unwrap().active_workspace, 2);
        assert_eq!(u.desktop.active_window.as_deref(), Some("55ef72c2b830"));

        // A window event reads the window list again (the fixture's, so the provisional
        // window is replaced by what Hyprland reports).
        events.write_all(b"openwindow>>abc,1,kitty,~\n").unwrap();
        let u = next(&rx);
        assert!(u.changes.windows);
        assert!(u.desktop.window("abc").is_none());

        // Hyprland restarts: disconnected, then a new instance and a fresh snapshot.
        let second = Fake::start(base.join("second"), "[]");
        *current.lock().unwrap() = second.instance();
        first.shut();
        drop(events);
        let u = next(&rx);
        assert!(!u.desktop.connected);
        let _events = second.events.recv_timeout(Duration::from_secs(5)).unwrap();
        let u = next(&rx);
        assert!(u.desktop.connected);
        assert!(u.desktop.windows.is_empty(), "state comes from the new instance");

        shared.stop.store(true, Ordering::SeqCst);
        let _ = std::fs::remove_dir_all(&base);
    }
}
