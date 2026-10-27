//! Natural-language dates, times and repeats, for quick entry in Calendar and
//! Reminders: "Meeting with Alex tomorrow at 2 PM", "Dentist next Friday at 10",
//! "Standup every weekday at 9:30", "Pay rent on the 1st every month".
//!
//! Parsing is deterministic and English only. It works in local wall-clock time:
//! the caller passes "now" and turns the result into a moment in its time zone.
//! When a phrase can mean two things ("next Friday", "at 4"), the most likely
//! reading is returned with the others in [`Parse::alternatives`], so the UI can
//! show them before saving.

use chrono::{Datelike, Duration, Months, NaiveDate, NaiveDateTime, NaiveTime, Timelike, Weekday};

/// What a phrase said.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Parse {
    /// The words that were not about time, e.g. "Meeting with Alex".
    pub title: String,
    pub date: Option<NaiveDate>,
    pub start: Option<NaiveTime>,
    pub end: Option<NaiveTime>,
    /// "for 30 minutes".
    pub duration: Option<Duration>,
    /// An RRULE value, e.g. `FREQ=WEEKLY;BYDAY=MO`.
    pub repeat: Option<String>,
    /// Other likely readings, most likely first. Empty when the phrase is clear.
    pub alternatives: Vec<Parse>,
}

impl Parse {
    /// A date without a time.
    pub fn all_day(&self) -> bool {
        self.date.is_some() && self.start.is_none()
    }

    /// Anything about time was found.
    pub fn found(&self) -> bool {
        self.date.is_some() || self.start.is_some() || self.repeat.is_some() || self.duration.is_some()
    }

    /// Start as a local date and time, if both are known.
    pub fn starts(&self) -> Option<NaiveDateTime> {
        Some(self.date?.and_time(self.start?))
    }

    /// End as a local date and time: the end time (the next day if it is not after
    /// the start), else start plus duration.
    pub fn ends(&self) -> Option<NaiveDateTime> {
        let start = self.starts()?;
        if let Some(end) = self.end {
            let end = start.date().and_time(end);
            return Some(if end <= start { end + Duration::days(1) } else { end });
        }
        Some(start + self.duration?)
    }
}

/// Reads `text` as of `now` (local wall-clock time).
pub fn parse(text: &str, now: NaiveDateTime) -> Parse {
    let tokens = tokenize(text);
    let mut p = Parser { tokens: &tokens, used: vec![false; tokens.len()], now, out: Found::default() };
    p.run();
    p.finish()
}

#[derive(Clone, Debug)]
struct Token {
    raw: String,
    word: String,
}

fn tokenize(text: &str) -> Vec<Token> {
    text.split_whitespace()
        .map(|raw| {
            let word = raw
                .trim_matches(|c: char| matches!(c, ',' | '.' | '!' | '?' | ';' | '(' | ')' | '"'))
                .to_lowercase()
                .replace('’', "'");
            Token { raw: raw.to_string(), word }
        })
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Meridiem {
    Am,
    Pm,
}

#[derive(Clone, Copy, Debug)]
struct Clock {
    hour: u32,
    minute: u32,
    meridiem: Option<Meridiem>,
    /// Written as 24-hour time ("14:30", "09:00").
    explicit: bool,
}

#[derive(Default)]
struct Found {
    date: Option<NaiveDate>,
    /// A second reading of the date ("next Friday").
    date_alt: Option<NaiveDate>,
    start: Option<Clock>,
    end: Option<Clock>,
    /// Part of day ("morning") decides am/pm for bare hours.
    day_part: Option<Meridiem>,
    default_time: Option<NaiveTime>,
    duration: Option<Duration>,
    /// A moment relative to now ("in 2 hours").
    moment: Option<NaiveDateTime>,
    freq: Option<&'static str>,
    interval: u32,
    by_day: Vec<Weekday>,
    by_month_day: Option<u32>,
}

/// Words that only join a time phrase to the title, dropped next to one.
const JOINERS: &[&str] = &["at", "on", "from", "by", "due", "starting", "in", "for", "this", "the", "@"];

struct Parser<'a> {
    tokens: &'a [Token],
    used: Vec<bool>,
    now: NaiveDateTime,
    out: Found,
}

