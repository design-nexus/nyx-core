//! Alerts that are due, from EDS's reminder watcher (`EReminderWatcher`).
//!
//! The watcher follows every enabled calendar and task list, fires when an
//! alert is due, and remembers dismissed and snoozed alerts in EDS's own
//! settings, so other EDS reminder tools agree on what is still pending.

use crate::ical::{self, Item};
use crate::{Error, Registry, borrowed, take_error};
use ecal_sys as ecal;
use glib::prelude::*;
use glib::translate::FromGlibPtrFull;
use std::marker::PhantomData;

/// One alert that is due.
pub struct Reminder {
    /// The calendar or task list it came from.
    pub source: String,
    pub item: Item,
    /// When the occurrence starts and ends (Unix seconds).
    pub starts: i64,
    pub ends: i64,
    /// When the alert was due.
    pub due: i64,
    raw: *mut ecal::EReminderData,
}

impl Drop for Reminder {
    fn drop(&mut self) {
        unsafe { ecal::e_reminder_data_free(self.raw.cast()) };
    }
}

impl std::fmt::Debug for Reminder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Reminder").field("source", &self.source).field("summary", &self.item.summary).field("due", &self.due).finish()
    }
}

/// The reminder watcher. Not `Send`: see the crate docs.
pub struct Reminders {
    obj: glib::Object,
    handlers: std::cell::RefCell<Vec<glib::SignalHandlerId>>,
    _thread: PhantomData<*const ()>,
}

impl Drop for Reminders {
    fn drop(&mut self) {
        // EDS may drop the watcher's last reference on another thread later; the
        // callbacks must be gone by then.
        for id in self.handlers.borrow_mut().drain(..) {
            self.obj.disconnect(id);
        }
        unsafe { ecal::e_reminder_watcher_set_timers_enabled(self.raw(), 0) };
    }
}

impl Reminders {
    pub fn new(registry: &Registry) -> Self {
        let ptr = unsafe { ecal::e_reminder_watcher_new(registry.raw()) };
        let obj = unsafe { glib::Object::from_glib_full(ptr.cast::<gobject_sys::GObject>()) };
        let zone = ical::zone(&ical::local_zone());
        unsafe { ecal::e_reminder_watcher_set_default_zone(obj.as_ptr().cast(), zone) };
        Self { obj, handlers: Default::default(), _thread: PhantomData }
    }

    fn raw(&self) -> *mut ecal::EReminderWatcher {
        self.obj.as_ptr().cast()
    }

    /// Calls `on_due` with the alerts that are due (`snoozed` is true when they
    /// come back after a snooze). Overdue alerts arrive once at start.
    /// The callback stays connected until the watcher is dropped.
    pub fn connect_due(&self, on_due: impl Fn(Vec<Reminder>, bool) + 'static) {
        let id = self.obj.connect_local("triggered", false, move |args| {
            let list = args.get(1).and_then(|v| v.get::<glib::Pointer>().ok()).unwrap_or(std::ptr::null_mut());
            let snoozed = args.get(2).and_then(|v| v.get::<bool>().ok()).unwrap_or(false);
            let mut out = Vec::new();
            let mut node = list.cast::<glib_sys::GSList>();
            while !node.is_null() {
                let rd = unsafe { (*node).data.cast::<ecal::EReminderData>() };
                if let Some(reminder) = unsafe { reminder(rd) } {
                    out.push(reminder);
                }
                node = unsafe { (*node).next };
            }
            if !out.is_empty() {
                on_due(out, snoozed);
            }
            None
        });
        self.handlers.borrow_mut().push(id);
    }

    /// Marks an alert done for everyone using EDS reminders.
    pub fn dismiss(&self, reminder: &Reminder) -> Result<(), Error> {
        let mut err = std::ptr::null_mut();
        if unsafe { ecal::e_reminder_watcher_dismiss_sync(self.raw(), reminder.raw, std::ptr::null_mut(), &mut err) } == 0 {
            return Err(unsafe { take_error(err) });
        }
        Ok(())
    }

    /// Brings an alert back at `until` (Unix seconds).
    pub fn snooze(&self, reminder: &Reminder, until: i64) {
        unsafe { ecal::e_reminder_watcher_snooze(self.raw(), reminder.raw, until) };
    }
}

/// Copies one `EReminderData` out of the signal's list.
unsafe fn reminder(rd: *const ecal::EReminderData) -> Option<Reminder> {
    unsafe {
        let instance = ecal::e_reminder_data_get_instance(rd);
        let comp = ecal::e_reminder_data_get_component(rd);
        if instance.is_null() || comp.is_null() {
            return None;
        }
        let icomp = ecal::e_cal_component_get_icalcomponent(comp);
        if icomp.is_null() {
            return None;
        }
        let utc = ical::zone("UTC");
        let item = ical::item(icomp, None, &ical::Zones::fixed(utc));
        let raw = ecal::e_reminder_data_copy(rd);
        Some(Reminder {
            source: borrowed(ecal::e_reminder_data_get_source_uid(rd)).unwrap_or_default(),
            item,
            starts: ecal::e_cal_component_alarm_instance_get_occur_start(instance) as i64,
            ends: ecal::e_cal_component_alarm_instance_get_occur_end(instance) as i64,
            due: ecal::e_cal_component_alarm_instance_get_time(instance) as i64,
            raw,
        })
    }
}
