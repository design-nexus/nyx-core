//! One calendar or task list: read, expand, change and watch its items.

use crate::ical::{self, Draft, Item, ItemKind};
use crate::{Error, Kind, Owned, Registry, cstring, owned, take_error};
use ecal_sys as ecal;
use glib::prelude::*;
use glib::translate::FromGlibPtrFull;
use ical_glib_sys as icalsys;
use std::marker::PhantomData;

/// Which occurrences of a repeating item a change applies to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    This,
    ThisAndFollowing,
    All,
}

impl Scope {
    fn raw(self) -> ecal::ECalObjModType {
        match self {
            Self::This => ecal::E_CAL_OBJ_MOD_THIS,
            Self::ThisAndFollowing => ecal::E_CAL_OBJ_MOD_THIS_AND_FUTURE,
            Self::All => ecal::E_CAL_OBJ_MOD_ALL,
        }
    }
}

/// A person's busy times: their address and (start, end) Unix seconds.
pub type Busy = (String, Vec<(i64, i64)>);

/// An open calendar or task list. Not `Send`: see the crate docs.
pub struct Calendar {
    obj: glib::Object,
    kind: Kind,
    /// EDS's local file backend ("On this computer").
    local_file: bool,
    zone: *mut icalsys::ICalTimezone,
    _thread: PhantomData<*const ()>,
}

/// Keeps a [`Calendar::watch`] callback connected until dropped.
pub struct View {
    obj: glib::Object,
    handlers: Vec<glib::SignalHandlerId>,
}

impl Drop for View {
    fn drop(&mut self) {
        for id in self.handlers.drain(..) {
            self.obj.disconnect(id);
        }
        unsafe { ecal::e_cal_client_view_stop(self.obj.as_ptr().cast(), std::ptr::null_mut()) };
    }
}

impl Calendar {
    /// Opens a source. `wait` is how many seconds to wait for the backend to come
    /// online; `None` (or 0) opens at once, from the offline cache if need be.
    /// (libecal itself treats 0 as "wait forever".)
    pub fn open(registry: &Registry, uid: &str, kind: Kind, wait: Option<u32>) -> Result<Self, Error> {
        let wait = wait.filter(|w| *w > 0).unwrap_or(u32::MAX);
        let local_file = registry.source(uid, kind).is_some_and(|s| s.backend == "local");
        let source = registry.ref_source(uid).ok_or_else(|| Error::NotFound("That calendar no longer exists.".into()))?;
        let source_type = match kind {
            Kind::Calendar => ecal::E_CAL_CLIENT_SOURCE_TYPE_EVENTS,
            Kind::TaskList => ecal::E_CAL_CLIENT_SOURCE_TYPE_TASKS,
        };
        let mut err = std::ptr::null_mut();
        let ptr = unsafe {
            ecal::e_cal_client_connect_sync(source.ptr().cast(), source_type, wait, std::ptr::null_mut(), &mut err)
        };
        if ptr.is_null() {
            return Err(unsafe { take_error(err) });
        }
        let obj = unsafe { glib::Object::from_glib_full(ptr.cast::<gobject_sys::GObject>()) };
        let zone = ical::zone(&ical::local_zone());
        unsafe { ecal::e_cal_client_set_default_timezone(obj.as_ptr().cast(), zone) };
        Ok(Self { obj, kind, local_file, zone, _thread: PhantomData })
    }

    fn raw(&self) -> *mut ecal::ECalClient {
        self.obj.as_ptr().cast()
    }

    /// A zone this calendar knows by TZID (from VTIMEZONEs it has stored).
    fn lookup(&self, tzid: &str) -> *mut icalsys::ICalTimezone {
        let c = cstring(tzid);
        let mut zone = std::ptr::null_mut();
        let found = unsafe {
            ecal::e_cal_client_get_timezone_sync(self.raw(), c.as_ptr(), &mut zone, std::ptr::null_mut(), std::ptr::null_mut())
        };
        if found == 0 { std::ptr::null_mut() } else { zone }
    }

