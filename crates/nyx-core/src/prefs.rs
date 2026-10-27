//! An app's own preferences file (`~/.config/<app>/settings.toml`), kept in
//! memory and written back shortly after the last change.
//!
//! An app declares its `Prefs` struct and one line:
//!
//! ```ignore
//! nyx_core::prefs_store!(Prefs);
//! ```
//!
//! which gives the module `get`, `update`, `flush` and `take_broken`.

use crate::{app, cmd, paths};
use gtk::glib;
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

/// How long after the last change the file is written, so dragging a slider
/// doesn't rewrite it on every tick.
const DEBOUNCE: Duration = Duration::from_millis(450);

pub struct Store<T> {
    file: PathBuf,
    value: RefCell<T>,
    broken: Cell<bool>,
    pending: Cell<Option<glib::SourceId>>,
}

impl<T: Serialize + DeserializeOwned + Default + Clone + 'static> Store<T> {
    /// Read `file`, falling back to the defaults. A file that can't be parsed
    /// is copied to `settings.toml.bak` before the defaults are saved over it.
    pub fn load(file: PathBuf) -> Rc<Self> {
        let mut broken = false;
        let value = match std::fs::read_to_string(&file) {
            Err(_) => T::default(),
            Ok(text) => match toml::from_str(&text) {
                Ok(v) => v,
                Err(e) => {
                    let backup = file.with_extension("toml.bak");
                    let _ = std::fs::copy(&file, &backup);
                    eprintln!("{}: {} couldn't be read ({e}); kept a copy as {}", app::slug(), file.display(), backup.display());
                    broken = true;
                    T::default()
                }
            },
        };
        Rc::new(Self { file, value: RefCell::new(value), broken: Cell::new(broken), pending: Cell::new(None) })
    }

    /// The app's standard file, `~/.config/<app>/settings.toml`.
    pub fn standard() -> Rc<Self> {
        Self::load(paths::prefs_file())
    }

    /// The standard file, then `fix` to fill in anything that must not stay
    /// empty. If `fix` changed something, the file is written back.
    pub fn standard_with(fix: fn(&mut T) -> bool) -> Rc<Self> {
        let store = Self::standard();
        let changed = fix(&mut store.value.borrow_mut());
        if changed {
            store.save();
        }
        store
    }

    pub fn get(&self) -> T {
        self.value.borrow().clone()
    }

    /// Read one thing without cloning the whole struct.
    pub fn with<R>(&self, f: impl FnOnce(&T) -> R) -> R {
        f(&self.value.borrow())
    }

    /// Change the prefs now; the file follows after [`DEBOUNCE`].
    pub fn update(self: &Rc<Self>, change: impl FnOnce(&mut T)) {
        change(&mut self.value.borrow_mut());
        if let Some(id) = self.pending.take() {
            id.remove();
        }
        let me = Rc::downgrade(self);
        let id = glib::timeout_add_local_once(DEBOUNCE, move || {
            if let Some(me) = me.upgrade() {
                me.pending.set(None);
                me.save();
            }
        });
        self.pending.set(Some(id));
    }

    /// Replace the in-memory copy with what's on disk now.
    pub fn reload(&self) {
        if let Ok(v) = std::fs::read_to_string(&self.file).map_err(|_| ()).and_then(|t| toml::from_str(&t).map_err(|_| ())) {
            *self.value.borrow_mut() = v;
        }
    }

    /// Write any pending change now (on quit).
    pub fn flush(&self) {
        if let Some(id) = self.pending.take() {
            id.remove();
            self.save();
        }
    }

    /// True (once) when the file couldn't be read at start.
    pub fn take_broken(&self) -> bool {
        self.broken.replace(false)
    }

    fn save(&self) {
        if let Ok(text) = toml::to_string_pretty(&*self.value.borrow()) {
            let _ = cmd::atomic_write(&self.file, text);
        }
    }
}

/// Declare the module-level `get`, `update`, `flush`, `reload` and
/// `take_broken` for an app's `Prefs` type, backed by its standard file.
#[macro_export]
macro_rules! prefs_store {
    ($ty:ty) => {
        $crate::prefs_store!($ty, $crate::prefs::Store::<$ty>::standard());
    };
    ($ty:ty, $init:expr) => {
        thread_local! {
            static STORE: ::std::rc::Rc<$crate::prefs::Store<$ty>> = $init;
        }

        pub fn get() -> $ty {
            STORE.with(|s| s.get())
        }

        pub fn update(change: impl FnOnce(&mut $ty)) {
            STORE.with(|s| s.update(change))
        }

        /// Write any pending change now (on quit).
        #[allow(dead_code)]
        pub fn flush() {
            STORE.with(|s| s.flush())
        }

        #[allow(dead_code)]
        pub fn reload() {
            STORE.with(|s| s.reload())
        }

        /// True (once) when the settings file couldn't be read at start.
        #[allow(dead_code)]
        pub fn take_broken() -> bool {
            STORE.with(|s| s.take_broken())
        }
    };
}
