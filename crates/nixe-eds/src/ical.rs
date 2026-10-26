//! Events and tasks as plain Rust values, read from and written to libical components.

use crate::{Owned, borrowed, cstring, owned};
use ical_glib_sys as ical;
use std::ffi::c_int;

/// A date or a moment.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum When {
    /// A whole day, with no time zone (all-day events, due dates without a time).
    Date { year: i32, month: u32, day: u32 },
    /// Seconds since the Unix epoch.
    Time(i64),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ItemKind {
    Event,
    Task,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Status {
    #[default]
    None,
    Tentative,
    Confirmed,
    Cancelled,
    NeedsAction,
    InProcess,
    Completed,
}

impl Status {
    fn from_raw(raw: ical::ICalPropertyStatus) -> Self {
        match raw {
            ical::I_CAL_STATUS_TENTATIVE => Self::Tentative,
            ical::I_CAL_STATUS_CONFIRMED => Self::Confirmed,
            ical::I_CAL_STATUS_CANCELLED => Self::Cancelled,
            ical::I_CAL_STATUS_NEEDSACTION => Self::NeedsAction,
            ical::I_CAL_STATUS_INPROCESS => Self::InProcess,
            ical::I_CAL_STATUS_COMPLETED => Self::Completed,
            _ => Self::None,
        }
    }

    fn raw(self) -> ical::ICalPropertyStatus {
        match self {
            Self::None => ical::I_CAL_STATUS_NONE,
            Self::Tentative => ical::I_CAL_STATUS_TENTATIVE,
            Self::Confirmed => ical::I_CAL_STATUS_CONFIRMED,
            Self::Cancelled => ical::I_CAL_STATUS_CANCELLED,
            Self::NeedsAction => ical::I_CAL_STATUS_NEEDSACTION,
            Self::InProcess => ical::I_CAL_STATUS_INPROCESS,
            Self::Completed => ical::I_CAL_STATUS_COMPLETED,
        }
    }
}

/// An attendee's answer to an invitation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Response {
    #[default]
    NeedsAction,
    Accepted,
    Declined,
    Tentative,
    Other,
}

impl Response {
    fn from_raw(raw: ical::ICalParameterPartstat) -> Self {
        match raw {
            ical::I_CAL_PARTSTAT_ACCEPTED => Self::Accepted,
            ical::I_CAL_PARTSTAT_DECLINED => Self::Declined,
            ical::I_CAL_PARTSTAT_TENTATIVE => Self::Tentative,
            ical::I_CAL_PARTSTAT_NEEDSACTION => Self::NeedsAction,
            _ => Self::Other,
        }
    }

    pub(crate) fn raw(self) -> ical::ICalParameterPartstat {
        match self {
            Self::Accepted => ical::I_CAL_PARTSTAT_ACCEPTED,
            Self::Declined => ical::I_CAL_PARTSTAT_DECLINED,
            Self::Tentative => ical::I_CAL_PARTSTAT_TENTATIVE,
            Self::NeedsAction | Self::Other => ical::I_CAL_PARTSTAT_NEEDSACTION,
        }
    }
}

/// Someone invited to an event, or its organizer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Attendee {
    /// The address, without `mailto:`.
    pub email: String,
    pub name: String,
    pub response: Response,
    /// The organizer's chair, or someone only told about it.
    pub chair: bool,
    pub optional: bool,
}

impl Attendee {
    pub fn label(&self) -> &str {
        if self.name.is_empty() { &self.email } else { &self.name }
    }
}

/// An event (or one occurrence of a repeating event) or a task.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub kind: ItemKind,
    pub uid: String,
    /// Which occurrence, for repeating items (the RECURRENCE-ID as text).
    pub rid: Option<String>,
    pub summary: String,
    pub location: String,
    pub description: String,
    /// For an occurrence, its own start and end.
    pub start: Option<When>,
    pub end: Option<When>,
    pub due: Option<When>,
    pub status: Status,
    pub completed: Option<When>,
    pub priority: Option<u8>,
    pub url: Option<String>,
    /// Tags (CATEGORIES), in order, without repeats.
    pub categories: Vec<String>,
    /// The parent task, for subtasks.
    pub parent: Option<String>,
    pub repeats: bool,
    /// The repeat rule (an RRULE value such as `FREQ=WEEKLY;BYDAY=MO`), on the
    /// series and on each of its occurrences.
    pub rule: Option<String>,
    pub has_alarms: bool,
    /// Alerts, in minutes before the start (negative: after it; "9:00 on the
    /// day" of an all-day event is -540); a task's count from its due time.
    /// Absolute alerts are not listed.
    pub alarms: Vec<i64>,
    pub organizer: Option<Attendee>,
    pub attendees: Vec<Attendee>,
}

impl Item {
    pub fn all_day(&self) -> bool {
        matches!(self.start, Some(When::Date { .. }))
    }

    pub fn is_done(&self) -> bool {
        self.status == Status::Completed || self.completed.is_some()
    }
}

