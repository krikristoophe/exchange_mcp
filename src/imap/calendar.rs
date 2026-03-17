//! ICS (iCalendar RFC 5545) parsing for Exchange calendar events.
//!
//! Exchange exposes calendar items via IMAP in a "Calendar" folder.
//! Each item is a MIME message containing a `text/calendar` part with
//! VEVENT data.

use std::collections::HashMap;

/// Summary of a calendar event (for list views).
#[derive(Debug, Clone, serde::Serialize)]
pub struct CalendarEvent {
    /// IMAP UID of the message containing the event
    pub uid: u32,
    /// ICS UID (unique event identifier)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ical_uid: Option<String>,
    /// Event title (SUMMARY)
    pub subject: String,
    /// Start date/time (formatted)
    pub start: String,
    /// End date/time (formatted)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end: Option<String>,
    /// Location
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    /// Organizer email/name
    #[serde(skip_serializing_if = "Option::is_none")]
    pub organizer: Option<String>,
    /// Event status (CONFIRMED, TENTATIVE, CANCELLED)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// Whether the event has a recurrence rule
    pub is_recurring: bool,
    /// All-day event
    pub all_day: bool,
}

/// Full details of a calendar event.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CalendarEventDetail {
    /// IMAP UID
    pub uid: u32,
    /// ICS UID
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ical_uid: Option<String>,
    /// Event title (SUMMARY)
    pub subject: String,
    /// Start date/time
    pub start: String,
    /// End date/time
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end: Option<String>,
    /// Location
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    /// Organizer
    #[serde(skip_serializing_if = "Option::is_none")]
    pub organizer: Option<String>,
    /// List of attendees
    pub attendees: Vec<String>,
    /// Event description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Event status
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// Recurrence rule (RRULE string)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recurrence: Option<String>,
    /// Categories/tags
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub categories: Vec<String>,
    /// All-day event
    pub all_day: bool,
    /// Transparency (OPAQUE/TRANSPARENT)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transparency: Option<String>,
    /// Priority (0-9)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<String>,
}

/// Parsed ICS properties for a single VEVENT.
struct VEventProps {
    props: HashMap<String, String>,
    attendees: Vec<String>,
    categories: Vec<String>,
}

/// Unfold ICS lines: lines starting with a space or tab are continuations.
fn unfold_ics(raw: &str) -> String {
    let mut result = String::with_capacity(raw.len());
    for line in raw.split('\n') {
        let line = line.trim_end_matches('\r');
        if line.starts_with(' ') || line.starts_with('\t') {
            // Continuation of previous line
            result.push_str(line.trim_start());
        } else {
            if !result.is_empty() {
                result.push('\n');
            }
            result.push_str(line);
        }
    }
    result
}

/// Parse an ICS content string and extract VEVENT properties.
/// Returns None if no VEVENT is found.
fn parse_vevent(ics: &str) -> Option<VEventProps> {
    let unfolded = unfold_ics(ics);
    let mut in_vevent = false;
    let mut props = HashMap::new();
    let mut attendees = Vec::new();
    let mut categories = Vec::new();

    for line in unfolded.lines() {
        let line = line.trim();
        if line.eq_ignore_ascii_case("BEGIN:VEVENT") {
            in_vevent = true;
            continue;
        }
        if line.eq_ignore_ascii_case("END:VEVENT") {
            break;
        }
        if !in_vevent {
            continue;
        }

        // Parse property: NAME;PARAMS:VALUE or NAME:VALUE
        if let Some(colon_pos) = find_property_colon(line) {
            let name_part = &line[..colon_pos];
            let value = &line[colon_pos + 1..];

            // Extract base property name (before any ;PARAMS)
            let base_name = name_part
                .split(';')
                .next()
                .unwrap_or(name_part)
                .to_uppercase();

            match base_name.as_str() {
                "ATTENDEE" => {
                    attendees.push(format_ics_person(name_part, value));
                }
                "CATEGORIES" => {
                    for cat in value.split(',') {
                        let cat = cat.trim();
                        if !cat.is_empty() {
                            categories.push(cat.to_string());
                        }
                    }
                }
                _ => {
                    props.insert(base_name, value.to_string());
                }
            }
        }
    }

    if props.is_empty() && attendees.is_empty() {
        return None;
    }

    Some(VEventProps {
        props,
        attendees,
        categories,
    })
}