    fn read(&self, comp: *mut icalsys::ICalComponent) -> Item {
        let lookup = |tzid: &str| self.lookup(tzid);
        let zones = ical::Zones { fallback: self.zone, lookup: Some(&lookup) };
        unsafe { ical::item(comp, None, &zones) }
    }

    pub fn kind(&self) -> Kind {
        self.kind
    }

    /// The calendar can't be changed (a subscription, a shared read-only calendar).
    pub fn read_only(&self) -> bool {
        unsafe { edata_server_sys::e_client_is_readonly(self.raw().cast()) != 0 }
    }

    /// The backend can reach its server. Offline calendars still read and write
    /// their cache; EDS sends queued changes when they come back online.
    pub fn online(&self) -> bool {
        unsafe { edata_server_sys::e_client_is_online(self.raw().cast()) != 0 }
    }

    /// Asks the backend to sync with its server now. A no-op for backends without
    /// refresh (local calendars). Changes arrive through [`Self::watch`].
    pub fn refresh(&self) -> Result<(), Error> {
        if unsafe { edata_server_sys::e_client_check_refresh_supported(self.raw().cast()) } == 0 {
            return Ok(());
        }
        let mut err = std::ptr::null_mut();
        if unsafe { edata_server_sys::e_client_refresh_sync(self.raw().cast(), std::ptr::null_mut(), &mut err) } == 0 {
            return Err(unsafe { take_error(err) });
        }
        Ok(())
    }

    /// The backend supports a capability (`edata_server_sys` / libecal names such
    /// as `"no-task-assignment"`). Used to hide controls a list can't store.
    pub fn can(&self, capability: &str) -> bool {
        let c = cstring(capability);
        unsafe { edata_server_sys::e_client_check_capability(self.raw().cast(), c.as_ptr()) != 0 }
    }

    /// Every occurrence that overlaps `[from, to)` (Unix seconds), with repeating
    /// events expanded by libical in their own time zones.
    pub fn instances(&self, from: i64, to: i64) -> Vec<Item> {
        let mut data = Collect { out: Vec::new(), zone: self.zone };
        if self.local_file {
            // EDS 3.62's local file backend leaves a changed occurrence out of its
            // time index (e-cal-backend-file.c, E_CAL_OBJ_MOD_THIS), so a range
            // query misses it until EDS restarts. Expand each matching item from
            // all of its components instead.
            let uids: std::collections::BTreeSet<String> =
                self.items(&range_query(from, to)).unwrap_or_default().into_iter().map(|i| i.uid).collect();
            for uid in uids {
                let Ok(master) = self.fetch(&uid, None) else { continue };
                unsafe {
                    ecal::e_cal_client_generate_instances_for_object_sync(
                        self.raw(),
                        master.ptr(),
                        from as libc::time_t,
                        to as libc::time_t,
                        std::ptr::null_mut(),
                        Some(collect),
                        (&mut data as *mut Collect).cast(),
                    );
                }
            }
        } else {
            unsafe {
                ecal::e_cal_client_generate_instances_sync(
                    self.raw(),
                    from as libc::time_t,
                    to as libc::time_t,
                    std::ptr::null_mut(),
                    Some(collect),
                    (&mut data as *mut Collect).cast(),
                );
            }
        }
        data.out.sort_by_key(|i| i.start);
        data.out
    }

    /// Items matching an EDS query (`"#t"` for all), not expanded. Used for task lists.
    pub fn items(&self, query: &str) -> Result<Vec<Item>, Error> {
        let query = cstring(query);
        let mut list = std::ptr::null_mut();
        let mut err = std::ptr::null_mut();
        let ok = unsafe {
            ecal::e_cal_client_get_object_list_as_comps_sync(
                self.raw(),
                query.as_ptr(),
                &mut list,
                std::ptr::null_mut(),
                &mut err,
            )
        };
        if ok == 0 {
            return Err(unsafe { take_error(err) });
        }
        let mut out = Vec::new();
        let mut node = list;
        while !node.is_null() {
            unsafe {
                let comp = ecal::e_cal_component_get_icalcomponent((*node).data.cast());
                if !comp.is_null() {
                    out.push(self.read(comp));
                }
                node = (*node).next;
            }
        }
        unsafe { edata_server_sys::e_client_util_free_object_slist(list) };
        Ok(out)
    }

