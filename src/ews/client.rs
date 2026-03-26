//! EWS HTTP client for calendar operations.

use std::sync::Arc;

use anyhow::{Context, Result};

use crate::auth::AuthProvider;
use crate::imap::calendar::{CalendarEvent, CalendarEventDetail};
use super::xml;

/// EWS client for accessing Exchange calendar data via SOAP/XML.
pub struct EwsClient {
    auth: Arc<dyn AuthProvider>,
    ews_url: String,
    http: reqwest::Client,
}

impl EwsClient {
    /// Create a new EWS client.
    ///
    /// `ews_url` is the full URL to the EWS endpoint, e.g.
    /// `https://outlook.office365.com/EWS/Exchange.asmx`
    pub fn new(auth: Arc<dyn AuthProvider>, ews_url: String) -> Self {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("Failed to build HTTP client");

        Self {
            auth,
            ews_url,
            http,
        }
    }

    /// Derive the EWS URL from an IMAP host.
    /// Falls back to the `EXCHANGE_EWS_URL` env var if set.
    pub fn ews_url_from_host(imap_host: &str) -> String {
        if let Ok(url) = std::env::var("EXCHANGE_EWS_URL") {
            return url;
        }
        format!("https://{imap_host}/EWS/Exchange.asmx")
    }

    /// Send a SOAP request to EWS and return the response body.
    async fn soap_request(&self, body: &str) -> Result<String> {
        let credentials = self.auth.get_credentials().await?;

        let response = self
            .http
            .post(&self.ews_url)
            .basic_auth(&credentials.username, Some(&credentials.password))
            .header("Content-Type", "text/xml; charset=utf-8")
            .body(body.to_string())
            .send()
            .await
            .context("EWS request failed")?;

        let status = response.status();
        let text = response.text().await.context("Failed to read EWS response")?;

        if !status.is_success() {
            if status.as_u16() == 401 {
                anyhow::bail!("EWS authentication failed (401). Check credentials.");
            }
            anyhow::bail!("EWS request failed with status {status}: {text}");
        }

        Ok(text)
    }

    /// List calendar events using CalendarView (expands recurring events).
    pub async fn list_calendar_events(
        &self,
        folder_id: Option<&str>,
        start_date: Option<&str>,
        end_date: Option<&str>,
        limit: Option<u32>,
    ) -> Result<Vec<CalendarEvent>> {
        let limit_val = limit.unwrap_or(50);

        // Default date range: from 30 days ago to 90 days ahead
        let now = chrono::Utc::now();
        let default_start = (now - chrono::Duration::days(30))
            .format("%Y-%m-%dT00:00:00Z")
            .to_string();
        let default_end = (now + chrono::Duration::days(90))
            .format("%Y-%m-%dT23:59:59Z")
            .to_string();

        let start = start_date
            .map(|s| normalize_to_iso8601(s))
            .unwrap_or(default_start);
        let end = end_date
            .map(|s| normalize_to_iso8601_end(s))
            .unwrap_or(default_end);

        let request_xml =
            xml::build_find_item_calendar_view(&start, &end, limit_val, folder_id);

        let response = self.soap_request(&request_xml).await?;
        let items = xml::parse_find_item_response(&response)?;

        let events = items
            .into_iter()
            .map(|item| CalendarEvent {
                uid: 0,
                ical_uid: Some(item.item_id.clone()),
                subject: item.subject,
                start: format_ews_datetime(&item.start),
                end: if item.end.is_empty() {
                    None
                } else {
                    Some(format_ews_datetime(&item.end))
                },
                location: if item.location.is_empty() {
                    None
                } else {
                    Some(item.location)
                },
                organizer: if item.organizer.is_empty() {
                    None
                } else {
                    Some(item.organizer)
                },
                status: None,
                is_recurring: item.is_recurring,
                all_day: item.is_all_day,
            })
            .collect();

        Ok(events)
    }

