//! CLDR 48 territory-001 Windows → IANA mapping, distributed under the Unicode license.
use chrono::{DateTime, NaiveDateTime, TimeZone};
use chrono_tz::Tz;
use std::{collections::HashMap, sync::OnceLock};
pub(crate) fn zone(name: &str) -> Option<Tz> {
    let name = name.strip_prefix("/freeassociation.sourceforge.net/").unwrap_or(name);
    if let Ok(zone) = name.parse() {
        return Some(zone);
    }
    static WINDOWS: OnceLock<HashMap<String, String>> = OnceLock::new();
    let zones = WINDOWS.get_or_init(|| {
        let doc = roxmltree::Document::parse_with_options(
            include_str!("../data/windowsZones.xml"),
            roxmltree::ParsingOptions { allow_dtd: true, ..Default::default() },
        )
        .expect("bundled CLDR timezones");
        doc.descendants()
            .filter(|n| n.has_tag_name("mapZone") && n.attribute("territory") == Some("001"))
            .filter_map(|n| Some((n.attribute("other")?.to_owned(), n.attribute("type")?.split_whitespace().next()?.to_owned())))
            .collect()
    });
    zones.get(name)?.parse().ok()
}
pub(crate) fn graph_time(value: &serde_json::Value, desired: Option<&str>, date_only: bool) -> Option<String> {
    let input = value.get("dateTime")?.as_str()?;
    let source = zone(value.get("timeZone").and_then(|v| v.as_str()).unwrap_or("UTC"))?;
    let instant = if let Ok(value) = DateTime::parse_from_rfc3339(input) {
        value.with_timezone(&chrono::Utc)
    } else {
        let value = NaiveDateTime::parse_from_str(input, "%Y-%m-%dT%H:%M:%S%.f").ok()?;
        source.from_local_datetime(&value).single()?.with_timezone(&chrono::Utc)
    };
    let target = if let Some(desired) = desired { zone(desired)? } else { source };
    let local = instant.with_timezone(&target);
    Some(if date_only {
        format!(";VALUE=DATE:{}", local.format("%Y%m%d"))
    } else {
        format!(";TZID={target}:{}", local.format("%Y%m%dT%H%M%S"))
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn windows_rules_preserve_dst() {
        let value = serde_json::json!({"dateTime":"2026-07-01T14:00:00","timeZone":"UTC"});
        assert_eq!(graph_time(&value, Some("Central Standard Time"), false).unwrap(), ";TZID=America/Chicago:20260701T090000");
        let value = serde_json::json!({"dateTime":"2026-12-01T15:00:00","timeZone":"UTC"});
        assert_eq!(graph_time(&value, Some("Central Standard Time"), false).unwrap(), ";TZID=America/Chicago:20261201T090000");
    }
    #[test]
    fn custom_unknown_zone_is_not_guessed() {
        assert!(zone("tzone://Microsoft/Custom").is_none());
    }
}
