use super::*;

/// Wednesday 2026-10-21, 10:30.
fn now() -> NaiveDateTime {
    NaiveDate::from_ymd_opt(2026, 10, 21).unwrap().and_hms_opt(10, 30, 0).unwrap()
}

fn d(m: u32, day: u32) -> Option<NaiveDate> {
    NaiveDate::from_ymd_opt(2026, m, day)
}

fn t(h: u32, m: u32) -> Option<NaiveTime> {
    NaiveTime::from_hms_opt(h, m, 0)
}

fn p(text: &str) -> Parse {
    parse(text, now())
}

#[test]
fn examples_from_the_brief() {
    let x = p("Meeting with Alex tomorrow at 2 PM");
    assert_eq!((x.title.as_str(), x.date, x.start), ("Meeting with Alex", d(10, 22), t(14, 0)));
    assert!(x.alternatives.is_empty());

    // "next Friday" on a Wednesday: Friday of next week, with this Friday as the other reading.
    let x = p("Dentist next Friday at 10");
    assert_eq!((x.title.as_str(), x.date, x.start), ("Dentist", d(10, 30), t(10, 0)));
    assert_eq!(x.alternatives.len(), 2);
    assert_eq!(x.alternatives[0].date, d(10, 23));
    assert_eq!(x.alternatives[1].start, t(22, 0));

    let x = p("Every Monday at 9 AM");
    assert_eq!(x.repeat.as_deref(), Some("FREQ=WEEKLY;BYDAY=MO"));
    assert_eq!((x.date, x.start), (d(10, 26), t(9, 0)));
    assert_eq!(x.title, "");
}

#[test]
fn relative_days_and_weekdays() {
    assert_eq!(p("Lunch today").date, d(10, 21));
    assert_eq!(p("call mom tmrw").date, d(10, 22));
    assert_eq!(p("party the day after tomorrow").date, d(10, 23));
    assert_eq!(p("gym friday").date, d(10, 23));
    assert_eq!(p("gym this friday").date, d(10, 23));
    // Today's weekday means next week's.
    assert_eq!(p("review wednesday").date, d(10, 28));
    assert_eq!(p("plan next week").date, d(10, 26));
    assert_eq!(p("invoices next month").date, d(11, 1));
    assert_eq!(p("hike this weekend").date, d(10, 24));
    assert_eq!(p("renew in 3 days").date, d(10, 24));
    assert_eq!(p("renew in two weeks").date, d(11, 4));
}

#[test]
fn calendar_dates() {
    assert_eq!(p("Holiday party December 24").date, d(12, 24));
    assert_eq!(p("party dec 24th").date, d(12, 24));
    assert_eq!(p("party 24th of dec").date, d(12, 24));
    assert_eq!(p("party 24 december").date, d(12, 24));
    assert_eq!(p("trip 2026-12-01").date, d(12, 1));
    assert_eq!(p("trip 12/1").date, d(12, 1));
    assert_eq!(p("trip 12/1/27").date, NaiveDate::from_ymd_opt(2027, 12, 1));
    assert_eq!(p("taxes april 15 2027").date, NaiveDate::from_ymd_opt(2027, 4, 15));
    // A passed date is next year's.
    assert_eq!(p("birthday march 3").date, NaiveDate::from_ymd_opt(2027, 3, 3));
    // "the 5th" is this month unless it has passed.
    assert_eq!(p("pay rent on the 25th").date, d(10, 25));
    assert_eq!(p("pay rent on the 1st").date, d(11, 1));
}

#[test]
fn times() {
    assert_eq!(p("call at 3").start, t(15, 0));
    assert_eq!(p("call at 3").alternatives[0].start, t(3, 0));
    assert_eq!(p("call at 9").start, t(9, 0));
    assert_eq!(p("call 9am").start, t(9, 0));
    assert_eq!(p("call 9 pm").start, t(21, 0));
    assert_eq!(p("call 2:30pm").start, t(14, 30));
    assert_eq!(p("call 14:30").start, t(14, 30));
    assert!(p("call 14:30").alternatives.is_empty());
    assert_eq!(p("lunch at noon").start, t(12, 0));
    assert_eq!(p("deploy at midnight").start, t(0, 0));
    assert_eq!(p("call at 4 o'clock").start, t(16, 0));
    // A part of the day decides am/pm.
    let x = p("run tomorrow morning at 7");
    assert_eq!((x.date, x.start), (d(10, 22), t(7, 0)));
    assert!(x.alternatives.is_empty());
    assert_eq!(p("dinner tonight").start, t(19, 0));
    assert_eq!(p("dinner tomorrow evening").start, t(18, 0));
    // A time that has passed today is tomorrow.
    assert_eq!(p("standup at 9am").date, d(10, 22));
    assert_eq!(p("standup at 11am").date, d(10, 21));
    // A bare number is not a time without "at".
    let x = p("buy 3 apples");
    assert_eq!((x.title.as_str(), x.start), ("buy 3 apples", None));
}