    /// One item (or one occurrence, with `rid`).
    pub fn get(&self, uid: &str, rid: Option<&str>) -> Result<Item, Error> {
        let comp = self.fetch(uid, rid)?;
        Ok(self.read(comp.ptr()))
    }

    /// One item as iCalendar text, for export.
    pub fn ics(&self, uid: &str, rid: Option<&str>) -> Result<String, Error> {
        let comp = self.fetch(uid, rid)?;
        Ok(unsafe { owned(icalsys::i_cal_component_as_ical_string(comp.ptr())) }.unwrap_or_default())
    }

    fn fetch(&self, uid: &str, rid: Option<&str>) -> Result<Owned<icalsys::ICalComponent>, Error> {
        let uid = cstring(uid);
        let rid = rid.map(cstring);
        let mut comp = std::ptr::null_mut();
        let mut err = std::ptr::null_mut();
        let ok = unsafe {
            ecal::e_cal_client_get_object_sync(
                self.raw(),
                uid.as_ptr(),
                rid.as_ref().map_or(std::ptr::null(), |r| r.as_ptr()),
                &mut comp,
                std::ptr::null_mut(),
                &mut err,
            )
        };
        if ok == 0 {
            return Err(unsafe { take_error(err) });
        }
        unsafe { Owned::new(comp) }.ok_or_else(|| Error::NotFound("That item no longer exists.".into()))
    }

    /// Creates an item from a draft and returns its uid.
    pub fn create(&self, draft: &Draft) -> Result<String, Error> {
        let kind = match self.kind {
            Kind::Calendar => ItemKind::Event,
            Kind::TaskList => ItemKind::Task,
        };
        let comp = ical::new_component(kind);
        let zone = unsafe { ical::apply(comp.ptr(), draft) };
        self.add_zone(zone)?;
        let mut uid = std::ptr::null_mut();
        let mut err = std::ptr::null_mut();
        let ok = unsafe {
            ecal::e_cal_client_create_object_sync(
                self.raw(),
                comp.ptr(),
                ecal::E_CAL_OPERATION_FLAG_NONE,
                &mut uid,
                std::ptr::null_mut(),
                &mut err,
            )
        };
        if ok == 0 {
            return Err(unsafe { take_error(err) });
        }
        Ok(unsafe { owned(uid) }.unwrap_or_default())
    }

    /// Applies a draft to an item. For a repeating item, `rid` and `scope` pick
    /// the occurrences.
    pub fn modify(&self, uid: &str, rid: Option<&str>, scope: Scope, draft: &Draft) -> Result<(), Error> {
        let comp = self.fetch(uid, rid)?;
        let scope = if rid.is_some() { scope } else { Scope::All };
        if let (Some(rid), Scope::This | Scope::ThisAndFollowing) = (rid, scope) {
            unsafe { ical::detach(comp.ptr(), rid, scope == Scope::ThisAndFollowing) };
        }
        let zone = unsafe { ical::apply(comp.ptr(), draft) };
        self.add_zone(zone)?;
        let mut err = std::ptr::null_mut();
        let ok = unsafe {
            ecal::e_cal_client_modify_object_sync(
                self.raw(),
                comp.ptr(),
                scope.raw(),
                ecal::E_CAL_OPERATION_FLAG_NONE,
                std::ptr::null_mut(),
                &mut err,
            )
        };
        if ok == 0 {
            return Err(unsafe { take_error(err) });
        }
        Ok(())
    }