impl<'a> Parser<'a> {
    fn word(&self, i: usize) -> Option<&'a str> {
        let tokens: &'a [Token] = self.tokens;
        tokens.get(i).map(|t| t.word.as_str())
    }

    fn free(&self, i: usize) -> bool {
        i < self.tokens.len() && !self.used[i]
    }

    fn take(&mut self, from: usize, count: usize) {
        for used in self.used.iter_mut().skip(from).take(count) {
            *used = true;
        }
        // Absorb joiners right before a time phrase ("at", "on the").
        let mut i = from;
        while i > 0 && self.free(i - 1) && JOINERS.contains(&self.tokens[i - 1].word.as_str()) {
            self.used[i - 1] = true;
            i -= 1;
        }
    }

    fn run(&mut self) {
        let matchers: [fn(&mut Self, usize) -> usize; 6] =
            [Self::repeat, Self::relative, Self::range, Self::date, Self::time, Self::duration];
        for matcher in matchers {
            let mut i = 0;
            while i < self.tokens.len() {
                let n = if self.free(i) { matcher(self, i) } else { 0 };
                if n > 0 {
                    self.take(i, n);
                    i += n;
                } else {
                    i += 1;
                }
            }
        }
    }

    // ---------- repeats ----------

    fn repeat(&mut self, i: usize) -> usize {
        let w = self.word(i).unwrap_or_default();
        let simple = match w {
            "daily" | "everyday" => Some(("DAILY", 1)),
            "weekly" => Some(("WEEKLY", 1)),
            "biweekly" | "fortnightly" => Some(("WEEKLY", 2)),
            "monthly" => Some(("MONTHLY", 1)),
            "yearly" | "annually" => Some(("YEARLY", 1)),
            "weekdays" => {
                self.set_repeat("WEEKLY", 1, WEEKDAYS.to_vec());
                return 1;
            }
            _ => None,
        };
        if let Some((freq, interval)) = simple {
            self.set_repeat(freq, interval, Vec::new());
            return 1;
        }
        // "mondays", "on tuesdays"
        if let Some(day) = w.strip_suffix('s').and_then(weekday) {
            self.set_repeat("WEEKLY", 1, vec![day]);
            return 1;
        }
        if w != "every" && w != "each" {
            return 0;
        }
        let mut j = i + 1;
        let mut interval = 1;
        match self.word(j) {
            Some("other") => {
                interval = 2;
                j += 1;
            }
            Some(n) if number(n).is_some() && self.word(j + 1).is_some_and(|u| unit(u).is_some()) => {
                interval = number(n).unwrap_or(1);
                j += 1;
            }
            _ => {}
        }
        let Some(next) = self.word(j) else { return 0 };
        match next {
            "day" | "days" => self.set_repeat("DAILY", interval, Vec::new()),
            "weekday" | "weekdays" => self.set_repeat("WEEKLY", interval, WEEKDAYS.to_vec()),
            "weekend" | "weekends" => self.set_repeat("WEEKLY", interval, vec![Weekday::Sat, Weekday::Sun]),
            "week" | "weeks" => self.set_repeat("WEEKLY", interval, Vec::new()),
            "month" | "months" => self.set_repeat("MONTHLY", interval, Vec::new()),
            "year" | "years" => self.set_repeat("YEARLY", interval, Vec::new()),
            word => {
                let Some(first) = weekday(word) else { return 0 };
                let mut days = vec![first];
                // "every monday and wednesday", "every mon, wed and fri"
                while let Some(word) = self.word(j + 1) {
                    if matches!(word, "and" | "&" | "+") && self.word(j + 2).and_then(weekday).is_some() {
                        days.push(self.word(j + 2).and_then(weekday).unwrap_or(first));
                        j += 2;
                    } else if let Some(day) = weekday(word) {
                        days.push(day);
                        j += 1;
                    } else {
                        break;
                    }
                }
                self.set_repeat("WEEKLY", interval, days);
            }
        }
        j + 1 - i
    }

    fn set_repeat(&mut self, freq: &'static str, interval: u32, days: Vec<Weekday>) {
        self.out.freq = Some(freq);
        self.out.interval = interval.max(1);
        self.out.by_day = days;
    }

    // ---------- relative moments: "in 2 hours", "in 3 days" ----------

    fn relative(&mut self, i: usize) -> usize {
        if self.word(i) != Some("in") {
            return 0;
        }
        let Some((amount, used)) = self.amount(i + 1) else { return 0 };
        let Some(unit) = self.word(i + 1 + used).and_then(unit) else { return 0 };
        let now = self.now;
        match unit {
            Unit::Minute => self.out.moment = Some(now + Duration::minutes(amount)),
            Unit::Hour => self.out.moment = Some(now + Duration::hours(amount)),
            Unit::Day => self.out.date = Some(now.date() + Duration::days(amount)),
            Unit::Week => self.out.date = Some(now.date() + Duration::weeks(amount)),
            Unit::Month => self.out.date = now.date().checked_add_months(Months::new(amount as u32)),
            Unit::Year => self.out.date = now.date().checked_add_months(Months::new(12 * amount as u32)),
        }
        2 + used
    }

    /// "3", "three", "a", "an", "half an" (as minutes are handled by duration).
    fn amount(&self, i: usize) -> Option<(i64, usize)> {
        let w = self.word(i)?;
        if matches!(w, "a" | "an") {
            return Some((1, 1));
        }
        number(w).map(|n| (n as i64, 1))
    }

    // ---------- time ranges: "2-3pm", "from 2 to 4", "10am to 11:30am" ----------

    fn range(&mut self, i: usize) -> usize {
        let w = self.word(i).unwrap_or_default();
        // One token: "2-3pm", "10:00-11:30"
        if let Some((a, b)) = w.split_once(['-', '–'])
            && let (Some(start), Some(end)) = (clock(a), clock(b))
            && (start.meridiem.is_some() || end.meridiem.is_some() || a.contains(':') || b.contains(':'))
        {
            let (start, end) = meridiem_pair(start, end);
            self.out.start = Some(start);
            self.out.end = Some(end);
            return 1;
        }
        // "from 2 to 4pm", "2pm - 3pm", "between 2 and 3"
        let mut j = i;
        let starts_with_word = matches!(w, "from" | "between");
        if starts_with_word {
            j += 1;
        }
        let Some((start, n1)) = self.clock_at(j, starts_with_word) else { return 0 };
        let joiner = self.word(j + n1);
        if !matches!(joiner, Some("to" | "-" | "–" | "until" | "till" | "and")) {
            return 0;
        }
        if joiner == Some("and") && w != "between" {
            return 0;
        }
        let Some((end, n2)) = self.clock_at(j + n1 + 1, true) else { return 0 };
        let (start, end) = meridiem_pair(start, end);
        self.out.start = Some(start);
        self.out.end = Some(end);
        j + n1 + 1 + n2 - i
    }

    /// A time at token `i`: "2", "2pm", "2 pm", "14:30", "noon". A bare number
    /// counts only when `loose` (after "at", "from", ...).
    fn clock_at(&self, i: usize, loose: bool) -> Option<(Clock, usize)> {
        let w = self.word(i)?;
        if !self.free(i) {
            return None;
        }
        if let Some(c) = named_time(w) {
            return Some((c, 1));
        }
        let mut c = clock(w)?;
        let mut used = 1;
        if c.meridiem.is_none()
            && let Some(m) = self.word(i + 1).and_then(meridiem)
        {
            c.meridiem = Some(m);
            used = 2;
        }
        if self.word(i + used) == Some("o'clock") {
            used += 1;
        }
        let bare = c.meridiem.is_none() && !w.contains(':');
        if bare && !loose && used == 1 {
            return None;
        }
        Some((c, used))
    }

    // ---------- dates ----------

    fn date(&mut self, i: usize) -> usize {
        let today = self.now.date();
        let w = self.word(i).unwrap_or_default();
        match w {
            "today" => {
                self.out.date = Some(today);
                return 1;
            }
            "tonight" => {
                self.out.date = Some(today);
                self.out.default_time = NaiveTime::from_hms_opt(19, 0, 0);
                self.out.day_part = Some(Meridiem::Pm);
                return 1;
            }
            "tomorrow" | "tmrw" | "tmr" | "tomorow" => {
                self.out.date = Some(today + Duration::days(1));
                return 1;
            }
            "day" if self.word(i + 1) == Some("after") && self.word(i + 2) == Some("tomorrow") => {
                self.out.date = Some(today + Duration::days(2));
                return 3;
            }
            _ => {}
        }
        // "next week", "next month", "next friday", "this friday", "coming friday"
        if matches!(w, "next" | "this" | "coming") {
            let Some(next) = self.word(i + 1) else { return 0 };
            if let Some(day) = weekday(next) {
                let coming = next_weekday(today, day, false);
                if w == "next" {
                    // "next Friday": the Friday of next week; the coming one is the other reading.
                    let week_start = today - Duration::days(today.weekday().num_days_from_monday() as i64);
                    let of_next_week = week_start + Duration::days(7 + day.num_days_from_monday() as i64);
                    self.out.date = Some(of_next_week);
                    if of_next_week != coming {
                        self.out.date_alt = Some(coming);
                    }
                } else {
                    self.out.date = Some(coming);
                }
                return 2;
            }
            match (w, next) {
                ("next", "week") => {
                    self.out.date = Some(next_weekday(today, Weekday::Mon, false));
                    return 2;
                }
                ("next", "month") => {
                    self.out.date = today.with_day(1).and_then(|d| d.checked_add_months(Months::new(1)));
                    return 2;
                }
                ("next", "year") => {
                    self.out.date = NaiveDate::from_ymd_opt(today.year() + 1, 1, 1);
                    return 2;
                }
                ("this", "weekend") | ("next", "weekend") => {
                    let sat = next_weekday(today, Weekday::Sat, true);
                    self.out.date = Some(if w == "next" && sat - today < Duration::days(6) { sat + Duration::days(7) } else { sat });
                    return 2;
                }
                _ => return 0,
            }
        }
        if let Some(day) = weekday(w) {
            self.out.date = Some(next_weekday(today, day, false));
            return 1;
        }
        // ISO "2026-12-24", US "12/24", "12/24/2026"
        if let Some(d) = numeric_date(w, today) {
            self.out.date = Some(d);
            return 1;
        }
        // "the 24th", "on the 1st"
        if w == "the"
            && let Some(day) = self.word(i + 1).and_then(ordinal)
            && self.word(i + 2).and_then(month).is_none()
            && self.word(i + 2) != Some("of")
        {
            if self.out.freq == Some("MONTHLY") {
                self.out.by_month_day = Some(day);
            }
            self.out.date = month_day_from(today, day);
            return 2;
        }
        // "december 24", "dec 24th", "december 24, 2026"
        if let Some(m) = month(w)
            && let Some(day) = self.word(i + 1).and_then(|d| ordinal(d).or_else(|| number(d).filter(|n| *n <= 31)))
        {
            let year = self.word(i + 2).and_then(year);
            self.out.date = month_date(today, m, day, year);
            return 2 + usize::from(year.is_some());
        }
        // "24 december", "24th of dec"
        if let Some(day) = ordinal(w).or_else(|| number(w).filter(|n| (1..=31).contains(n))) {
            let of = usize::from(self.word(i + 1) == Some("of"));
            if let Some(m) = self.word(i + 1 + of).and_then(month) {
                let year = self.word(i + 2 + of).and_then(year);
                self.out.date = month_date(today, m, day, year);
                return 2 + of + usize::from(year.is_some());
            }
        }
        0
    }

    // ---------- times ----------

    fn time(&mut self, i: usize) -> usize {
        let w = self.word(i).unwrap_or_default();
        match w {
            "morning" => {
                self.out.day_part = Some(Meridiem::Am);
                self.out.default_time = NaiveTime::from_hms_opt(9, 0, 0);
                return 1;
            }
            "afternoon" => {
                self.out.day_part = Some(Meridiem::Pm);
                self.out.default_time = NaiveTime::from_hms_opt(14, 0, 0);
                return 1;
            }
            "evening" | "night" => {
                self.out.day_part = Some(Meridiem::Pm);
                self.out.default_time = NaiveTime::from_hms_opt(if w == "night" { 20 } else { 18 }, 0, 0);
                return 1;
            }
            _ => {}
        }
        if self.out.start.is_some() {
            return 0;
        }
        let loose = matches!(w, "at" | "@");
        let at = usize::from(loose);
        let Some((c, n)) = self.clock_at(i + at, loose) else { return 0 };
        self.out.start = Some(c);
        at + n
    }

    // ---------- durations: "for 30 minutes", "for an hour" ----------

    fn duration(&mut self, i: usize) -> usize {
        if self.word(i) != Some("for") {
            return 0;
        }
        let w = self.word(i + 1).unwrap_or_default();
        // "for 90min", "for 2h", "for 1.5h"
        if let Some(d) = compact_duration(w) {
            self.out.duration = Some(d);
            return 2;
        }
        let (minutes, used) = if matches!(w, "half") && self.word(i + 2) == Some("an") && self.word(i + 3) == Some("hour") {
            (30.0, 3)
        } else if matches!(w, "a" | "an") {
            (1.0, 1)
        } else if let Ok(n) = w.parse::<f64>() {
            (n, 1)
        } else if let Some(n) = number(w) {
            (n as f64, 1)
        } else {
            return 0;
        };
        if used == 3 {
            self.out.duration = Some(Duration::minutes(minutes as i64));
            return 4;
        }
        let minutes = match self.word(i + 1 + used).and_then(unit) {
            Some(Unit::Minute) => minutes,
            Some(Unit::Hour) => minutes * 60.0,
            Some(Unit::Day) => minutes * 1440.0,
            _ => return 0,
        };
        self.out.duration = Some(Duration::minutes(minutes.round() as i64));
        2 + used
    }

    // ---------- result ----------

    fn finish(self) -> Parse {
        let title = self.title();
        let out = self.out;
        let now = self.now;
        let mut alternatives = Vec::new();

        let mut date = out.date;
        let mut start = None;
        let mut end = None;
        let mut am_pm_alt = None;
        if let Some(moment) = out.moment {
            date = Some(moment.date());
            start = Some(round_minute(moment.time()));
        } else if let Some(c) = out.start {
            let (time, other) = resolve(c, out.day_part);
            start = Some(time);
            am_pm_alt = other;
            end = out.end.map(|e| resolve(e, out.day_part.or(c.meridiem)).0);
        } else if let Some(t) = out.default_time {
            start = Some(t);
        }

        // A weekly repeat on given days starts on the first of them.
        if date.is_none() && !out.by_day.is_empty() {
            let first = out.by_day.iter().map(|d| next_weekday(now.date(), *d, true)).min();
            date = first;
        }
        // A time with no date is today, or tomorrow once it has passed.
        if date.is_none()
            && let Some(time) = start
        {
            date = Some(if time > now.time() { now.date() } else { now.date() + Duration::days(1) });
        }
        if date.is_none() && out.freq.is_some() {
            date = Some(now.date());
        }

        let repeat = out.freq.map(|freq| {
            let mut rule = format!("FREQ={freq}");
            if out.interval > 1 {
                rule.push_str(&format!(";INTERVAL={}", out.interval));
            }
            if !out.by_day.is_empty() {
                let days: Vec<_> = out.by_day.iter().map(|d| ical_day(*d)).collect();
                rule.push_str(&format!(";BYDAY={}", days.join(",")));
            }
            if let Some(day) = out.by_month_day {
                rule.push_str(&format!(";BYMONTHDAY={day}"));
            }
            rule
        });

        let base = Parse { title, date, start, end, duration: out.duration, repeat, alternatives: Vec::new() };
        if let Some(alt) = out.date_alt {
            alternatives.push(Parse { date: Some(alt), ..base.clone() });
        }
        if let Some(other) = am_pm_alt {
            let shift = |t: Option<NaiveTime>| t.map(|t| t.overflowing_add_signed(Duration::hours(12)).0);
            alternatives.push(Parse { start: Some(other), end: shift(base.end), ..base.clone() });
        }
        Parse { alternatives, ..base }
    }

    fn title(&self) -> String {
        let mut words: Vec<&str> = self
            .tokens
            .iter()
            .zip(&self.used)
            .filter(|(_, used)| !**used)
            .map(|(t, _)| t.raw.as_str())
            .collect();
        while words.last().is_some_and(|w| JOINERS.contains(&w.to_lowercase().trim_matches(',')) || *w == "-") {
            words.pop();
        }
        while words.first().is_some_and(|w| matches!(w.to_lowercase().as_str(), "on" | "at" | "-")) {
            words.remove(0);
        }
        words.join(" ").trim_matches(|c: char| c == ',' || c.is_whitespace()).to_string()
    }
}