/// Changes to apply to an item. `None` leaves a field as it is; `Some(None)`
/// clears it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Draft {
    pub summary: Option<String>,
    pub location: Option<String>,
    pub description: Option<String>,
    pub start: Option<Option<When>>,
    pub end: Option<Option<When>>,
    pub due: Option<Option<When>>,
    /// The IANA time zone times are written in (keeps repeating events at the
    /// same wall-clock time across daylight saving). Defaults to [`local_zone`].
    pub zone: Option<String>,
    /// An RRULE value, e.g. `FREQ=WEEKLY;BYDAY=MO`.
    pub repeat: Option<Option<String>>,
    pub status: Option<Status>,
    /// Marks a task done (now) or not done.
    pub done: Option<bool>,
    pub priority: Option<Option<u8>>,
    pub url: Option<Option<String>>,
    /// Replaces every tag (CATEGORIES); empty removes them.
    pub categories: Option<Vec<String>>,
    pub parent: Option<Option<String>>,
    /// Replaces every alert; see [`Item::alarms`].
    pub alarms: Option<Vec<i64>>,
}

/// The system's IANA time zone name (`TZ`, else `/etc/localtime`), or `UTC`.
pub fn local_zone() -> String {
    if let Ok(tz) = std::env::var("TZ") {
        let tz = tz.trim_start_matches(':');
        if !tz.is_empty() && !tz.starts_with('/') {
            return tz.to_string();
        }
    }
    std::fs::read_link("/etc/localtime")
        .ok()
        .and_then(|p| p.to_str().and_then(|p| p.split_once("zoneinfo/").map(|(_, z)| z.to_string())))
        .unwrap_or_else(|| "UTC".into())
}

/// libical's built-in zone for an IANA name, or UTC. Borrowed: libical keeps it.
pub(crate) fn zone(name: &str) -> *mut ical::ICalTimezone {
    let c = cstring(name);
    let zone = unsafe { ical::i_cal_timezone_get_builtin_timezone(c.as_ptr()) };
    if zone.is_null() { unsafe { ical::i_cal_timezone_get_utc_timezone() } } else { zone }
}

/// How times without a resolved zone are read: a calendar's own zones (from an
/// imported file, say) through `lookup`, else `fallback` for floating times.
pub(crate) struct Zones<'a> {
    pub fallback: *mut ical::ICalTimezone,
    pub lookup: Option<&'a dyn Fn(&str) -> *mut ical::ICalTimezone>,
}

impl Zones<'_> {
    pub(crate) fn fixed(fallback: *mut ical::ICalTimezone) -> Self {
        Self { fallback, lookup: None }
    }
}

/// The zone a time is in: its own, else the libical built-in zone its TZID
/// names (a lone component has no VTIMEZONE to resolve against), else the
/// calendar's zone of that name. Null for floating times and dates.
///
/// # Safety
/// `tt` must be a valid `ICalTime`.
pub(crate) unsafe fn zone_of(tt: *mut ical::ICalTime) -> *mut ical::ICalTimezone {
    unsafe { zone_in(tt, None, None) }
}

/// `param` is the TZID parameter of the property the time came from: an
/// unresolved time doesn't carry its TZID itself.
unsafe fn zone_in(
    tt: *mut ical::ICalTime,
    param: Option<&str>,
    lookup: Option<&dyn Fn(&str) -> *mut ical::ICalTimezone>,
) -> *mut ical::ICalTimezone {
    unsafe {
        let own = ical::i_cal_time_get_timezone(tt);
        if !own.is_null() {
            return own;
        }
        let Some(name) = borrowed(ical::i_cal_time_get_tzid(tt)).or_else(|| param.map(str::to_string)) else {
            return std::ptr::null_mut();
        };
        let tzid = cstring(&name);
        let found = ical::i_cal_timezone_get_builtin_timezone_from_tzid(tzid.as_ptr());
        if !found.is_null() {
            return found;
        }
        let location = cstring(name.rsplit_once("/freeassociation.sourceforge.net/").map_or(name.as_str(), |(_, l)| l));
        let builtin = ical::i_cal_timezone_get_builtin_timezone(location.as_ptr());
        if !builtin.is_null() {
            return builtin;
        }
        lookup.map_or(std::ptr::null_mut(), |find| find(&name))
    }
}

pub(crate) fn is_utc(zone: *mut ical::ICalTimezone) -> bool {
    zone == unsafe { ical::i_cal_timezone_get_utc_timezone() }
}

/// # Safety
/// `tt` must be null or a valid `ICalTime`; `fallback` a valid zone.
pub(crate) unsafe fn when(tt: *mut ical::ICalTime, zones: &Zones) -> Option<When> {
    unsafe { when_in(tt, None, zones) }
}

