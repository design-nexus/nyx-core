mod reminders;
pub use reminders::{Reminder, Reminders};
// Native local calendar/task storage with libical recurrence and raw payload preservation.
mod client;
mod ical;
mod registry;
pub use client::{Busy, Calendar, Scope, View, range_query};
pub use ical::{Attendee, Draft, Item, ItemKind, Response, Status, When, local_zone};
pub use registry::{Kind, Registry, Source, Watch};

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
mod api;
mod api_write;
pub mod contacts;
mod sync;
mod zones;

mod issues;
pub use issues::{Resolution, SyncIssue};

mod contact_sync;

mod history;
pub use history::{HistoryDetail, HistorySummary, HistoryVersion};

mod detach;
