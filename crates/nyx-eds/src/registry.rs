//! The EDS source registry: which calendars and task lists exist.

use crate::{Error, Owned, borrowed, cstring, owned, take_error};
use edata_server_sys as eds;
use glib::prelude::*;
use glib::translate::FromGlibPtrFull;
use std::marker::PhantomData;

/// EDS's own local sources, which have no account.
const LOCAL: &[&str] = &["system-calendar", "system-task-list", "birthdays"];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Calendar,
    TaskList,
}

impl Kind {
    pub(crate) fn extension(self) -> &'static [u8] {
        match self {
            Self::Calendar => eds::E_SOURCE_EXTENSION_CALENDAR,
            Self::TaskList => eds::E_SOURCE_EXTENSION_TASK_LIST,
        }
    }
}

/// One enabled calendar or task list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
    pub uid: String,
    pub name: String,
    pub kind: Kind,
    /// `#rrggbb` as EDS stores it, if the source has a color.
    pub color: Option<String>,
    /// Shown in views (EDS's "selected").
    pub selected: bool,
    /// The EDS backend: `caldav`, `google`, `local`, `contacts`, `gtasks`, ...
    pub backend: String,
    /// The GNOME Online Accounts id of the account it belongs to, if any.
    pub account: Option<String>,
    /// The account's or collection's name, for grouping. `None` for local sources.
    pub group: Option<String>,
    /// One of EDS's own local sources ("On this computer", birthdays).
    pub local: bool,
    /// Its settings can be changed (color, visibility).
    pub writable: bool,
}

impl Source {
    pub fn label(&self) -> &str {
        if self.name.is_empty() { &self.uid } else { &self.name }
    }
}

/// The registry of EDS sources. Not `Send`: see the crate docs.
pub struct Registry {
    obj: glib::Object,
    _thread: PhantomData<*const ()>,
}

/// Keeps a [`Registry::watch`] callback connected until dropped.
pub struct Watch {
    obj: glib::Object,
    handlers: Vec<glib::SignalHandlerId>,
}

impl Drop for Watch {
    fn drop(&mut self) {
        for id in self.handlers.drain(..) {
            self.obj.disconnect(id);
        }
    }
}

impl Registry {
    /// Connects to `evolution-source-registry` on the session bus, starting it if
    /// it is installed. Fails with [`Error::Unavailable`] when EDS is missing.
    pub fn connect() -> Result<Self, Error> {
        let mut err = std::ptr::null_mut();
        let ptr = unsafe { eds::e_source_registry_new_sync(std::ptr::null_mut(), &mut err) };
        if ptr.is_null() {
            return Err(if err.is_null() {
                Error::Unavailable("Calendar data isn't available.".into())
            } else {
                unsafe { take_error(err) }
            });
        }
        let obj = unsafe { glib::Object::from_glib_full(ptr.cast::<gobject_sys::GObject>()) };
        Ok(Self { obj, _thread: PhantomData })
    }

    pub(crate) fn raw(&self) -> *mut eds::ESourceRegistry {
        self.obj.as_ptr().cast()
    }

    /// The enabled sources of a kind (an account's service switched off in
    /// Settings disables its sources), sorted by group then name.
    pub fn sources(&self, kind: Kind) -> Vec<Source> {
        let ext = kind.extension();
        let list = unsafe { eds::e_source_registry_list_sources(self.raw(), ext.as_ptr().cast()) };
        let mut out = Vec::new();
        let mut node = list;
        while !node.is_null() {
            let source = unsafe { (*node).data.cast::<eds::ESource>() };
            if unsafe { eds::e_source_registry_check_enabled(self.raw(), source) } != 0 {
                out.push(unsafe { self.describe(source, kind) });
            }
            unsafe {
                gobject_sys::g_object_unref(source.cast());
                node = (*node).next;
            }
        }
        unsafe { glib_sys::g_list_free(list) };
        out.sort_by(|a, b| (a.local, &a.group, &a.name).cmp(&(b.local, &b.group, &b.name)));
        out
    }

    /// Every source of a kind with whether it's switched on itself, for turning
    /// sources on and off. Sources off only because their account's service is
    /// off in Settings are left out (switching them on here wouldn't show them).
    pub fn all_sources(&self, kind: Kind) -> Vec<(Source, bool)> {
        let ext = kind.extension();
        let list = unsafe { eds::e_source_registry_list_sources(self.raw(), ext.as_ptr().cast()) };
        let mut out = Vec::new();
        let mut node = list;
        while !node.is_null() {
            let source = unsafe { (*node).data.cast::<eds::ESource>() };
            let own = unsafe { eds::e_source_get_enabled(source) } != 0;
            let shown = unsafe { eds::e_source_registry_check_enabled(self.raw(), source) } != 0;
            if shown || !own {
                out.push((unsafe { self.describe(source, kind) }, own));
            }
            unsafe {
                gobject_sys::g_object_unref(source.cast());
                node = (*node).next;
            }
        }
        unsafe { glib_sys::g_list_free(list) };
        out.sort_by(|(a, _), (b, _)| (a.local, &a.group, &a.name).cmp(&(b.local, &b.group, &b.name)));
        out
    }

