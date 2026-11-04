use crate::{registry::error, *};
use nyx_accounts_native::remote::Remote;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

fn date(value: When, google: bool) -> Result<Value, Error> {
    Ok(match value {
        When::Date { year, month, day } => {
            if google {
                json!({"date":format!("{year:04}-{month:02}-{day:02}")})
            } else {
                json!({"dateTime":format!("{year:04}-{month:02}-{day:02}T00:00:00"),"timeZone":"UTC"})
            }
        }
        When::Time(seconds) => {
            let time = chrono::DateTime::from_timestamp(seconds, 0).ok_or_else(|| error("Invalid event time"))?;
            if google {
                json!({"dateTime":time.to_rfc3339(),"timeZone":"UTC"})
            } else {
                json!({"dateTime":time.format("%Y-%m-%dT%H:%M:%S").to_string(),"timeZone":"UTC"})
            }
        }
    })
}
fn event_body(item: &Item, payload: &str, google: bool, skip_recurrence: bool) -> Result<Value, Error> {
    let start = item.start.ok_or_else(|| error("Event has no start"))?;
    let end = item.end.unwrap_or(start);
    let mut value = if google {
        json!({"summary":item.summary,"description":item.description,"location":item.location,"start":date(start,true)?,"end":date(end,true)?,"status":match item.status{Status::Cancelled=>"cancelled",Status::Tentative=>"tentative",_=>"confirmed"}})
    } else {
        json!({"subject":item.summary,"body":{"contentType":"text","content":item.description},"location":{"displayName":item.location},"start":date(start,false)?,"end":date(end,false)?,"isAllDay":item.all_day(),"showAs":if item.status==Status::Tentative{"tentative"}else{"busy"}})
    };
    let event_zone = payload.lines().find_map(|line| line.strip_prefix("DTSTART;TZID=").and_then(|s| s.split(':').next()));
    if !google {
        if let Some(zone) = event_zone.and_then(crate::zones::zone) {
            for (key, when) in [("start", item.start), ("end", item.end)] {
                if let Some(When::Time(seconds)) = when {
                    let time = chrono::DateTime::from_timestamp(seconds, 0)
                        .ok_or_else(|| error("Invalid event time"))?
                        .with_timezone(&zone);
                    value[key] = json!({"dateTime":time.format("%Y-%m-%dT%H:%M:%S").to_string(),"timeZone":zone.name()});
                }
            }
        }
        if item.alarms.len() > 1 || item.alarms.iter().any(|m| *m < 0) {
            return Err(error("Microsoft supports one nonnegative reminder; your edit is retained"));
        }
        value["isReminderOn"] = json!(!item.alarms.is_empty());
        value["reminderMinutesBeforeStart"] = json!(item.alarms.first().copied().unwrap_or(0));
    }
    if google {
        let recurrence: Vec<_> = payload
            .replace("\r\n ", "")
            .replace("\r\n\t", "")
            .lines()
            .filter(|l| ["RRULE:", "RDATE:", "RDATE;", "EXDATE:", "EXDATE;", "EXRULE:"].iter().any(|p| l.starts_with(p)))
            .map(str::to_owned)
            .collect();
        value["recurrence"] = json!(recurrence);
        if let Some(zone) = payload.lines().find_map(|line| line.strip_prefix("DTSTART;TZID=").and_then(|s| s.split(':').next()))
        {
            let canonical = crate::zones::zone(zone).map(|z|z.name().to_owned())
                .ok_or_else(||error("The provider cannot represent this event's time zone; your edit is retained"))?;
            value["start"]["timeZone"] = json!(canonical);
            value["end"]["timeZone"] = json!(canonical);
        }
        value["reminders"] = json!({"useDefault":false,"overrides":item.alarms.iter().map(|minutes|json!({"method":"popup","minutes":minutes})).collect::<Vec<_>>()});
    } else if skip_recurrence {
    } else if item.rule.is_some() {
        value["recurrence"] = graph_recurrence(item, payload)?;
    } else {
        value["recurrence"] = Value::Null;
    }
    if !item.attendees.is_empty() {
        value["attendees"] = json!(
            item.attendees
                .iter()
                .map(|a| if google {
                    json!({"email":a.email,"displayName":a.name})
                } else {
                    json!({"emailAddress":{"address":a.email,"name":a.name},"type":"required"})
                })
                .collect::<Vec<_>>()
        );
    }
    Ok(value)
}
fn graph_recurrence(item: &Item, payload: &str) -> Result<Value, Error> {
    use chrono::{Datelike, TimeZone};
    let rule = item.rule.as_ref().ok_or_else(|| error("Missing recurrence rule"))?;
    let parts: std::collections::HashMap<_, _> = rule.split(';').filter_map(|s| s.split_once('=')).collect();
    let zone = payload
        .lines()
        .find_map(|line| line.strip_prefix("DTSTART;TZID=").and_then(|s| s.split(':').next()))
        .and_then(crate::zones::zone)
        .unwrap_or(chrono_tz::UTC);
    let start = match item.start {
        Some(When::Time(t)) => {
            chrono::Utc.timestamp_opt(t, 0).single().ok_or_else(|| error("Invalid start"))?.with_timezone(&zone).date_naive()
        }
        Some(When::Date { year, month, day }) => {
            chrono::NaiveDate::from_ymd_opt(year, month, day).ok_or_else(|| error("Invalid date"))?
        }
        _ => return Err(error("Repeating event has no start")),
    };
    let interval = parts.get("INTERVAL").and_then(|s| s.parse::<u32>().ok()).unwrap_or(1);
    let mut pattern = json!({"interval":interval});
    match parts.get("FREQ").copied() {
        Some("DAILY") => {
            if parts.contains_key("BYDAY") || parts.contains_key("BYMONTHDAY") || parts.contains_key("BYMONTH") {
                return Err(error("Restricted daily recurrence cannot be represented safely"));
            }
            pattern["type"] = json!("daily");
        }
        Some("WEEKLY") => {
            if parts.contains_key("BYMONTHDAY") || parts.contains_key("BYMONTH") {
                return Err(error("Restricted weekly recurrence cannot be represented safely"));
            }
            pattern["type"] = json!("weekly");
            pattern["firstDayOfWeek"] = json!("monday");
            let mut days = Vec::new();
            for day in parts.get("BYDAY").copied().unwrap_or(weekday(start.weekday())).split(',') {
                days.push(match day {
                    "MO" => "monday",
                    "TU" => "tuesday",
                    "WE" => "wednesday",
                    "TH" => "thursday",
                    "FR" => "friday",
                    "SA" => "saturday",
                    "SU" => "sunday",
                    _ => return Err(error("Unsupported weekly recurrence")),
                });
            }
            pattern["daysOfWeek"] = json!(days);
        }
        Some("MONTHLY") | Some("YEARLY") => {
            let yearly = parts["FREQ"] == "YEARLY";
            if let Some(day) = parts.get("BYDAY") {
                if parts.contains_key("BYMONTHDAY") {
                    return Err(error("Combined month-day and weekday restrictions cannot be represented safely"));
                }
                let mut weekdays = Vec::new();
                let mut ordinal = parts.get("BYSETPOS").copied();
                for day in day.split(',') {
                    let (index, day) = if day.len() > 2 { day.split_at(day.len() - 2) } else { ("", day) };
                    if !index.is_empty() {
                        if ordinal.is_some_and(|o| o != index) {
                            return Err(error("Mixed recurrence ordinals are unsupported"));
                        }
                        ordinal = Some(index);
                    }
                    weekdays.push(match day {
                        "MO" => "monday",
                        "TU" => "tuesday",
                        "WE" => "wednesday",
                        "TH" => "thursday",
                        "FR" => "friday",
                        "SA" => "saturday",
                        "SU" => "sunday",
                        _ => return Err(error("Unsupported recurrence day")),
                    });
                }
                let index =
                    ordinal.ok_or_else(|| error("A monthly weekday set without an ordinal cannot be represented safely"))?;
                pattern["type"] = json!(if yearly { "relativeYearly" } else { "relativeMonthly" });
                pattern["index"] = json!(match index {
                    "1" => "first",
                    "2" => "second",
                    "3" => "third",
                    "4" => "fourth",
                    "-1" => "last",
                    _ => return Err(error("Unsupported ordinal recurrence")),
                });
                pattern["daysOfWeek"] = json!(weekdays);
            } else {
                pattern["type"] = json!(if yearly { "absoluteYearly" } else { "absoluteMonthly" });
                pattern["dayOfMonth"] = json!(parts.get("BYMONTHDAY").and_then(|s| s.parse::<u32>().ok()).unwrap_or(start.day()));
            }
            if yearly {
                pattern["month"] = json!(parts.get("BYMONTH").and_then(|s| s.parse::<u32>().ok()).unwrap_or(start.month()));
            }
        }
        _ => return Err(error("This recurrence cannot be represented by Microsoft Graph without changing its meaning")),
    }
    for key in parts.keys() {
        if !["FREQ", "INTERVAL", "BYDAY", "BYMONTHDAY", "BYMONTH", "COUNT", "UNTIL", "WKST", "BYSETPOS"].contains(key) {
            return Err(error("Unsupported recurrence restriction; upload refused"));
        }
    }
    let mut range = json!({"type":"noEnd","startDate":start.to_string(),"recurrenceTimeZone":zone.name()});
    if let Some(count) = parts.get("COUNT").and_then(|s| s.parse::<u32>().ok()) {
        range["type"] = json!("numbered");
        range["numberOfOccurrences"] = json!(count);
    } else if let Some(until) = parts.get("UNTIL") {
        let day = if until.ends_with('Z') {
            chrono::NaiveDateTime::parse_from_str(until, "%Y%m%dT%H%M%SZ")
                .map_err(error)?
                .and_utc()
                .with_timezone(&zone)
                .date_naive()
        } else {
            until
                .get(..8)
                .and_then(|d| chrono::NaiveDate::parse_from_str(d, "%Y%m%d").ok())
                .ok_or_else(|| error("Invalid recurrence end"))?
        };
        range["type"] = json!("endDate");
        range["endDate"] = json!(day.to_string());
    }
    Ok(json!({"pattern":pattern,"range":range}))
}
fn weekday(day: chrono::Weekday) -> &'static str {
    match day {
        chrono::Weekday::Mon => "MO",
        chrono::Weekday::Tue => "TU",
        chrono::Weekday::Wed => "WE",
        chrono::Weekday::Thu => "TH",
        chrono::Weekday::Fri => "FR",
        chrono::Weekday::Sat => "SA",
        chrono::Weekday::Sun => "SU",
    }
}
pub(crate) fn stable_id(source: &str, uid: &str, rid: &str) -> String {
    let digest = Sha256::digest(format!("{source}:{uid}:{rid}"));
    digest.iter().map(|b| format!("{b:02x}")).collect()
}
impl Registry {
    pub(crate) fn api_resource(&self, source: &str, uid: &str, rid: &str) -> Option<(String, String)> {
        self.db
            .borrow()
            .query_row(
                "SELECT remote_id,etag FROM api_resources WHERE source=?1 AND uid=?2 AND rid=?3",
                rusqlite::params![source, uid, rid],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .ok()
    }
    pub(crate) fn save_api_resource(&self, source: &str, uid: &str, rid: &str, id: &str, etag: &str) -> Result<(), Error> {
        let duplicate: Option<(String, String)> = self
            .db
            .borrow()
            .query_row(
                "SELECT uid,rid FROM api_resources WHERE source=?1 AND remote_id=?2 AND (uid<>?3 OR rid<>?4)",
                rusqlite::params![source, id, uid, rid],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .ok();
        if let Some((other, other_rid)) = duplicate {
            let dirty: bool = self
                .db
                .borrow()
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM objects WHERE source=?1 AND uid=?2 AND rid=?3 AND dirty=1)",
                    rusqlite::params![source, other, other_rid],
                    |r| r.get(0),
                )
                .map_err(error)?;
            if dirty {
                return Err(error("Two local edits refer to the same remote event; both are retained for resolution"));
            }
            self.db
                .borrow()
                .execute(
                    "UPDATE objects SET deleted=1 WHERE source=?1 AND uid=?2 AND rid=?3",
                    rusqlite::params![source, other, other_rid],
                )
                .map_err(error)?;
            self.db
                .borrow()
                .execute(
                    "DELETE FROM api_resources WHERE source=?1 AND uid=?2 AND rid=?3",
                    rusqlite::params![source, other, other_rid],
                )
                .map_err(error)?;
        }
        self.db.borrow().execute("INSERT INTO api_resources VALUES(?1,?2,?3,?4,?5) ON CONFLICT(source,uid,rid) DO UPDATE SET remote_id=excluded.remote_id,etag=excluded.etag",rusqlite::params![source,uid,rid,id,etag]).map_err(error)?;
        Ok(())
    }
    fn resolve_api_instance(
        &self,
        source: &str,
        endpoint: &str,
        remote: &Remote,
        calendar: &Calendar,
        uid: &str,
        rid: &str,
    ) -> Result<Option<(String, String)>, Error> {
        if let Some(resource) = self.api_resource(source, uid, rid) {
            return Ok(Some(resource));
        }
        let Some((master, _)) = self.api_resource(source, uid, "") else { return Ok(None) };
        let google = remote.account.provider == "google";
        let payload = calendar.ics(uid, None)?;
        let zone =
            payload.lines().find_map(|l| l.strip_prefix("DTSTART;TZID=").and_then(|s| s.split(':').next())).unwrap_or("UTC");
        let occurrence = unsafe { crate::Owned::new(ical_glib_sys::i_cal_time_new_from_string(crate::cstring(rid).as_ptr())) }
            .ok_or_else(|| error("Invalid recurrence identity"))?;
        let stamp = unsafe { ical_glib_sys::i_cal_time_as_timet_with_zone(occurrence.ptr(), crate::ical::zone(zone)) };
        let mut url = url::Url::parse(endpoint).map_err(error)?;
        url.path_segments_mut().map_err(|_| error("Invalid instance endpoint"))?.pop_if_empty().push(&master).push("instances");
        let from = chrono::DateTime::from_timestamp(stamp - 86400, 0).ok_or_else(|| error("Invalid occurrence"))?.to_rfc3339();
        let to = chrono::DateTime::from_timestamp(stamp + 86400, 0).ok_or_else(|| error("Invalid occurrence"))?.to_rfc3339();
        if google {
            url.query_pairs_mut().append_pair("timeMin", &from).append_pair("timeMax", &to).append_pair("showDeleted", "true");
        } else {
            url.query_pairs_mut().append_pair("startDateTime", &from).append_pair("endDateTime", &to);
        }
        for value in crate::api::pages(remote, url.as_str(), if google { "items" } else { "value" })? {
            let candidate = if google {
                crate::api::time(&value["originalStartTime"]).and_then(|s| s.split_once(':').map(|(_, s)| s.to_owned())).and_then(
                    |s| {
                        let time =
                            unsafe { crate::Owned::new(ical_glib_sys::i_cal_time_new_from_string(crate::cstring(&s).as_ptr())) }?;
                        Some(unsafe { ical_glib_sys::i_cal_time_as_timet_with_zone(time.ptr(), crate::ical::zone(zone)) })
                    },
                )
            } else {
                value["originalStart"].as_str().and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok()).map(|t| t.timestamp())
            };
            if candidate != Some(stamp) {
                continue;
            }
            let id = value["id"].as_str().ok_or_else(|| error("Instance has no provider ID"))?;
            let etag = value[if google { "etag" } else { "@odata.etag" }].as_str().unwrap_or("");
            self.save_api_resource(source, uid, rid, id, etag)?;
            if let Some(payload) = crate::api::event(&value, google) {
                self.db.borrow().execute("INSERT INTO api_baselines VALUES(?1,?2,?3,?4) ON CONFLICT(source,uid,rid) DO UPDATE SET payload=excluded.payload",rusqlite::params![source,uid,rid,payload]).map_err(error)?;
            }
            return Ok(Some((id.into(), etag.into())));
        }
        Err(error("Provider occurrence could not be resolved safely; your local edit is retained"))
    }
    pub(crate) fn respond_api(
        &self,
        source_id: &str,
        uid: &str,
        rid: Option<&str>,
        emails: &[String],
        response: Response,
    ) -> Result<(), Error> {
        let source = self.source(source_id, Kind::Calendar).ok_or_else(|| error("Calendar not found"))?;
        let account_id = source.account.ok_or_else(|| error("Account not found"))?;
        let endpoint: String = self
            .db
            .borrow()
            .query_row("SELECT endpoint FROM api_endpoints WHERE source=?1", [source_id], |r| r.get(0))
            .map_err(error)?;
        let dirty: bool = self
            .db
            .borrow()
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM objects WHERE source=?1 AND uid=?2 AND dirty=1)",
                rusqlite::params![source_id, uid],
                |r| r.get(0),
            )
            .map_err(error)?;
        if dirty {
            return Err(error("Sync this event’s pending edits before replying"));
        }
        let client = nyx_accounts_native::Client::connect().map_err(error)?;
        let account =
            client.list().map_err(error)?.into_iter().find(|a| a.id == account_id).ok_or_else(|| error("Account not found"))?;
        let secret = client.credential(&account_id, nyx_accounts_native::Service::Calendar).map_err(error)?;
        let remote = Remote::new(account, secret).map_err(error)?.for_service(nyx_accounts_native::Service::Calendar);
        let calendar = Calendar::open(self, source_id, Kind::Calendar, None)?;
        let resource = if let Some(rid) = rid {
            self.resolve_api_instance(source_id, &endpoint, &remote, &calendar, uid, rid)?
        } else {
            self.api_resource(source_id, uid, "")
        }
        .ok_or_else(|| error("Provider event has not been resolved"))?;
        let mut target = url::Url::parse(&endpoint).map_err(error)?;
        target.path_segments_mut().map_err(|_| error("Invalid event endpoint"))?.pop_if_empty().push(&resource.0);
        let google = remote.account.provider == "google";
        let updated = if google {
            let current = remote.json(target.as_str()).map_err(error)?;
            let mut attendees = current["attendees"].as_array().cloned().ok_or_else(|| error("Event has no guest list"))?;
            let answer = match response {
                Response::Accepted => "accepted",
                Response::Declined => "declined",
                Response::Tentative => "tentative",
                Response::NeedsAction => "needsAction",
                _ => return Err(error("Unsupported invitation response")),
            };
            let mut changed = false;
            for attendee in &mut attendees {
                if attendee["email"].as_str().is_some_and(|email| emails.iter().any(|me| me.eq_ignore_ascii_case(email))) {
                    attendee["responseStatus"] = json!(answer);
                    changed = true;
                }
            }
            if !changed {
                return Err(error("You are not on this event’s guest list"));
            }
            remote
                .json_write(target.as_str(), reqwest::Method::PATCH, &json!({"attendees":attendees}), current["etag"].as_str())
                .map_err(error)?
        } else {
            let action = match response {
                Response::Accepted => "accept",
                Response::Declined => "decline",
                Response::Tentative => "tentativelyAccept",
                _ => return Err(error("This response is not supported by Microsoft")),
            };
            let mut action_url = target.clone();
            action_url.path_segments_mut().map_err(|_| error("Invalid invitation endpoint"))?.push(action);
            remote.json_write(action_url.as_str(), reqwest::Method::POST, &json!({"sendResponse":true}), None).map_err(error)?;
            remote.json(target.as_str()).unwrap_or(Value::Null)
        };
        if let Some(etag) = updated[if google { "etag" } else { "@odata.etag" }].as_str() {
            self.save_api_resource(source_id, uid, rid.unwrap_or(""), &resource.0, etag)?;
        }
        Ok(())
    }
    pub(crate) fn upload_api_calendar(&self, source: &str, endpoint: &str, remote: &Remote) -> Result<(), Error> {
        let google = remote.account.provider == "google";
        let calendar = Calendar::open(self, source, Kind::Calendar, None)?;
        let pending: Vec<(String, String, String, bool)> = {
            let db = self.db.borrow();
            let mut q = db
                .prepare("SELECT uid,rid,payload,deleted FROM objects WHERE source=?1 AND dirty=1 ORDER BY rid")
                .map_err(error)?;
            q.query_map([source], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
                .map_err(error)?
                .filter_map(Result::ok)
                .collect()
        };
        for (uid, rid, payload, deleted) in pending {
            let existing = if rid.is_empty() {
                self.api_resource(source, &uid, &rid)
            } else {
                self.resolve_api_instance(source, endpoint, remote, &calendar, &uid, &rid)?
            };
            if existing.is_none() && !rid.is_empty() {
                return Err(error(
                    "Resolve this occurrence against the provider before uploading an exception; your edit is retained",
                ));
            }
            let mut target = url::Url::parse(endpoint).map_err(error)?;
            if let Some((id, _)) = &existing {
                target.path_segments_mut().map_err(|_| error("Invalid API endpoint"))?.pop_if_empty().push(id);
            }
            if deleted {
                if let Some((_, etag)) = &existing {
                    if etag.is_empty() {
                        return Err(error("Safe deletion requires a known revision"));
                    }
                    remote.json_write(target.as_str(), reqwest::Method::DELETE, &Value::Null, Some(etag)).map_err(error)?;
                }
            } else {
                let component = calendar.component(&payload)?;
                let item = calendar.read(component.ptr());
                let baseline: Option<String> = self
                    .db
                    .borrow()
                    .query_row(
                        "SELECT payload FROM api_baselines WHERE source=?1 AND uid=?2 AND rid=?3",
                        rusqlite::params![source, uid, rid],
                        |r| r.get(0),
                    )
                    .ok();
                let skip_recurrence = baseline
                    .as_ref()
                    .and_then(|p| calendar.component(p).ok())
                    .map(|c| calendar.read(c.ptr()).rule == item.rule)
                    .unwrap_or(false);
                let mut body = event_body(&item, &payload, google, skip_recurrence || !rid.is_empty())?;
                if !rid.is_empty() {
                    body.as_object_mut().unwrap().remove("recurrence");
                }
                let method = if existing.is_some() { reqwest::Method::PATCH } else { reqwest::Method::POST };
                if existing.is_none() {
                    let stable = stable_id(source, &uid, &rid);
                    if google {
                        body["id"] = json!(stable);
                        body["extendedProperties"] = json!({"private":{"nyxUid":uid}});
                    } else {
                        body["transactionId"] = json!(stable);
                    }
                }
                if let Ok(original_payload) = self
                    .db
                    .borrow()
                    .query_row(
                        "SELECT payload FROM api_baselines WHERE source=?1 AND uid=?2 AND rid=?3",
                        rusqlite::params![source, uid, rid],
                        |r| r.get::<_, String>(0),
                    )
                {
                    let original = calendar.component(&original_payload)?;
                    let original = calendar.read(original.ptr());
                    let fields = body.as_object_mut().ok_or_else(|| error("Invalid event update"))?;
                    for (same, keys) in [
                        (item.summary == original.summary, if google { vec!["summary"] } else { vec!["subject"] }),
                        (item.description == original.description, if google { vec!["description"] } else { vec!["body"] }),
                        (item.location == original.location, vec!["location"]),
                        (item.start == original.start && item.end == original.end, vec!["start", "end", "isAllDay"]),
                        (item.rule == original.rule, vec!["recurrence"]),
                        (item.alarms == original.alarms, vec!["reminders", "isReminderOn", "reminderMinutesBeforeStart"]),
                        (item.status == original.status, vec!["status", "showAs"]),
                        (item.attendees == original.attendees, vec!["attendees"]),
                    ] {
                        if same {
                            for key in keys {
                                fields.remove(key);
                            }
                        }
                    }
                    if !google && item.status == Status::Cancelled && original.status != Status::Cancelled {
                        return Err(error("Use the provider cancellation action for this meeting; your change is retained"));
                    }
                    if item.url != original.url
                        || item.categories != original.categories
                        || item.organizer != original.organizer
                        || item.priority != original.priority
                    {
                        return Err(error("This edited field is not supported by the provider; your local edit is retained"));
                    }
                }
                let etag = existing.as_ref().map(|(_, etag)| etag.as_str());
                if etag == Some("") {
                    return Err(error("Safe update requires a known revision"));
                }
                let response = remote.json_write(target.as_str(), method, &body, etag).map_err(error)?;
                let id = response["id"]
                    .as_str()
                    .or_else(|| existing.as_ref().map(|(id, _)| id.as_str()))
                    .ok_or_else(|| error("Provider did not return an event ID"))?;
                let etag = response[if google { "etag" } else { "@odata.etag" }].as_str().unwrap_or("");
                self.save_api_resource(source, &uid, &rid, id, etag)?;
                self.db.borrow().execute("INSERT INTO api_baselines VALUES(?1,?2,?3,?4) ON CONFLICT(source,uid,rid) DO UPDATE SET payload=excluded.payload",rusqlite::params![source,uid,rid,payload]).map_err(error)?;
            }
            self.db
                .borrow()
                .execute(
                    "UPDATE objects SET dirty=0 WHERE source=?1 AND uid=?2 AND rid=?3 AND payload=?4 AND deleted=?5",
                    rusqlite::params![source, uid, rid, payload, deleted],
                )
                .map_err(error)?;
        }
        Ok(())
    }
}
fn task_body(item: &Item, google: bool, payload: &str, skip_recurrence: bool) -> Result<Value, Error> {
    if item.url.is_some() || !item.location.is_empty() {
        return Err(error("This task field is not supported by the provider; your local edit is retained"));
    }
    if google {
        if item.rule.is_some()
            || !item.alarms.is_empty()
            || item.priority.is_some()
            || !item.categories.is_empty()
            || item.start.is_some()
        {
            return Err(error("Google Tasks does not support these task fields; your local edit is retained"));
        }
        let due = match item.due {
            Some(When::Date { year, month, day }) => json!(format!("{year:04}-{month:02}-{day:02}T00:00:00Z")),
            None => Value::Null,
            _ => return Err(error("Google Tasks supports date-only deadlines")),
        };
        Ok(
            json!({"title":item.summary,"notes":item.description,"due":due,"status":if item.is_done(){"completed"}else{"needsAction"}}),
        )
    } else {
        if item.parent.is_some() {
            return Err(error("Microsoft task nesting is not supported by this adapter"));
        }
        let mut body = json!({"title":item.summary,"body":{"contentType":"text","content":item.description},"status":if item.is_done(){"completed"}else if item.status==Status::InProcess{"inProgress"}else{"notStarted"},"importance":match item.priority{Some(1)=>"high",Some(9)=>"low",None|Some(5)=>"normal",_=>return Err(error("Microsoft supports high, normal and low task priorities"))},"categories":item.categories});
        body["dueDateTime"] = item.due.map(|d| date(d, false)).transpose()?.unwrap_or(Value::Null);
        body["startDateTime"] = item.start.map(|d| date(d, false)).transpose()?.unwrap_or(Value::Null);
        if item.alarms.len() > 1 {
            return Err(error("Microsoft supports one task reminder"));
        }
        body["isReminderOn"] = json!(!item.alarms.is_empty());
        body["reminderDateTime"] = if let Some(minutes) = item.alarms.first() {
            let base = item.due.or(item.start).ok_or_else(|| error("Set a due or start date for this reminder"))?;
            let time = crate::ical::time(base, crate::ical::zone(&crate::local_zone()));
            let stamp =
                unsafe { ical_glib_sys::i_cal_time_as_timet_with_zone(time.ptr(), crate::ical::zone(&crate::local_zone())) };
            date(When::Time(stamp - minutes * 60), false)?
        } else {
            Value::Null
        };
        if !skip_recurrence {
            body["recurrence"] = if item.rule.is_some() {
                let mut item = item.clone();
                item.start = item.start.or(item.due);
                graph_recurrence(&item, payload)?
            } else {
                Value::Null
            };
        }
        Ok(body)
    }
}
impl Registry {
    pub(crate) fn upload_api_tasks(&self, source: &str, endpoint: &str, remote: &Remote) -> Result<(), Error> {
        let google = remote.account.provider == "google";
        let calendar = Calendar::open(self, source, Kind::TaskList, None)?;
        self.db
            .borrow()
            .execute_batch("CREATE TABLE IF NOT EXISTS uncertain_creates(source TEXT,uid TEXT,PRIMARY KEY(source,uid));")
            .map_err(error)?;
        let pending: Vec<(String, String, bool)> = {
            let db = self.db.borrow();
            let mut q =
                db.prepare("SELECT uid,payload,deleted FROM objects WHERE source=?1 AND dirty=1 AND rid='' ").map_err(error)?;
            q.query_map([source], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).map_err(error)?.filter_map(Result::ok).collect()
        };
        for (uid, payload, deleted) in pending {
            let existing = self.api_resource(source, &uid, "");
            let mut target = url::Url::parse(endpoint).map_err(error)?;
            target.set_query(None);
            if let Some((id, _)) = &existing {
                target.path_segments_mut().map_err(|_| error("Invalid task endpoint"))?.pop_if_empty().push(id);
            }
            if deleted {
                if let Some((_, etag)) = &existing {
                    if etag.is_empty() {
                        return Err(error("Safe task deletion requires a known revision"));
                    }
                    remote.json_write(target.as_str(), reqwest::Method::DELETE, &Value::Null, Some(etag)).map_err(error)?;
                }
            } else {
                let component = calendar.component(&payload)?;
                let item = calendar.read(component.ptr());
                let original: Option<String> = self
                    .db
                    .borrow()
                    .query_row(
                        "SELECT payload FROM api_baselines WHERE source=?1 AND uid=?2 AND rid=''",
                        rusqlite::params![source, uid],
                        |r| r.get(0),
                    )
                    .ok();
                let original = original.as_ref().map(|p| calendar.component(p).map(|c| calendar.read(c.ptr()))).transpose()?;
                let mut body = task_body(&item, google, &payload, original.as_ref().is_some_and(|old| old.rule == item.rule))?;
                if let Some(original) = &original {
                    let fields = body.as_object_mut().unwrap();
                    for (same, keys) in [
                        (item.summary == original.summary, vec!["title"]),
                        (item.description == original.description, vec!["notes", "body"]),
                        (item.due == original.due, vec!["due", "dueDateTime"]),
                        (item.start == original.start, vec!["startDateTime"]),
                        (item.is_done() == original.is_done() && item.status == original.status, vec!["status"]),
                        (item.priority == original.priority, vec!["importance"]),
                        (item.categories == original.categories, vec!["categories"]),
                        (item.alarms == original.alarms, vec!["isReminderOn", "reminderDateTime"]),
                        (item.rule == original.rule, vec!["recurrence"]),
                    ] {
                        if same {
                            for key in keys {
                                fields.remove(key);
                            }
                        }
                    }
                }
                if existing.is_none() {
                    let uncertain: bool = self
                        .db
                        .borrow()
                        .query_row(
                            "SELECT EXISTS(SELECT 1 FROM uncertain_creates WHERE source=?1 AND uid=?2)",
                            rusqlite::params![source, uid],
                            |r| r.get(0),
                        )
                        .map_err(error)?;
                    if uncertain {
                        return Err(error(
                            "A previous task creation has an uncertain outcome. Check the provider before retrying; your local task is retained.",
                        ));
                    }
                    if google
                        && let Some(parent) = &item.parent {
                            let parent = self
                                .api_resource(source, parent, "")
                                .map(|(id, _)| id)
                                .ok_or_else(|| error("Sync the parent task first"))?;
                            target.query_pairs_mut().append_pair("parent", &parent);
                        }
                    self.db
                        .borrow()
                        .execute("INSERT INTO uncertain_creates VALUES(?1,?2)", rusqlite::params![source, uid])
                        .map_err(error)?;
                }
                let etag = existing.as_ref().map(|(_, etag)| etag.as_str());
                if etag == Some("") {
                    return Err(error("Safe task update requires a known revision"));
                }
                let response = match remote.json_write(
                    target.as_str(),
                    if existing.is_some() { reqwest::Method::PATCH } else { reqwest::Method::POST },
                    &body,
                    etag,
                ) {
                    Ok(response) => response,
                    Err(failure) => {
                        if existing.is_none()
                            && failure
                                .downcast_ref::<nyx_accounts_native::remote::ApiWriteError>()
                                .is_some_and(|e| (400..500).contains(&e.status) && e.status != 408)
                        {
                            let _ = self.db.borrow().execute(
                                "DELETE FROM uncertain_creates WHERE source=?1 AND uid=?2",
                                rusqlite::params![source, uid],
                            );
                        }
                        return Err(error(failure));
                    }
                };
                let id = response["id"]
                    .as_str()
                    .or_else(|| existing.as_ref().map(|(id, _)| id.as_str()))
                    .ok_or_else(|| error("Task response has no identifier"))?;
                let etag = response[if google { "etag" } else { "@odata.etag" }].as_str().unwrap_or("");
                self.save_api_resource(source, &uid, "", id, etag)?;
                if google && existing.is_some() && original.as_ref().is_some_and(|old| old.parent != item.parent) {
                    let mut move_url = target.clone();
                    move_url.path_segments_mut().map_err(|_| error("Invalid move endpoint"))?.push("move");
                    if let Some(parent) = &item.parent {
                        let parent = self
                            .api_resource(source, parent, "")
                            .map(|(id, _)| id)
                            .ok_or_else(|| error("Sync the parent task first"))?;
                        move_url.query_pairs_mut().append_pair("parent", &parent);
                    }
                    remote.json_write(move_url.as_str(), reqwest::Method::POST, &json!({}), None).map_err(error)?;
                }
                self.db
                    .borrow()
                    .execute("DELETE FROM uncertain_creates WHERE source=?1 AND uid=?2", rusqlite::params![source, uid])
                    .map_err(error)?;
                self.db.borrow().execute("INSERT INTO api_baselines VALUES(?1,?2,'',?3) ON CONFLICT(source,uid,rid) DO UPDATE SET payload=excluded.payload",rusqlite::params![source,uid,payload]).map_err(error)?;
            }
            self.db
                .borrow()
                .execute(
                    "UPDATE objects SET dirty=0 WHERE source=?1 AND uid=?2 AND rid='' AND payload=?3 AND deleted=?4",
                    rusqlite::params![source, uid, payload, deleted],
                )
                .map_err(error)?;
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn item(rule: Option<&str>) -> Item {
        let path = std::env::temp_dir().join(format!("nyx-api-fixture-{}/pim.db", uuid::Uuid::new_v4()));
        let registry = Registry::open(path).unwrap();
        let calendar = Calendar::open(&registry, "system-calendar", Kind::Calendar, None).unwrap();
        let payload = format!(
            "BEGIN:VEVENT\r\nUID:fixture\r\nDTSTART;TZID=America/Chicago:20261030T090000\r\nDTEND;TZID=America/Chicago:20261030T100000\r\nSUMMARY:Fixture\r\n{}END:VEVENT\r\n",
            rule.map(|r| format!("RRULE:{r}\r\n")).unwrap_or_default()
        );
        calendar.import(&payload).unwrap();
        calendar.get("fixture", None).unwrap()
    }
    #[test]
    fn google_recurring_events_keep_libical_named_timezones() {
        let value=event_body(&item(Some("FREQ=DAILY;COUNT=3")),
            "DTSTART;TZID=/freeassociation.sourceforge.net/America/Chicago:20261030T090000\r\nRRULE:FREQ=DAILY;COUNT=3",true,false).unwrap();
        assert_eq!(value["start"]["timeZone"],"America/Chicago");
        assert_eq!(value["end"]["timeZone"],"America/Chicago");
    }
    #[test]
    fn monthly_graph_rule_retains_ordinal_and_count() {
        let value = graph_recurrence(
            &item(Some("FREQ=MONTHLY;BYDAY=MO;BYSETPOS=-1;COUNT=3")),
            "DTSTART;TZID=America/Chicago:20261030T090000",
        )
        .unwrap();
        assert_eq!(value["pattern"]["index"], "last");
        assert_eq!(value["range"]["numberOfOccurrences"], 3);
        assert_eq!(value["range"]["recurrenceTimeZone"], "America/Chicago");
    }
    #[test]
    fn unsupported_rule_is_not_broadened() {
        assert!(graph_recurrence(&item(Some("FREQ=DAILY;BYDAY=MO,TU")), "").is_err());
    }
    #[test]
    fn stable_create_keys_differ_by_source() {
        assert_eq!(stable_id("source", "uid", ""), stable_id("source", "uid", ""));
        assert_ne!(stable_id("source", "uid", ""), stable_id("other", "uid", ""));
    }
}