/// Find the colon that separates property name(+params) from value.
/// Must skip colons inside quoted parameter values.
fn find_property_colon(line: &str) -> Option<usize> {
    let mut in_quotes = false;
    for (i, ch) in line.char_indices() {
        match ch {
            '"' => in_quotes = !in_quotes,
            ':' if !in_quotes => return Some(i),
            _ => {}
        }
    }
    None
}

/// Format an ORGANIZER or ATTENDEE value, extracting CN param and mailto.
fn format_ics_person(name_part: &str, value: &str) -> String {
    let email = value
        .strip_prefix("mailto:")
        .or_else(|| value.strip_prefix("MAILTO:"))
        .unwrap_or(value)
        .trim()
        .to_string();

    // Extract CN parameter
    let cn = extract_param(name_part, "CN");

    match cn {
        Some(name) if !name.is_empty() => format!("{name} <{email}>"),
        _ => email,
    }
}

/// Extract a parameter value from the name part (e.g., "ATTENDEE;CN=John Doe;ROLE=REQ-PARTICIPANT").
fn extract_param(name_part: &str, param_name: &str) -> Option<String> {
    let upper = param_name.to_uppercase();
    for segment in name_part.split(';') {
        let segment = segment.trim();
        if let Some(eq_pos) = segment.find('=') {
            let key = segment[..eq_pos].trim().to_uppercase();
            if key == upper {
                let val = segment[eq_pos + 1..].trim();
                // Remove surrounding quotes
                let val = val.trim_matches('"');
                return Some(val.to_string());
            }
        }
    }
    None
}

/// Format a DTSTART/DTEND value into a human-readable string.
/// Returns (formatted_string, is_all_day).
fn format_ics_datetime(value: &str, name_part: &str) -> (String, bool) {
    // Check if VALUE=DATE (all-day event)
    let is_date_only = name_part.to_uppercase().contains("VALUE=DATE")
        && !name_part.to_uppercase().contains("VALUE=DATE-TIME");

    if is_date_only || (value.len() == 8 && value.chars().all(|c| c.is_ascii_digit())) {
        // All-day: 20240115
        if value.len() >= 8 {
            let formatted = format!(
                "{}-{}-{}",
                &value[..4],
                &value[4..6],
                &value[6..8]
            );
            return (formatted, true);
        }
        return (value.to_string(), true);
    }

    // Date-time: 20240115T090000Z or 20240115T090000
    if value.len() >= 15 {
        let date = &value[..8];
        let time = &value[9..15];
        let tz_suffix = if value.ends_with('Z') {
            " UTC".to_string()
        } else {
            // Extract TZID parameter if present (e.g., DTSTART;TZID=Europe/Paris)
            extract_param(name_part, "TZID")
                .map(|tz| format!(" ({tz})"))
                .unwrap_or_default()
        };
        let formatted = format!(
            "{}-{}-{} {}:{}:{}{}",
            &date[..4],
            &date[4..6],
            &date[6..8],
            &time[..2],
            &time[2..4],
            &time[4..6],
            tz_suffix,
        );
        return (formatted, false);
    }

    (value.to_string(), false)
}

/// Extract ICS content from a MIME message body.
/// Looks for text/calendar parts in the MIME structure.
pub fn extract_ics_from_mime(body: &[u8]) -> Option<String> {
    match mailparse::parse_mail(body) {
        Ok(parsed) => {
            let mut ics = None;
            find_calendar_part(&parsed, &mut ics);
            ics
        }
        Err(_) => {
            // Try as raw text
            let text = String::from_utf8_lossy(body);
            if text.contains("BEGIN:VCALENDAR") || text.contains("BEGIN:VEVENT") {
                Some(text.to_string())
            } else {
                None
            }
        }
    }
}

fn find_calendar_part(mail: &mailparse::ParsedMail, result: &mut Option<String>) {
    if result.is_some() {
        return;
    }
    if mail.subparts.is_empty() {
        let content_type = mail.ctype.mimetype.to_lowercase();
        if content_type.contains("text/calendar") || content_type.contains("application/ics") {
            if let Ok(body) = mail.get_body() {
                *result = Some(body);
            }
        }
    } else {
        for part in &mail.subparts {
            find_calendar_part(part, result);
        }
    }
}

