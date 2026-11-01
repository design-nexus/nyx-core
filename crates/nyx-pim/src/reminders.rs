use crate::*;
use std::cell::RefCell;
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct Reminder {
    pub source: String,
    pub item: Item,
    pub starts: i64,
    pub ends: i64,
    pub due: i64,
    key_rid: String,
}
pub struct Reminders {
    registry: Registry,
    sources: RefCell<Vec<glib::SourceId>>,
}
impl Drop for Reminders {
    fn drop(&mut self) {
        for source in self.sources.get_mut().drain(..) {
            source.remove();
        }
    }
}
fn seconds(when: Option<When>) -> Option<i64> {
    match when? {
        When::Time(t) => Some(t),
        date @ When::Date { .. } => {
            let time = crate::ical::time(date, crate::ical::zone(&crate::local_zone()));
            Some(unsafe { ical_glib_sys::i_cal_time_as_timet_with_zone(time.ptr(), crate::ical::zone(&crate::local_zone())) })
        }
    }
}
fn emit(registry: &Registry, reminder: Reminder, now: i64, due: &mut Vec<Reminder>) {
    if reminder.due > now {
        return;
    }
    let state = registry
        .db
        .borrow()
        .query_row(
            "SELECT until,dismissed FROM alarms WHERE source=?1 AND uid=?2 AND rid=?3 AND due=?4",
            rusqlite::params![reminder.source, reminder.item.uid, reminder.key_rid, reminder.due],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
        )
        .ok();
    if state.is_some_and(|(until, dismissed)| dismissed != 0 || until > now) {
        return;
    }
    let saved = registry.db.borrow().execute(
        "INSERT INTO alarms VALUES(?1,?2,?3,?4,0,2) ON CONFLICT(source,uid,rid,due) DO UPDATE SET dismissed=2",
        rusqlite::params![reminder.source, reminder.item.uid, reminder.key_rid, reminder.due],
    );
    if saved.is_ok() {
        if let Ok(data) = serde_json::to_string(&reminder) {
            let _=registry.db.borrow().execute("INSERT INTO alarm_snapshots VALUES(?1,?2,?3,?4,?5) ON CONFLICT(source,uid,rid,due) DO UPDATE SET data=excluded.data",rusqlite::params![reminder.source,reminder.item.uid,reminder.key_rid,reminder.due,data]);
        }
        due.push(reminder);
    }
}
fn collect(registry: &Registry, now: i64) -> Vec<Reminder> {
    let mut due = Vec::new();
    let mut active = std::collections::HashSet::new();
    for kind in [Kind::Calendar, Kind::TaskList] {
        for source in registry.sources(kind) {
            active.insert(source.uid.clone());
            let Ok(calendar) = Calendar::open(registry, &source.uid, kind, None) else { continue };
            let masters = calendar.items("#t").unwrap_or_default();
            let maximum_before =
                masters.iter().flat_map(|i| i.alarms.iter()).copied().max().unwrap_or(0).max(0).saturating_mul(60);
            let maximum_after =
                masters.iter().flat_map(|i| i.alarms.iter()).copied().min().unwrap_or(0).min(0).saturating_mul(-60);
            let instances = calendar.instances(
                now.saturating_sub(86400).saturating_sub(maximum_after),
                now.saturating_add(86400).saturating_add(maximum_before),
            );
            for item in instances {
                if item.is_done() || item.status == Status::Cancelled {
                    continue;
                }
                let Some(starts) = seconds(if kind == Kind::TaskList { item.due.or(item.start) } else { item.start }) else {
                    continue;
                };
                let ends = seconds(item.end).unwrap_or(starts);
                for minutes in &item.alarms {
                    let when = starts.saturating_sub(minutes.saturating_mul(60));
                    emit(
                        registry,
                        Reminder {
                            source: source.uid.clone(),
                            key_rid: item.rid.clone().unwrap_or_default(),
                            item: item.clone(),
                            starts,
                            ends,
                            due: when,
                        },
                        now,
                        &mut due,
                    );
                }
            }
            for item in masters {
                if item.is_done() || item.status == Status::Cancelled {
                    continue;
                }
                for when in calendar.absolute_alarms(&item.uid, item.rid.as_deref()) {
                    if when < now - 86400 {
                        continue;
                    }
                    let starts = seconds(item.start.or(item.due)).unwrap_or(when);
                    let ends = seconds(item.end).unwrap_or(starts);
                    emit(
                        registry,
                        Reminder {
                            source: source.uid.clone(),
                            key_rid: String::new(),
                            item: item.clone(),
                            starts,
                            ends,
                            due: when,
                        },
                        now,
                        &mut due,
                    );
                }
            }
        }
    }
    let snoozed: Vec<String> = {
        let db = registry.db.borrow();
        let Ok(mut query)=db.prepare("SELECT s.data FROM alarm_snapshots s JOIN alarms a USING(source,uid,rid,due) WHERE a.dismissed=0 AND a.until>0 AND a.until<=?1")else{return due};
        query.query_map([now], |r| r.get(0)).map(|r| r.filter_map(Result::ok).collect()).unwrap_or_default()
    };
    for data in snoozed {
        if let Ok(reminder) = serde_json::from_str::<Reminder>(&data) {
            if active.contains(&reminder.source) {
                let current = [Kind::Calendar, Kind::TaskList]
                    .into_iter()
                    .find(|kind| registry.source(&reminder.source, *kind).is_some())
                    .and_then(|kind| Calendar::open(registry, &reminder.source, kind, None).ok())
                    .and_then(|calendar| {
                        calendar
                            .get(&reminder.item.uid, reminder.item.rid.as_deref())
                            .or_else(|_| calendar.get(&reminder.item.uid, None))
                            .ok()
                    });
                if current.is_some_and(|item| !item.is_done() && item.status != Status::Cancelled) {
                    emit(registry, reminder, now, &mut due);
                }
            }
        }
    }
    due
}
impl Reminders {
    pub fn new(registry: &Registry) -> Self {
        Self { registry: registry.clone(), sources: Default::default() }
    }
    pub fn connect_due(&self, on_due: impl Fn(Vec<Reminder>, bool) + 'static) {
        let registry = self.registry.clone();
        let source = glib::timeout_add_local(std::time::Duration::from_secs(15), move || {
            let due = collect(&registry, crate::ical::now());
            if !due.is_empty() {
                on_due(due, false);
            }
            glib::ControlFlow::Continue
        });
        self.sources.borrow_mut().push(source);
    }
    pub fn dismiss(&self, reminder: &Reminder) -> Result<(), Error> {
        self.registry
            .db
            .borrow()
            .execute(
                "UPDATE alarms SET dismissed=1 WHERE source=?1 AND uid=?2 AND rid=?3 AND due=?4",
                rusqlite::params![reminder.source, reminder.item.uid, reminder.key_rid, reminder.due],
            )
            .map_err(crate::registry::error)?;
        Ok(())
    }
    pub fn snooze(&self, reminder: &Reminder, until: i64) {
        let _ = self.registry.db.borrow().execute(
            "UPDATE alarms SET dismissed=0,until=?5 WHERE source=?1 AND uid=?2 AND rid=?3 AND due=?4",
            rusqlite::params![reminder.source, reminder.item.uid, reminder.key_rid, reminder.due, until],
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn registry() -> Registry {
        Registry::open(std::env::temp_dir().join(format!("nyx-alarm-fixture-{}/pim.db", uuid::Uuid::new_v4()))).unwrap()
    }
    #[test]
    fn snoozes_survive_beyond_event_query_window() {
        let registry = registry();
        let calendar = Calendar::open(&registry, "system-calendar", Kind::Calendar, None).unwrap();
        calendar.import("BEGIN:VEVENT\r\nUID:snooze\r\nDTSTART:20261030T120100Z\r\nDTEND:20261030T130100Z\r\nBEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER:-PT1M\r\nEND:VALARM\r\nEND:VEVENT\r\n").unwrap();
        let now = chrono::NaiveDate::from_ymd_opt(2026, 10, 30).unwrap().and_hms_opt(12, 0, 0).unwrap().and_utc().timestamp();
        let due = collect(&registry, now);
        assert_eq!(due.len(), 1);
        Reminders::new(&registry).snooze(&due[0], now + 864000);
        assert!(collect(&registry, now + 863999).is_empty());
        assert_eq!(collect(&registry, now + 864000).len(), 1);
    }
    #[test]
    fn absolute_alarm_does_not_require_event_to_be_near_today() {
        let registry = registry();
        let calendar = Calendar::open(&registry, "system-calendar", Kind::Calendar, None).unwrap();
        calendar.import("BEGIN:VEVENT\r\nUID:absolute\r\nDTSTART:20261201T120000Z\r\nDTEND:20261201T130000Z\r\nBEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER;VALUE=DATE-TIME:20261030T120000Z\r\nEND:VALARM\r\nEND:VEVENT\r\n").unwrap();
        let now = chrono::NaiveDate::from_ymd_opt(2026, 10, 30).unwrap().and_hms_opt(12, 0, 0).unwrap().and_utc().timestamp();
        assert_eq!(collect(&registry, now).len(), 1);
    }
}
