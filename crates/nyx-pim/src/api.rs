//! Provider projections retain the original JSON beside their iCalendar/vCard view.
use crate::{registry::error, *};
use nyx_accounts_native::{model::Service, remote::Remote};
use serde_json::Value;
fn text(v: &Value, k: &str) -> String {
    v.get(k).and_then(Value::as_str).unwrap_or("").into()
}
fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\n', "\\n").replace(';', "\\;").replace(',', "\\,").replace('\r', "")
}
fn body_text(value: &Value) -> String {
    let content = text(&value["body"], "content");
    if value["body"]["contentType"].as_str().is_some_and(|s| s.eq_ignore_ascii_case("html")) {
        html2text::from_read(content.as_bytes(), 100).unwrap_or(content)
    } else {
        content
    }
}
fn property(lines: &mut Vec<String>, name: &str, value: &str) {
    if !value.is_empty() {
        lines.push(format!("{name}:{}", escape(value)));
    }
}
pub(crate) fn time(v: &Value) -> Option<String> {
    if let Some(date) = v.get("date").and_then(Value::as_str) {
        return Some(format!(";VALUE=DATE:{}", date.replace('-', "")));
    }
    let value = v.get("dateTime").and_then(Value::as_str)?;
    if let Some(zone) = v.get("timeZone").and_then(Value::as_str) {
        if zone.contains('/') && zone.chars().all(|c| c.is_ascii_alphanumeric() || "/_+-".contains(c)) {
            if let Ok(value) = chrono::DateTime::parse_from_rfc3339(value) {
                return Some(format!(";TZID={zone}:{}", value.naive_local().format("%Y%m%dT%H%M%S")));
            }
        }
    }
    if let Ok(value) = chrono::DateTime::parse_from_rfc3339(value) {
        return Some(format!(":{}", value.with_timezone(&chrono::Utc).format("%Y%m%dT%H%M%SZ")));
    }
    // Graph UTC responses can omit the trailing Z.
    if v.get("timeZone").and_then(Value::as_str) == Some("UTC") {
        if let Ok(value) = chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S%.f") {
            return Some(format!(":{}Z", value.format("%Y%m%dT%H%M%S")));
        }
    }
    None
}
pub(crate) fn event(value: &Value, google: bool) -> Option<String> {
    let id = if google { text(value, "iCalUID") } else { text(value, "iCalUId") };
    if id.is_empty() {
        return None;
    }
    let mut lines = vec!["BEGIN:VEVENT".into(), format!("UID:{}", escape(&id))];
    property(&mut lines, "SUMMARY", &text(value, if google { "summary" } else { "subject" }));
    property(&mut lines, "DESCRIPTION", &if google { text(value, "description") } else { body_text(value) });
    let location = if google { text(value, "location") } else { text(&value["location"], "displayName") };
    property(&mut lines, "LOCATION", &location);
    let original_zone = value.get("originalStartTimeZone").and_then(Value::as_str);
    let all_day = value.get("isAllDay").and_then(Value::as_bool) == Some(true);
    let starts = if google {
        time(&value["start"]).or_else(|| time(&value["originalStartTime"]))
    } else {
        crate::zones::graph_time(&value["start"], original_zone, all_day)
    };
    if let Some(start) = starts {
        lines.push(format!("DTSTART{start}"));
    } else {
        return None;
    }
    let ends = if google { time(&value["end"]) } else { crate::zones::graph_time(&value["end"], original_zone, all_day) };
    if let Some(end) = ends {
        lines.push(format!("DTEND{end}"));
    }
    if let Some(rid) = time(&value["originalStartTime"]) {
        lines.push(format!("RECURRENCE-ID{rid}"));
    }
    if google {
        if let Some(rules) = value.get("recurrence").and_then(Value::as_array) {
            for rule in rules.iter().filter_map(Value::as_str) {
                if !rule.contains(['\r', '\n']) {
                    lines.push(rule.into());
                }
            }
        }
        match text(value, "status").as_str() {
            "cancelled" => lines.push("STATUS:CANCELLED".into()),
            "tentative" => lines.push("STATUS:TENTATIVE".into()),
            "confirmed" => lines.push("STATUS:CONFIRMED".into()),
            _ => {}
        }
    } else {
        if !value["recurrence"].is_null() {
            lines.push(graph_rule(&value["recurrence"], all_day, original_zone)?);
        }
    }
    if !google {
        lines.push(
            if value["isCancelled"].as_bool() == Some(true) {
                "STATUS:CANCELLED"
            } else if value["showAs"].as_str() == Some("tentative") {
                "STATUS:TENTATIVE"
            } else {
                "STATUS:CONFIRMED"
            }
            .into(),
        );
    }
    let parameter = |text: &str| text.replace('^', "^^").replace('\n', "^n").replace('\r', "").replace('"', "^'");
    let organizer = if google { &value["organizer"] } else { &value["organizer"]["emailAddress"] };
    let organizer_email = text(organizer, if google { "email" } else { "address" });
    if !organizer_email.is_empty() && !organizer_email.contains(['\r', '\n']) {
        lines.push(format!(
            "ORGANIZER;CN=\"{}\":mailto:{}",
            parameter(&text(organizer, if google { "displayName" } else { "name" })),
            organizer_email
        ));
    }
    if let Some(attendees) = value["attendees"].as_array() {
        for attendee in attendees {
            let address = if google { attendee } else { &attendee["emailAddress"] };
            let email = text(address, if google { "email" } else { "address" });
            if email.is_empty() || email.contains(['\r', '\n']) {
                continue;
            }
            let response = if google { attendee["responseStatus"].as_str() } else { attendee["status"]["response"].as_str() };
            let partstat = match response {
                Some("accepted") => "ACCEPTED",
                Some("declined") => "DECLINED",
                Some("tentative" | "tentativelyAccepted") => "TENTATIVE",
                _ => "NEEDS-ACTION",
            };
            let optional = attendee["optional"].as_bool() == Some(true) || attendee["type"].as_str() == Some("optional");
            lines.push(format!(
                "ATTENDEE;CN=\"{}\";PARTSTAT={partstat};ROLE={}:mailto:{email}",
                parameter(&text(address, if google { "displayName" } else { "name" })),
                if optional { "OPT-PARTICIPANT" } else { "REQ-PARTICIPANT" }
            ));
        }
    }
    property(&mut lines, "URL", &text(value, if google { "htmlLink" } else { "webLink" }));
    let alarms: Vec<i64> = if google {
        let reminders = if value["reminders"]["useDefault"].as_bool() == Some(true) {
            &value["nyxDefaultReminders"]
        } else {
            &value["reminders"]["overrides"]
        };
        reminders.as_array().map(|a| a.iter().filter_map(|v| v["minutes"].as_i64()).collect()).unwrap_or_default()
    } else if value["isReminderOn"].as_bool() == Some(true) {
        vec![value["reminderMinutesBeforeStart"].as_i64().unwrap_or(15)]
    } else {
        Vec::new()
    };
    for minutes in alarms {
        lines.extend([
            "BEGIN:VALARM".into(),
            "ACTION:DISPLAY".into(),
            "DESCRIPTION:Reminder".into(),
            format!("TRIGGER:-PT{minutes}M"),
            "END:VALARM".into(),
        ]);
    }
    lines.push("END:VEVENT".into());
    Some(lines.join("\r\n") + "\r\n")
}
fn graph_rule(recurrence: &Value, all_day: bool, zone: Option<&str>) -> Option<String> {
    use chrono::{NaiveDate, TimeZone};
    if recurrence.is_null() {
        return None;
    }
    let pattern = &recurrence["pattern"];
    let range = &recurrence["range"];
    let frequency = match pattern["type"].as_str()? {
        "daily" => "DAILY",
        "weekly" => "WEEKLY",
        "absoluteMonthly" | "relativeMonthly" => "MONTHLY",
        "absoluteYearly" | "relativeYearly" => "YEARLY",
        _ => return None,
    };
    let mut parts = vec![format!("FREQ={frequency}"), format!("INTERVAL={}", pattern["interval"].as_u64().unwrap_or(1))];
    let days = |day: &str| {
        Some(match day {
            "monday" => "MO",
            "tuesday" => "TU",
            "wednesday" => "WE",
            "thursday" => "TH",
            "friday" => "FR",
            "saturday" => "SA",
            "sunday" => "SU",
            _ => return None,
        })
    };
    if let Some(values) = pattern["daysOfWeek"].as_array().filter(|a| !a.is_empty()) {
        let values: Option<Vec<_>> = values.iter().map(|v| days(v.as_str()?)).collect();
        parts.push(format!("BYDAY={}", values?.join(",")));
    }
    if pattern["type"].as_str()?.starts_with("relative") {
        parts.push(format!(
            "BYSETPOS={}",
            match pattern["index"].as_str()? {
                "first" => 1,
                "second" => 2,
                "third" => 3,
                "fourth" => 4,
                "last" => -1,
                _ => return None,
            }
        ));
    }
    if let Some(day) = pattern["dayOfMonth"].as_u64().filter(|n| *n > 0) {
        parts.push(format!("BYMONTHDAY={day}"));
    }
    if let Some(month) = pattern["month"].as_u64().filter(|n| *n > 0) {
        parts.push(format!("BYMONTH={month}"));
    }
    if let Some(day) = pattern["firstDayOfWeek"].as_str().and_then(days) {
        parts.push(format!("WKST={day}"));
    }
    match range["type"].as_str()? {
        "numbered" => parts.push(format!("COUNT={}", range["numberOfOccurrences"].as_u64()?)),
        "endDate" => {
            let date = NaiveDate::parse_from_str(range["endDate"].as_str()?, "%Y-%m-%d").ok()?;
            let until = if all_day {
                date.format("%Y%m%d").to_string()
            } else {
                let zone = crate::zones::zone(zone.or_else(|| range["recurrenceTimeZone"].as_str()).unwrap_or("UTC"))?;
                zone.from_local_datetime(&date.and_hms_opt(23, 59, 59)?)
                    .single()?
                    .with_timezone(&chrono::Utc)
                    .format("%Y%m%dT%H%M%SZ")
                    .to_string()
            };
            parts.push(format!("UNTIL={until}"));
        }
        "noEnd" => {}
        _ => return None,
    }
    Some(format!("RRULE:{}", parts.join(";")))
}
pub(crate) fn card(value: &Value, google: bool) -> Option<String> {
    let uid = text(value, if google { "resourceName" } else { "id" });
    if uid.is_empty() {
        return None;
    }
    let name = if google {
        value["names"].as_array().and_then(|a| a.first()).map(|v| text(v, "displayName")).unwrap_or_default()
    } else {
        text(value, "displayName")
    };
    let mut lines =
        vec!["BEGIN:VCARD".into(), "VERSION:3.0".into(), format!("UID:{}", escape(&uid)), format!("FN:{}", escape(&name))];
    if let Some(emails) = value.get("emailAddresses").and_then(Value::as_array) {
        for email in emails {
            let kind = if google { text(email, "type") } else { String::new() };
            let kind: String = kind.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').collect();
            property(
                &mut lines,
                &if kind.is_empty() { "EMAIL".into() } else { format!("EMAIL;TYPE={kind}") },
                &text(email, if google { "value" } else { "address" }),
            );
        }
    }
    if google {
        if let Some(phones) = value.get("phoneNumbers").and_then(Value::as_array) {
            for phone in phones {
                let kind: String = text(phone, "type").chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').collect();
                property(
                    &mut lines,
                    &if kind.is_empty() { "TEL".into() } else { format!("TEL;TYPE={kind}") },
                    &text(phone, "value"),
                );
            }
        }
    } else {
        property(&mut lines, "TEL;TYPE=CELL", &text(value, "mobilePhone"));
    }
    let name =
        if google { value["names"].as_array().and_then(|a| a.first()).cloned().unwrap_or(Value::Null) } else { value.clone() };
    let name_fields = if google {
        ["familyName", "givenName", "middleName", "honorificPrefix", "honorificSuffix"]
    } else {
        ["surname", "givenName", "middleName", "title", "generation"]
    };
    lines.push(format!("N:{}", name_fields.iter().map(|key| escape(&text(&name, key))).collect::<Vec<_>>().join(";")));
    if google {
        if let Some(organizations) = value["organizations"].as_array() {
            if let Some(organization) = organizations.first() {
                property(&mut lines, "ORG", &text(organization, "name"));
                property(&mut lines, "TITLE", &text(organization, "title"));
            }
        }
        if let Some(notes) = value["biographies"].as_array().and_then(|a| a.first()) {
            property(&mut lines, "NOTE", &text(notes, "value"));
        }
        if let Some(birthday) = value["birthdays"].as_array().and_then(|a| a.first()).and_then(|b| b.get("date")) {
            if let (Some(month), Some(day)) = (birthday["month"].as_u64(), birthday["day"].as_u64()) {
                let date = if let Some(year) = birthday["year"].as_u64().filter(|n| *n > 0) {
                    format!("{year:04}-{month:02}-{day:02}")
                } else {
                    format!("--{month:02}-{day:02}")
                };
                property(&mut lines, "BDAY", &date);
            }
        }
        if let Some(urls) = value["urls"].as_array() {
            for url in urls {
                let kind: String = text(url, "type").chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').collect();
                property(
                    &mut lines,
                    &if kind.is_empty() { "URL".into() } else { format!("URL;TYPE={kind}") },
                    &text(url, "value"),
                );
            }
        }
        if let Some(addresses) = value["addresses"].as_array() {
            for address in addresses {
                let fields = ["poBox", "extendedAddress", "streetAddress", "city", "region", "postalCode", "country"];
                lines.push(format!("ADR:{}", fields.iter().map(|key| escape(&text(address, key))).collect::<Vec<_>>().join(";")));
            }
        }
    } else {
        property(&mut lines, "ORG", &text(value, "companyName"));
        property(&mut lines, "TITLE", &text(value, "jobTitle"));
        property(&mut lines, "NOTE", &text(value, "personalNotes"));
        property(&mut lines, "URL", &text(value, "businessHomePage"));
        let birthday = text(value, "birthday");
        if birthday.len() >= 10 {
            property(&mut lines, "BDAY", &birthday[..10]);
        }
        for (key, kind) in [("businessPhones", "WORK"), ("homePhones", "HOME")] {
            if let Some(phones) = value[key].as_array() {
                for phone in phones.iter().filter_map(Value::as_str) {
                    property(&mut lines, &format!("TEL;TYPE={kind}"), phone);
                }
            }
        }
        for (key, kind) in [("businessAddress", "WORK"), ("homeAddress", "HOME"), ("otherAddress", "OTHER")] {
            let address = &value[key];
            if address.is_object() {
                lines.push(format!(
                    "ADR;TYPE={kind}:;;{}",
                    ["street", "city", "state", "postalCode", "countryOrRegion"]
                        .iter()
                        .map(|key| escape(&text(address, key)))
                        .collect::<Vec<_>>()
                        .join(";")
                ));
            }
        }
    }
    lines.push("END:VCARD".into());
    Some(lines.join("\r\n") + "\r\n")
}
pub(crate) fn pages(remote: &Remote, start: &str, key: &str) -> Result<Vec<Value>, Error> {
    let mut url = start.to_owned();
    let mut rows = Vec::new();
    for _ in 0..1000 {
        let page = remote.json(&url).map_err(error)?;
        if let Some(items) = page.get(key).and_then(Value::as_array) {
            rows.extend(items.iter().cloned());
        }
        if let Some(next) = page.get("@odata.nextLink").and_then(Value::as_str) {
            url = next.into();
        } else if let Some(token) = page.get("nextPageToken").and_then(Value::as_str) {
            let mut next = url::Url::parse(start).map_err(error)?;
            next.query_pairs_mut().append_pair("pageToken", token);
            url = next.into();
        } else {
            return Ok(rows);
        }
    }
    Err(error("Provider pagination exceeded limit"))
}
impl Registry {
    pub(crate) fn sync_api(&self, remote: &Remote, service: Service) -> Result<(), Error> {
        let google = remote.account.provider == "google";
        self.db.borrow().execute_batch("CREATE TABLE IF NOT EXISTS provider_payloads(account TEXT,kind TEXT,id TEXT,data TEXT,PRIMARY KEY(account,kind,id));").map_err(error)?;
        if service == Service::Contacts {
            return self.sync_contacts_api(remote);
        }
        if service == Service::Tasks {
            for list in remote.list(Service::Tasks).map_err(error)? {
                let id = text(&list, "id");
                let source_id = format!("{}:tasks:{id}", remote.account.id);
                self.add_source(&Source {
                    uid: source_id.clone(),
                    name: text(&list, if google { "title" } else { "displayName" }),
                    kind: Kind::TaskList,
                    color: None,
                    selected: true,
                    backend: if google { "gtasks" } else { "graph-tasks" }.into(),
                    account: Some(remote.account.id.clone()),
                    group: Some(remote.account.label().into()),
                    local: false,
                    writable: true,
                })?;
                let mut endpoint = url::Url::parse(if google {
                    "https://tasks.googleapis.com/tasks/v1/lists/"
                } else {
                    "https://graph.microsoft.com/v1.0/me/todo/lists/"
                })
                .map_err(error)?;
                endpoint.path_segments_mut().map_err(|_| error("Invalid task endpoint"))?.pop_if_empty().push(&id).push("tasks");
                if google {
                    endpoint.query_pairs_mut().append_pair("showCompleted", "true").append_pair("showHidden", "true");
                }
                self.db
                    .borrow()
                    .execute(
                        "INSERT INTO api_endpoints VALUES(?1,?2) ON CONFLICT(source) DO UPDATE SET endpoint=excluded.endpoint",
                        rusqlite::params![source_id, endpoint.as_str()],
                    )
                    .map_err(error)?;
                let calendar = Calendar::open(self, &source_id, Kind::TaskList, None)?;
                let mut seen = std::collections::HashSet::new();
                for task in pages(remote, endpoint.as_str(), if google { "items" } else { "value" })? {
                    let snapshot = self.write_snapshot()?;
                    let id = text(&task, "id");
                    seen.insert(id.clone());
                    self.save_api_payload(&remote.account.id, "task", &id, &task)?;
                    let mut lines = vec!["BEGIN:VTODO".into(), format!("UID:{}", escape(&id))];
                    property(&mut lines, "SUMMARY", &text(&task, "title"));
                    property(&mut lines, "DESCRIPTION", &if google { text(&task, "notes") } else { body_text(&task) });
                    if google {
                        let due = text(&task, "due");
                        if due.len() >= 10 {
                            lines.push(format!("DUE;VALUE=DATE:{}", due[..10].replace('-', "")));
                        }
                        property(&mut lines, "RELATED-TO", &text(&task, "parent"));
                    } else {
                        if let Some(due) = crate::zones::graph_time(&task["dueDateTime"], None, false) {
                            lines.push(format!("DUE{due}"));
                        }
                        if let Some(start) = crate::zones::graph_time(&task["startDateTime"], None, false) {
                            lines.push(format!("DTSTART{start}"));
                        }
                        if !task["recurrence"].is_null() {
                            if let Some(rule) = graph_rule(&task["recurrence"], false, task["dueDateTime"]["timeZone"].as_str()) {
                                lines.push(rule);
                            }
                        }
                        lines.push(format!(
                            "PRIORITY:{}",
                            match task["importance"].as_str() {
                                Some("high") => 1,
                                Some("low") => 9,
                                _ => 5,
                            }
                        ));
                        if let Some(categories) = task["categories"].as_array() {
                            lines.push(format!(
                                "CATEGORIES:{}",
                                categories.iter().filter_map(Value::as_str).map(escape).collect::<Vec<_>>().join(",")
                            ));
                        }
                    }
                    lines.push(
                        if text(&task, "status") == "completed" { "STATUS:COMPLETED" } else { "STATUS:NEEDS-ACTION" }.into(),
                    );
                    lines.push("END:VTODO".into());
                    let alias: Option<String> = self
                        .db
                        .borrow()
                        .query_row(
                            "SELECT uid FROM api_resources WHERE source=?1 AND remote_id=?2",
                            rusqlite::params![source_id, id],
                            |r| r.get(0),
                        )
                        .ok();
                    if let Some(alias) = alias {
                        for line in &mut lines {
                            if line.starts_with("UID:") {
                                *line = format!("UID:{}", escape(&alias));
                            }
                        }
                    }
                    let payload = lines.join("\r\n") + "\r\n";
                    let parsed = calendar.component(&payload)?;
                    let parsed = calendar.read(parsed.ptr());
                    let uid = &parsed.uid;
                    let etag = text(&task, if google { "etag" } else { "@odata.etag" });
                    let dirty: bool = self
                        .db
                        .borrow()
                        .query_row(
                            "SELECT EXISTS(SELECT 1 FROM objects WHERE source=?1 AND uid=?2 AND dirty=1)",
                            rusqlite::params![source_id, uid],
                            |r| r.get(0),
                        )
                        .map_err(error)?;
                    if dirty && self.api_resource(&source_id, uid, "").is_some_and(|(_, old)| old != etag) {
                        self.record_issue(&source_id, uid, "", "api", &payload, &etag, &id, false)?;
                        snapshot.commit()?;
                        return Err(error("Online task changed while you edited it; your local edit is retained"));
                    }
                    calendar.import_all(&payload)?;
                    self.save_api_resource(&source_id, uid, "", &id, &etag)?;
                    self.db.borrow().execute("INSERT INTO api_baselines VALUES(?1,?2,'',?3) ON CONFLICT(source,uid,rid) DO UPDATE SET payload=excluded.payload",rusqlite::params![source_id,uid,payload]).map_err(error)?;
                    snapshot.commit()?;
                }
                let known: Vec<(String, String)> = {
                    let db = self.db.borrow();
                    let mut query =
                        db.prepare("SELECT uid,remote_id FROM api_resources WHERE source=?1 AND rid='' ").map_err(error)?;
                    query
                        .query_map([&source_id], |r| Ok((r.get(0)?, r.get(1)?)))
                        .map_err(error)?
                        .collect::<Result<_, _>>()
                        .map_err(error)?
                };
                for (uid, id) in known {
                    if !seen.contains(&id) {
                        let snapshot = self.write_snapshot()?;
                        let dirty: bool = self
                            .db
                            .borrow()
                            .query_row(
                                "SELECT EXISTS(SELECT 1 FROM objects WHERE source=?1 AND uid=?2 AND dirty=1 AND deleted=0)",
                                rusqlite::params![source_id, uid],
                                |r| r.get(0),
                            )
                            .map_err(error)?;
                        if dirty {
                            self.record_issue(&source_id, &uid, "*", "api", "", "", &id, true)?;
                            snapshot.commit()?;
                            return Err(error("An edited task was removed remotely. Your local edit is retained."));
                        }
                        self.db
                            .borrow()
                            .execute(
                                "UPDATE objects SET deleted=1,dirty=0 WHERE source=?1 AND uid=?2",
                                rusqlite::params![source_id, uid],
                            )
                            .map_err(error)?;
                        snapshot.commit()?;
                    }
                }
                self.upload_api_tasks(&source_id, endpoint.as_str(), remote)?;
                self.set_healthy(&source_id, true);
            }
            return Ok(());
        }
        if service != Service::Calendar {
            return Ok(());
        }
        for collection in remote.list(Service::Calendar).map_err(error)? {
            let id = text(&collection, "id");
            let source_id = format!("{}:calendar:{id}", remote.account.id);
            let name = text(&collection, if google { "summary" } else { "name" });
            self.add_source(&Source {
                uid: source_id.clone(),
                name,
                kind: Kind::Calendar,
                color: None,
                selected: true,
                backend: if google { "google" } else { "graph" }.into(),
                account: Some(remote.account.id.clone()),
                group: Some(remote.account.label().into()),
                local: false,
                writable: if google {
                    matches!(collection["accessRole"].as_str(), Some("owner" | "writer"))
                } else {
                    collection["canEdit"].as_bool().unwrap_or(false)
                },
            })?;
            let mut endpoint = url::Url::parse(if google {
                "https://www.googleapis.com/calendar/v3/calendars/"
            } else {
                "https://graph.microsoft.com/v1.0/me/calendars/"
            })
            .map_err(error)?;
            endpoint.path_segments_mut().map_err(|_| error("Invalid provider endpoint"))?.pop_if_empty().push(&id).push("events");
            self.db
                .borrow()
                .execute(
                    "INSERT INTO api_endpoints VALUES(?1,?2) ON CONFLICT(source) DO UPDATE SET endpoint=excluded.endpoint",
                    rusqlite::params![source_id, endpoint.as_str()],
                )
                .map_err(error)?;
            self.set_healthy(&source_id, false);
            let values = pages(remote, endpoint.as_str(), if google { "items" } else { "value" })?;
            let calendar = Calendar::open(self, &source_id, Kind::Calendar, None)?;
            let masters: std::collections::HashMap<String, Value> = values
                .iter()
                .filter(|value| value["recurringEventId"].is_null())
                .filter_map(|value| value["id"].as_str().map(|id| (id.into(), value.clone())))
                .collect();
            let seen: std::collections::HashSet<String> =
                values.iter().filter_map(|v| v["id"].as_str().map(str::to_owned)).collect();
            let mut projection_failed = false;
            for value in values {
                let snapshot = self.write_snapshot()?;
                self.save_api_payload(&remote.account.id, "event", &text(&value, "id"), &value)?;
                if google && value["status"].as_str() == Some("cancelled") {
                    let remote_id = text(&value, "id");
                    let known: Option<(String, String)> = self
                        .db
                        .borrow()
                        .query_row(
                            "SELECT uid,rid FROM api_resources WHERE source=?1 AND remote_id=?2",
                            rusqlite::params![source_id, remote_id],
                            |r| Ok((r.get(0)?, r.get(1)?)),
                        )
                        .ok();
                    let known = known.or_else(|| {
                        let parent = masters.get(value["recurringEventId"].as_str()?)?;
                        let uid = parent["extendedProperties"]["private"]["nyxUid"]
                            .as_str()
                            .or_else(|| parent["iCalUID"].as_str())?
                            .to_owned();
                        let rid = time(&value["originalStartTime"])?;
                        let rid = rid.split_once(':')?.1.to_owned();
                        Some((uid, rid))
                    });
                    if let Some((uid, rid)) = known {
                        let dirty: bool = self
                            .db
                            .borrow()
                            .query_row(
                                "SELECT EXISTS(SELECT 1 FROM objects WHERE source=?1 AND uid=?2 AND (?3='' OR rid=?3) AND dirty=1)",
                                rusqlite::params![source_id, uid, rid],
                                |r| r.get(0),
                            )
                            .map_err(error)?;
                        if dirty {
                            if rid.is_empty() {
                                self.record_issue(&source_id, &uid, "*", "api", "", "", &remote_id, true)?;
                            } else {
                                let payload = calendar.ics(&uid, Some(&rid)).or_else(|_| calendar.ics(&uid, None))?;
                                let component = calendar.component(&payload)?;
                                unsafe {
                                    crate::ical::detach(component.ptr(), &rid, false);
                                    crate::ical::apply(
                                        component.ptr(),
                                        &Draft { status: Some(Status::Cancelled), ..Default::default() },
                                    );
                                }
                                let payload =
                                    unsafe { crate::owned(ical_glib_sys::i_cal_component_as_ical_string(component.ptr())) }
                                        .ok_or_else(|| error("Cancelled occurrence could not be preserved"))?;
                                self.record_issue(
                                    &source_id,
                                    &uid,
                                    &rid,
                                    "api",
                                    &payload,
                                    &text(&value, "etag"),
                                    &remote_id,
                                    false,
                                )?;
                            }
                            snapshot.commit()?;
                            return Err(error("A locally edited event was cancelled remotely; your edit is retained"));
                        }
                        if rid.is_empty() {
                            self.db
                                .borrow()
                                .execute(
                                    "UPDATE objects SET deleted=1 WHERE source=?1 AND uid=?2 AND dirty=0",
                                    rusqlite::params![source_id, uid],
                                )
                                .map_err(error)?;
                        } else {
                            let payload =
                                calendar.ics(&uid, Some(&rid)).or_else(|_| calendar.ics(&uid, None)).ok().or_else(|| {
                                    masters.get(value["recurringEventId"].as_str()?).and_then(|parent| event(parent, true))
                                });
                            if let Some(payload) = payload {
                                let component = calendar.component(&payload)?;
                                unsafe {
                                    crate::ical::detach(component.ptr(), &rid, false);
                                    crate::ical::apply(
                                        component.ptr(),
                                        &Draft { status: Some(Status::Cancelled), ..Default::default() },
                                    );
                                }
                                let payload =
                                    unsafe { crate::owned(ical_glib_sys::i_cal_component_as_ical_string(component.ptr())) }
                                        .ok_or_else(|| error("Cancelled occurrence could not be preserved"))?;
                                calendar.import_all(&payload)?;
                                self.save_api_resource(&source_id, &uid, &rid, &remote_id, &text(&value, "etag"))?;
                            }
                        }
                    }
                    snapshot.commit()?;
                    continue;
                }
                let mut projected = value.clone();
                projected["nyxDefaultReminders"] = collection["defaultReminders"].clone();
                if let Some(payload) = event(&projected, google) {
                    let remote_id = text(&value, "id");
                    let alias: Option<String> = self
                        .db
                        .borrow()
                        .query_row(
                            "SELECT uid FROM api_resources WHERE source=?1 AND remote_id=?2",
                            rusqlite::params![source_id, remote_id],
                            |r| r.get(0),
                        )
                        .ok();
                    let pending_alias = if let Some(transaction) = value["transactionId"].as_str() {
                        let db = self.db.borrow();
                        let mut q =
                            db.prepare("SELECT uid FROM objects WHERE source=?1 AND rid='' AND dirty=1").map_err(error)?;
                        let pending: Vec<String> =
                            q.query_map([&source_id], |r| r.get(0)).map_err(error)?.filter_map(Result::ok).collect();
                        pending.into_iter().find(|uid| crate::api_write::stable_id(&source_id, uid, "") == transaction)
                    } else {
                        None
                    };
                    let alias = pending_alias.or(alias);
                    let alias = alias.or_else(|| value["extendedProperties"]["private"]["nyxUid"].as_str().map(str::to_owned));
                    let payload = if let Some(alias) = alias {
                        payload
                            .lines()
                            .map(
                                |line| if line.starts_with("UID:") { format!("UID:{}", escape(&alias)) } else { line.to_owned() },
                            )
                            .collect::<Vec<_>>()
                            .join("\r\n")
                            + "\r\n"
                    } else {
                        payload
                    };
                    let parsed = calendar.component(&payload)?;
                    let parsed = calendar.read(parsed.ptr());
                    let uid = parsed.uid.as_str();
                    let rid = parsed.rid.as_deref().unwrap_or("");
                    let etag = text(&value, if google { "etag" } else { "@odata.etag" });
                    let dirty: bool = self
                        .db
                        .borrow()
                        .query_row(
                            "SELECT EXISTS(SELECT 1 FROM objects WHERE source=?1 AND uid=?2 AND rid=?3 AND dirty=1)",
                            rusqlite::params![source_id, uid, rid],
                            |r| r.get(0),
                        )
                        .map_err(error)?;
                    if dirty && self.api_resource(&source_id, uid, rid).is_some_and(|(_, previous)| previous != etag) {
                        self.record_issue(&source_id, uid, rid, "api", &payload, &etag, &remote_id, false)?;
                        snapshot.commit()?;
                        return Err(error("Online event changed while you edited it. Both versions are retained."));
                    }
                    calendar.import_all(&payload)?;
                    self.save_api_resource(&source_id, uid, rid, &remote_id, &etag)?;
                    self.db.borrow().execute("INSERT INTO api_baselines VALUES(?1,?2,?3,?4) ON CONFLICT(source,uid,rid) DO UPDATE SET payload=excluded.payload",rusqlite::params![source_id,uid,rid,payload]).map_err(error)?;
                } else {
                    projection_failed = true;
                }
                snapshot.commit()?;
            }
            if projection_failed {
                return Err(error(
                    "Some event data or time zones are unsupported; raw provider data and cached events were retained",
                ));
            }
            let known: Vec<(String, String)> = {
                let db = self.db.borrow();
                let mut q = db.prepare("SELECT uid,remote_id FROM api_resources WHERE source=?1 AND rid='' ").map_err(error)?;
                q.query_map([&source_id], |r| Ok((r.get(0)?, r.get(1)?))).map_err(error)?.filter_map(Result::ok).collect()
            };
            for (uid, remote_id) in known {
                if !seen.contains(&remote_id) {
                    let snapshot = self.write_snapshot()?;
                    let dirty: bool = self
                        .db
                        .borrow()
                        .query_row(
                            "SELECT EXISTS(SELECT 1 FROM objects WHERE source=?1 AND uid=?2 AND dirty=1 AND deleted=0)",
                            rusqlite::params![source_id, uid],
                            |r| r.get(0),
                        )
                        .map_err(error)?;
                    if dirty {
                        self.record_issue(&source_id, &uid, "*", "api", "", "", &remote_id, true)?;
                        snapshot.commit()?;
                        return Err(error("An edited event was removed remotely. Your local edit is retained."));
                    }
                    self.db
                        .borrow()
                        .execute(
                            "UPDATE objects SET deleted=1,dirty=0 WHERE source=?1 AND uid=?2",
                            rusqlite::params![source_id, uid],
                        )
                        .map_err(error)?;
                    snapshot.commit()?;
                }
            }
            self.upload_api_calendar(&source_id, endpoint.as_str(), remote)?;
            self.set_healthy(&source_id, true);
        }
        Ok(())
    }
    fn save_api_payload(&self, account: &str, kind: &str, id: &str, value: &Value) -> Result<(), Error> {
        self.db
            .borrow()
            .execute(
                "INSERT INTO provider_payloads VALUES(?1,?2,?3,?4) ON CONFLICT(account,kind,id) DO UPDATE SET data=excluded.data",
                rusqlite::params![account, kind, id, serde_json::to_string(value).map_err(error)?],
            )
            .map_err(error)?;
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn google_all_day_dates_remain_dates() {
        let payload = event(
            &serde_json::json!({"iCalUID":"fixture","summary":"Day","start":{"date":"2026-10-30"},"end":{"date":"2026-10-31"}}),
            true,
        )
        .unwrap();
        assert!(payload.contains("DTSTART;VALUE=DATE:20261030"));
    }
    #[test]
    fn vcard_values_cannot_inject_fields() {
        let payload = card(&serde_json::json!({"id":"fixture","displayName":"Person\nEMAIL:fake"}), false).unwrap();
        assert!(!payload.contains("\r\nEMAIL:fake"));
    }
}
