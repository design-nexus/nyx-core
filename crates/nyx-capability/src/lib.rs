//! What this computer can do right now, normalized.
//!
//! A [`Capability`] is one thing a settings page or control depends on: an installed
//! program, a running service, a piece of hardware, a reachable daemon. Each has a
//! [`State`] that says more than "installed or not", and a reason when it isn't
//! available. [`Provider`]s probe them; a [`Registry`] keeps the latest answer and tells
//! subscribers what changed.
//!
//! Nothing polls. Each provider names the [`Source`]s whose events can change its answer
//! (a folder, a D-Bus name, a udev subsystem, a long-running command such as
//! `pactl subscribe`), and is probed again only when one of them fires.
//!
//! A capability's state is runtime information. It never decides whether a stored
//! preference exists: an unavailable capability makes a preference temporarily
//! inapplicable, it doesn't delete it.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, SystemTime};

pub mod providers;
mod sources;

/// How available a capability is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum State {
    /// Present and usable.
    Available,
    /// Installed, but not running or turned off; something can start it.
    Inactive,
    /// Expected, but not reachable right now (a daemon restarting, a device unplugged).
    TemporarilyUnavailable,
    /// This computer can't do it (no such hardware, or hardware no driver supports).
    Unsupported,
    /// The software isn't installed.
    NotInstalled,
    /// Present, but this user isn't allowed to use it.
    PermissionRestricted,
}

impl State {
    pub fn is_available(self) -> bool {
        self == State::Available
    }

    /// Whether the capability exists on this computer at all (so controls for it can be
    /// shown, possibly disabled, rather than hidden).
    pub fn exists(self) -> bool {
        matches!(self, State::Available | State::Inactive | State::TemporarilyUnavailable | State::PermissionRestricted)
    }

    pub fn label(self) -> &'static str {
        match self {
            State::Available => "available",
            State::Inactive => "installed but not running",
            State::TemporarilyUnavailable => "temporarily unavailable",
            State::Unsupported => "not supported on this computer",
            State::NotInstalled => "not installed",
            State::PermissionRestricted => "not allowed for this account",
        }
    }
}

/// One capability, as last probed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Capability {
    /// `kind:name`, e.g. `program:nyx-dock`, `service:user:nyx-notifyd`, `hw:battery`.
    pub id: String,
    /// The provider that probed it.
    pub provider: String,
    pub state: State,
    /// Why it isn't available, in words a settings page can show.
    pub reason: Option<String>,
    /// Operations it supports, when the provider knows (e.g. `start` for an inactive unit).
    pub operations: Vec<String>,
    /// Settings sections that depend on it, when the provider knows.
    pub sections: Vec<String>,
    #[serde(with = "unix_secs")]
    pub updated: SystemTime,
}

impl Capability {
    pub fn new(id: impl Into<String>, provider: &str, state: State) -> Self {
        Capability {
            id: id.into(),
            provider: provider.to_string(),
            state,
            reason: None,
            operations: Vec::new(),
            sections: Vec::new(),
            updated: SystemTime::now(),
        }
    }

    pub fn reason(mut self, reason: impl Into<String>) -> Self {
        self.reason = Some(reason.into());
        self
    }

    pub fn operations(mut self, ops: &[&str]) -> Self {
        self.operations = ops.iter().map(|s| s.to_string()).collect();
        self
    }

    pub fn sections(mut self, sections: &[&str]) -> Self {
        self.sections = sections.iter().map(|s| s.to_string()).collect();
        self
    }

    /// Equal apart from when it was probed.
    fn same(&self, other: &Capability) -> bool {
        self.state == other.state
            && self.reason == other.reason
            && self.operations == other.operations
            && self.sections == other.sections
            && self.provider == other.provider
    }
}

/// An event that can change a provider's answer.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Source {
    /// Anything created, removed or changed in this folder (not recursive).
    Folder(PathBuf),
    /// A name on the session (`system: false`) or system bus gaining or losing its owner.
    BusName { system: bool, name: String },
    /// A systemd unit changing state, on the user's or the system's manager.
    Units { system: bool },
    /// A udev event in this subsystem (`power_supply`, `input`, `usb`, `bluetooth`, ...).
    Udev(String),
    /// A line a long-running command prints that contains one of `matching` (any line if
    /// it's empty), and the command ending. It's restarted, with a delay, when it ends.
    Lines { cmd: Vec<String>, matching: Vec<String> },
}

/// Something that knows the state of some capabilities.
pub trait Provider: Send + Sync {
    /// A short name, e.g. `programs`.
    fn name(&self) -> &str;
    /// Every capability this provider knows about, as of now. Called off the main thread.
    fn probe(&self) -> Vec<Capability>;
    /// What can change the answer.
    fn sources(&self) -> Vec<Source> {
        Vec::new()
    }
}