/// Parse an ICS body into a CalendarEvent summary.
pub fn parse_calendar_event(uid: u32, ics: &str) -> Option<CalendarEvent> {
    let vevent = parse_vevent(ics)?;

    let subject = vevent.props.get("SUMMARY").cloned().unwrap_or_default();
    if subject.is_empty() && vevent.props.get("DTSTART").is_none() {
        return None;
    }

    let (start, all_day) = vevent.props.get("DTSTART").map(|v| {
        // Find the full DTSTART line to check params
        let name_part = find_dtstart_params(ics);
        format_ics_datetime(v, &name_part)
    }).unwrap_or_default();

    let end = vevent.props.get("DTEND").map(|v| {
        let name_part = find_dtend_params(ics);
        format_ics_datetime(v, &name_part).0
    });

    let location = vevent.props.get("LOCATION").and_then(|v| {
        let v = v.trim();
        if v.is_empty() { None } else { Some(v.to_string()) }
    });

    let organizer = vevent.props.get("ORGANIZER").map(|v| {
        let name_part = find_property_name_part(ics, "ORGANIZER");
        format_ics_person(&name_part, v)
    });

    let status = vevent.props.get("STATUS").and_then(|v| {
        let v = v.trim();
        if v.is_empty() { None } else { Some(v.to_string()) }
    });

    let is_recurring = vevent.props.contains_key("RRULE");

    Some(CalendarEvent {
        uid,
        ical_uid: vevent.props.get("UID").cloned(),
        subject,
        start,
        end,
        location,
        organizer,
        status,
        is_recurring,
        all_day,
    })
}

/// Parse an ICS body into full CalendarEventDetail.
pub fn parse_calendar_event_detail(uid: u32, ics: &str) -> Option<CalendarEventDetail> {
    let vevent = parse_vevent(ics)?;

    let subject = vevent.props.get("SUMMARY").cloned().unwrap_or_default();

    let (start, all_day) = vevent.props.get("DTSTART").map(|v| {
        let name_part = find_dtstart_params(ics);
        format_ics_datetime(v, &name_part)
    }).unwrap_or_default();

    let end = vevent.props.get("DTEND").map(|v| {
        let name_part = find_dtend_params(ics);
        format_ics_datetime(v, &name_part).0
    });

    let location = vevent.props.get("LOCATION").and_then(|v| {
        let v = v.trim();
        if v.is_empty() { None } else { Some(unescape_ics_text(v)) }
    });

    let organizer = vevent.props.get("ORGANIZER").map(|v| {
        let name_part = find_property_name_part(ics, "ORGANIZER");
        format_ics_person(&name_part, v)
    });

    let description = vevent.props.get("DESCRIPTION").and_then(|v| {
        let v = unescape_ics_text(v.trim());
        if v.is_empty() { None } else { Some(v) }
    });

    let status = vevent.props.get("STATUS").and_then(|v| {
        let v = v.trim();
        if v.is_empty() { None } else { Some(v.to_string()) }
    });

    let recurrence = vevent.props.get("RRULE").cloned();

    let transparency = vevent.props.get("TRANSP").and_then(|v| {
        let v = v.trim();
        if v.is_empty() { None } else { Some(v.to_string()) }
    });

    let priority = vevent.props.get("PRIORITY").and_then(|v| {
        let v = v.trim();
        if v.is_empty() || v == "0" { None } else { Some(v.to_string()) }
    });

    Some(CalendarEventDetail {
        uid,
        ical_uid: vevent.props.get("UID").cloned(),
        subject,
        start,
        end,
        location,
        organizer,
        attendees: vevent.attendees,
        description,
        status,
        recurrence,
        categories: vevent.categories,
        all_day,
        transparency,
        priority,
    })
}

/// Unescape ICS text values (backslash escapes).
fn unescape_ics_text(s: &str) -> String {
    s.replace("\\n", "\n")
        .replace("\\N", "\n")
        .replace("\\,", ",")
        .replace("\\;", ";")
        .replace("\\\\", "\\")
}

/// Find DTSTART parameter string from raw ICS for VALUE=DATE detection.
fn find_dtstart_params(ics: &str) -> String {
    find_property_name_part(ics, "DTSTART")
}

/// Find DTEND parameter string from raw ICS.
fn find_dtend_params(ics: &str) -> String {
    find_property_name_part(ics, "DTEND")
}

