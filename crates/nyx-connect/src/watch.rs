//! Following fluxd: a thread that subscribes, hands each snapshot on, and reconnects
//! when fluxd restarts.

use crate::client::{Error, connect, parse_snapshot};
use crate::model::Snapshot;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// The whole state, after every change (fluxd coalesces bursts).
    State(Arc<Snapshot>),
    /// A short message fluxd shows in its window.
    Toast(String),
    /// fluxd isn't there (sent once each time it goes; the next `State` means it's back).
    Unavailable(Error),
}

/// The longest wait between attempts to reach fluxd.
const MAX_BACKOFF: Duration = Duration::from_secs(30);

/// Stops when dropped.
pub struct Watch {
    stop: Arc<AtomicBool>,
    stream: Arc<Mutex<Option<UnixStream>>>,
    thread: Option<JoinHandle<()>>,
}

impl Watch {
    /// Follow fluxd at the standard socket. `on_event` runs on the watch's own thread.
    pub fn start(on_event: impl FnMut(Event) + Send + 'static) -> Watch {
        Watch::at(crate::socket_path(), on_event)
    }

    pub fn at(socket: PathBuf, mut on_event: impl FnMut(Event) + Send + 'static) -> Watch {
        let stop = Arc::new(AtomicBool::new(false));
        let stream: Arc<Mutex<Option<UnixStream>>> = Arc::new(Mutex::new(None));
        let (stop2, stream2) = (stop.clone(), stream.clone());
        let thread = std::thread::Builder::new()
            .name("nyx-connect".into())
            .spawn(move || {
                let mut backoff = Duration::from_secs(1);
                let mut announced = false;
                while !stop2.load(Ordering::Relaxed) {
                    let reason = match follow(&socket, &stop2, &stream2, &mut on_event) {
                        Ok(true) => {
                            // It was up: the next try comes soon.
                            backoff = Duration::from_secs(1);
                            announced = false;
                            Error::unavailable("Flux stopped")
                        }
                        Ok(false) => break,
                        Err(e) => e,
                    };
                    if stop2.load(Ordering::Relaxed) {
                        break;
                    }
                    if !announced {
                        on_event(Event::Unavailable(reason));
                        announced = true;
                    }
                    sleep(&stop2, backoff);
                    backoff = (backoff * 2).min(MAX_BACKOFF);
                }
            })
            .ok();
        Watch { stop, stream, thread }
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(s) = self.stream.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            let _ = s.shutdown(std::net::Shutdown::Both);
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        self.stop();
    }
}

fn sleep(stop: &AtomicBool, total: Duration) {
    let step = Duration::from_millis(100);
    let mut slept = Duration::ZERO;
    while slept < total && !stop.load(Ordering::Relaxed) {
        std::thread::sleep(step);
        slept += step;
    }
}

/// One subscription. `Ok(true)`: it ran and then fluxd went; `Ok(false)`: stopped;
/// `Err`: fluxd couldn't be reached.
fn follow(
    socket: &std::path::Path,
    stop: &AtomicBool,
    keep: &Mutex<Option<UnixStream>>,
    on_event: &mut impl FnMut(Event),
) -> Result<bool, Error> {
    let mut stream = connect(socket)?;
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
    let reader = stream.try_clone().map_err(|e| Error::unavailable(e.to_string()))?;
    *keep.lock().unwrap_or_else(|e| e.into_inner()) = stream.try_clone().ok();
    if stop.load(Ordering::Relaxed) {
        return Ok(false);
    }
    // Events start with the next change, so ask for the state as it is now too.
    let hello = format!("{}\n{}\n", json!({ "id": 1, "method": "subscribe" }), json!({ "id": 2, "method": "state" }));
    if stream.write_all(hello.as_bytes()).is_err() {
        return Err(Error::unavailable("Flux closed the connection"));
    }
    for line in BufReader::new(reader).lines() {
        if stop.load(Ordering::Relaxed) {
            return Ok(false);
        }
        let Ok(line) = line else { break };
        let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
        let state = match (msg.get("event").and_then(Value::as_str), msg.get("id").and_then(Value::as_i64)) {
            (Some("state"), _) => msg.get("data").cloned(),
            (Some("toast"), _) => {
                let text = msg["data"].get("text").and_then(Value::as_str).unwrap_or_default();
                if !text.is_empty() {
                    on_event(Event::Toast(text.to_string()));
                }
                None
            }
            (None, Some(2)) => msg.get("result").cloned(),
            (None, Some(1)) if msg.get("error").is_some() => {
                return Err(Error::new("unknown_method", "This fluxd can't be followed"));
            }
            _ => None,
        };
        if let Some(value) = state
            && let Ok(snapshot) = parse_snapshot(value)
        {
            on_event(Event::State(Arc::new(snapshot)));
        }
    }
    *keep.lock().unwrap_or_else(|e| e.into_inner()) = None;
    Ok(!stop.load(Ordering::Relaxed))
}
