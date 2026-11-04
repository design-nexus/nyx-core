//! Live system data for plugins. Each `watch` starts a thread that calls you with the
//! new state whenever it changes (D-Bus signals, `pactl subscribe`), with a slow
//! safety refresh. A sandboxed plugin must declare the D-Bus names a service uses in
//! `session_talk` or `system_talk`; each module says which.

#[cfg(feature = "audio")]
pub mod audio;
#[cfg(feature = "bluetooth")]
pub mod bluetooth;
#[cfg(any(feature = "dbus", feature = "network", feature = "bluetooth", feature = "power", feature = "mpris"))]
pub mod dbus;
#[cfg(feature = "mpris")]
pub mod mpris;
#[cfg(feature = "network")]
pub mod network;
#[cfg(feature = "power")]
pub mod power;

use std::sync::mpsc;
use std::time::Duration;

/// Call `refresh` when `trigger` fires (at most every 150 ms) or every `every`.
pub fn refresher(every: Duration, mut refresh: impl FnMut() + Send + 'static) -> mpsc::Sender<()> {
    let (tx, rx) = mpsc::channel::<()>();
    std::thread::spawn(move || {
        refresh();
        loop {
            match rx.recv_timeout(every) {
                Ok(()) => {
                    std::thread::sleep(Duration::from_millis(150));
                    while rx.try_recv().is_ok() {}
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }
            refresh();
        }
    });
    tx
}