    /// Marks a task done. A repeating task moves on to its next occurrence
    /// instead (and stays open), the way EDS's own task tools do it.
    pub fn complete(&self, uid: &str) -> Result<(), Error> {
        let comp = self.fetch(uid, None)?;
        let mut err = std::ptr::null_mut();
        if unsafe { ecal::e_cal_util_mark_task_complete_sync(comp.ptr(), -1, self.raw(), std::ptr::null_mut(), &mut err) } == 0 {
            return Err(unsafe { take_error(err) });
        }
        let mut err = std::ptr::null_mut();
        let ok = unsafe {
            ecal::e_cal_client_modify_object_sync(
                self.raw(),
                comp.ptr(),
                ecal::E_CAL_OBJ_MOD_ALL,
                ecal::E_CAL_OPERATION_FLAG_NONE,
                std::ptr::null_mut(),
                &mut err,
            )
        };
        if ok == 0 {
            return Err(unsafe { take_error(err) });
        }
        Ok(())
    }

    /// Answers an invitation for whichever of `emails` is on the attendee list
    /// (the user's addresses). The server tells the organizer. Returns
    /// `Ok(false)` when none of the addresses is invited.
    pub fn respond(&self, uid: &str, rid: Option<&str>, emails: &[String], response: crate::Response) -> Result<bool, Error> {
        let comp = self.fetch(uid, rid)?;
        let scope = if let Some(rid) = rid {
            unsafe { ical::detach(comp.ptr(), rid, false) };
            Scope::This
        } else {
            Scope::All
        };
        if !unsafe { ical::respond(comp.ptr(), emails, response) } {
            return Ok(false);
        }
        let mut err = std::ptr::null_mut();
        let ok = unsafe {
            ecal::e_cal_client_modify_object_sync(
                self.raw(),
                comp.ptr(),
                scope.raw(),
                ecal::E_CAL_OPERATION_FLAG_NONE,
                std::ptr::null_mut(),
                &mut err,
            )
        };
        if ok == 0 {
            return Err(unsafe { take_error(err) });
        }
        Ok(true)
    }

    /// Removes an item, or some occurrences of a repeating one.
    pub fn remove(&self, uid: &str, rid: Option<&str>, scope: Scope) -> Result<(), Error> {
        let uid = cstring(uid);
        let rid = rid.map(cstring);
        let scope = if rid.is_some() { scope } else { Scope::All };
        let mut err = std::ptr::null_mut();
        let ok = unsafe {
            ecal::e_cal_client_remove_object_sync(
                self.raw(),
                uid.as_ptr(),
                rid.as_ref().map_or(std::ptr::null(), |r| r.as_ptr()),
                scope.raw(),
                ecal::E_CAL_OPERATION_FLAG_NONE,
                std::ptr::null_mut(),
                &mut err,
            )
        };
        if ok == 0 {
            return Err(unsafe { take_error(err) });
        }
        Ok(())
    }

    /// Imports one VEVENT or VTODO (or the first one inside a VCALENDAR) as a new
    /// item, keeping its uid. Returns the uid.
    pub fn import(&self, ics: &str) -> Result<String, Error> {
        let text = cstring(ics);
        let parsed = unsafe { Owned::new(icalsys::i_cal_component_new_from_string(text.as_ptr())) }
            .ok_or_else(|| Error::Failed("That isn't an iCalendar file.".into()))?;
        let wanted = match self.kind {
            Kind::Calendar => icalsys::I_CAL_VEVENT_COMPONENT,
            Kind::TaskList => icalsys::I_CAL_VTODO_COMPONENT,
        };
        let comp = if unsafe { icalsys::i_cal_component_isa(parsed.ptr()) } == wanted {
            parsed
        } else {
            unsafe { Owned::new(icalsys::i_cal_component_get_first_component(parsed.ptr(), wanted)) }
                .ok_or_else(|| Error::Failed("The file has nothing to add here.".into()))?
        };
        let mut uid = std::ptr::null_mut();
        let mut err = std::ptr::null_mut();
        let ok = unsafe {
            ecal::e_cal_client_create_object_sync(
                self.raw(),
                comp.ptr(),
                ecal::E_CAL_OPERATION_FLAG_NONE,
                &mut uid,
                std::ptr::null_mut(),
                &mut err,
            )
        };
        if ok == 0 {
            return Err(unsafe { take_error(err) });
        }
        Ok(unsafe { owned(uid) }.unwrap_or_default())
    }