/// What changed in one capability: `before` is `None` for a new one, `after` for one
/// that's gone.
#[derive(Debug, Clone, PartialEq)]
pub struct Change {
    pub id: String,
    pub before: Option<State>,
    pub after: Option<Capability>,
}

type Subscriber = Box<dyn Fn(&[Change]) + Send>;

struct Inner {
    providers: Vec<Box<dyn Provider>>,
    caps: Mutex<BTreeMap<String, Capability>>,
    subscribers: Mutex<Vec<Subscriber>>,
    poke: Mutex<Option<mpsc::Sender<usize>>>,
}

/// The latest state of every capability, and who to tell when it changes.
#[derive(Clone)]
pub struct Registry {
    inner: Arc<Inner>,
}

/// How long to wait after an event for more before probing again (a package install or a
/// device plugging in fires several at once).
const SETTLE: Duration = Duration::from_millis(250);

impl Registry {
    pub fn new(providers: Vec<Box<dyn Provider>>) -> Self {
        Registry {
            inner: Arc::new(Inner {
                providers,
                caps: Mutex::new(BTreeMap::new()),
                subscribers: Mutex::new(Vec::new()),
                poke: Mutex::new(None),
            }),
        }
    }

    /// Probe everything once (on this thread), then watch every provider's sources on
    /// background threads. Call once.
    pub fn start(&self) {
        for i in 0..self.inner.providers.len() {
            self.reprobe(i);
        }
        let (tx, rx) = mpsc::channel::<usize>();
        *self.inner.poke.lock().unwrap() = Some(tx.clone());
        let mut by_source: BTreeMap<String, (Source, Vec<usize>)> = BTreeMap::new();
        for (i, p) in self.inner.providers.iter().enumerate() {
            for s in p.sources() {
                by_source.entry(format!("{s:?}")).or_insert_with(|| (s, Vec::new())).1.push(i);
            }
        }
        sources::spawn(by_source.into_values().collect(), tx);
        let me = self.clone();
        std::thread::Builder::new()
            .name("nyx-capability".into())
            .spawn(move || {
                while let Ok(first) = rx.recv() {
                    let mut due = vec![first];
                    while let Ok(next) = rx.recv_timeout(SETTLE) {
                        due.push(next);
                    }
                    due.sort_unstable();
                    due.dedup();
                    for i in due {
                        me.reprobe(i);
                    }
                }
            })
            .ok();
    }

    /// Probe one provider again now (after the user did something the sources can't see,
    /// such as installing an app through Settings).
    pub fn refresh(&self, provider: &str) {
        if let Some(i) = self.inner.providers.iter().position(|p| p.name() == provider) {
            match &*self.inner.poke.lock().unwrap() {
                Some(tx) => {
                    let _ = tx.send(i);
                }
                None => self.reprobe(i),
            }
        }
    }

    /// Probe every provider again.
    pub fn refresh_all(&self) {
        for i in 0..self.inner.providers.len() {
            self.reprobe(i);
        }
    }

    fn reprobe(&self, i: usize) {
        let provider = &self.inner.providers[i];
        let fresh = provider.probe();
        let mut changes = Vec::new();
        {
            let mut caps = self.inner.caps.lock().unwrap();
            let name = provider.name();
            let mut seen = Vec::new();
            for cap in fresh {
                seen.push(cap.id.clone());
                match caps.get(&cap.id) {
                    Some(old) if old.same(&cap) => {}
                    old => {
                        changes.push(Change { id: cap.id.clone(), before: old.map(|c| c.state), after: Some(cap.clone()) });
                        caps.insert(cap.id.clone(), cap);
                    }
                }
            }
            let gone: Vec<String> =
                caps.values().filter(|c| c.provider == name && !seen.contains(&c.id)).map(|c| c.id.clone()).collect();
            for id in gone {
                let old = caps.remove(&id);
                changes.push(Change { id, before: old.map(|c| c.state), after: None });
            }
        }
        if !changes.is_empty() {
            for s in self.inner.subscribers.lock().unwrap().iter() {
                s(&changes);
            }
        }
    }

    pub fn snapshot(&self) -> BTreeMap<String, Capability> {
        self.inner.caps.lock().unwrap().clone()
    }

    pub fn get(&self, id: &str) -> Option<Capability> {
        self.inner.caps.lock().unwrap().get(id).cloned()
    }

    /// The state of a capability; one nobody has reported is `NotInstalled`.
    pub fn state(&self, id: &str) -> State {
        self.get(id).map(|c| c.state).unwrap_or(State::NotInstalled)
    }

    /// Called (on a background thread) with every batch of changes.
    pub fn subscribe(&self, f: impl Fn(&[Change]) + Send + 'static) {
        self.inner.subscribers.lock().unwrap().push(Box::new(f));
    }

