//! Evolution Data Server calendars and task lists for Nixe apps.
//!
//! EDS owns the data: sync with CalDAV, Google and other providers, the offline
//! cache, the offline change queue and conflicts. This crate is a narrow, safe
//! layer over libedataserver, libecal and libical-glib (the generated
//! `nixe-*-sys` crates) that covers what Calendar and Reminders use:
//!
//! - [`Registry`]: the calendars and task lists, their account, color and visibility.
//! - [`Reminders`]: alerts as they come due, with dismiss and snooze.
//! - [`Calendar`]: one calendar or task list. It expands recurrences into instances
//!   (with time zones, through libical), reads, creates, changes and removes items,
//!   and gives a live [`View`] of changes.
//!
//! The calls block. They also deliver signals on the thread-default main context
//! of the thread that made the object. So a GTK app makes these objects on one
//! worker thread and keeps them there; none of the types is `Send`.

mod client;
mod ical;
mod registry;
mod reminders;

pub use client::{Busy, Calendar, Scope, View, range_query};
pub use ical::{Attendee, Draft, Item, ItemKind, Response, Status, When, local_zone};
pub use registry::{Kind, Registry, Source, Watch};
pub use reminders::{Reminder, Reminders};

use glib::translate::FromGlibPtrFull;

/// What went wrong, as a short sentence for the UI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// EDS is not installed, not running or not answering.
    Unavailable(String),
    /// No calendar, task list or item with that id.
    NotFound(String),
    /// EDS refused or failed the operation.
    Failed(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable(m) | Self::NotFound(m) | Self::Failed(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for Error {}

/// Takes ownership of a `GError` set by an EDS call.
///
/// # Safety
/// `err` must be a non-null `GError` the caller owns.
pub(crate) unsafe fn take_error(err: *mut glib_sys::GError) -> Error {
    let err = unsafe { glib::Error::from_glib_full(err) };
    let message = err.message().to_string();
    let lower = message.to_lowercase();
    // Only bus-level failures mean EDS itself is unreachable; EDS sends its own
    // errors over D-Bus too.
    const GONE: &[&str] = &["serviceunknown", "namehasnoowner", "noreply", "timeout", "timed out", "disconnected", "spawn"];
    if GONE.iter().any(|g| lower.contains(g)) {
        return Error::Unavailable("Calendar data isn't available.".into());
    }
    // "GDBus.Error:org.gnome.evolution....: text" -> "text"
    let text = match message.strip_prefix("GDBus.Error:").and_then(|rest| rest.split_once(": ")) {
        Some((_, text)) => text.to_string(),
        None => message,
    };
    if lower.contains("not found") || lower.contains("no such") {
        Error::NotFound(text)
    } else {
        Error::Failed(text)
    }
}

/// A Rust string from a C string EDS keeps ownership of.
///
/// # Safety
/// `ptr` must be null or a valid NUL-terminated string.
pub(crate) unsafe fn borrowed(ptr: *const std::ffi::c_char) -> Option<String> {
    if ptr.is_null() {
        return None;
    }
    let text = unsafe { std::ffi::CStr::from_ptr(ptr) }.to_string_lossy().into_owned();
    Some(text)
}

/// A Rust string from a C string the caller must free.
///
/// # Safety
/// `ptr` must be null or a NUL-terminated string allocated with `g_malloc`.
pub(crate) unsafe fn owned(ptr: *mut std::ffi::c_char) -> Option<String> {
    let text = unsafe { borrowed(ptr) };
    if !ptr.is_null() {
        unsafe { glib_sys::g_free(ptr.cast()) };
    }
    text
}

/// A C string for an argument. Interior NULs are dropped rather than failing.
pub(crate) fn cstring(text: &str) -> std::ffi::CString {
    std::ffi::CString::new(text.replace('\0', "")).unwrap_or_default()
}

/// Owns a GObject reference returned with transfer full, as a plain pointer.
pub(crate) struct Owned<T>(*mut T);

impl<T> Owned<T> {
    /// # Safety
    /// `ptr` must be null or a GObject the caller owns a reference to.
    pub(crate) unsafe fn new(ptr: *mut T) -> Option<Self> {
        if ptr.is_null() { None } else { Some(Self(ptr)) }
    }

    pub(crate) fn ptr(&self) -> *mut T {
        self.0
    }
}

impl<T> Drop for Owned<T> {
    fn drop(&mut self) {
        unsafe { gobject_sys::g_object_unref(self.0.cast()) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cstring_drops_interior_nul() {
        assert_eq!(cstring("a\0b").as_bytes(), b"ab");
    }

    #[test]
    fn error_display_is_the_message() {
        assert_eq!(Error::Failed("Nope.".into()).to_string(), "Nope.");
    }
}