    /// Imports every event (or task) in an iCalendar file, with its time zones,
    /// the way EDS imports a published calendar: new items are added and items
    /// already here (same uid) are updated. Returns the uids it contained.
    pub fn import_all(&self, ics: &str) -> Result<Vec<String>, Error> {
        let text = cstring(ics);
        let parsed = unsafe { Owned::new(icalsys::i_cal_component_new_from_string(text.as_ptr())) }
            .ok_or_else(|| Error::Failed("That isn't an iCalendar file.".into()))?;
        let wanted = match self.kind {
            Kind::Calendar => icalsys::I_CAL_VEVENT_COMPONENT,
            Kind::TaskList => icalsys::I_CAL_VTODO_COMPONENT,
        };
        let calendar = if unsafe { icalsys::i_cal_component_isa(parsed.ptr()) } == icalsys::I_CAL_VCALENDAR_COMPONENT {
            parsed
        } else {
            let wrapper = unsafe { Owned::new(icalsys::i_cal_component_new_vcalendar()) }.expect("libical allocates components");
            unsafe { icalsys::i_cal_component_add_component(wrapper.ptr(), parsed.ptr()) };
            wrapper
        };
        let mut uids = Vec::new();
        unsafe {
            let mut next = Owned::new(icalsys::i_cal_component_get_first_component(calendar.ptr(), wanted));
            while let Some(comp) = next {
                if let Some(uid) = crate::borrowed(icalsys::i_cal_component_get_uid(comp.ptr()))
                    && !uids.contains(&uid)
                {
                    uids.push(uid);
                }
                next = Owned::new(icalsys::i_cal_component_get_next_component(calendar.ptr(), wanted));
            }
        }
        if uids.is_empty() {
            return Err(Error::Failed("The file has nothing to add here.".into()));
        }
        // EDS 3.62's local backend aborts (in libical's merge) when a file brings
        // a VTIMEZONE it already has, so send only zones it doesn't know yet.
        unsafe {
            let mut known = Vec::new();
            let mut next = Owned::new(icalsys::i_cal_component_get_first_component(calendar.ptr(), icalsys::I_CAL_VTIMEZONE_COMPONENT));
            while let Some(zone) = next {
                let tzid = Owned::new(icalsys::i_cal_component_get_first_property(zone.ptr(), icalsys::I_CAL_TZID_PROPERTY))
                    .and_then(|p| owned(icalsys::i_cal_property_get_value_as_string(p.ptr())));
                if tzid.is_some_and(|t| !self.lookup(&t).is_null()) {
                    known.push(zone);
                }
                next = Owned::new(icalsys::i_cal_component_get_next_component(calendar.ptr(), icalsys::I_CAL_VTIMEZONE_COMPONENT));
            }
            for zone in known {
                icalsys::i_cal_component_remove_component(calendar.ptr(), zone.ptr());
            }
        }
        let mut err = std::ptr::null_mut();
        let ok = unsafe {
            ecal::e_cal_client_receive_objects_sync(
                self.raw(),
                calendar.ptr(),
                ecal::E_CAL_OPERATION_FLAG_NONE,
                std::ptr::null_mut(),
                &mut err,
            )
        };
        if ok == 0 {
            return Err(unsafe { take_error(err) });
        }
        Ok(uids)
    }

