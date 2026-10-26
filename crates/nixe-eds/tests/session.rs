//! Against a real Evolution Data Server on a private session bus with scratch XDG
//! directories. Run with `tools/test-eds.sh`; plain `cargo test` skips these.

use nixe_eds::{Calendar, Draft, Kind, Registry, Scope, When, range_query};
use std::time::{Duration, Instant};

const LOCAL_CALENDAR: &str = "system-calendar";
const LOCAL_TASKS: &str = "system-task-list";

fn registry() -> Registry {
    assert!(
        std::env::var("NIXE_EDS_SCRATCH").is_ok(),
        "run through tools/test-eds.sh: these tests write to EDS"
    );
    Registry::connect().expect("EDS registry")
}

fn pump_until(mut done: impl FnMut() -> bool) -> bool {
    let ctx = glib::MainContext::default();
    let until = Instant::now() + Duration::from_secs(10);
    while Instant::now() < until {
        while ctx.iteration(false) {}
        if done() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

// 2026-10-26 and 2026-11-02 are Mondays either side of the US switch back from
// daylight saving on 2026-11-01. 10:00 in New York is 14:00 UTC, then 15:00 UTC.
const OCT_26_1000_EDT: i64 = 1_793_023_200;
const NOV_02_1000_EST: i64 = 1_793_631_600;
const HOUR: i64 = 3600;
const DAY: i64 = 86_400;

#[test]
#[ignore]
fn weekly_event_keeps_wall_clock_across_dst() {
    let registry = registry();
    let sources = registry.sources(Kind::Calendar);
    let local = sources.iter().find(|s| s.uid == LOCAL_CALENDAR).expect("local calendar listed");
    assert!(local.local && local.account.is_none());

    let cal = Calendar::open(&registry, LOCAL_CALENDAR, Kind::Calendar, None).unwrap();
    assert!(!cal.read_only());
    assert!(cal.online());
    cal.refresh().unwrap();
    let uid = cal
        .create(&Draft {
            summary: Some("Standup".into()),
            start: Some(Some(When::Time(OCT_26_1000_EDT))),
            end: Some(Some(When::Time(OCT_26_1000_EDT + HOUR / 4))),
            zone: Some("America/New_York".into()),
            repeat: Some(Some("FREQ=WEEKLY;COUNT=3".into())),
            ..Draft::default()
        })
        .unwrap();

    let all: Vec<_> = cal
        .instances(OCT_26_1000_EDT - DAY, OCT_26_1000_EDT + 30 * DAY)
        .into_iter()
        .filter(|i| i.uid == uid)
        .collect();
    let starts: Vec<_> = all.iter().map(|i| i.start).collect();
    assert_eq!(
        starts,
        vec![
            Some(When::Time(OCT_26_1000_EDT)),
            Some(When::Time(NOV_02_1000_EST)),
            Some(When::Time(NOV_02_1000_EST + 7 * DAY)),
        ]
    );
    assert!(all.iter().all(|i| i.repeats && i.rid.is_some()));

    // Move only the second occurrence, then drop the third.
    let second = all[1].rid.clone().unwrap();
    cal.modify(&uid, Some(&second), Scope::This, &Draft { summary: Some("Standup (moved)".into()), ..Draft::default() })
        .unwrap();
    let third = all[2].rid.clone().unwrap();
    cal.remove(&uid, Some(&third), Scope::This).unwrap();

    let after: Vec<_> = cal
        .instances(OCT_26_1000_EDT - DAY, OCT_26_1000_EDT + 30 * DAY)
        .into_iter()
        .filter(|i| i.uid == uid)
        .map(|i| i.summary)
        .collect();
    assert_eq!(after, vec!["Standup".to_string(), "Standup (moved)".to_string()]);

    cal.remove(&uid, None, Scope::All).unwrap();
    assert!(cal.instances(OCT_26_1000_EDT - DAY, OCT_26_1000_EDT + 30 * DAY).iter().all(|i| i.uid != uid));
}

#[test]
#[ignore]
fn all_day_event_and_import() {
    let registry = registry();
    let cal = Calendar::open(&registry, LOCAL_CALENDAR, Kind::Calendar, None).unwrap();
    let day = When::Date { year: 2026, month: 12, day: 24 };
    let uid = cal
        .create(&Draft {
            summary: Some("Holiday".into()),
            start: Some(Some(day)),
            end: Some(Some(When::Date { year: 2026, month: 12, day: 25 })),
            ..Draft::default()
        })
        .unwrap();
    let item = cal.get(&uid, None).unwrap();
    assert!(item.all_day());
    assert_eq!(item.start, Some(day));

    let ics = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//test//EN\r\nBEGIN:VEVENT\r\nUID:imported-1\r\n\
               DTSTAMP:20261021T000000Z\r\nDTSTART:20261201T090000Z\r\nDTEND:20261201T100000Z\r\n\
               SUMMARY:Imported\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
    assert_eq!(cal.import(ics).unwrap(), "imported-1");
    assert!(cal.ics("imported-1", None).unwrap().contains("SUMMARY:Imported"));
    cal.remove("imported-1", None, Scope::All).unwrap();
    cal.remove(&uid, None, Scope::All).unwrap();
}

#[test]
#[ignore]
fn tasks_subtasks_and_completion() {
    let registry = registry();
    assert!(registry.sources(Kind::TaskList).iter().any(|s| s.uid == LOCAL_TASKS));
    let list = Calendar::open(&registry, LOCAL_TASKS, Kind::TaskList, None).unwrap();
    let parent = list
        .create(&Draft {
            summary: Some("Trip".into()),
            due: Some(Some(When::Date { year: 2026, month: 11, day: 20 })),
            priority: Some(Some(1)),
            ..Draft::default()
        })
        .unwrap();
    let child = list
        .create(&Draft { summary: Some("Book train".into()), parent: Some(Some(parent.clone())), ..Draft::default() })
        .unwrap();

    list.modify(&child, None, Scope::All, &Draft { done: Some(true), ..Draft::default() }).unwrap();
    let items = list.items("#t").unwrap();
    let child_item = items.iter().find(|i| i.uid == child).unwrap();
    assert!(child_item.is_done());
    assert_eq!(child_item.parent.as_deref(), Some(parent.as_str()));
    let parent_item = items.iter().find(|i| i.uid == parent).unwrap();
    assert_eq!(parent_item.priority, Some(1));
    assert!(!parent_item.is_done());

    list.modify(&child, None, Scope::All, &Draft { done: Some(false), ..Draft::default() }).unwrap();
    assert!(!list.get(&child, None).unwrap().is_done());
    list.remove(&child, None, Scope::All).unwrap();
    list.remove(&parent, None, Scope::All).unwrap();
}

#[test]
#[ignore]
fn view_reports_changes_and_registry_toggles() {
    let registry = registry();
    let cal = Calendar::open(&registry, LOCAL_CALENDAR, Kind::Calendar, None).unwrap();
    let changes = std::rc::Rc::new(std::cell::Cell::new(0));
    let seen = changes.clone();
    let _view = cal
        .watch(&range_query(NOV_02_1000_EST - DAY, NOV_02_1000_EST + DAY), move || seen.set(seen.get() + 1))
        .unwrap();
    pump_until(|| true);
    let before = changes.get();
    let uid = cal
        .create(&Draft {
            summary: Some("Watched".into()),
            start: Some(Some(When::Time(NOV_02_1000_EST))),
            end: Some(Some(When::Time(NOV_02_1000_EST + HOUR))),
            ..Draft::default()
        })
        .unwrap();
    assert!(pump_until(|| changes.get() > before), "view saw the new event");
    cal.remove(&uid, None, Scope::All).unwrap();

    let source_changes = std::rc::Rc::new(std::cell::Cell::new(0));
    let seen = source_changes.clone();
    let _watch = registry.watch(move || seen.set(seen.get() + 1));
    let original = registry.source(LOCAL_CALENDAR, Kind::Calendar).unwrap();
    registry.set_color(LOCAL_CALENDAR, Kind::Calendar, "#7aa2f7").unwrap();
    assert!(pump_until(|| source_changes.get() > 0), "registry reported the change");
    assert!(pump_until(|| {
        registry.source(LOCAL_CALENDAR, Kind::Calendar).unwrap().color.as_deref() == Some("#7aa2f7")
    }));
    registry.set_color(LOCAL_CALENDAR, Kind::Calendar, original.color.as_deref().unwrap_or("#becedd")).unwrap();
}

#[test]
#[ignore]
fn missing_items_are_not_found() {
    let registry = registry();
    let cal = Calendar::open(&registry, LOCAL_CALENDAR, Kind::Calendar, None).unwrap();
    assert!(cal.get("no-such-uid", None).is_err());
    assert!(Calendar::open(&registry, "no-such-source", Kind::Calendar, None).is_err());
}

#[test]
#[ignore]
fn answers_an_invitation() {
    let registry = registry();
    let cal = Calendar::open(&registry, LOCAL_CALENDAR, Kind::Calendar, None).unwrap();
    let ics = "BEGIN:VEVENT\r\nUID:invite-1\r\nDTSTAMP:20261021T000000Z\r\nDTSTART:20261203T150000Z\r\n\
               DTEND:20261203T160000Z\r\nSUMMARY:Planning\r\nORGANIZER;CN=Alex:mailto:alex@example.com\r\n\
               ATTENDEE;CN=Alex;PARTSTAT=ACCEPTED:mailto:alex@example.com\r\n\
               ATTENDEE;PARTSTAT=NEEDS-ACTION:mailto:me@example.com\r\nEND:VEVENT\r\n";
    cal.import(ics).unwrap();
    assert!(!cal.respond("invite-1", None, &["other@example.com".into()], nixe_eds::Response::Accepted).unwrap());
    assert!(cal.respond("invite-1", None, &["ME@example.com".into()], nixe_eds::Response::Accepted).unwrap());
    let item = cal.get("invite-1", None).unwrap();
    let me = item.attendees.iter().find(|a| a.email == "me@example.com").unwrap();
    assert_eq!(me.response, nixe_eds::Response::Accepted);
    assert_eq!(item.organizer.unwrap().name, "Alex");
    cal.remove("invite-1", None, Scope::All).unwrap();
}

#[test]
#[ignore]
fn reminder_fires_when_due() {
    let registry = registry();
    let cal = Calendar::open(&registry, LOCAL_CALENDAR, Kind::Calendar, None).unwrap();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
    let uid = cal
        .create(&Draft {
            summary: Some("Due soon".into()),
            start: Some(Some(When::Time(now + 20))),
            end: Some(Some(When::Time(now + 620))),
            alarms: Some(vec![0]),
            ..Draft::default()
        })
        .unwrap();
    assert_eq!(cal.get(&uid, None).unwrap().alarms, vec![0]);
    let reminders = nixe_eds::Reminders::new(&registry);
    let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let sink = seen.clone();
    reminders.connect_due(move |due, _| sink.borrow_mut().extend(due));
    let until = Instant::now() + Duration::from_secs(90);
    let ctx = glib::MainContext::default();
    while Instant::now() < until && !seen.borrow().iter().any(|r: &nixe_eds::Reminder| r.item.uid == uid) {
        while ctx.iteration(false) {}
        std::thread::sleep(Duration::from_millis(100));
    }
    let seen = seen.borrow();
    let due = seen.iter().find(|r| r.item.uid == uid).expect("the reminder fired");
    assert_eq!(due.item.summary, "Due soon");
    assert_eq!(due.starts, now + 20);
    assert_eq!(due.source, LOCAL_CALENDAR);
    reminders.dismiss(due).unwrap();
    cal.remove(&uid, None, Scope::All).unwrap();
}

#[test]
#[ignore]
fn imports_a_file_with_its_zone_and_exports_a_series() {
    let registry = registry();
    let cal = Calendar::open(&registry, LOCAL_CALENDAR, Kind::Calendar, None).unwrap();
    // A zone EDS has never seen, named the way Outlook names them.
    let ics = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//test//EN\r\nMETHOD:PUBLISH\r\n\
        BEGIN:VTIMEZONE\r\nTZID:Test Standard Time\r\nBEGIN:STANDARD\r\nDTSTART:19700101T000000\r\n\
        TZOFFSETFROM:-0300\r\nTZOFFSETTO:-0300\r\nTZNAME:TST\r\nEND:STANDARD\r\nEND:VTIMEZONE\r\n\
        BEGIN:VEVENT\r\nUID:file-1\r\nDTSTAMP:20261021T000000Z\r\nDTSTART;TZID=Test Standard Time:20261202T090000\r\n\
        DTEND;TZID=Test Standard Time:20261202T100000\r\nRRULE:FREQ=DAILY;COUNT=2\r\nSUMMARY:From a file\r\nEND:VEVENT\r\n\
        BEGIN:VEVENT\r\nUID:file-2\r\nDTSTAMP:20261021T000000Z\r\nDTSTART;VALUE=DATE:20261205\r\nSUMMARY:Second\r\nEND:VEVENT\r\n\
        END:VCALENDAR\r\n";
    assert_eq!(cal.import_all(ics).unwrap(), vec!["file-1".to_string(), "file-2".to_string()]);
    // 09:00 at UTC-3 is 12:00 UTC.
    assert_eq!(cal.get("file-1", None).unwrap().start, Some(When::Time(1_796_212_800)));
    // Importing again updates rather than failing.
    assert_eq!(cal.import_all(ics).unwrap().len(), 2);

    let out = cal.export("file-1").unwrap();
    assert!(out.starts_with("BEGIN:VCALENDAR"), "{out}");
    assert!(out.contains("BEGIN:VTIMEZONE") && out.contains("Test Standard Time"), "{out}");
    assert!(out.contains("SUMMARY:From a file"), "{out}");
    cal.remove("file-1", None, Scope::All).unwrap();
    cal.remove("file-2", None, Scope::All).unwrap();
}

#[test]
#[ignore]
fn changes_this_and_following() {
    let registry = registry();
    let cal = Calendar::open(&registry, LOCAL_CALENDAR, Kind::Calendar, None).unwrap();
    let uid = cal
        .create(&Draft {
            summary: Some("Class".into()),
            start: Some(Some(When::Time(OCT_26_1000_EDT))),
            end: Some(Some(When::Time(OCT_26_1000_EDT + HOUR))),
            zone: Some("America/New_York".into()),
            repeat: Some(Some("FREQ=WEEKLY;COUNT=4".into())),
            ..Draft::default()
        })
        .unwrap();
    let range = (OCT_26_1000_EDT - DAY, OCT_26_1000_EDT + 40 * DAY);
    let all: Vec<_> = cal.instances(range.0, range.1).into_iter().filter(|i| i.uid == uid).collect();
    assert_eq!(all.len(), 4);
    let third = all[2].rid.clone().unwrap();
    cal.modify(&uid, Some(&third), Scope::ThisAndFollowing, &Draft { summary: Some("Class (new room)".into()), ..Draft::default() })
        .unwrap();
    // EDS splits the series: the old one ends before the third class, and the
    // rest becomes a new series (with a new uid).
    let after = cal.instances(range.0, range.1);
    let titles: Vec<&str> = after.iter().filter(|i| i.summary.starts_with("Class")).map(|i| i.summary.as_str()).collect();
    assert_eq!(titles, vec!["Class", "Class", "Class (new room)", "Class (new room)"]);
    let new_series = after.iter().find(|i| i.summary == "Class (new room)").map(|i| i.uid.clone()).unwrap();
    assert_ne!(new_series, uid);
    cal.remove(&new_series, None, Scope::All).unwrap();
    cal.remove(&uid, None, Scope::All).unwrap();
}

#[test]
#[ignore]
fn task_alert_counts_from_the_due_time() {
    let registry = registry();
    let list = Calendar::open(&registry, LOCAL_TASKS, Kind::TaskList, None).unwrap();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
    let uid = list
        .create(&Draft { summary: Some("Call Sam".into()), due: Some(Some(When::Time(now + 20))), alarms: Some(vec![0]), ..Draft::default() })
        .unwrap();
    assert!(list.ics(&uid, None).unwrap().contains("RELATED=END"));
    let reminders = nixe_eds::Reminders::new(&registry);
    let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let sink = seen.clone();
    reminders.connect_due(move |due, _| sink.borrow_mut().extend(due.into_iter().map(|r| (r.source.clone(), r.item.summary.clone()))));
    let fired = pump_until_secs(90, || seen.borrow().iter().any(|(_, s)| s == "Call Sam"));
    list.remove(&uid, None, Scope::All).unwrap();
    assert!(fired, "the task's alert fired");
    assert!(seen.borrow().iter().any(|(source, _)| source == LOCAL_TASKS));
}

fn pump_until_secs(secs: u64, mut done: impl FnMut() -> bool) -> bool {
    let ctx = glib::MainContext::default();
    let until = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < until {
        while ctx.iteration(false) {}
        if done() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

#[test]
#[ignore]
fn completing_a_repeating_task_moves_it_on() {
    let registry = registry();
    let list = Calendar::open(&registry, LOCAL_TASKS, Kind::TaskList, None).unwrap();
    let once = list.create(&Draft { summary: Some("Once".into()), due: Some(Some(When::Date { year: 2026, month: 11, day: 2 })), ..Draft::default() }).unwrap();
    let weekly = list
        .create(&Draft {
            summary: Some("Water plants".into()),
            due: Some(Some(When::Date { year: 2026, month: 11, day: 2 })),
            repeat: Some(Some("FREQ=WEEKLY".into())),
            ..Draft::default()
        })
        .unwrap();
    list.complete(&once).unwrap();
    list.complete(&weekly).unwrap();
    assert!(list.get(&once, None).unwrap().is_done());
    let next = list.get(&weekly, None).unwrap();
    assert!(!next.is_done(), "a repeating task stays open");
    assert_eq!(next.due, Some(When::Date { year: 2026, month: 11, day: 9 }));
    list.remove(&once, None, Scope::All).unwrap();
    list.remove(&weekly, None, Scope::All).unwrap();
}

#[test]
#[ignore]
fn tags_and_free_busy() {
    let registry = registry();
    let tasks = Calendar::open(&registry, LOCAL_TASKS, Kind::TaskList, None).unwrap();
    let uid = tasks.create(&Draft { summary: Some("Tagged".into()), categories: Some(vec!["home".into(), "bills".into()]), ..Draft::default() }).unwrap();
    assert_eq!(tasks.get(&uid, None).unwrap().categories, ["home", "bills"]);
    tasks.modify(&uid, None, Scope::All, &Draft { categories: Some(Vec::new()), ..Draft::default() }).unwrap();
    assert!(tasks.get(&uid, None).unwrap().categories.is_empty());
    tasks.remove(&uid, None, Scope::All).unwrap();

    // The local calendar answers for its own mail accounts only (none here).
    let cal = Calendar::open(&registry, LOCAL_CALENDAR, Kind::Calendar, None).unwrap();
    let busy = cal.free_busy(OCT_26_1000_EDT, OCT_26_1000_EDT + DAY, &["sam@example.com".to_string()]);
    assert!(busy.is_ok(), "{busy:?}");
}