const WEEKDAYS: [Weekday; 5] = [Weekday::Mon, Weekday::Tue, Weekday::Wed, Weekday::Thu, Weekday::Fri];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Unit {
    Minute,
    Hour,
    Day,
    Week,
    Month,
    Year,
}

fn unit(w: &str) -> Option<Unit> {
    Some(match w {
        "min" | "mins" | "minute" | "minutes" | "m" => Unit::Minute,
        "h" | "hr" | "hrs" | "hour" | "hours" => Unit::Hour,
        "day" | "days" | "d" => Unit::Day,
        "week" | "weeks" | "wk" | "wks" => Unit::Week,
        "month" | "months" => Unit::Month,
        "year" | "years" | "yr" | "yrs" => Unit::Year,
        _ => return None,
    })
}

fn number(w: &str) -> Option<u32> {
    if let Ok(n) = w.parse::<u32>() {
        return Some(n);
    }
    const WORDS: [&str; 13] =
        ["zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten", "eleven", "twelve"];
    WORDS.iter().position(|n| *n == w).map(|n| n as u32)
}

fn ordinal(w: &str) -> Option<u32> {
    let digits = w.strip_suffix("st").or_else(|| w.strip_suffix("nd")).or_else(|| w.strip_suffix("rd")).or_else(|| w.strip_suffix("th"))?;
    digits.parse().ok().filter(|d| (1..=31).contains(d))
}