    /// An item with all its changed occurrences and the time zones they use, as
    /// a complete iCalendar file.
    pub fn export(&self, uid: &str) -> Result<String, Error> {
        let uid = cstring(uid);
        let mut list = std::ptr::null_mut();
        let mut err = std::ptr::null_mut();
        let ok = unsafe {
            ecal::e_cal_client_get_objects_for_uid_sync(self.raw(), uid.as_ptr(), &mut list, std::ptr::null_mut(), &mut err)
        };
        if ok == 0 {
            return Err(unsafe { take_error(err) });
        }
        let calendar = unsafe { Owned::new(icalsys::i_cal_component_new_vcalendar()) }.expect("libical allocates components");
        let mut tzids: Vec<String> = Vec::new();
        unsafe {
            for prop in [
                icalsys::i_cal_property_new_version(cstring("2.0").as_ptr()),
                icalsys::i_cal_property_new_prodid(cstring("-//Nyx//Calendar//EN").as_ptr()),
            ] {
                if let Some(prop) = Owned::new(prop) {
                    icalsys::i_cal_component_add_property(calendar.ptr(), prop.ptr());
                }
            }
            let mut node = list;
            while !node.is_null() {
                let comp = ecal::e_cal_component_get_icalcomponent((*node).data.cast());
                if !comp.is_null() {
                    for kind in [
                        icalsys::I_CAL_DTSTART_PROPERTY,
                        icalsys::I_CAL_DTEND_PROPERTY,
                        icalsys::I_CAL_DUE_PROPERTY,
                        icalsys::I_CAL_RECURRENCEID_PROPERTY,
                    ] {
                        if let Some(tzid) = ical::tzid_param(comp, kind)
                            && !tzids.contains(&tzid)
                        {
                            tzids.push(tzid);
                        }
                    }
                    if let Some(copy) = Owned::new(icalsys::i_cal_component_clone(comp)) {
                        icalsys::i_cal_component_add_component(calendar.ptr(), copy.ptr());
                    }
                }
                node = (*node).next;
            }
            edata_server_sys::e_client_util_free_object_slist(list);
            for tzid in tzids {
                let c = cstring(&tzid);
                let mut zone = std::ptr::null_mut();
                let found = ecal::e_cal_client_get_timezone_sync(self.raw(), c.as_ptr(), &mut zone, std::ptr::null_mut(), std::ptr::null_mut()) != 0;
                if found
                    && !zone.is_null()
                    && let Some(vtimezone) = Owned::new(icalsys::i_cal_timezone_get_component(zone))
                    && let Some(copy) = Owned::new(icalsys::i_cal_component_clone(vtimezone.ptr()))
                {
                    icalsys::i_cal_component_add_component(calendar.ptr(), copy.ptr());
                }
            }
            Ok(owned(icalsys::i_cal_component_as_ical_string(calendar.ptr())).unwrap_or_default())
        }
    }

    /// The busy times of `people` (email addresses) from `from` to `to` (Unix
    /// seconds), as this calendar's server knows them (Exchange and Microsoft 365,
    /// CalDAV servers with scheduling). Each person found, with (start, end) times.
    pub fn free_busy(&self, from: i64, to: i64, people: &[String]) -> Result<Vec<Busy>, Error> {
        let names: Vec<std::ffi::CString> = people.iter().map(|p| cstring(p)).collect();
        let mut out: Vec<Busy> = Vec::new();
        unsafe {
            let mut users: *mut glib_sys::GSList = std::ptr::null_mut();
            for name in &names {
                users = glib_sys::g_slist_append(users, name.as_ptr() as glib_sys::gpointer);
            }
            let mut list = std::ptr::null_mut();
            let mut err = std::ptr::null_mut();
            let ok = ecal::e_cal_client_get_free_busy_sync(
                self.raw(),
                from as _,
                to as _,
                users,
                &mut list,
                std::ptr::null_mut(),
                &mut err,
            );
            glib_sys::g_slist_free(users);
            if ok == 0 {
                return Err(take_error(err));
            }
            let mut node = list;
            while !node.is_null() {
                let comp = ecal::e_cal_component_get_icalcomponent((*node).data.cast());
                if !comp.is_null() {
                    let (who, busy) = ical::busy_periods(comp);
                    match out.iter_mut().find(|(w, _)| w.eq_ignore_ascii_case(&who)) {
                        Some((_, times)) => times.extend(busy),
                        None => out.push((who, busy)),
                    }
                }
                node = (*node).next;
            }
            edata_server_sys::e_client_util_free_object_slist(list);
        }
        Ok(out)
    }

