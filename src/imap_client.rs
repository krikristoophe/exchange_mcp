use anyhow::{Context, Result};
use base64::Engine;
use std::sync::Arc;

use crate::oauth::OAuthManager;

pub struct ImapClient {
    oauth: Arc<OAuthManager>,
    host: String,
    port: u16,
}

/// XOAUTH2 authenticator for the imap crate
struct XOAuth2Auth {
    user: String,
    access_token: String,
}

impl imap::Authenticator for XOAuth2Auth {
    type Response = String;

    fn process(&self, _challenge: &[u8]) -> Self::Response {
        format!(
            "user={}\x01auth=Bearer {}\x01\x01",
            self.user, self.access_token
        )
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct FolderInfo {
    pub name: String,
    pub attributes: Vec<String>,
    pub delimiter: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct EmailSummary {
    pub uid: u32,
    pub subject: String,
    pub from: String,
    pub date: String,
    pub flags: Vec<String>,
    pub size: Option<u32>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct EmailDetail {
    pub uid: u32,
    pub subject: String,
    pub from: String,
    pub to: String,
    pub cc: String,
    pub date: String,
    pub flags: Vec<String>,
    pub body_text: String,
    pub body_html: Option<String>,
    pub attachments: Vec<AttachmentInfo>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AttachmentInfo {
    pub filename: String,
    pub content_type: String,
    pub size: Option<usize>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct FolderStatus {
    pub name: String,
    pub total: u32,
    pub unseen: u32,
    pub recent: u32,
}

impl ImapClient {
    pub fn new(oauth: Arc<OAuthManager>, host: String, port: u16) -> Self {
        Self { oauth, host, port }
    }

    /// Connect and authenticate to IMAP (blocking, call via spawn_blocking)
    fn connect_sync(
        host: &str,
        port: u16,
        email: &str,
        access_token: &str,
    ) -> Result<imap::Session<native_tls::TlsStream<std::net::TcpStream>>> {
        let tls = native_tls::TlsConnector::new()?;
        let client = imap::connect((host, port), host, &tls)
            .context("Failed to connect to IMAP server")?;

        let auth = XOAuth2Auth {
            user: email.to_string(),
            access_token: access_token.to_string(),
        };

        let session = client
            .authenticate("XOAUTH2", &auth)
            .map_err(|(e, _)| e)
            .context("XOAUTH2 authentication failed")?;

        Ok(session)
    }

    /// List all mailbox folders
    pub async fn list_folders(&self) -> Result<Vec<FolderInfo>> {
        let access_token = self.oauth.get_access_token().await?;
        let host = self.host.clone();
        let port = self.port;
        let email = self.oauth.email().to_string();

        tokio::task::spawn_blocking(move || {
            let mut session = Self::connect_sync(&host, port, &email, &access_token)?;
            let mailboxes = session.list(Some(""), Some("*"))?;

            let folders: Vec<FolderInfo> = mailboxes
                .iter()
                .map(|mb| FolderInfo {
                    name: mb.name().to_string(),
                    attributes: mb.attributes().iter().map(|a| format!("{a:?}")).collect(),
                    delimiter: mb.delimiter().map(|d| d.to_string()),
                })
                .collect();

            session.logout()?;
            Ok(folders)
        })
        .await?
    }

    /// List emails in a folder
    pub async fn list_emails(
        &self,
        folder: &str,
        limit: Option<u32>,
    ) -> Result<Vec<EmailSummary>> {
        let access_token = self.oauth.get_access_token().await?;
        let host = self.host.clone();
        let port = self.port;
        let email = self.oauth.email().to_string();
        let folder = folder.to_string();

        tokio::task::spawn_blocking(move || {
            let mut session = Self::connect_sync(&host, port, &email, &access_token)?;
            let mailbox = session.select(&folder)?;

            let limit = limit.unwrap_or(20);
            let total = mailbox.exists;

            if total == 0 {
                session.logout()?;
                return Ok(vec![]);
            }

            let start = if total > limit { total - limit + 1 } else { 1 };
            let range = format!("{start}:{total}");

            let messages = session.fetch(&range, "(UID FLAGS ENVELOPE RFC822.SIZE)")?;

            let summaries: Vec<EmailSummary> = messages
                .iter()
                .filter_map(|msg| parse_email_summary(msg))
                .collect();

            session.logout()?;
            Ok(summaries)
        })
        .await?
    }

    /// Read a specific email by UID
    pub async fn read_email(&self, folder: &str, uid: u32) -> Result<EmailDetail> {
        let access_token = self.oauth.get_access_token().await?;
        let host = self.host.clone();
        let port = self.port;
        let email = self.oauth.email().to_string();
        let folder = folder.to_string();

        tokio::task::spawn_blocking(move || {
            let mut session = Self::connect_sync(&host, port, &email, &access_token)?;
            session.select(&folder)?;

            let messages = session.uid_fetch(uid.to_string(), "(UID FLAGS ENVELOPE BODY[])")?;

            let msg = messages.iter().next().context("Email not found")?;
            let detail = parse_email_detail(msg)?;

            session.logout()?;
            Ok(detail)
        })
        .await?
    }

    /// Search emails in a folder
    pub async fn search_emails(
        &self,
        folder: &str,
        query: &str,
        limit: Option<u32>,
    ) -> Result<Vec<EmailSummary>> {
        let access_token = self.oauth.get_access_token().await?;
        let host = self.host.clone();
        let port = self.port;
        let email = self.oauth.email().to_string();
        let folder = folder.to_string();
        let query = query.to_string();

        tokio::task::spawn_blocking(move || {
            let mut session = Self::connect_sync(&host, port, &email, &access_token)?;
            session.select(&folder)?;

            let uids = session.uid_search(&query)?;

            if uids.is_empty() {
                session.logout()?;
                return Ok(vec![]);
            }

            let limit = limit.unwrap_or(20) as usize;
            let mut uid_vec: Vec<u32> = uids.into_iter().collect();
            uid_vec.sort_unstable();
            uid_vec.reverse();
            uid_vec.truncate(limit);

            let uid_range: String = uid_vec
                .iter()
                .map(|u| u.to_string())
                .collect::<Vec<_>>()
                .join(",");

            let messages =
                session.uid_fetch(&uid_range, "(UID FLAGS ENVELOPE RFC822.SIZE)")?;

            let summaries: Vec<EmailSummary> = messages
                .iter()
                .filter_map(|msg| parse_email_summary(msg))
                .collect();

            session.logout()?;
            Ok(summaries)
        })
        .await?
    }

    /// Get folder status (total, unseen, recent)
    pub async fn get_folder_status(&self, folder: &str) -> Result<FolderStatus> {
        let access_token = self.oauth.get_access_token().await?;
        let host = self.host.clone();
        let port = self.port;
        let email = self.oauth.email().to_string();
        let folder = folder.to_string();

        tokio::task::spawn_blocking(move || {
            let mut session = Self::connect_sync(&host, port, &email, &access_token)?;
            let mailbox = session.examine(&folder)?;

            let unseen = session
                .uid_search("UNSEEN")
                .map(|uids| uids.len() as u32)
                .unwrap_or(0);

            let status = FolderStatus {
                name: folder,
                total: mailbox.exists,
                unseen,
                recent: mailbox.recent,
            };

            session.logout()?;
            Ok(status)
        })
        .await?
    }

    /// Mark an email as read
    pub async fn mark_as_read(&self, folder: &str, uid: u32) -> Result<()> {
        self.store_flag(folder, uid, "+FLAGS (\\Seen)").await
    }

    /// Mark an email as unread
    pub async fn mark_as_unread(&self, folder: &str, uid: u32) -> Result<()> {
        self.store_flag(folder, uid, "-FLAGS (\\Seen)").await
    }

    /// Move an email to another folder
    pub async fn move_email(&self, folder: &str, uid: u32, target_folder: &str) -> Result<()> {
        let access_token = self.oauth.get_access_token().await?;
        let host = self.host.clone();
        let port = self.port;
        let email = self.oauth.email().to_string();
        let folder = folder.to_string();
        let target_folder = target_folder.to_string();

        tokio::task::spawn_blocking(move || {
            let mut session = Self::connect_sync(&host, port, &email, &access_token)?;
            session.select(&folder)?;

            session.uid_copy(uid.to_string(), &target_folder)?;
            session.uid_store(uid.to_string(), "+FLAGS (\\Deleted)")?;
            session.expunge()?;

            session.logout()?;
            Ok(())
        })
        .await?
    }

    /// Delete an email (move to Deleted Items)
    pub async fn delete_email(&self, folder: &str, uid: u32) -> Result<()> {
        self.move_email(folder, uid, "Deleted Items").await
    }

    /// Set or remove an IMAP flag
    pub async fn set_flag(&self, folder: &str, uid: u32, flag: &str, add: bool) -> Result<()> {
        let op = if add { "+FLAGS" } else { "-FLAGS" };
        let query = format!("{op} ({flag})");
        self.store_flag(folder, uid, &query).await
    }

    async fn store_flag(&self, folder: &str, uid: u32, flag_cmd: &str) -> Result<()> {
        let access_token = self.oauth.get_access_token().await?;
        let host = self.host.clone();
        let port = self.port;
        let email = self.oauth.email().to_string();
        let folder = folder.to_string();
        let flag_cmd = flag_cmd.to_string();

        tokio::task::spawn_blocking(move || {
            let mut session = Self::connect_sync(&host, port, &email, &access_token)?;
            session.select(&folder)?;
            session.uid_store(uid.to_string(), &flag_cmd)?;
            session.logout()?;
            Ok(())
        })
        .await?
    }
}

fn parse_email_summary(msg: &imap::types::Fetch) -> Option<EmailSummary> {
    let uid = msg.uid?;
    let envelope = msg.envelope()?;

    let subject = envelope
        .subject
        .as_ref()
        .map(|s| decode_imap_utf8(s))
        .unwrap_or_default();

    let from = envelope
        .from
        .as_ref()
        .and_then(|addrs| addrs.first())
        .map(|addr| format_address(addr))
        .unwrap_or_default();

    let date = envelope
        .date
        .as_ref()
        .map(|d| String::from_utf8_lossy(d).to_string())
        .unwrap_or_default();

    let flags: Vec<String> = msg.flags().iter().map(|f| format!("{f:?}")).collect();

    Some(EmailSummary {
        uid,
        subject,
        from,
        date,
        flags,
        size: msg.size,
    })
}

fn parse_email_detail(msg: &imap::types::Fetch) -> Result<EmailDetail> {
    let uid = msg.uid.context("No UID")?;
    let envelope = msg.envelope().context("No envelope")?;

    let subject = envelope
        .subject
        .as_ref()
        .map(|s| decode_imap_utf8(s))
        .unwrap_or_default();

    let from = format_addresses(envelope.from.as_deref());
    let to = format_addresses(envelope.to.as_deref());
    let cc = format_addresses(envelope.cc.as_deref());

    let date = envelope
        .date
        .as_ref()
        .map(|d| String::from_utf8_lossy(d).to_string())
        .unwrap_or_default();

    let flags: Vec<String> = msg.flags().iter().map(|f| format!("{f:?}")).collect();

    let (body_text, body_html, attachments) = if let Some(body) = msg.body() {
        let (text, html) = parse_body(body);
        let attachments = extract_attachments(body);
        (text, html, attachments)
    } else {
        ("(no body)".to_string(), None, vec![])
    };

    Ok(EmailDetail {
        uid,
        subject,
        from,
        to,
        cc,
        date,
        flags,
        body_text,
        body_html,
        attachments,
    })
}

fn format_address(addr: &imap_proto::types::Address<'_>) -> String {
    let name = addr
        .name
        .as_ref()
        .map(|n| decode_imap_utf8(n))
        .unwrap_or_default();
    let mailbox = addr
        .mailbox
        .as_ref()
        .map(|m| String::from_utf8_lossy(m).to_string())
        .unwrap_or_default();
    let host = addr
        .host
        .as_ref()
        .map(|h| String::from_utf8_lossy(h).to_string())
        .unwrap_or_default();
    if name.is_empty() {
        format!("{mailbox}@{host}")
    } else {
        format!("{name} <{mailbox}@{host}>")
    }
}

fn format_addresses(addrs: Option<&[imap_proto::types::Address<'_>]>) -> String {
    addrs
        .map(|addrs: &[imap_proto::types::Address<'_>]| {
            addrs
                .iter()
                .map(|addr| format_address(addr))
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default()
}

fn decode_imap_utf8(data: &[u8]) -> String {
    let s = String::from_utf8_lossy(data).to_string();
    if s.contains("=?") {
        decode_rfc2047(&s)
    } else {
        s
    }
}

fn decode_rfc2047(s: &str) -> String {
    let mut result = String::new();
    let mut remaining = s;

    while let Some(start) = remaining.find("=?") {
        result.push_str(&remaining[..start]);
        let after_start = &remaining[start + 2..];

        if let Some(end) = after_start.find("?=") {
            let encoded_word = &after_start[..end];
            let parts: Vec<&str> = encoded_word.splitn(3, '?').collect();
            if parts.len() == 3 {
                let encoding = parts[1].to_uppercase();
                let text = parts[2];

                let decoded = match encoding.as_str() {
                    "B" => base64::engine::general_purpose::STANDARD
                        .decode(text)
                        .ok()
                        .and_then(|bytes| String::from_utf8(bytes).ok()),
                    "Q" => {
                        let text = text.replace('_', " ");
                        quoted_printable_decode(&text)
                    }
                    _ => None,
                };

                if let Some(decoded) = decoded {
                    result.push_str(&decoded);
                } else {
                    result.push_str(&remaining[start..start + 2 + end + 2]);
                }
            }
            remaining = &after_start[end + 2..];
        } else {
            result.push_str(&remaining[start..]);
            remaining = "";
        }
    }
    result.push_str(remaining);
    result
}

fn quoted_printable_decode(s: &str) -> Option<String> {
    let mut result = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'=' && i + 2 < bytes.len() {
            if let Ok(byte) =
                u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?, 16)
            {
                result.push(byte);
                i += 3;
                continue;
            }
        }
        result.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(result).ok()
}

fn parse_body(body: &[u8]) -> (String, Option<String>) {
    match mailparse::parse_mail(body) {
        Ok(parsed) => {
            let mut text_body = String::new();
            let mut html_body = None;

            extract_parts(&parsed, &mut text_body, &mut html_body);

            if text_body.is_empty() && html_body.is_none() {
                text_body = parsed
                    .get_body()
                    .unwrap_or_else(|_| String::from_utf8_lossy(body).to_string());
            }

            (text_body, html_body)
        }
        Err(_) => (String::from_utf8_lossy(body).to_string(), None),
    }
}

fn extract_parts(
    mail: &mailparse::ParsedMail,
    text_body: &mut String,
    html_body: &mut Option<String>,
) {
    if mail.subparts.is_empty() {
        let content_type = mail.ctype.mimetype.to_lowercase();
        // Skip attachments
        let disposition = mail
            .headers
            .iter()
            .find(|h| h.get_key().eq_ignore_ascii_case("content-disposition"))
            .map(|h| h.get_value())
            .unwrap_or_default();
        if disposition.starts_with("attachment") {
            return;
        }
        if let Ok(body) = mail.get_body() {
            if content_type.contains("text/plain") {
                if !text_body.is_empty() {
                    text_body.push('\n');
                }
                text_body.push_str(&body);
            } else if content_type.contains("text/html") {
                *html_body = Some(body);
            }
        }
    } else {
        for part in &mail.subparts {
            extract_parts(part, text_body, html_body);
        }
    }
}

fn extract_attachments(body: &[u8]) -> Vec<AttachmentInfo> {
    let mut attachments = Vec::new();
    if let Ok(parsed) = mailparse::parse_mail(body) {
        collect_attachments(&parsed, &mut attachments);
    }
    attachments
}

fn collect_attachments(mail: &mailparse::ParsedMail, attachments: &mut Vec<AttachmentInfo>) {
    if mail.subparts.is_empty() {
        let content_type = mail.ctype.mimetype.to_lowercase();
        let disposition = mail
            .headers
            .iter()
            .find(|h| h.get_key().eq_ignore_ascii_case("content-disposition"))
            .map(|h| h.get_value())
            .unwrap_or_default();

        let is_attachment = disposition.starts_with("attachment")
            || (!content_type.contains("text/plain")
                && !content_type.contains("text/html")
                && !content_type.starts_with("multipart/"));

        if is_attachment {
            let filename = mail
                .ctype
                .params
                .get("name")
                .cloned()
                .or_else(|| {
                    // Try content-disposition filename param
                    extract_disposition_filename(&disposition)
                })
                .unwrap_or_else(|| "unnamed".to_string());

            let size = mail.get_body_raw().ok().map(|b| b.len());

            attachments.push(AttachmentInfo {
                filename,
                content_type: content_type.to_string(),
                size,
            });
        }
    } else {
        for part in &mail.subparts {
            collect_attachments(part, attachments);
        }
    }
}

fn extract_disposition_filename(disposition: &str) -> Option<String> {
    // Parse "attachment; filename=\"file.pdf\""
    for part in disposition.split(';') {
        let part = part.trim();
        if let Some(rest) = part.strip_prefix("filename=") {
            let name = rest.trim_matches('"').trim_matches('\'');
            if !name.is_empty() {
                return Some(name.to_string());
            }
        }
    }
    None
}

/// Simple HTML to text converter — strips tags and decodes common entities
pub fn html_to_text(html: &str) -> String {
    let mut result = String::with_capacity(html.len());
    let mut in_tag = false;
    let mut in_script = false;
    let mut in_style = false;
    let mut last_was_space = false;

    let html_lower = html.to_lowercase();
    let bytes = html.as_bytes();
    let lower_bytes = html_lower.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i < len {
        if in_script {
            if i + 9 <= len && &lower_bytes[i..i + 9] == b"</script>" {
                in_script = false;
                i += 9;
            } else {
                i += 1;
            }
            continue;
        }
        if in_style {
            if i + 8 <= len && &lower_bytes[i..i + 8] == b"</style>" {
                in_style = false;
                i += 8;
            } else {
                i += 1;
            }
            continue;
        }

        if bytes[i] == b'<' {
            // Check for block elements that should add newlines
            let rest = &html_lower[i..];
            if rest.starts_with("<br") || rest.starts_with("<p") || rest.starts_with("<div")
                || rest.starts_with("<tr") || rest.starts_with("<li")
                || rest.starts_with("</p") || rest.starts_with("</div")
                || rest.starts_with("</tr") || rest.starts_with("<h")
                || rest.starts_with("</h")
            {
                if !result.ends_with('\n') {
                    result.push('\n');
                }
                last_was_space = true;
            }
            if rest.starts_with("<script") {
                in_script = true;
            } else if rest.starts_with("<style") {
                in_style = true;
            }
            in_tag = true;
            i += 1;
        } else if bytes[i] == b'>' {
            in_tag = false;
            i += 1;
        } else if in_tag {
            i += 1;
        } else if bytes[i] == b'&' {
            // Decode HTML entities
            let rest = &html[i..];
            if rest.starts_with("&amp;") {
                result.push('&');
                i += 5;
            } else if rest.starts_with("&lt;") {
                result.push('<');
                i += 4;
            } else if rest.starts_with("&gt;") {
                result.push('>');
                i += 4;
            } else if rest.starts_with("&quot;") {
                result.push('"');
                i += 6;
            } else if rest.starts_with("&apos;") {
                result.push('\'');
                i += 6;
            } else if rest.starts_with("&nbsp;") {
                result.push(' ');
                i += 6;
            } else if rest.starts_with("&#") {
                if let Some(semi) = rest[2..].find(';') {
                    let num_str = &rest[2..2 + semi];
                    let code = if let Some(hex) = num_str.strip_prefix('x') {
                        u32::from_str_radix(hex, 16).ok()
                    } else {
                        num_str.parse::<u32>().ok()
                    };
                    if let Some(ch) = code.and_then(char::from_u32) {
                        result.push(ch);
                    }
                    i += 2 + semi + 1;
                } else {
                    result.push('&');
                    i += 1;
                }
            } else {
                result.push('&');
                i += 1;
            }
            last_was_space = false;
        } else {
            let ch = bytes[i] as char;
            if ch.is_whitespace() {
                if !last_was_space {
                    result.push(if ch == '\n' { '\n' } else { ' ' });
                    last_was_space = true;
                }
            } else {
                result.push(ch);
                last_was_space = false;
            }
            i += 1;
        }
    }

    // Clean up excessive newlines
    let mut cleaned = String::new();
    let mut newline_count = 0;
    for ch in result.chars() {
        if ch == '\n' {
            newline_count += 1;
            if newline_count <= 2 {
                cleaned.push(ch);
            }
        } else {
            newline_count = 0;
            cleaned.push(ch);
        }
    }

    cleaned.trim().to_string()
}