fn year(w: &str) -> Option<i32> {
    w.parse().ok().filter(|y| (1970..=2200).contains(y))
}

fn weekday(w: &str) -> Option<Weekday> {
    Some(match w {
        "mon" | "monday" => Weekday::Mon,
        "tue" | "tues" | "tuesday" => Weekday::Tue,
        "wed" | "weds" | "wednesday" => Weekday::Wed,
        "thu" | "thur" | "thurs" | "thursday" => Weekday::Thu,
        "fri" | "friday" => Weekday::Fri,
        "sat" | "saturday" => Weekday::Sat,
        "sun" | "sunday" => Weekday::Sun,
        _ => return None,
    })
}

fn month(w: &str) -> Option<u32> {
    const NAMES: [&str; 12] =
        ["january", "february", "march", "april", "may", "june", "july", "august", "september", "october", "november", "december"];
    if w == "sept" {
        return Some(9);
    }
    NAMES.iter().position(|m| *m == w || (w.len() >= 3 && m.starts_with(w))).map(|m| m as u32 + 1)
}

fn meridiem(w: &str) -> Option<Meridiem> {
    match w {
        "am" | "a.m" | "a.m." => Some(Meridiem::Am),
        "pm" | "p.m" | "p.m." => Some(Meridiem::Pm),
        _ => None,
    }
}