/// Find the full property name part (before the colon) for a given property.
fn find_property_name_part(ics: &str, prop: &str) -> String {
    let unfolded = unfold_ics(ics);
    let prop_upper = prop.to_uppercase();
    for line in unfolded.lines() {
        let line = line.trim();
        let upper_line = line.to_uppercase();
        if upper_line.starts_with(&prop_upper) {
            // Check next char is ; or :
            let rest = &upper_line[prop_upper.len()..];
            if rest.starts_with(';') || rest.starts_with(':') {
                if let Some(colon_pos) = find_property_colon(line) {
                    return line[..colon_pos].to_string();
                }
            }
        }
    }
    prop.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_event() {
        let ics = r#"BEGIN:VCALENDAR
BEGIN:VEVENT
UID:test-123@example.com
SUMMARY:Team Meeting
DTSTART:20240115T090000Z
DTEND:20240115T100000Z
LOCATION:Conference Room A
ORGANIZER;CN=Boss:mailto:boss@example.com
ATTENDEE;CN=User One:mailto:user1@example.com
ATTENDEE;CN=User Two:mailto:user2@example.com
STATUS:CONFIRMED
DESCRIPTION:Weekly team sync
END:VEVENT
END:VCALENDAR"#;

        let event = parse_calendar_event(42, ics).unwrap();
        assert_eq!(event.uid, 42);
        assert_eq!(event.subject, "Team Meeting");
        assert_eq!(event.start, "2024-01-15 09:00:00 UTC");
        assert_eq!(event.end.as_deref(), Some("2024-01-15 10:00:00 UTC"));
        assert_eq!(event.location.as_deref(), Some("Conference Room A"));
        assert_eq!(event.organizer.as_deref(), Some("Boss <boss@example.com>"));
        assert_eq!(event.status.as_deref(), Some("CONFIRMED"));
        assert!(!event.is_recurring);
        assert!(!event.all_day);

        let detail = parse_calendar_event_detail(42, ics).unwrap();
        assert_eq!(detail.attendees.len(), 2);
        assert_eq!(detail.attendees[0], "User One <user1@example.com>");
        assert_eq!(detail.description.as_deref(), Some("Weekly team sync"));
    }

    #[test]
    fn test_parse_all_day_event() {
        let ics = r#"BEGIN:VCALENDAR
BEGIN:VEVENT
DTSTART;VALUE=DATE:20240115
DTEND;VALUE=DATE:20240116
SUMMARY:Company Holiday
END:VEVENT
END:VCALENDAR"#;

        let event = parse_calendar_event(1, ics).unwrap();
        assert!(event.all_day);
        assert_eq!(event.start, "2024-01-15");
    }

    #[test]
    fn test_parse_recurring_event() {
        let ics = r#"BEGIN:VCALENDAR
BEGIN:VEVENT
SUMMARY:Daily Standup
DTSTART:20240115T093000Z
DTEND:20240115T094500Z
RRULE:FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR
END:VEVENT
END:VCALENDAR"#;

        let event = parse_calendar_event(5, ics).unwrap();
        assert!(event.is_recurring);

        let detail = parse_calendar_event_detail(5, ics).unwrap();
        assert_eq!(
            detail.recurrence.as_deref(),
            Some("FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR")
        );
    }

    #[test]
    fn test_unfold_ics() {
        let raw = "DESCRIPTION:This is a long\r\n  description that spans\r\n  multiple lines";
        let unfolded = unfold_ics(raw);
        assert!(unfolded.contains("This is a longdescription that spansmultiple lines"));
    }

    #[test]
    fn test_unescape_ics_text() {
        assert_eq!(
            unescape_ics_text("Line 1\\nLine 2\\, with comma"),
            "Line 1\nLine 2, with comma"
        );
    }

    #[test]
    fn test_parse_event_with_tzid() {
        let ics = r#"BEGIN:VCALENDAR
BEGIN:VEVENT
SUMMARY:Paris Meeting
DTSTART;TZID=Europe/Paris:20240115T090000
DTEND;TZID=Europe/Paris:20240115T100000
END:VEVENT
END:VCALENDAR"#;

        let event = parse_calendar_event(10, ics).unwrap();
        assert_eq!(event.start, "2024-01-15 09:00:00 (Europe/Paris)");
        assert_eq!(event.end.as_deref(), Some("2024-01-15 10:00:00 (Europe/Paris)"));
        assert!(!event.all_day);
    }
}