unsafe fn when_in(tt: *mut ical::ICalTime, param: Option<&str>, zones: &Zones) -> Option<When> {
    unsafe {
        if tt.is_null() || ical::i_cal_time_is_null_time(tt) != 0 || ical::i_cal_time_is_valid_time(tt) == 0 {
            return None;
        }
        if ical::i_cal_time_is_date(tt) != 0 {
            return Some(When::Date {
                year: ical::i_cal_time_get_year(tt),
                month: ical::i_cal_time_get_month(tt) as u32,
                day: ical::i_cal_time_get_day(tt) as u32,
            });
        }
        let own = zone_in(tt, param, zones.lookup);
        let zone = if own.is_null() { zones.fallback } else { own };
        Some(When::Time(ical::i_cal_time_as_timet_with_zone(tt, zone) as i64))
    }
}

/// A new `ICalTime` the caller owns.
pub(crate) fn time(when: When, zone: *mut ical::ICalTimezone) -> Owned<ical::ICalTime> {
    unsafe {
        let tt = match when {
            When::Date { year, month, day } => {
                let tt = ical::i_cal_time_new_null_time();
                ical::i_cal_time_set_date(tt, year, month as c_int, day as c_int);
                ical::i_cal_time_set_is_date(tt, 1);
                tt
            }
            When::Time(secs) => ical::i_cal_time_new_from_timet_with_zone(secs as libc::time_t, 0, zone),
        };
        Owned::new(tt).expect("libical allocates times")
    }
}

/// Reads one component. `instance` is the occurrence's start and end when the
/// component came from recurrence expansion.
///
/// # Safety
/// `comp` must be a valid VEVENT or VTODO; the instance times valid or null.
pub(crate) unsafe fn item(
    comp: *mut ical::ICalComponent,
    instance: Option<(*mut ical::ICalTime, *mut ical::ICalTime)>,
    zones: &Zones,
) -> Item {
    unsafe {
        let kind = if ical::i_cal_component_isa(comp) == ical::I_CAL_VTODO_COMPONENT { ItemKind::Task } else { ItemKind::Event };
        let text = |p: *const std::ffi::c_char| borrowed(p).unwrap_or_default();
        let owned_time = |tt: *mut ical::ICalTime, kind: ical::ICalPropertyKind| {
            let param = tzid_param(comp, kind).or_else(|| tzid_param(comp, ical::I_CAL_DTSTART_PROPERTY));
            let value = when_in(tt, param.as_deref(), zones);
            if !tt.is_null() {
                gobject_sys::g_object_unref(tt.cast());
            }
            value
        };
        let repeats = ical::i_cal_component_count_properties(comp, ical::I_CAL_RRULE_PROPERTY) > 0;
        let rid_time = ical::i_cal_component_get_recurrenceid(comp);
        let mut rid = None;
        if !rid_time.is_null() {
            if ical::i_cal_time_is_null_time(rid_time) == 0 {
                rid = owned(ical::i_cal_time_as_ical_string(rid_time));
            }
            gobject_sys::g_object_unref(rid_time.cast());
        }
        let (start, end) = match instance {
            Some((start, end)) => {
                if rid.is_none() && repeats && !start.is_null() {
                    rid = owned(ical::i_cal_time_as_ical_string(start));
                }
                (when(start, zones), when(end, zones))
            }
            None => (
                owned_time(ical::i_cal_component_get_dtstart(comp), ical::I_CAL_DTSTART_PROPERTY),
                owned_time(ical::i_cal_component_get_dtend(comp), ical::I_CAL_DTEND_PROPERTY),
            ),
        };
        let due = if kind == ItemKind::Task { owned_time(ical::i_cal_component_get_due(comp), ical::I_CAL_DUE_PROPERTY) } else { None };
        let completed = property(comp, ical::I_CAL_COMPLETED_PROPERTY).and_then(|p| {
            let tt = ical::i_cal_property_get_completed(p.ptr());
            owned_time(tt, ical::I_CAL_COMPLETED_PROPERTY)
        });
        let repeats = repeats || rid.is_some();
        let priority = property(comp, ical::I_CAL_PRIORITY_PROPERTY)
            .map(|p| ical::i_cal_property_get_priority(p.ptr()))
            .and_then(|p| u8::try_from(p).ok())
            .filter(|p| *p > 0);
        Item {
            kind,
            uid: text(ical::i_cal_component_get_uid(comp)),
            rid,
            summary: text(ical::i_cal_component_get_summary(comp)),
            location: text(ical::i_cal_component_get_location(comp)),
            description: text(ical::i_cal_component_get_description(comp)),
            start,
            end,
            due,
            status: Status::from_raw(ical::i_cal_component_get_status(comp)),
            completed,
            priority,
            url: property_text(comp, ical::I_CAL_URL_PROPERTY),
            categories: categories(comp),
            parent: property_text(comp, ical::I_CAL_RELATEDTO_PROPERTY),
            repeats,
            rule: property_text(comp, ical::I_CAL_RRULE_PROPERTY),
            has_alarms: ical::i_cal_component_count_components(comp, ical::I_CAL_VALARM_COMPONENT) > 0,
            alarms: alarms(comp),
            organizer: property(comp, ical::I_CAL_ORGANIZER_PROPERTY).map(|p| attendee(p.ptr(), true)),
            attendees: attendees(comp),
        }
    }
}