fn named_time(w: &str) -> Option<Clock> {
    let (hour, meridiem) = match w {
        "noon" | "midday" => (12, Meridiem::Pm),
        "midnight" => (12, Meridiem::Am),
        _ => return None,
    };
    Some(Clock { hour, minute: 0, meridiem: Some(meridiem), explicit: true })
}

/// "2", "2pm", "2:30", "2:30pm", "14:30", "9a", "930am".
fn clock(w: &str) -> Option<Clock> {
    if let Some(c) = named_time(w) {
        return Some(c);
    }
    let (body, meridiem) = if let Some(b) = w.strip_suffix("am").or_else(|| w.strip_suffix('a')) {
        (b, Some(Meridiem::Am))
    } else if let Some(b) = w.strip_suffix("pm").or_else(|| w.strip_suffix('p')) {
        (b, Some(Meridiem::Pm))
    } else {
        (w, None)
    };
    if body.is_empty() || !body.chars().all(|c| c.is_ascii_digit() || c == ':' || c == '.') {
        return None;
    }
    let (hour, minute, explicit) = if let Some((h, m)) = body.split_once([':', '.']) {
        let h: u32 = h.parse().ok()?;
        let m: u32 = m.parse().ok()?;
        (h, m, h > 12 || body.starts_with('0'))
    } else if body.len() == 3 || body.len() == 4 {
        // "930am", "1430"
        let n: u32 = body.parse().ok()?;
        if meridiem.is_none() && body.len() == 4 && n / 100 > 23 {
            return None;
        }
        if meridiem.is_none() && body.len() == 4 {
            // Four bare digits are usually a year, not a time.
            return None;
        }
        (n / 100, n % 100, n / 100 > 12)
    } else {
        (body.parse().ok()?, 0, false)
    };
    let explicit = explicit || hour > 12 || hour == 0;
    if hour > 23 || minute > 59 || (meridiem.is_some() && (hour == 0 || hour > 12)) {
        return None;
    }
    Some(Clock { hour, minute, meridiem, explicit })
}

