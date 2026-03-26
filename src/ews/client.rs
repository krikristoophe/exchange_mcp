//! EWS HTTP client for calendar operations.
//! Supports NTLM authentication (required by many Exchange servers like OVH)
//! with Basic Auth fallback.

use std::sync::Arc;

use anyhow::{Context, Result};
use base64::prelude::{BASE64_STANDARD, Engine};

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
            .redirect(reqwest::redirect::Policy::none())
            .cookie_store(true)
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

    /// Send a SOAP request to EWS using NTLM auth, falling back to Basic Auth.
    async fn soap_request(&self, body: &str) -> Result<String> {
        // Try NTLM first, fall back to Basic Auth
        match self.soap_request_ntlm(body).await {
            Ok(text) => Ok(text),
            Err(ntlm_err) => {
                tracing::debug!("EWS NTLM auth failed, trying Basic Auth: {ntlm_err}");
                self.soap_request_basic(body).await.map_err(|basic_err| {
                    anyhow::anyhow!(
                        "EWS authentication failed at {}.\n\
                         NTLM error: {ntlm_err}\n\
                         Basic Auth error: {basic_err}",
                        self.ews_url
                    )
                })
            }
        }
    }

    /// Send a SOAP request using NTLM 3-step handshake.
    async fn soap_request_ntlm(&self, body: &str) -> Result<String> {
        let credentials = self.auth.get_credentials().await?;

        // Split username into domain\user if applicable
        let (domain, username) = parse_ntlm_username(&credentials.username);

        let hostname = hostname();

        // Step 1: NEGOTIATE (Type 1 message)
        let nego_flags = ntlmclient::Flags::NEGOTIATE_UNICODE
            | ntlmclient::Flags::REQUEST_TARGET
            | ntlmclient::Flags::NEGOTIATE_NTLM
            | ntlmclient::Flags::NEGOTIATE_WORKSTATION_SUPPLIED;

        let nego_msg = ntlmclient::Message::Negotiate(ntlmclient::NegotiateMessage {
            flags: nego_flags,
            supplied_domain: String::new(),
            supplied_workstation: hostname.clone(),
            os_version: Default::default(),
        });
        let nego_bytes = nego_msg.to_bytes().context("Failed to build NTLM negotiate message")?;
        let nego_b64 = BASE64_STANDARD.encode(&nego_bytes);

        tracing::debug!("EWS NTLM negotiate to: {}", self.ews_url);

        let resp = self
            .http
            .post(&self.ews_url)
            .header("Authorization", format!("NTLM {nego_b64}"))
            .header("Content-Type", "text/xml; charset=utf-8")
            .body(body.to_string())
            .send()
            .await
            .context("EWS NTLM negotiate request failed")?;

        // Step 2: Parse CHALLENGE (Type 2 message) from server
        let challenge_header = resp
            .headers()
            .get("www-authenticate")
            .ok_or_else(|| anyhow::anyhow!("No WWW-Authenticate header in NTLM challenge response"))?
            .to_str()
            .context("Invalid WWW-Authenticate header")?;

        let challenge_b64 = challenge_header
            .strip_prefix("NTLM ")
            .or_else(|| challenge_header.strip_prefix("ntlm "))
            .ok_or_else(|| anyhow::anyhow!("WWW-Authenticate is not NTLM: {challenge_header}"))?;

        let challenge_bytes = BASE64_STANDARD
            .decode(challenge_b64.trim())
            .context("Failed to decode NTLM challenge")?;
        let challenge = ntlmclient::Message::try_from(challenge_bytes.as_slice())
            .context("Failed to parse NTLM challenge message")?;

        let challenge_msg = match challenge {
            ntlmclient::Message::Challenge(c) => c,
            _ => anyhow::bail!("Expected NTLM Challenge message, got different type"),
        };

        // Extract target_info for NTLMv2
        let target_info_bytes: Vec<u8> = challenge_msg
            .target_information
            .iter()
            .flat_map(|ie| ie.to_bytes())
            .collect();

        // Step 3: AUTHENTICATE (Type 3 message)
        let creds = ntlmclient::Credentials {
            username: username.to_owned(),
            password: credentials.password.clone(),
            domain: domain.to_owned(),
        };

        let challenge_response = ntlmclient::respond_challenge_ntlm_v2(
            challenge_msg.challenge,
            &target_info_bytes,
            ntlmclient::get_ntlm_time(),
            &creds,
        );

        let auth_flags = ntlmclient::Flags::NEGOTIATE_UNICODE
            | ntlmclient::Flags::NEGOTIATE_NTLM;

        let auth_msg = challenge_response.to_message(&creds, &hostname, auth_flags);
        let auth_bytes = auth_msg
            .to_bytes()
            .context("Failed to build NTLM authenticate message")?;
        let auth_b64 = BASE64_STANDARD.encode(&auth_bytes);

        tracing::debug!("EWS NTLM authenticate to: {}", self.ews_url);

        let response = self
            .http
            .post(&self.ews_url)
            .header("Authorization", format!("NTLM {auth_b64}"))
            .header("Content-Type", "text/xml; charset=utf-8")
            .body(body.to_string())
            .send()
            .await
            .context("EWS NTLM authenticate request failed")?;

        let status = response.status();
        let text = response.text().await.context("Failed to read EWS response")?;

        if !status.is_success() {
            anyhow::bail!("EWS NTLM request failed with status {status}");
        }

        validate_soap_response(&text, &self.ews_url)?;

        tracing::debug!("EWS NTLM request succeeded (status {status})");
        Ok(text)
    }

    /// Send a SOAP request using Basic Auth (fallback).
    async fn soap_request_basic(&self, body: &str) -> Result<String> {
        let credentials = self.auth.get_credentials().await?;

        tracing::debug!("EWS Basic Auth request to: {}", self.ews_url);

        let response = self
            .http
            .post(&self.ews_url)
            .basic_auth(&credentials.username, Some(&credentials.password))
            .header("Content-Type", "text/xml; charset=utf-8")
            .body(body.to_string())
            .send()
            .await
            .context(format!("EWS request to {} failed", self.ews_url))?;

        let status = response.status();
        let text = response.text().await.context("Failed to read EWS response")?;

        if !status.is_success() {
            if status.as_u16() == 401 {
                anyhow::bail!(
                    "EWS Basic Auth failed (401) at {}",
                    self.ews_url
                );
            }
            anyhow::bail!("EWS request failed with status {status} at {}: {text}", self.ews_url);
        }

        validate_soap_response(&text, &self.ews_url)?;

        tracing::debug!("EWS Basic Auth request succeeded (status {status})");
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

/// Validate that the response is actually SOAP XML.
fn validate_soap_response(text: &str, ews_url: &str) -> Result<()> {
    let trimmed = text.trim_start();
    if !trimmed.starts_with("<?xml")
        && !trimmed.starts_with("<s:Envelope")
        && !trimmed.starts_with("<soap:Envelope")
    {
        tracing::warn!(
            "EWS response from {} is not SOAP XML (starts with: {:?}...)",
            ews_url,
            &trimmed[..trimmed.len().min(100)]
        );
        anyhow::bail!(
            "EWS response is not valid SOAP XML. \
             The server at {} may require different authentication or EWS may be disabled.",
            ews_url
        );
    }
    Ok(())
}

/// Parse NTLM username: "DOMAIN\user" -> ("DOMAIN", "user"), "user@domain" -> ("", "user@domain")
fn parse_ntlm_username(username: &str) -> (&str, &str) {
    if let Some(pos) = username.find('\\') {
        (&username[..pos], &username[pos + 1..])
    } else {
        ("", username)
    }
}

/// Get the local hostname for NTLM workstation field.
fn hostname() -> String {
    std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .unwrap_or_else(|_| "WORKSTATION".to_string())
}

/// Format an EWS ISO 8601 datetime into a more readable format.
/// Input: "2024-01-15T09:00:00Z" -> Output: "2024-01-15 09:00:00 UTC"
/// Input: "2024-01-15T09:00:00+01:00" -> Output: "2024-01-15 09:00:00 (+01:00)"
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
    // yyyy-mm-dd -> yyyy-mm-ddT00:00:00Z
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