#[test]
fn ranges_and_durations() {
    let x = p("workshop friday 2-4pm");
    assert_eq!((x.date, x.start, x.end), (d(10, 23), t(14, 0), t(16, 0)));
    let x = p("workshop from 10am to 11:30am");
    assert_eq!((x.start, x.end), (t(10, 0), t(11, 30)));
    let x = p("class from 11 to 1pm");
    assert_eq!((x.start, x.end), (t(11, 0), t(13, 0)));
    let x = p("sync between 2 and 3pm");
    assert_eq!((x.start, x.end, x.title.as_str()), (t(14, 0), t(15, 0), "sync"));
    let x = p("focus tomorrow at 9 for 90 minutes");
    assert_eq!((x.start, x.duration), (t(9, 0), Some(Duration::minutes(90))));
    assert_eq!(x.ends(), Some(d(10, 22).unwrap().and_hms_opt(10, 30, 0).unwrap()));
    assert_eq!(p("call at 4 for an hour").duration, Some(Duration::hours(1)));
    assert_eq!(p("call at 4 for 1.5 hours").duration, Some(Duration::minutes(90)));
    assert_eq!(p("call at 4 for 2h").duration, Some(Duration::hours(2)));
    assert_eq!(p("call at 4 for half an hour").duration, Some(Duration::minutes(30)));
    let x = p("late shift 10pm-2am");
    assert_eq!(x.ends(), Some(d(10, 22).unwrap().and_hms_opt(2, 0, 0).unwrap()));
    let x = p("check oven in 20 minutes");
    assert_eq!((x.date, x.start, x.title.as_str()), (d(10, 21), t(10, 50), "check oven"));
}

#[test]
fn repeats() {
    let r = |s: &str| p(s).repeat;
    assert_eq!(r("standup every weekday at 9:30").as_deref(), Some("FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR"));
    assert_eq!(r("water plants every day").as_deref(), Some("FREQ=DAILY"));
    assert_eq!(r("vitamins daily").as_deref(), Some("FREQ=DAILY"));
    assert_eq!(r("1:1 every other week").as_deref(), Some("FREQ=WEEKLY;INTERVAL=2"));
    assert_eq!(r("backup every 3 days").as_deref(), Some("FREQ=DAILY;INTERVAL=3"));
    assert_eq!(r("gym every mon, wed and fri at 7am").as_deref(), Some("FREQ=WEEKLY;BYDAY=MO,WE,FR"));
    assert_eq!(r("piano on tuesdays at 5").as_deref(), Some("FREQ=WEEKLY;BYDAY=TU"));
    assert_eq!(r("pay rent every month on the 1st").as_deref(), Some("FREQ=MONTHLY;BYMONTHDAY=1"));
    assert_eq!(r("anniversary yearly").as_deref(), Some("FREQ=YEARLY"));
    let x = p("gym every mon, wed and fri at 7am");
    assert_eq!((x.title.as_str(), x.date, x.start), ("gym", d(10, 21), t(7, 0)));
    let x = p("Team sync every weekend");
    assert_eq!((x.repeat.as_deref(), x.date), (Some("FREQ=WEEKLY;BYDAY=SA,SU"), d(10, 24)));
}

#[test]
fn titles_keep_their_words() {
    assert_eq!(p("Call Sam about the trip tomorrow at 9").title, "Call Sam about the trip");
    assert_eq!(p("Read: chapter 3").title, "Read: chapter 3");
    assert_eq!(p("Book flights").title, "Book flights");
    assert!(!p("Book flights").found());
    assert_eq!(p("Pick up Alex at the airport friday at 6pm").title, "Pick up Alex at the airport");
}

#[test]
fn all_day_and_moments() {
    let x = p("Conference dec 3");
    assert!(x.all_day());
    assert_eq!(x.starts(), None);
    let x = p("Conference dec 3 at 9");
    assert!(!x.all_day());
    assert_eq!(x.starts(), Some(d(12, 3).unwrap().and_hms_opt(9, 0, 0).unwrap()));
}