/// "2-3pm" means both pm; "11am-1" means 1pm.
fn meridiem_pair(mut start: Clock, mut end: Clock) -> (Clock, Clock) {
    if start.meridiem.is_none() && !start.explicit && let Some(m) = end.meridiem {
        start.meridiem = Some(if m == Meridiem::Pm && start.hour > end.hour && start.hour != 12 { Meridiem::Am } else { m });
    }
    if end.meridiem.is_none() && !end.explicit && let Some(m) = start.meridiem {
        let to_pm = m == Meridiem::Am && (end.hour < start.hour || end.hour == 12);
        end.meridiem = Some(if to_pm { Meridiem::Pm } else { m });
    }
    (start, end)
}

/// The time, and the other half of the day when am/pm was a guess.
fn resolve(c: Clock, day_part: Option<Meridiem>) -> (NaiveTime, Option<NaiveTime>) {
    let make = |hour: u32| NaiveTime::from_hms_opt(hour, c.minute, 0).unwrap_or(NaiveTime::MIN);
    let to24 = |m: Meridiem| match (m, c.hour) {
        (Meridiem::Am, 12) => 0,
        (Meridiem::Am, h) => h,
        (Meridiem::Pm, 12) => 12,
        (Meridiem::Pm, h) => h + 12,
    };
    if let Some(m) = c.meridiem.or(day_part) {
        return (make(to24(m)), None);
    }
    if c.explicit {
        return (make(c.hour), None);
    }
    // Bare 1-6 is usually afternoon, 7-11 morning; 12 is noon.
    let guess = if (1..=6).contains(&c.hour) { Meridiem::Pm } else { Meridiem::Am };
    let other = if guess == Meridiem::Pm { Meridiem::Am } else { Meridiem::Pm };
    if c.hour == 12 {
        return (make(12), None);
    }
    (make(to24(guess)), Some(make(to24(other))))
}