    /// Called on the GTK main loop with every batch of changes.
    #[cfg(feature = "glib")]
    pub fn subscribe_local(&self, f: impl Fn(&[Change]) + 'static) {
        let (tx, rx) = async_channel::unbounded::<Vec<Change>>();
        self.subscribe(move |changes| {
            let _ = tx.send_blocking(changes.to_vec());
        });
        glib::spawn_future_local(async move {
            while let Ok(changes) = rx.recv().await {
                f(&changes);
            }
        });
    }
}

/// Call `f` (on a background thread) after any of `sources` fires, once per burst: for a
/// page that shows live state and should redraw when it changes rather than on a timer.
/// The watchers run for the rest of the process.
pub fn watch(sources: Vec<Source>, f: impl Fn() + Send + 'static) {
    let (tx, rx) = mpsc::channel::<usize>();
    sources::spawn(sources.into_iter().map(|s| (s, vec![0])).collect(), tx);
    std::thread::Builder::new()
        .name("nyx-capability-watch".into())
        .spawn(move || {
            while rx.recv().is_ok() {
                while rx.recv_timeout(SETTLE).is_ok() {}
                f();
            }
        })
        .ok();
}

mod unix_secs {
    use serde::{Deserialize, Deserializer, Serializer};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    pub fn serialize<S: Serializer>(t: &SystemTime, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u64(t.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<SystemTime, D::Error> {
        Ok(UNIX_EPOCH + Duration::from_secs(u64::deserialize(d)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU8, Ordering};

    struct Fake {
        state: Arc<AtomicU8>,
    }

    impl Provider for Fake {
        fn name(&self) -> &str {
            "fake"
        }
        fn probe(&self) -> Vec<Capability> {
            match self.state.load(Ordering::SeqCst) {
                0 => vec![],
                1 => vec![Capability::new("hw:fake", "fake", State::Available)],
                _ => vec![Capability::new("hw:fake", "fake", State::TemporarilyUnavailable).reason("unplugged")],
            }
        }
    }

    fn registry() -> (Registry, Arc<AtomicU8>, Arc<Mutex<Vec<Change>>>) {
        let state = Arc::new(AtomicU8::new(0));
        let r = Registry::new(vec![Box::new(Fake { state: state.clone() })]);
        let seen = Arc::new(Mutex::new(Vec::new()));
        let s = seen.clone();
        r.subscribe(move |c| s.lock().unwrap().extend_from_slice(c));
        (r, state, seen)
    }

    #[test]
    fn changes_are_reported_once_each() {
        let (r, state, seen) = registry();
        r.refresh_all();
        assert_eq!(r.state("hw:fake"), State::NotInstalled);
        assert!(seen.lock().unwrap().is_empty());
        state.store(1, Ordering::SeqCst);
        r.refresh_all();
        r.refresh_all();
        assert_eq!(seen.lock().unwrap().len(), 1, "an unchanged probe isn't a change");
        assert_eq!(r.state("hw:fake"), State::Available);
        state.store(2, Ordering::SeqCst);
        r.refresh_all();
        let c = seen.lock().unwrap().last().cloned().unwrap();
        assert_eq!(c.before, Some(State::Available));
        assert_eq!(c.after.unwrap().reason.as_deref(), Some("unplugged"));
        state.store(0, Ordering::SeqCst);
        r.refresh_all();
        let c = seen.lock().unwrap().last().cloned().unwrap();
        assert_eq!((c.before, c.after), (Some(State::TemporarilyUnavailable), None));
        assert!(r.snapshot().is_empty());
    }

    #[test]
    fn refresh_after_start_goes_through_the_watcher() {
        let (r, state, seen) = registry();
        r.start();
        state.store(1, Ordering::SeqCst);
        r.refresh("fake");
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while seen.lock().unwrap().is_empty() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(r.state("hw:fake"), State::Available);
    }

    #[test]
    fn watch_calls_once_per_burst() {
        let d = tempfile::tempdir().unwrap();
        let (tx, rx) = mpsc::channel();
        watch(vec![Source::Folder(d.path().to_path_buf())], move || {
            let _ = tx.send(());
        });
        std::thread::sleep(Duration::from_millis(100));
        for i in 0..5 {
            std::fs::write(d.path().join(i.to_string()), "").unwrap();
        }
        assert!(rx.recv_timeout(Duration::from_secs(2)).is_ok());
        assert!(rx.recv_timeout(Duration::from_millis(500)).is_err(), "one call for the burst");
    }

    #[test]
    fn states_say_whether_controls_show() {
        assert!(State::TemporarilyUnavailable.exists());
        assert!(State::PermissionRestricted.exists());
        assert!(!State::NotInstalled.exists());
        assert!(!State::Unsupported.exists());
    }
}