fn address(value: Option<String>) -> String {
    let value = value.unwrap_or_default();
    match value.get(..7) {
        Some(prefix) if prefix.eq_ignore_ascii_case("mailto:") => value[7..].to_string(),
        _ => value,
    }
}

unsafe fn attendee(prop: *mut ical::ICalProperty, organizer: bool) -> Attendee {
    unsafe {
        let email = address(borrowed(if organizer {
            ical::i_cal_property_get_organizer(prop)
        } else {
            ical::i_cal_property_get_attendee(prop)
        }));
        let param = |kind| Owned::new(ical::i_cal_property_get_first_parameter(prop, kind));
        let name = param(ical::I_CAL_CN_PARAMETER).and_then(|p| borrowed(ical::i_cal_parameter_get_cn(p.ptr()))).unwrap_or_default();
        let response = param(ical::I_CAL_PARTSTAT_PARAMETER)
            .map(|p| Response::from_raw(ical::i_cal_parameter_get_partstat(p.ptr())))
            .unwrap_or_default();
        let role = param(ical::I_CAL_ROLE_PARAMETER).map(|p| ical::i_cal_parameter_get_role(p.ptr()));
        Attendee {
            email,
            name,
            response,
            chair: role == Some(ical::I_CAL_ROLE_CHAIR),
            optional: role == Some(ical::I_CAL_ROLE_OPTPARTICIPANT) || role == Some(ical::I_CAL_ROLE_NONPARTICIPANT),
        }
    }
}

/// Every CATEGORIES value; one property may list several, comma-separated.
unsafe fn categories(comp: *mut ical::ICalComponent) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    unsafe {
        let mut next = Owned::new(ical::i_cal_component_get_first_property(comp, ical::I_CAL_CATEGORIES_PROPERTY));
        while let Some(prop) = next {
            for tag in borrowed(ical::i_cal_property_get_categories(prop.ptr())).unwrap_or_default().split(',') {
                let tag = tag.trim();
                if !tag.is_empty() && !out.iter().any(|t| t.eq_ignore_ascii_case(tag)) {
                    out.push(tag.to_string());
                }
            }
            next = Owned::new(ical::i_cal_component_get_next_property(comp, ical::I_CAL_CATEGORIES_PROPERTY));
        }
    }
    out
}

unsafe fn attendees(comp: *mut ical::ICalComponent) -> Vec<Attendee> {
    let mut out = Vec::new();
    unsafe {
        let mut next = Owned::new(ical::i_cal_component_get_first_property(comp, ical::I_CAL_ATTENDEE_PROPERTY));
        while let Some(prop) = next {
            out.push(attendee(prop.ptr(), false));
            next = Owned::new(ical::i_cal_component_get_next_property(comp, ical::I_CAL_ATTENDEE_PROPERTY));
        }
    }
    out
}

/// Sets this user's answer on every attendee line with one of `emails`.
/// Returns whether any line matched.
///
/// # Safety
/// `comp` must be a valid component the caller owns.
pub(crate) unsafe fn respond(comp: *mut ical::ICalComponent, emails: &[String], response: Response) -> bool {
    let mut matched = false;
    unsafe {
        let mut next = Owned::new(ical::i_cal_component_get_first_property(comp, ical::I_CAL_ATTENDEE_PROPERTY));
        while let Some(prop) = next {
            let email = address(borrowed(ical::i_cal_property_get_attendee(prop.ptr())));
            if emails.iter().any(|e| e.eq_ignore_ascii_case(&email)) {
                ical::i_cal_property_remove_parameter_by_kind(prop.ptr(), ical::I_CAL_PARTSTAT_PARAMETER);
                if let Some(param) = Owned::new(ical::i_cal_parameter_new_partstat(response.raw())) {
                    ical::i_cal_property_add_parameter(prop.ptr(), param.ptr());
                }
                matched = true;
            }
            next = Owned::new(ical::i_cal_component_get_next_property(comp, ical::I_CAL_ATTENDEE_PROPERTY));
        }
    }
    matched
}