fn compact_duration(w: &str) -> Option<Duration> {
    let split = w.find(|c: char| c.is_ascii_alphabetic())?;
    let (n, u) = w.split_at(split);
    let n: f64 = n.parse().ok()?;
    let minutes = match unit(u)? {
        Unit::Minute => n,
        Unit::Hour => n * 60.0,
        _ => return None,
    };
    Some(Duration::minutes(minutes.round() as i64))
}

fn round_minute(t: NaiveTime) -> NaiveTime {
    NaiveTime::from_hms_opt(t.hour(), t.minute(), 0).unwrap_or(t)
}

/// The next `day` after `today` (or `today` itself when `inclusive`).
fn next_weekday(today: NaiveDate, day: Weekday, inclusive: bool) -> NaiveDate {
    let ahead = (day.num_days_from_monday() + 7 - today.weekday().num_days_from_monday()) % 7;
    let ahead = if ahead == 0 && !inclusive { 7 } else { ahead };
    today + Duration::days(ahead as i64)
}

/// The next date with this day of the month (this month if it hasn't passed).
fn month_day_from(today: NaiveDate, day: u32) -> Option<NaiveDate> {
    (0..13).filter_map(|m| today.with_day(1)?.checked_add_months(Months::new(m))?.with_day(day)).find(|d| *d >= today)
}

