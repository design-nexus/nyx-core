use crate::{
    ical,
    registry::{Watch, error},
    *,
};
use ical_glib_sys as sys;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    This,
    ThisAndFollowing,
    All,
}
pub type Busy = (String, Vec<(i64, i64)>);
pub(crate) struct Component {
    object: Owned<sys::ICalComponent>,
    _root: Option<Owned<sys::ICalComponent>>,
}
impl Component {
    pub(crate) fn ptr(&self) -> *mut sys::ICalComponent {
        self.object.ptr()
    }
}
pub struct Calendar {
    registry: Registry,
    uid: String,
    kind: Kind,
}
pub struct View {
    _watch: Watch,
}
pub fn range_query(from: i64, to: i64) -> String {
    format!("native-range:{from}:{to}")
}
impl Calendar {
    pub fn open(registry: &Registry, uid: &str, kind: Kind, _wait: Option<u32>) -> Result<Self, Error> {
        registry.source(uid, kind).ok_or_else(|| error("Source not found"))?;
        Ok(Self { registry: registry.clone(), uid: uid.into(), kind })
    }
    pub fn kind(&self) -> Kind {
        self.kind
    }
    pub fn read_only(&self) -> bool {
        self.registry.source(&self.uid, self.kind).is_none_or(|s| !s.writable)
    }
    pub fn online(&self) -> bool {
        self.registry.source(&self.uid, self.kind).is_some_and(|s| s.local || self.registry.healthy(&self.uid))
    }
    pub fn can(&self, _capability: &str) -> bool {
        false
    }
    pub fn refresh(&self) -> Result<(), Error> {
        self.registry.refresh_accounts(if self.kind == Kind::Calendar {
            nyx_accounts_native::Service::Calendar
        } else {
            nyx_accounts_native::Service::Tasks
        })
    }
    pub(crate) fn component(&self, payload: &str) -> Result<Component, Error> {
        let object = unsafe { Owned::new(sys::i_cal_component_new_from_string(cstring(payload).as_ptr())) }
            .ok_or_else(|| error("Invalid iCalendar payload"))?;
        if unsafe { sys::i_cal_component_isa(object.ptr()) } == sys::I_CAL_VCALENDAR_COMPONENT {
            return Ok(Component { object, _root: None });
        }
        let root =
            unsafe { Owned::new(sys::i_cal_component_new_vcalendar()) }.ok_or_else(|| error("Could not allocate calendar"))?;
        let zones: Vec<String> = {
            let db = self.registry.db.borrow();
            let mut q = db.prepare("SELECT payload FROM timezones WHERE source=?1").map_err(error)?;
            q.query_map([&self.uid], |r| r.get(0)).map_err(error)?.filter_map(Result::ok).collect()
        };
        for zone in zones {
            if let Some(zone) = unsafe { Owned::new(sys::i_cal_component_new_from_string(cstring(&zone).as_ptr())) } {
                unsafe {
                    sys::i_cal_component_add_component(root.ptr(), zone.ptr());
                }
            }
        }
        unsafe {
            sys::i_cal_component_add_component(root.ptr(), object.ptr());
        }
        Ok(Component { object, _root: Some(root) })
    }
    pub(crate) fn read(&self, comp: *mut sys::ICalComponent) -> Item {
        let parent = unsafe { Owned::new(sys::i_cal_component_get_parent(comp)) };
        let zones = std::cell::RefCell::new(Vec::new());
        let lookup = |tzid: &str| {
            let Some(parent) = parent.as_ref() else { return std::ptr::null_mut() };
            let zone = unsafe { Owned::new(sys::i_cal_component_get_timezone(parent.ptr(), cstring(tzid).as_ptr())) };
            if let Some(zone) = zone {
                let ptr = zone.ptr();
                zones.borrow_mut().push(zone);
                ptr
            } else {
                std::ptr::null_mut()
            }
        };
        unsafe { ical::item(comp, None, &ical::Zones { fallback: ical::zone(&ical::local_zone()), lookup: Some(&lookup) }) }
    }
    fn payloads(&self) -> Result<Vec<String>, Error> {
        let db = self.registry.db.borrow();
        let mut q = db.prepare("SELECT payload FROM objects WHERE source=?1 AND deleted=0 ORDER BY rid").map_err(error)?;
        q.query_map([&self.uid], |r| r.get(0)).map_err(error)?.map(|r| r.map_err(error)).collect()
    }
    pub fn items(&self, _query: &str) -> Result<Vec<Item>, Error> {
        self.payloads()?.iter().map(|p| self.component(p).map(|c| self.read(c.ptr()))).collect()
    }
    pub fn instances(&self, from: i64, to: i64) -> Vec<Item> {
        let mut out = Vec::new();
        let mut cancelled_series = std::collections::HashSet::new();
        for payload in self.payloads().unwrap_or_default() {
            let Ok(comp) = self.component(&payload) else { continue };
            let item = self.read(comp.ptr());
            if item.rid.is_none() && item.status == Status::Cancelled {
                cancelled_series.insert(item.uid.clone());
            }
            if item.rid.is_some() {
                out.push(item);
                continue;
            }
            struct Context {
                items: Vec<Item>,
                zone: *mut sys::ICalTimezone,
                all_day: bool,
            }
            unsafe extern "C" fn occurrence(
                comp: *mut sys::ICalComponent,
                span: *mut sys::ICalTimeSpan,
                data: glib_sys::gpointer,
            ) {
                unsafe {
                    let context = &mut *data.cast::<Context>();
                    let start = ical::time(When::Time(sys::i_cal_time_span_get_start(span)), context.zone);
                    let end = ical::time(When::Time(sys::i_cal_time_span_get_end(span)), context.zone);
                    if context.all_day {
                        sys::i_cal_time_set_is_date(start.ptr(), 1);
                        sys::i_cal_time_set_is_date(end.ptr(), 1);
                    }
                    context.items.push(ical::item(comp, Some((start.ptr(), end.ptr())), &ical::Zones::fixed(context.zone)));
                }
            }
            let parent = unsafe { Owned::new(sys::i_cal_component_get_parent(comp.ptr())) };
            let custom_zone = parent.as_ref().and_then(|parent| unsafe {
                let tzid = ical::tzid_param(comp.ptr(), sys::I_CAL_DTSTART_PROPERTY)?;
                Owned::new(sys::i_cal_component_get_timezone(parent.ptr(), cstring(&tzid).as_ptr()))
            });
            let zone = unsafe {
                let start = Owned::new(sys::i_cal_component_get_dtstart(comp.ptr()));
                let zone = start.as_ref().map(|t| ical::zone_of(t.ptr())).unwrap_or(std::ptr::null_mut());
                if !zone.is_null() {
                    zone
                } else {
                    ical::zone(&ical::tzid_param(comp.ptr(), sys::I_CAL_DTSTART_PROPERTY).unwrap_or_else(ical::local_zone))
                }
            };
            let zone = custom_zone.as_ref().map(|z| z.ptr()).unwrap_or(zone);
            let start = ical::time(When::Time(from), zone);
            let end = ical::time(When::Time(to), zone);
            let mut context = Context { items: Vec::new(), zone, all_day: item.all_day() };
            unsafe {
                sys::i_cal_component_foreach_recurrence(
                    comp.ptr(),
                    start.ptr(),
                    end.ptr(),
                    Some(occurrence),
                    (&mut context as *mut Context).cast(),
                )
            };
            if context.items.is_empty() && self.kind == Kind::TaskList {
                context.items.push(item);
            }
            out.extend(context.items);
        }
        // Detached exceptions replace generated instances with matching UID/RID.
        let mut unique = std::collections::BTreeMap::new();
        for item in out {
            unique.insert(
                (
                    item.uid.clone(),
                    item.rid.as_ref().map(|rid| {
                        self.recurrence_when(&item.uid, rid).map(|value| format!("{value:?}")).unwrap_or_else(|| rid.clone())
                    }),
                ),
                item,
            );
        }
        unique.into_values().filter(|i| i.status != Status::Cancelled && !cancelled_series.contains(&i.uid)).collect()
    }
    pub(crate) fn recurrence_when(&self, uid: &str, rid: &str) -> Option<When> {
        let time = unsafe { Owned::new(sys::i_cal_time_new_from_string(cstring(rid).as_ptr())) }?;
        let master = self.ics(uid, None).ok().and_then(|payload| self.component(&payload).ok());
        let tzid = master
            .as_ref()
            .and_then(|comp| unsafe { ical::tzid_param(comp.ptr(), sys::I_CAL_DTSTART_PROPERTY) })
            .unwrap_or_else(ical::local_zone);
        let parent = master.as_ref().and_then(|comp| unsafe { Owned::new(sys::i_cal_component_get_parent(comp.ptr())) });
        let custom = parent
            .as_ref()
            .and_then(|parent| unsafe { Owned::new(sys::i_cal_component_get_timezone(parent.ptr(), cstring(&tzid).as_ptr())) });
        let zone = custom.as_ref().map(|zone| zone.ptr()).unwrap_or_else(|| ical::zone(&tzid));
        unsafe { ical::when(time.ptr(), &ical::Zones::fixed(zone)) }
    }
    pub fn ics(&self, uid: &str, rid: Option<&str>) -> Result<String, Error> {
        self.registry
            .db
            .borrow()
            .query_row(
                "SELECT payload FROM objects WHERE source=?1 AND uid=?2 AND rid=?3 AND deleted=0",
                rusqlite::params![self.uid, uid, rid.unwrap_or("")],
                |r| r.get(0),
            )
            .map_err(error)
    }
    pub fn get(&self, uid: &str, rid: Option<&str>) -> Result<Item, Error> {
        let c = self.component(&self.ics(uid, rid)?)?;
        Ok(self.read(c.ptr()))
    }
    fn put(&self, comp: *mut sys::ICalComponent, dirty: bool) -> Result<String, Error> {
        if dirty && self.read_only() {
            return Err(error("Calendar is read-only"));
        }
        let item = self.read(comp);
        if item.uid.trim().is_empty() {
            return Err(error("A calendar item without UID cannot be safely stored"));
        }
        let payload =
            unsafe { owned(sys::i_cal_component_as_ical_string(comp)) }.ok_or_else(|| error("Could not serialize item"))?;
        self.registry.db.borrow().execute("INSERT INTO objects(source,uid,rid,payload,dirty) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(source,uid,rid) DO UPDATE SET payload=excluded.payload,dirty=excluded.dirty,deleted=0 WHERE objects.dirty=0 OR excluded.dirty=1",rusqlite::params![self.uid,item.uid,item.rid.unwrap_or_default(),payload,dirty]).map_err(error)?;
        if dirty && self.registry.source(&self.uid, self.kind).is_some_and(|s| !s.local) {
            self.registry.queue_sync(if self.kind == Kind::Calendar {
                nyx_accounts_native::Service::Calendar
            } else {
                nyx_accounts_native::Service::Tasks
            });
        }
        Ok(item.uid)
    }
    pub fn create(&self, draft: &Draft) -> Result<String, Error> {
        let c = ical::new_component(if self.kind == Kind::Calendar { ItemKind::Event } else { ItemKind::Task });
        unsafe {
            ical::apply(c.ptr(), draft);
            sys::i_cal_component_set_uid(c.ptr(), cstring(&uuid::Uuid::new_v4().to_string()).as_ptr());
        }
        self.put(c.ptr(), true)
    }
    pub fn modify(&self, uid: &str, rid: Option<&str>, scope: Scope, draft: &Draft) -> Result<(), Error> {
        if let (Some(rid), Scope::ThisAndFollowing) = (rid, scope) {
            return self.split_following(uid, rid, Some(draft));
        }
        let payload = self.ics(uid, if scope == Scope::All { None } else { rid }).or_else(|_| self.ics(uid, None))?;
        let c = self.component(&payload)?;
        if let (Some(rid), Scope::This) = (rid, scope) {
            unsafe { ical::detach(c.ptr(), rid, false) };
        }
        unsafe {
            ical::apply(c.ptr(), draft);
        }
        self.put(c.ptr(), true)?;
        Ok(())
    }
    fn split_following(&self, uid: &str, rid: &str, draft: Option<&Draft>) -> Result<(), Error> {
        if self.read_only() {
            return Err(error("Calendar is read-only"));
        }
        let payload = self.ics(uid, None)?;
        let master = self.component(&payload)?;
        let item = self.read(master.ptr());
        let rule = item.rule.as_ref().ok_or_else(|| error("This item has no repeating series"))?;
        let starts =
            unsafe { Owned::new(sys::i_cal_component_get_dtstart(master.ptr())) }.ok_or_else(|| error("Series has no start"))?;
        let target = unsafe { Owned::new(sys::i_cal_time_new_from_string(cstring(rid).as_ptr())) }
            .ok_or_else(|| error("Invalid occurrence"))?;
        let zone = unsafe { ical::zone_of(starts.ptr()) };
        let zone = if zone.is_null() {
            ical::zone(&unsafe { ical::tzid_param(master.ptr(), sys::I_CAL_DTSTART_PROPERTY) }.unwrap_or_else(ical::local_zone))
        } else {
            zone
        };
        let cutoff = unsafe { sys::i_cal_time_as_timet_with_zone(target.ptr(), zone) };
        let recurrence = unsafe { Owned::new(sys::i_cal_recurrence_new_from_string(cstring(rule).as_ptr())) }
            .ok_or_else(|| error("Invalid recurrence"))?;
        let iterator = unsafe { Owned::new(sys::i_cal_recur_iterator_new(recurrence.ptr(), starts.ptr())) }
            .ok_or_else(|| error("Cannot expand recurrence"))?;
        let mut before = 0usize;
        let mut previous = None;
        let mut found = false;
        for _ in 0..100000 {
            let Some(value) = (unsafe { Owned::new(sys::i_cal_recur_iterator_next(iterator.ptr())) }) else { break };
            if unsafe { sys::i_cal_time_is_null_time(value.ptr()) } != 0 {
                break;
            }
            let stamp = unsafe { sys::i_cal_time_as_timet_with_zone(value.ptr(), zone) };
            if stamp >= cutoff {
                found = stamp == cutoff;
                break;
            }
            before += 1;
            previous = Some(value);
        }
        if !found {
            return Err(error("Occurrence could not be located safely; the series was not changed"));
        }
        if before == 0 {
            return if let Some(draft) = draft {
                self.modify(uid, None, Scope::All, draft)
            } else {
                self.remove(uid, None, Scope::All)
            };
        }
        let previous = previous.unwrap();
        let until = if item.all_day() {
            unsafe { owned(sys::i_cal_time_as_ical_string(previous.ptr())) }.unwrap_or_default()
        } else {
            let stamp = unsafe { sys::i_cal_time_as_timet_with_zone(previous.ptr(), zone) };
            let value = ical::time(When::Time(stamp), ical::zone("UTC"));
            unsafe { owned(sys::i_cal_time_as_ical_string(value.ptr())) }.unwrap_or_default()
        };
        let mut earlier: Vec<_> =
            rule.split(';').filter(|s| !s.starts_with("COUNT=") && !s.starts_with("UNTIL=")).map(str::to_owned).collect();
        earlier.push(format!("UNTIL={until}"));
        self.registry.db.borrow().execute_batch("SAVEPOINT split_series").map_err(error)?;
        let result = (|| {
            unsafe {
                ical::apply(master.ptr(), &Draft { repeat: Some(Some(earlier.join(";"))), ..Default::default() });
            }
            self.put(master.ptr(), true)?;
            let exceptions: Vec<(String, String)> = {
                let db = self.registry.db.borrow();
                let mut q = db
                    .prepare("SELECT rid,payload FROM objects WHERE source=?1 AND uid=?2 AND rid<>'' AND deleted=0")
                    .map_err(error)?;
                q.query_map(rusqlite::params![self.uid, uid], |r| Ok((r.get(0)?, r.get(1)?)))
                    .map_err(error)?
                    .filter_map(Result::ok)
                    .collect()
            };
            let mut new_id = None;
            if let Some(draft) = draft {
                let later = self.component(&payload)?;
                unsafe {
                    ical::detach(later.ptr(), rid, true);
                    ical::remove_all(later.ptr(), sys::I_CAL_RECURRENCEID_PROPERTY);
                }
                let id = uuid::Uuid::new_v4().to_string();
                unsafe {
                    sys::i_cal_component_set_uid(later.ptr(), cstring(&id).as_ptr());
                }
                let mut draft = draft.clone();
                if draft.repeat.is_none() {
                    let mut parts = Vec::new();
                    for part in rule.split(';') {
                        if let Some(count) = part.strip_prefix("COUNT=").and_then(|s| s.parse::<usize>().ok()) {
                            parts.push(format!("COUNT={}", count.saturating_sub(before).max(1)));
                        } else {
                            parts.push(part.to_owned());
                        }
                    }
                    draft.repeat = Some(Some(parts.join(";")));
                }
                unsafe {
                    ical::apply(later.ptr(), &draft);
                }
                self.put(later.ptr(), true)?;
                new_id = Some(id);
            }
            for (exception_rid, payload) in exceptions {
                let recurrence = unsafe { Owned::new(sys::i_cal_time_new_from_string(cstring(&exception_rid).as_ptr())) }
                    .ok_or_else(|| error("Invalid exception"))?;
                if unsafe { sys::i_cal_time_as_timet_with_zone(recurrence.ptr(), zone) } < cutoff {
                    continue;
                }
                if let Some(id) = &new_id {
                    let exception = self.component(&payload)?;
                    unsafe {
                        sys::i_cal_component_set_uid(exception.ptr(), cstring(id).as_ptr());
                    }
                    self.put(exception.ptr(), true)?;
                }
                self.registry
                    .db
                    .borrow()
                    .execute(
                        "UPDATE objects SET deleted=1,dirty=1 WHERE source=?1 AND uid=?2 AND rid=?3",
                        rusqlite::params![self.uid, uid, exception_rid],
                    )
                    .map_err(error)?;
            }
            Ok::<_, Error>(())
        })();
        if result.is_ok() {
            self.registry.db.borrow().execute_batch("RELEASE split_series").map_err(error)?;
        } else {
            let _ = self.registry.db.borrow().execute_batch("ROLLBACK TO split_series; RELEASE split_series");
        }
        result
    }
    pub fn complete(&self, uid: &str) -> Result<(), Error> {
        if self.registry.source(&self.uid, self.kind).is_some_and(|s| s.backend == "graph-tasks") {
            return self.modify(uid, None, Scope::All, &Draft { done: Some(true), ..Default::default() });
        }
        let item = self.get(uid, None)?;
        if let Some(rule) = &item.rule {
            let comp = self.component(&self.ics(uid, None)?)?;
            let zone = unsafe {
                ical::tzid_param(comp.ptr(), sys::I_CAL_DTSTART_PROPERTY)
                    .or_else(|| ical::tzid_param(comp.ptr(), sys::I_CAL_DUE_PROPERTY))
            }
            .unwrap_or_else(ical::local_zone);
            let base = item.start.or(item.due).ok_or_else(|| error("Repeating task has no start or due date"))?;
            let time = ical::time(base, ical::zone(&zone));
            let recurrence = unsafe { Owned::new(sys::i_cal_recurrence_new_from_string(cstring(rule).as_ptr())) }
                .ok_or_else(|| error("Invalid recurrence rule"))?;
            let iterator = unsafe { Owned::new(sys::i_cal_recur_iterator_new(recurrence.ptr(), time.ptr())) }
                .ok_or_else(|| error("Could not expand recurrence"))?;
            let original = unsafe { sys::i_cal_time_as_timet_with_zone(time.ptr(), ical::zone(&zone)) };
            let mut next = None;
            for _ in 0..10000 {
                let Some(value) = (unsafe { Owned::new(sys::i_cal_recur_iterator_next(iterator.ptr())) }) else { break };
                if unsafe { sys::i_cal_time_is_null_time(value.ptr()) } != 0 {
                    break;
                }
                let stamp = unsafe { sys::i_cal_time_as_timet_with_zone(value.ptr(), ical::zone(&zone)) };
                if stamp > original {
                    next = Some(stamp);
                    break;
                }
            }
            if let Some(next) = next {
                let delta = next - original;
                let shift = |value: Option<When>| {
                    value.map(|value| match value {
                        When::Time(seconds) => When::Time(seconds + delta),
                        date @ When::Date { .. } => {
                            let value = ical::time(date, ical::zone(&zone));
                            unsafe {
                                sys::i_cal_time_adjust(value.ptr(), (delta / 86400) as i32, 0, 0, 0);
                                ical::when(value.ptr(), &ical::Zones::fixed(ical::zone(&zone))).unwrap_or(date)
                            }
                        }
                    })
                };
                let mut parts = Vec::new();
                for part in rule.split(';') {
                    if let Some(count) = part.strip_prefix("COUNT=").and_then(|n| n.parse::<usize>().ok()) {
                        parts.push(format!("COUNT={}", count.saturating_sub(1).max(1)));
                    } else {
                        parts.push(part.to_owned());
                    }
                }
                return self.modify(
                    uid,
                    None,
                    Scope::All,
                    &Draft {
                        start: Some(shift(item.start)),
                        due: Some(shift(item.due)),
                        end: Some(shift(item.end)),
                        done: Some(false),
                        repeat: Some(Some(parts.join(";"))),
                        zone: Some(zone),
                        ..Default::default()
                    },
                );
            }
        }
        self.modify(uid, None, Scope::All, &Draft { done: Some(true), ..Default::default() })
    }
    pub fn remove(&self, uid: &str, rid: Option<&str>, scope: Scope) -> Result<(), Error> {
        if self.read_only() {
            return Err(error("Calendar is read-only"));
        }
        if let (Some(rid), Scope::ThisAndFollowing) = (rid, scope) {
            return self.split_following(uid, rid, None);
        }
        if let (Some(rid), Scope::This) = (rid, scope) {
            return self.modify(uid, Some(rid), Scope::This, &Draft { status: Some(Status::Cancelled), ..Default::default() });
        }
        self.registry
            .db
            .borrow()
            .execute("UPDATE objects SET deleted=1,dirty=1 WHERE source=?1 AND uid=?2", rusqlite::params![self.uid, uid])
            .map_err(error)?;
        if self.registry.source(&self.uid, self.kind).is_some_and(|s| !s.local) {
            self.registry.queue_sync(if self.kind == Kind::Calendar {
                nyx_accounts_native::Service::Calendar
            } else {
                nyx_accounts_native::Service::Tasks
            });
        }
        Ok(())
    }
    pub fn respond(&self, uid: &str, rid: Option<&str>, emails: &[String], response: Response) -> Result<bool, Error> {
        let comp = self.component(&self.ics(uid, rid)?)?;
        if !unsafe { ical::respond(comp.ptr(), emails, response) } {
            return Ok(false);
        }
        let source = self.registry.source(&self.uid, self.kind).ok_or_else(|| error("Calendar not found"))?;
        if matches!(source.backend.as_str(), "google" | "graph") {
            self.registry.respond_api(&self.uid, uid, rid, emails, response)?;
            self.put(comp.ptr(), false)?;
        } else {
            if source.backend == "dav" {
                let account_id = source.account.as_ref().ok_or_else(|| error("Calendar has no account"))?;
                let client = nyx_accounts_native::Client::connect().map_err(error)?;
                let account = client
                    .list()
                    .map_err(error)?
                    .into_iter()
                    .find(|a| &a.id == account_id)
                    .ok_or_else(|| error("Account not found"))?;
                let endpoint = self
                    .registry
                    .db
                    .borrow()
                    .query_row("SELECT endpoint FROM api_endpoints WHERE source=?1", [&self.uid], |r| r.get::<_, String>(0))
                    .ok()
                    .or_else(|| account.endpoints.get("caldav").cloned())
                    .ok_or_else(|| error("Configure a CalDAV endpoint"))?;
                let secret = client.credential(account_id, nyx_accounts_native::Service::Calendar).map_err(error)?;
                let remote = nyx_accounts_native::remote::Remote::new(account, secret)
                    .map_err(error)?
                    .for_service(nyx_accounts_native::Service::Calendar);
                if !remote.dav_supports_scheduling(&endpoint).map_err(error)? {
                    return Err(error(
                        "This server does not advertise automatic invitation delivery. Respond using the provider’s calendar.",
                    ));
                }
                let original = self.ics(uid, rid)?.replace("\r\n ", "").replace("\r\n\t", "").to_ascii_uppercase();
                if original.contains("SCHEDULE-AGENT=CLIENT") || original.contains("SCHEDULE-AGENT=NONE") {
                    return Err(error(
                        "This invitation requires client-managed delivery. Respond using the provider’s calendar.",
                    ));
                }
            }
            self.put(comp.ptr(), true)?;
        }
        Ok(true)
    }
    pub fn import(&self, ics: &str) -> Result<String, Error> {
        self.import_all(ics)?.into_iter().next().ok_or_else(|| error("No matching items"))
    }
    pub fn import_all(&self, payload: &str) -> Result<Vec<String>, Error> {
        let root = self.component(payload)?;
        // Keep the complete source, including VTIMEZONE and unknown properties.
        self.registry
            .db
            .borrow()
            .execute("INSERT OR IGNORE INTO raw_imports VALUES(?1,?2)", rusqlite::params![self.uid, payload])
            .map_err(error)?;
        let mut timezone = unsafe { sys::i_cal_component_get_first_component(root.ptr(), sys::I_CAL_VTIMEZONE_COMPONENT) };
        while let Some(zone) = unsafe { Owned::new(timezone) } {
            if let Some(property) =
                unsafe { Owned::new(sys::i_cal_component_get_first_property(zone.ptr(), sys::I_CAL_TZID_PROPERTY)) }
            {
                if let (Some(tzid), Some(payload)) = unsafe {
                    (
                        borrowed(sys::i_cal_property_get_tzid(property.ptr())),
                        owned(sys::i_cal_component_as_ical_string(zone.ptr())),
                    )
                } {
                    self.registry.db.borrow().execute("INSERT INTO timezones VALUES(?1,?2,?3) ON CONFLICT(source,tzid) DO UPDATE SET payload=excluded.payload",rusqlite::params![self.uid,tzid,payload]).map_err(error)?;
                }
            }
            timezone = unsafe { sys::i_cal_component_get_next_component(root.ptr(), sys::I_CAL_VTIMEZONE_COMPONENT) };
        }
        let mut ids = Vec::new();
        let kind = if self.kind == Kind::Calendar { sys::I_CAL_VEVENT_COMPONENT } else { sys::I_CAL_VTODO_COMPONENT };
        if unsafe { sys::i_cal_component_isa(root.ptr()) } == kind {
            ids.push(self.put(root.ptr(), false)?);
        } else {
            let mut comp = unsafe { sys::i_cal_component_get_first_component(root.ptr(), kind) };
            while !comp.is_null() {
                let owned = unsafe { Owned::new(comp) }.unwrap();
                ids.push(self.put(owned.ptr(), false)?);
                comp = unsafe { sys::i_cal_component_get_next_component(root.ptr(), kind) };
            }
        }
        Ok(ids)
    }
    pub(crate) fn absolute_alarms(&self, uid: &str, rid: Option<&str>) -> Vec<i64> {
        let Ok(payload) = self.ics(uid, rid) else { return Vec::new() };
        let Ok(component) = self.component(&payload) else { return Vec::new() };
        let mut result = Vec::new();
        let mut next = unsafe { sys::i_cal_component_get_first_component(component.ptr(), sys::I_CAL_VALARM_COMPONENT) };
        while let Some(alarm) = unsafe { Owned::new(next) } {
            if let Some(property) =
                unsafe { Owned::new(sys::i_cal_component_get_first_property(alarm.ptr(), sys::I_CAL_TRIGGER_PROPERTY)) }
            {
                if let Some(trigger) = unsafe { Owned::new(sys::i_cal_property_get_trigger(property.ptr())) } {
                    if let Some(time) = unsafe { Owned::new(sys::i_cal_trigger_get_time(trigger.ptr())) } {
                        if unsafe { sys::i_cal_time_is_null_time(time.ptr()) } == 0 {
                            result.push(unsafe { sys::i_cal_time_as_timet_with_zone(time.ptr(), ical::zone("UTC")) });
                        }
                    }
                }
            }
            next = unsafe { sys::i_cal_component_get_next_component(component.ptr(), sys::I_CAL_VALARM_COMPONENT) };
        }
        result
    }
    pub fn export(&self, uid: &str) -> Result<String, Error> {
        let payloads: Vec<String> = {
            let db = self.registry.db.borrow();
            let mut q =
                db.prepare("SELECT payload FROM objects WHERE source=?1 AND uid=?2 AND deleted=0 ORDER BY rid").map_err(error)?;
            q.query_map(rusqlite::params![self.uid, uid], |r| r.get(0))
                .map_err(error)?
                .map(|r| r.map_err(error))
                .collect::<Result<_, _>>()?
        };
        if payloads.is_empty() {
            return Err(error("Item not found"));
        }
        let mut zones = std::collections::BTreeMap::new();
        for payload in &payloads {
            let comp = self.component(payload)?;
            for kind in [
                sys::I_CAL_DTSTART_PROPERTY,
                sys::I_CAL_DTEND_PROPERTY,
                sys::I_CAL_DUE_PROPERTY,
                sys::I_CAL_RECURRENCEID_PROPERTY,
            ] {
                if let Some(tzid) = unsafe { ical::tzid_param(comp.ptr(), kind) } {
                    if zones.contains_key(&tzid) {
                        continue;
                    }
                    let stored: Option<String> = self
                        .registry
                        .db
                        .borrow()
                        .query_row(
                            "SELECT payload FROM timezones WHERE source=?1 AND tzid=?2",
                            rusqlite::params![self.uid, tzid],
                            |r| r.get(0),
                        )
                        .ok();
                    let value = if let Some(stored) = stored {
                        Some(stored)
                    } else {
                        unsafe {
                            Owned::new(sys::i_cal_timezone_get_component(ical::zone(&tzid)))
                                .and_then(|zone| Owned::new(sys::i_cal_component_clone(zone.ptr())))
                                .and_then(|zone| {
                                    if let Some(property) =
                                        Owned::new(sys::i_cal_component_get_first_property(zone.ptr(), sys::I_CAL_TZID_PROPERTY))
                                    {
                                        sys::i_cal_property_set_tzid(property.ptr(), cstring(&tzid).as_ptr());
                                    }
                                    owned(sys::i_cal_component_as_ical_string(zone.ptr()))
                                })
                        }
                    };
                    if let Some(value) = value {
                        zones.insert(tzid, value);
                    }
                }
            }
        }
        let mut result = String::from("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Nyx//Calendar//EN\r\n");
        for zone in zones.into_values() {
            result.push_str(&zone);
        }
        for payload in payloads {
            result.push_str(&payload);
        }
        result.push_str("END:VCALENDAR\r\n");
        Ok(result)
    }
    pub fn free_busy(&self, from: i64, to: i64, emails: &[String]) -> Result<Vec<Busy>, Error> {
        if emails.is_empty() {
            return Ok(Vec::new());
        }
        let source = self.registry.source(&self.uid, self.kind).ok_or_else(|| error("Calendar not found"))?;
        if !matches!(source.backend.as_str(), "google" | "dav") {
            return Err(error("Free/busy lookup is not supported for personal Microsoft accounts or this local source"));
        }
        let account_id = source.account.ok_or_else(|| error("Calendar has no account"))?;
        let client = nyx_accounts_native::Client::connect().map_err(error)?;
        let account =
            client.list().map_err(error)?.into_iter().find(|a| a.id == account_id).ok_or_else(|| error("Account not found"))?;
        let secret = client.credential(&account_id, nyx_accounts_native::Service::Calendar).map_err(error)?;
        let remote = nyx_accounts_native::remote::Remote::new(account, secret)
            .map_err(error)?
            .for_service(nyx_accounts_native::Service::Calendar);
        if from >= to || emails.len() > 64 {
            return Err(error("Invalid free/busy range or attendee count"));
        }
        if source.backend == "dav" {
            let endpoint = remote.account.endpoints.get("caldav").ok_or_else(|| error("Configure a CalDAV endpoint"))?;
            let stamp = |t| {
                chrono::DateTime::from_timestamp(t, 0)
                    .map(|t| t.format("%Y%m%dT%H%M%SZ").to_string())
                    .ok_or_else(|| error("Invalid time range"))
            };
            let responses = remote
                .dav_free_busy(endpoint, &stamp(from)?, &stamp(to)?, &stamp(chrono::Utc::now().timestamp())?, emails)
                .map_err(error)?;
            let mut results = Vec::new();
            for (email, payload) in responses {
                let periods = read_free_busy(&payload, &email)?;
                results.push((email, periods));
            }
            return Ok(results);
        }
        let format =
            |t| chrono::DateTime::from_timestamp(t, 0).map(|t| t.to_rfc3339()).ok_or_else(|| error("Invalid time range"));
        let value=remote.json_write("https://www.googleapis.com/calendar/v3/freeBusy",reqwest::Method::POST,&serde_json::json!({"timeMin":format(from)?,"timeMax":format(to)?,"items":emails.iter().map(|id|serde_json::json!({"id":id})).collect::<Vec<_>>()}),None).map_err(error)?;
        let mut result = Vec::new();
        for email in emails {
            let calendar = value
                .get("calendars")
                .and_then(|v| v.get(email))
                .ok_or_else(|| error("Availability data omitted a requested attendee"))?;
            if calendar.get("errors").and_then(|v| v.as_array()).is_some_and(|errors| !errors.is_empty()) {
                return Err(error("Availability is unavailable for a requested attendee"));
            }
            let mut periods = Vec::new();
            let busy =
                calendar.get("busy").and_then(|v| v.as_array()).ok_or_else(|| error("Invalid provider availability data"))?;
            for period in busy {
                let start = period["start"]
                    .as_str()
                    .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                    .ok_or_else(|| error("Invalid busy period"))?;
                let end = period["end"]
                    .as_str()
                    .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                    .ok_or_else(|| error("Invalid busy period"))?;
                if end <= start {
                    return Err(error("Invalid busy period"));
                }
                periods.push((start.timestamp(), end.timestamp()));
            }
            result.push((email.clone(), periods));
        }
        Ok(result)
    }
    pub fn watch(&self, _query: &str, on_change: impl Fn() + 'static) -> Result<View, Error> {
        Ok(View { _watch: self.registry.watch(on_change) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn calendar() -> Calendar {
        let path = std::env::temp_dir().join(format!("nyx-pim-test-{}/pim.db", uuid::Uuid::new_v4()));
        let registry = Registry::open(path).unwrap();
        Calendar::open(&registry, "system-calendar", Kind::Calendar, None).unwrap()
    }
    #[test]
    fn import_is_repeatable_and_preserves_unknown_fields() {
        let calendar = calendar();
        let payload = "BEGIN:VEVENT\r\nUID:fixture\r\nDTSTART:20261030T120000Z\r\nDTEND:20261030T130000Z\r\nSUMMARY:Test\r\nX-FIXTURE:preserve\r\nEND:VEVENT\r\n";
        calendar.import(payload).unwrap();
        calendar.import(payload).unwrap();
        assert_eq!(calendar.items("#t").unwrap().len(), 1);
        assert!(calendar.export("fixture").unwrap().contains("X-FIXTURE:preserve"));
        calendar.modify("fixture", None, Scope::All, &Draft { summary: Some("Changed".into()), ..Default::default() }).unwrap();
        assert!(calendar.export("fixture").unwrap().contains("X-FIXTURE:preserve"));
    }
    #[test]
    fn recurrence_is_expanded_by_libical() {
        let calendar = calendar();
        calendar.import("BEGIN:VEVENT\r\nUID:repeat\r\nDTSTART:20261030T120000Z\r\nDTEND:20261030T130000Z\r\nRRULE:FREQ=DAILY;COUNT=3\r\nSUMMARY:Repeat\r\nEND:VEVENT\r\n").unwrap();
        let instances = calendar.instances(1793300000, 1793700000);
        assert_eq!(instances.len(), 3);
    }
    #[test]
    fn custom_timezone_survives_storage_and_reload() {
        let calendar = calendar();
        calendar.import("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VTIMEZONE\r\nTZID:Custom/Test\r\nBEGIN:STANDARD\r\nDTSTART:19700101T000000\r\nTZOFFSETFROM:+0230\r\nTZOFFSETTO:+0230\r\nEND:STANDARD\r\nEND:VTIMEZONE\r\nBEGIN:VEVENT\r\nUID:custom-zone\r\nDTSTART;TZID=Custom/Test:20261030T120000\r\nDTEND;TZID=Custom/Test:20261030T130000\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n").unwrap();
        let expected =
            chrono::NaiveDate::from_ymd_opt(2026, 10, 30).unwrap().and_hms_opt(9, 30, 0).unwrap().and_utc().timestamp();
        assert_eq!(calendar.get("custom-zone", None).unwrap().start, Some(When::Time(expected)));
    }
    #[test]
    fn recurring_task_advances_until_final_completion() {
        let path = std::env::temp_dir().join(format!("nyx-pim-task-{}/pim.db", uuid::Uuid::new_v4()));
        let registry = Registry::open(path).unwrap();
        let calendar = Calendar::open(&registry, "system-task-list", Kind::TaskList, None).unwrap();
        calendar.import("BEGIN:VTODO\r\nUID:task\r\nDTSTART:20261030T120000Z\r\nDUE:20261030T130000Z\r\nRRULE:FREQ=DAILY;COUNT=2\r\nSUMMARY:Task\r\nEND:VTODO\r\n").unwrap();
        let before = calendar.get("task", None).unwrap();
        calendar.complete("task").unwrap();
        let after = calendar.get("task", None).unwrap();
        assert!(!after.is_done());
        assert_ne!(before.due, after.due);
        calendar.complete("task").unwrap();
        assert!(calendar.get("task", None).unwrap().is_done());
    }
    #[test]
    fn split_series_preserves_number_of_occurrences() {
        let calendar = calendar();
        calendar.import("BEGIN:VEVENT\r\nUID:split\r\nDTSTART:20261030T120000Z\r\nDTEND:20261030T130000Z\r\nRRULE:FREQ=DAILY;COUNT=3\r\nSUMMARY:Before\r\nEND:VEVENT\r\n").unwrap();
        calendar
            .modify(
                "split",
                Some("20261031T120000Z"),
                Scope::ThisAndFollowing,
                &Draft { summary: Some("After".into()), ..Default::default() },
            )
            .unwrap();
        let events = calendar.instances(1793300000, 1793700000);
        assert_eq!(events.len(), 3);
        assert_eq!(events.iter().filter(|e| e.summary == "After").count(), 2);
    }
    #[test]
    fn deleting_following_occurrences_keeps_earlier_data() {
        let calendar = calendar();
        calendar.import("BEGIN:VEVENT\r\nUID:cut\r\nDTSTART:20261030T120000Z\r\nDTEND:20261030T130000Z\r\nRRULE:FREQ=DAILY;COUNT=3\r\nSUMMARY:Before\r\nEND:VEVENT\r\n").unwrap();
        calendar.remove("cut", Some("20261031T120000Z"), Scope::ThisAndFollowing).unwrap();
        assert_eq!(calendar.instances(1793300000, 1793700000).len(), 1);
    }

    #[test]
    fn utc_exception_replaces_same_occurrence_in_local_zone() {
        let calendar = calendar();
        calendar.import("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:mixed-zone\r\nDTSTART;TZID=America/Chicago:20261030T090000\r\nDTEND;TZID=America/Chicago:20261030T100000\r\nRRULE:FREQ=DAILY;COUNT=2\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:mixed-zone\r\nRECURRENCE-ID:20261030T140000Z\r\nDTSTART:20261030T140000Z\r\nDTEND:20261030T150000Z\r\nSTATUS:CANCELLED\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n").unwrap();
        assert_eq!(calendar.instances(1793300000, 1793700000).len(), 1);
    }
}

fn read_free_busy(payload: &str, email: &str) -> Result<Vec<(i64, i64)>, Error> {
    let root = unsafe { Owned::new(sys::i_cal_component_new_from_string(cstring(payload).as_ptr())) }
        .ok_or_else(|| error("Invalid availability data"))?;
    let child = if unsafe { sys::i_cal_component_isa(root.ptr()) } == sys::I_CAL_VFREEBUSY_COMPONENT {
        None
    } else {
        Some(
            unsafe { Owned::new(sys::i_cal_component_get_first_component(root.ptr(), sys::I_CAL_VFREEBUSY_COMPONENT)) }
                .ok_or_else(|| error("Scheduling response has no VFREEBUSY component"))?,
        )
    };
    let component = child.as_ref().map(|c| c.ptr()).unwrap_or(root.ptr());
    let (who, periods) = unsafe { ical::busy_periods(component) };
    if !who.eq_ignore_ascii_case(email) {
        return Err(error("Availability data did not match the requested attendee"));
    }
    Ok(periods)
}
#[cfg(test)]
mod availability_tests {
    #[test]
    fn parses_periods_and_rejects_wrong_attendee() {
        let payload = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VFREEBUSY\r\nATTENDEE:mailto:friend@example.test\r\nFREEBUSY:20261030T120000Z/20261030T130000Z\r\nFREEBUSY;FBTYPE=FREE:20261030T130000Z/20261030T140000Z\r\nEND:VFREEBUSY\r\nEND:VCALENDAR\r\n";
        let periods = super::read_free_busy(payload, "friend@example.test").unwrap();
        assert_eq!(periods.len(), 1);
        assert_eq!(periods[0].1 - periods[0].0, 3600);
        assert!(super::read_free_busy(payload, "other@example.test").is_err());
    }
}