unsafe fn alarms(comp: *mut ical::ICalComponent) -> Vec<i64> {
    let mut out = Vec::new();
    unsafe {
        let mut next = Owned::new(ical::i_cal_component_get_first_component(comp, ical::I_CAL_VALARM_COMPONENT));
        while let Some(alarm) = next {
            if let Some(prop) = property(alarm.ptr(), ical::I_CAL_TRIGGER_PROPERTY)
                && let Some(trigger) = Owned::new(ical::i_cal_property_get_trigger(prop.ptr()))
                && let Some(duration) = Owned::new(ical::i_cal_trigger_get_duration(trigger.ptr()))
                && ical::i_cal_duration_is_bad_duration(duration.ptr()) == 0
            {
                let seconds = ical::i_cal_duration_as_seconds(duration.ptr()) as i64;
                // An absolute trigger has a null duration and a time; skip those.
                let absolute = Owned::new(ical::i_cal_trigger_get_time(trigger.ptr()))
                    .is_some_and(|t| ical::i_cal_time_is_null_time(t.ptr()) == 0);
                if !absolute {
                    out.push(-seconds / 60);
                }
            }
            next = Owned::new(ical::i_cal_component_get_next_component(comp, ical::I_CAL_VALARM_COMPONENT));
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

unsafe fn set_alarms(comp: *mut ical::ICalComponent, minutes: &[i64]) {
    unsafe {
        while let Some(alarm) = Owned::new(ical::i_cal_component_get_first_component(comp, ical::I_CAL_VALARM_COMPONENT)) {
            ical::i_cal_component_remove_component(comp, alarm.ptr());
        }
        for before in minutes {
            let Some(alarm) = Owned::new(ical::i_cal_component_new_valarm()) else { continue };
            add(alarm.ptr(), ical::i_cal_property_new_action(ical::I_CAL_ACTION_DISPLAY));
            if let Some(trigger) = Owned::new(ical::i_cal_trigger_new_from_seconds((-before * 60) as c_int))
                && let Some(prop) = Owned::new(ical::i_cal_property_new_trigger(trigger.ptr()))
            {
                // A task usually has a due time and no start: its alerts count from the due time.
                if ical::i_cal_component_isa(comp) == ical::I_CAL_VTODO_COMPONENT
                    && let Some(related) = Owned::new(ical::i_cal_parameter_new_related(ical::I_CAL_RELATED_END))
                {
                    ical::i_cal_property_add_parameter(prop.ptr(), related.ptr());
                }
                ical::i_cal_component_add_property(alarm.ptr(), prop.ptr());
            }
            add(alarm.ptr(), ical::i_cal_property_new_description(cstring("Reminder").as_ptr()));
            ical::i_cal_component_add_component(comp, alarm.ptr());
        }
    }
}

/// The TZID parameter of a component's first property of `kind`.
pub(crate) unsafe fn tzid_param(comp: *mut ical::ICalComponent, kind: ical::ICalPropertyKind) -> Option<String> {
    unsafe {
        let prop = property(comp, kind)?;
        let param = Owned::new(ical::i_cal_property_get_first_parameter(prop.ptr(), ical::I_CAL_TZID_PARAMETER))?;
        borrowed(ical::i_cal_parameter_get_tzid(param.ptr()))
    }
}

unsafe fn property(comp: *mut ical::ICalComponent, kind: ical::ICalPropertyKind) -> Option<Owned<ical::ICalProperty>> {
    unsafe { Owned::new(ical::i_cal_component_get_first_property(comp, kind)) }
}

unsafe fn property_text(comp: *mut ical::ICalComponent, kind: ical::ICalPropertyKind) -> Option<String> {
    unsafe {
        let prop = property(comp, kind)?;
        owned(ical::i_cal_property_get_value_as_string(prop.ptr())).filter(|v| !v.is_empty())
    }
}

unsafe fn remove_all(comp: *mut ical::ICalComponent, kind: ical::ICalPropertyKind) {
    unsafe {
        while let Some(prop) = property(comp, kind) {
            ical::i_cal_component_remove_property(comp, prop.ptr());
        }
    }
}

unsafe fn add(comp: *mut ical::ICalComponent, prop: *mut ical::ICalProperty) {
    unsafe {
        if let Some(prop) = Owned::new(prop) {
            ical::i_cal_component_add_property(comp, prop.ptr());
        }
    }
}

/// A new empty VEVENT or VTODO the caller owns.
pub(crate) fn new_component(kind: ItemKind) -> Owned<ical::ICalComponent> {
    let ptr = unsafe {
        match kind {
            ItemKind::Event => ical::i_cal_component_new_vevent(),
            ItemKind::Task => ical::i_cal_component_new_vtodo(),
        }
    };
    unsafe { Owned::new(ptr) }.expect("libical allocates components")
}

/// Writes a draft into a component. Returns the zone used for times, which the
/// calendar must know about before the component is saved.
///
/// # Safety
/// `comp` must be a valid VEVENT or VTODO the caller owns.
pub(crate) unsafe fn apply(comp: *mut ical::ICalComponent, draft: &Draft) -> *mut ical::ICalTimezone {
    let zone = zone(draft.zone.as_deref().unwrap_or(&local_zone()));
    unsafe {
        if let Some(text) = &draft.summary {
            ical::i_cal_component_set_summary(comp, cstring(text).as_ptr());
        }
        if let Some(text) = &draft.location {
            remove_all(comp, ical::I_CAL_LOCATION_PROPERTY);
            if !text.is_empty() {
                ical::i_cal_component_set_location(comp, cstring(text).as_ptr());
            }
        }
        if let Some(text) = &draft.description {
            remove_all(comp, ical::I_CAL_DESCRIPTION_PROPERTY);
            if !text.is_empty() {
                ical::i_cal_component_set_description(comp, cstring(text).as_ptr());
            }
        }
        for (value, kind, set) in [
            (draft.start, ical::I_CAL_DTSTART_PROPERTY, ical::i_cal_component_set_dtstart as unsafe extern "C" fn(_, _)),
            (draft.end, ical::I_CAL_DTEND_PROPERTY, ical::i_cal_component_set_dtend),
            (draft.due, ical::I_CAL_DUE_PROPERTY, ical::i_cal_component_set_due),
        ] {
            if let Some(value) = value {
                remove_all(comp, kind);
                if let Some(value) = value {
                    set(comp, time(value, zone).ptr());
                }
            }
        }
        if let Some(rule) = &draft.repeat {
            remove_all(comp, ical::I_CAL_RRULE_PROPERTY);
            if let Some(rule) = rule.as_deref().filter(|r| !r.is_empty()) {
                let rule = cstring(rule.trim_start_matches("RRULE:"));
                if let Some(recur) = Owned::new(ical::i_cal_recurrence_new_from_string(rule.as_ptr())) {
                    add(comp, ical::i_cal_property_new_rrule(recur.ptr()));
                }
            }
        }
        if let Some(status) = draft.status {
            ical::i_cal_component_set_status(comp, status.raw());
        }
        if let Some(done) = draft.done {
            remove_all(comp, ical::I_CAL_COMPLETED_PROPERTY);
            remove_all(comp, ical::I_CAL_PERCENTCOMPLETE_PROPERTY);
            if done {
                let now = time(When::Time(now()), ical::i_cal_timezone_get_utc_timezone());
                add(comp, ical::i_cal_property_new_completed(now.ptr()));
                add(comp, ical::i_cal_property_new_percentcomplete(100));
                ical::i_cal_component_set_status(comp, ical::I_CAL_STATUS_COMPLETED);
            } else {
                ical::i_cal_component_set_status(comp, ical::I_CAL_STATUS_NEEDSACTION);
            }
        }
        if let Some(priority) = draft.priority {
            remove_all(comp, ical::I_CAL_PRIORITY_PROPERTY);
            if let Some(p) = priority.filter(|p| *p > 0) {
                add(comp, ical::i_cal_property_new_priority(p.min(9) as c_int));
            }
        }
        if let Some(url) = &draft.url {
            remove_all(comp, ical::I_CAL_URL_PROPERTY);
            if let Some(url) = url.as_deref().filter(|u| !u.is_empty()) {
                add(comp, ical::i_cal_property_new_url(cstring(url).as_ptr()));
            }
        }
        if let Some(tags) = &draft.categories {
            remove_all(comp, ical::I_CAL_CATEGORIES_PROPERTY);
            // One property per tag, so a tag never needs its commas escaped.
            for tag in tags.iter().map(|t| t.replace(',', " ")).filter(|t| !t.trim().is_empty()) {
                add(comp, ical::i_cal_property_new_categories(cstring(tag.trim()).as_ptr()));
            }
        }
        if let Some(minutes) = &draft.alarms {
            set_alarms(comp, minutes);
        }
        if let Some(parent) = &draft.parent {
            remove_all(comp, ical::I_CAL_RELATEDTO_PROPERTY);
            if let Some(parent) = parent.as_deref().filter(|p| !p.is_empty()) {
                add(comp, ical::i_cal_property_new_relatedto(cstring(parent).as_ptr()));
            }
        }
        // A repeating task repeats from its start; keep the start on the due date
        // so completing it moves both to the next occurrence.
        if ical::i_cal_component_isa(comp) == ical::I_CAL_VTODO_COMPONENT
            && ical::i_cal_component_count_properties(comp, ical::I_CAL_RRULE_PROPERTY) > 0
            && (draft.due.is_some() || draft.repeat.is_some())
            && let Some(due) = Owned::new(ical::i_cal_component_get_due(comp))
            && ical::i_cal_time_is_null_time(due.ptr()) == 0
        {
            remove_all(comp, ical::I_CAL_DTSTART_PROPERTY);
            ical::i_cal_component_set_dtstart(comp, due.ptr());
        }
    }
    zone
}

/// Turns a fetched master into the occurrence starting at `rid`, the way EDS
/// expects a single-occurrence change: RECURRENCE-ID and DTSTART set to the
/// occurrence, DTEND moved by the same length, and (for one occurrence, not
/// "this and following") the repeat rules dropped.
/// A component without repeat rules (already an occurrence) is left alone.
///
/// # Safety
/// `comp` must be a valid VEVENT or VTODO the caller owns.
pub(crate) unsafe fn detach(comp: *mut ical::ICalComponent, rid: &str, keep_rules: bool) {
    unsafe {
        // EDS hands back the master (with RECURRENCE-ID already filled in) for an
        // occurrence that has not been changed before; only a master has rules.
        let rules = [ical::I_CAL_RRULE_PROPERTY, ical::I_CAL_RDATE_PROPERTY];
        if rules.iter().all(|kind| ical::i_cal_component_count_properties(comp, *kind) == 0) {
            return;
        }
        let Some(start) = Owned::new(ical::i_cal_component_get_dtstart(comp)) else { return };
        let zone = zone_of(start.ptr());
        let rid = cstring(rid);
        let Some(occurrence) = Owned::new(ical::i_cal_time_new_from_string(rid.as_ptr())) else { return };
        // RECURRENCE-ID keeps EDS's own text for the occurrence (EDS matches on
        // it); DTSTART is written in the master's zone.
        let begins = if zone.is_null() || is_utc(zone) || ical::i_cal_time_is_date(start.ptr()) != 0 {
            Owned::new(ical::i_cal_time_clone(occurrence.ptr())).expect("libical allocates times")
        } else if ical::i_cal_time_is_utc(occurrence.ptr()) != 0 {
            Owned::new(ical::i_cal_time_convert_to_zone(occurrence.ptr(), zone)).expect("libical allocates times")
        } else {
            let local = Owned::new(ical::i_cal_time_clone(occurrence.ptr())).expect("libical allocates times");
            ical::i_cal_time_set_timezone(local.ptr(), zone);
            ical::i_cal_time_set_timezone(occurrence.ptr(), zone);
            local
        };
        if let Some(end) = Owned::new(ical::i_cal_component_get_dtend(comp))
            && ical::i_cal_time_is_null_time(end.ptr()) == 0
        {
            let length = ical::i_cal_time_as_timet_with_zone(end.ptr(), zone) - ical::i_cal_time_as_timet_with_zone(start.ptr(), zone);
            let moved = Owned::new(ical::i_cal_time_clone(begins.ptr())).expect("libical allocates times");
            ical::i_cal_time_adjust(moved.ptr(), 0, 0, 0, length as c_int);
            remove_all(comp, ical::I_CAL_DTEND_PROPERTY);
            ical::i_cal_component_set_dtend(comp, moved.ptr());
        }
        remove_all(comp, ical::I_CAL_DTSTART_PROPERTY);
        remove_all(comp, ical::I_CAL_RECURRENCEID_PROPERTY);
        ical::i_cal_component_set_dtstart(comp, begins.ptr());
        ical::i_cal_component_set_recurrenceid(comp, occurrence.ptr());
        if !keep_rules {
            for kind in [ical::I_CAL_RRULE_PROPERTY, ical::I_CAL_RDATE_PROPERTY, ical::I_CAL_EXDATE_PROPERTY, ical::I_CAL_EXRULE_PROPERTY] {
                remove_all(comp, kind);
            }
        }
    }
}

pub(crate) fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(ics: &str) -> Item {
        let c = cstring(ics);
        let comp = unsafe { Owned::new(ical::i_cal_component_new_from_string(c.as_ptr())) }.unwrap();
        unsafe { item(comp.ptr(), None, &Zones::fixed(zone("UTC"))) }
    }

    #[test]
    fn reads_a_timed_event_in_its_zone() {
        let item = read(
            "BEGIN:VEVENT\r\nUID:a\r\nSUMMARY:Dentist\r\nLOCATION:Main St\r\n\
             DTSTART;TZID=America/New_York:20261102T100000\r\nDTEND;TZID=America/New_York:20261102T110000\r\n\
             END:VEVENT\r\n",
        );
        assert_eq!(item.kind, ItemKind::Event);
        assert_eq!(item.summary, "Dentist");
        assert_eq!(item.location, "Main St");
        // 2026-11-02 10:00 EST (after the switch back from daylight saving) is 15:00 UTC.
        assert_eq!(item.start, Some(When::Time(1_793_631_600)));
        assert_eq!(item.end, Some(When::Time(1_793_635_200)));
        assert!(!item.repeats);
    }

    #[test]
    fn reads_an_all_day_event() {
        let item = read("BEGIN:VEVENT\r\nUID:b\r\nDTSTART;VALUE=DATE:20261224\r\nDTEND;VALUE=DATE:20261225\r\nEND:VEVENT\r\n");
        assert!(item.all_day());
        assert_eq!(item.start, Some(When::Date { year: 2026, month: 12, day: 24 }));
    }

    #[test]
    fn reads_a_task() {
        let item = read(
            "BEGIN:VTODO\r\nUID:t\r\nSUMMARY:Call Sam\r\nDUE;VALUE=DATE:20261030\r\nPRIORITY:1\r\n\
             STATUS:COMPLETED\r\nCOMPLETED:20261021T120000Z\r\nRELATED-TO:parent\r\nURL:https://example.com\r\nCATEGORIES:Work,errands\r\nCATEGORIES:work\r\nEND:VTODO\r\n",
        );
        assert_eq!(item.kind, ItemKind::Task);
        assert_eq!(item.due, Some(When::Date { year: 2026, month: 10, day: 30 }));
        assert_eq!(item.priority, Some(1));
        assert!(item.is_done());
        assert_eq!(item.parent.as_deref(), Some("parent"));
        assert_eq!(item.url.as_deref(), Some("https://example.com"));
        assert_eq!(item.categories, ["Work", "errands"]);
    }

    #[test]
    fn draft_round_trips() {
        let comp = new_component(ItemKind::Task);
        let draft = Draft {
            summary: Some("Pay rent".into()),
            due: Some(Some(When::Date { year: 2026, month: 11, day: 1 })),
            repeat: Some(Some("FREQ=MONTHLY".into())),
            priority: Some(Some(5)),
            url: Some(Some("https://bank.example".into())),
            categories: Some(vec!["Home".into(), "Bills".into()]),
            parent: Some(Some("p1".into())),
            ..Draft::default()
        };
        unsafe {
            apply(comp.ptr(), &draft);
            ical::i_cal_component_set_uid(comp.ptr(), cstring("x").as_ptr());
            let item = item(comp.ptr(), None, &Zones::fixed(zone("UTC")));
            assert_eq!(item.summary, "Pay rent");
            assert_eq!(item.due, Some(When::Date { year: 2026, month: 11, day: 1 }));
            assert!(item.repeats);
            assert_eq!(item.rule.as_deref(), Some("FREQ=MONTHLY"));
            assert_eq!(item.priority, Some(5));
            assert_eq!(item.parent.as_deref(), Some("p1"));
            assert_eq!(item.categories, ["Home", "Bills"]);

            apply(comp.ptr(), &Draft { done: Some(true), url: Some(None), repeat: Some(None), categories: Some(Vec::new()), ..Draft::default() });
            let item = super::item(comp.ptr(), None, &Zones::fixed(zone("UTC")));
            assert!(item.is_done());
            assert_eq!(item.url, None);
            assert!(item.categories.is_empty());
            assert!(!item.repeats);
        }
    }

    #[test]
    fn timed_draft_keeps_its_zone() {
        let comp = new_component(ItemKind::Event);
        let draft = Draft {
            start: Some(Some(When::Time(1_793_631_600))),
            zone: Some("America/New_York".into()),
            ..Draft::default()
        };
        unsafe {
            apply(comp.ptr(), &draft);
            let ics = owned(ical::i_cal_component_as_ical_string(comp.ptr())).unwrap();
            assert!(ics.contains("America/New_York"), "{ics}");
            assert!(ics.contains("20261102T100000"), "{ics}");
            assert_eq!(item(comp.ptr(), None, &Zones::fixed(zone("UTC"))).start, Some(When::Time(1_793_631_600)));
        }
    }

    #[test]
    fn reads_attendees_and_answers() {
        let c = cstring(
            "BEGIN:VEVENT\r\nUID:i\r\nDTSTART:20261102T150000Z\r\n\
             ORGANIZER;CN=Alex:mailto:alex@example.com\r\n\
             ATTENDEE;CN=Alex;ROLE=CHAIR;PARTSTAT=ACCEPTED:mailto:alex@example.com\r\n\
             ATTENDEE;PARTSTAT=NEEDS-ACTION;ROLE=OPT-PARTICIPANT:MAILTO:Me@Example.com\r\nEND:VEVENT\r\n",
        );
        let comp = unsafe { Owned::new(ical::i_cal_component_new_from_string(c.as_ptr())) }.unwrap();
        unsafe {
            let it = item(comp.ptr(), None, &Zones::fixed(zone("UTC")));
            assert_eq!(it.organizer.as_ref().map(|o| o.label()), Some("Alex"));
            assert_eq!(it.attendees.len(), 2);
            assert!(it.attendees[0].chair && it.attendees[0].response == Response::Accepted);
            assert_eq!((it.attendees[1].email.as_str(), it.attendees[1].optional), ("Me@Example.com", true));
            assert!(respond(comp.ptr(), &["me@example.com".into()], Response::Tentative));
            assert!(!respond(comp.ptr(), &["nobody@example.com".into()], Response::Declined));
            let it = item(comp.ptr(), None, &Zones::fixed(zone("UTC")));
            assert_eq!(it.attendees[1].response, Response::Tentative);
            assert_eq!(it.attendees[0].response, Response::Accepted);
        }
    }

    #[test]
    fn alarms_round_trip() {
        let comp = new_component(ItemKind::Event);
        unsafe {
            apply(comp.ptr(), &Draft { alarms: Some(vec![10, 0, 1440]), ..Draft::default() });
            assert_eq!(item(comp.ptr(), None, &Zones::fixed(zone("UTC"))).alarms, vec![0, 10, 1440]);
            apply(comp.ptr(), &Draft { alarms: Some(vec![-540]), ..Draft::default() });
            let it = item(comp.ptr(), None, &Zones::fixed(zone("UTC")));
            assert_eq!(it.alarms, vec![-540]);
            assert!(it.has_alarms);
            apply(comp.ptr(), &Draft { alarms: Some(Vec::new()), ..Draft::default() });
            assert!(item(comp.ptr(), None, &Zones::fixed(zone("UTC"))).alarms.is_empty());
        }
    }

    #[test]
    fn unknown_zone_falls_back_to_utc() {
        assert!(is_utc(zone("Not/AZone")));
    }
}