/// A month and day, this year unless it has passed (then next year).
fn month_date(today: NaiveDate, month: u32, day: u32, year: Option<i32>) -> Option<NaiveDate> {
    if let Some(year) = year {
        return NaiveDate::from_ymd_opt(year, month, day);
    }
    let this = NaiveDate::from_ymd_opt(today.year(), month, day)?;
    if this >= today { Some(this) } else { NaiveDate::from_ymd_opt(today.year() + 1, month, day) }
}

/// "2026-12-24", "12/24", "12/24/26", "12/24/2026" (US order).
fn numeric_date(w: &str, today: NaiveDate) -> Option<NaiveDate> {
    if let Some((y, rest)) = w.split_once('-')
        && let Some((m, d)) = rest.split_once('-')
        && y.len() == 4
    {
        return NaiveDate::from_ymd_opt(y.parse().ok()?, m.parse().ok()?, d.parse().ok()?);
    }
    let parts: Vec<&str> = w.split('/').collect();
    match parts.as_slice() {
        [m, d] => month_date(today, m.parse().ok()?, d.parse().ok()?, None),
        [m, d, y] => {
            let y: i32 = y.parse().ok()?;
            let y = if y < 100 { 2000 + y } else { y };
            NaiveDate::from_ymd_opt(y, m.parse().ok()?, d.parse().ok()?)
        }
        _ => None,
    }
}

fn ical_day(d: Weekday) -> &'static str {
    match d {
        Weekday::Mon => "MO",
        Weekday::Tue => "TU",
        Weekday::Wed => "WE",
        Weekday::Thu => "TH",
        Weekday::Fri => "FR",
        Weekday::Sat => "SA",
        Weekday::Sun => "SU",
    }
}

#[cfg(test)]
mod tests;