    /// Read full details of a calendar event by its EWS ItemId.
    pub async fn read_calendar_event(
        &self,
        item_id: &str,
    ) -> Result<CalendarEventDetail> {
        let request_xml = xml::build_get_item(item_id);
        let response = self.soap_request(&request_xml).await?;
        let detail = xml::parse_get_item_response(&response)?;

        Ok(CalendarEventDetail {
            uid: 0,
            ical_uid: if detail.uid.is_empty() {
                Some(detail.item_id)
            } else {
                Some(detail.uid)
            },
            subject: detail.subject,
            start: format_ews_datetime(&detail.start),
            end: if detail.end.is_empty() {
                None
            } else {
                Some(format_ews_datetime(&detail.end))
            },
            location: if detail.location.is_empty() {
                None
            } else {
                Some(detail.location)
            },
            organizer: if detail.organizer.is_empty() {
                None
            } else {
                Some(detail.organizer)
            },
            attendees: detail.attendees,
            description: if detail.body.is_empty() {
                None
            } else {
                Some(detail.body)
            },
            status: if detail.status.is_empty() {
                None
            } else {
                Some(detail.status)
            },
            recurrence: None,
            categories: detail.categories,
            all_day: detail.is_all_day,
            transparency: None,
            priority: None,
        })
    }

    /// Search calendar events by text query.
    pub async fn search_calendar_events(
        &self,
        folder_id: Option<&str>,
        query: &str,
        limit: Option<u32>,
    ) -> Result<Vec<CalendarEvent>> {
        let limit_val = limit.unwrap_or(20);

        let request_xml = xml::build_find_item_search(query, limit_val, folder_id);
        let response = self.soap_request(&request_xml).await?;
        let items = xml::parse_find_item_response(&response)?;

        let events = items
            .into_iter()
            .map(|item| CalendarEvent {
                uid: 0,
                ical_uid: Some(item.item_id.clone()),
                subject: item.subject,
                start: format_ews_datetime(&item.start),
                end: if item.end.is_empty() {
                    None
                } else {
                    Some(format_ews_datetime(&item.end))
                },
                location: if item.location.is_empty() {
                    None
                } else {
                    Some(item.location)
                },
                organizer: if item.organizer.is_empty() {
                    None
                } else {
                    Some(item.organizer)
                },
                status: None,
                is_recurring: item.is_recurring,
                all_day: item.is_all_day,
            })
            .collect();

        Ok(events)
    }
}

/// Format an EWS ISO 8601 datetime into a more readable format.
/// Input: "2024-01-15T09:00:00Z" → Output: "2024-01-15 09:00:00 UTC"
/// Input: "2024-01-15T09:00:00+01:00" → Output: "2024-01-15 09:00:00 (+01:00)"
fn format_ews_datetime(dt: &str) -> String {
    if dt.is_empty() {
        return dt.to_string();
    }

    // Try to parse and reformat
    if let Some(t_pos) = dt.find('T') {
        let date = &dt[..t_pos];
        let time_rest = &dt[t_pos + 1..];

        // Extract time (first 8 chars: HH:MM:SS) and timezone
        if time_rest.len() >= 8 {
            let time = &time_rest[..8];
            let tz = &time_rest[8..];
            let tz_display = match tz {
                "Z" | "z" => " UTC".to_string(),
                s if s.starts_with('+') || s.starts_with('-') => format!(" ({s})"),
                _ => String::new(),
            };
            return format!("{date} {time}{tz_display}");
        }
    }

    dt.to_string()
}

/// Normalize a user-provided date (yyyy-mm-dd) to ISO 8601 start-of-day.
fn normalize_to_iso8601(date: &str) -> String {
    // If already ISO 8601 with time, return as-is
    if date.contains('T') {
        return date.to_string();
    }
    // yyyy-mm-dd → yyyy-mm-ddT00:00:00Z
    if date.len() == 10 && date.chars().nth(4) == Some('-') {
        return format!("{date}T00:00:00Z");
    }
    date.to_string()
}

/// Normalize a user-provided date to ISO 8601 end-of-day.
fn normalize_to_iso8601_end(date: &str) -> String {
    if date.contains('T') {
        return date.to_string();
    }
    if date.len() == 10 && date.chars().nth(4) == Some('-') {
        return format!("{date}T23:59:59Z");
    }
    date.to_string()
}