    /// Switches a source on or off for every app (off: not loaded or synced).
    pub fn set_enabled(&self, uid: &str, enabled: bool) -> Result<(), Error> {
        let source = self.ref_source(uid).ok_or_else(|| Error::NotFound("That calendar no longer exists.".into()))?;
        unsafe { eds::e_source_set_enabled(source.ptr(), enabled.into()) };
        let mut err = std::ptr::null_mut();
        if unsafe { eds::e_source_write_sync(source.ptr(), std::ptr::null_mut(), &mut err) } == 0 {
            return Err(unsafe { take_error(err) });
        }
        Ok(())
    }

    /// One source by uid, if it exists and has that kind.
    pub fn source(&self, uid: &str, kind: Kind) -> Option<Source> {
        let source = self.ref_source(uid)?;
        let has = unsafe { eds::e_source_has_extension(source.ptr(), kind.extension().as_ptr().cast()) } != 0;
        has.then(|| unsafe { self.describe(source.ptr(), kind) })
    }

    pub(crate) fn ref_source(&self, uid: &str) -> Option<Owned<eds::ESource>> {
        let uid = cstring(uid);
        unsafe { Owned::new(eds::e_source_registry_ref_source(self.raw(), uid.as_ptr())) }
    }

    /// Shows or hides a source in views. Saved by EDS for every app.
    pub fn set_selected(&self, uid: &str, kind: Kind, selected: bool) -> Result<(), Error> {
        self.edit(uid, kind, |ext| unsafe {
            eds::e_source_selectable_set_selected(ext, selected.into());
        })
    }

    /// Sets a source's color (`#rrggbb`). Saved by EDS (and synced where the
    /// provider stores colors).
    pub fn set_color(&self, uid: &str, kind: Kind, color: &str) -> Result<(), Error> {
        let color = cstring(color);
        self.edit(uid, kind, |ext| unsafe {
            eds::e_source_selectable_set_color(ext, color.as_ptr());
        })
    }

    fn edit(&self, uid: &str, kind: Kind, change: impl FnOnce(*mut eds::ESourceSelectable)) -> Result<(), Error> {
        let source = self.ref_source(uid).ok_or_else(|| Error::NotFound("That calendar no longer exists.".into()))?;
        let ext = unsafe { eds::e_source_get_extension(source.ptr(), kind.extension().as_ptr().cast()) };
        change(ext.cast());
        let mut err = std::ptr::null_mut();
        if unsafe { eds::e_source_write_sync(source.ptr(), std::ptr::null_mut(), &mut err) } == 0 {
            return Err(unsafe { take_error(err) });
        }
        Ok(())
    }

    /// Calls `on_change` whenever a source is added, removed, enabled, disabled or
    /// changed (renamed, recolored, shown, hidden). Callers re-read [`Self::sources`].
    pub fn watch(&self, on_change: impl Fn() + 'static) -> Watch {
        let on_change = std::rc::Rc::new(on_change);
        let handlers = ["source-added", "source-removed", "source-enabled", "source-disabled", "source-changed"]
            .into_iter()
            .map(|signal| {
                let on_change = on_change.clone();
                self.obj.connect_local(signal, false, move |_| {
                    on_change();
                    None
                })
            })
            .collect();
        Watch { obj: self.obj.clone(), handlers }
    }

    /// # Safety
    /// `source` must be a valid `ESource` with the extension for `kind`.
    unsafe fn describe(&self, source: *mut eds::ESource, kind: Kind) -> Source {
        unsafe {
            let uid = borrowed(eds::e_source_get_uid(source)).unwrap_or_default();
            let name = owned(eds::e_source_dup_display_name(source)).unwrap_or_default();
            let ext = eds::e_source_get_extension(source, kind.extension().as_ptr().cast());
            let color = borrowed(eds::e_source_selectable_get_color(ext.cast())).filter(|c| !c.is_empty());
            let selected = eds::e_source_selectable_get_selected(ext.cast()) != 0;
            let backend = borrowed(eds::e_source_backend_get_backend_name(ext.cast())).unwrap_or_default();
            let writable = eds::e_source_get_writable(source) != 0;
            let local = LOCAL.contains(&uid.as_str());
            let (account, group) = match borrowed(eds::e_source_get_parent(source)) {
                Some(parent) if !local => self.account_of(&parent),
                _ => (None, None),
            };
            Source { uid, name, kind, color, selected, backend, account, group, local, writable }
        }
    }

    /// The GOA account id and display name of a source's collection parent.
    fn account_of(&self, parent: &str) -> (Option<String>, Option<String>) {
        let Some(parent) = self.ref_source(parent) else { return (None, None) };
        unsafe {
            let name = owned(eds::e_source_dup_display_name(parent.ptr())).filter(|n| !n.is_empty());
            let goa = eds::E_SOURCE_EXTENSION_GOA.as_ptr().cast();
            let account = if eds::e_source_has_extension(parent.ptr(), goa) != 0 {
                let ext = eds::e_source_get_extension(parent.ptr(), goa);
                borrowed(eds::e_source_goa_get_account_id(ext.cast())).filter(|a| !a.is_empty())
            } else {
                None
            };
            (account, name)
        }
    }
}