    fn add_zone(&self, zone: *mut icalsys::ICalTimezone) -> Result<(), Error> {
        if ical::is_utc(zone) {
            return Ok(());
        }
        let mut err = std::ptr::null_mut();
        if unsafe { ecal::e_cal_client_add_timezone_sync(self.raw(), zone, std::ptr::null_mut(), &mut err) } == 0 {
            return Err(unsafe { take_error(err) });
        }
        Ok(())
    }

    /// Calls `on_change` when items matching `query` are added, changed or removed
    /// (by this app, another app or a sync). Callers re-read what they show.
    pub fn watch(&self, query: &str, on_change: impl Fn() + 'static) -> Result<View, Error> {
        let query = cstring(query);
        let mut view = std::ptr::null_mut();
        let mut err = std::ptr::null_mut();
        let ok = unsafe {
            ecal::e_cal_client_get_view_sync(self.raw(), query.as_ptr(), &mut view, std::ptr::null_mut(), &mut err)
        };
        if ok == 0 || view.is_null() {
            return Err(if err.is_null() { Error::Failed("Couldn't watch the calendar.".into()) } else { unsafe { take_error(err) } });
        }
        let obj = unsafe { glib::Object::from_glib_full(view.cast::<gobject_sys::GObject>()) };
        let on_change = std::rc::Rc::new(on_change);
        let handlers = ["objects-added", "objects-modified", "objects-removed"]
            .into_iter()
            .map(|signal| {
                let on_change = on_change.clone();
                obj.connect_local(signal, false, move |_| {
                    on_change();
                    None
                })
            })
            .collect();
        unsafe {
            // Initial contents arrive as objects-added; callers have already read them.
            ecal::e_cal_client_view_set_flags(obj.as_ptr().cast(), ecal::E_CAL_CLIENT_VIEW_FLAGS_NONE, std::ptr::null_mut());
            let mut err = std::ptr::null_mut();
            ecal::e_cal_client_view_start(obj.as_ptr().cast(), &mut err);
            if !err.is_null() {
                return Err(take_error(err));
            }
        }
        Ok(View { obj, handlers })
    }
}

struct Collect {
    out: Vec<Item>,
    zone: *mut icalsys::ICalTimezone,
}

unsafe extern "C" fn collect(
    comp: *mut icalsys::ICalComponent,
    start: *mut icalsys::ICalTime,
    end: *mut icalsys::ICalTime,
    data: glib_sys::gpointer,
    _cancellable: *mut gio_sys::GCancellable,
    _error: *mut *mut glib_sys::GError,
) -> glib_sys::gboolean {
    let data = unsafe { &mut *data.cast::<Collect>() };
    data.out.push(unsafe { ical::item(comp, Some((start, end)), &ical::Zones::fixed(data.zone)) });
    1
}

/// An EDS query for items that occur in `[from, to)` (Unix seconds).
pub fn range_query(from: i64, to: i64) -> String {
    format!(r#"(occur-in-time-range? (make-time "{}") (make-time "{}"))"#, utc_stamp(from), utc_stamp(to))
}

fn utc_stamp(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil(days);
    format!("{y:04}{m:02}{d:02}T{:02}{:02}{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

/// Days since 1970-01-01 to a proleptic Gregorian date.
fn civil(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_query_uses_utc_stamps() {
        assert_eq!(
            range_query(1_793_631_600, 1_793_635_200),
            r#"(occur-in-time-range? (make-time "20261102T150000Z") (make-time "20261102T160000Z"))"#
        );
        assert_eq!(utc_stamp(0), "19700101T000000Z");
        assert_eq!(utc_stamp(951_782_400), "20000229T000000Z");
    }
}
