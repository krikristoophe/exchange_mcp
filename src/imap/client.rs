use anyhow::{Context, Result};
use std::sync::Arc;

use crate::auth::{AuthProvider, ImapCredentials};
use crate::cache::EmailCache;
use super::parse;

pub struct ImapClient {
    auth: Arc<dyn AuthProvider>,
    host: String,
    port: u16,
    smtp_host: String,
    smtp_port: u16,
    cache: Arc<EmailCache>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
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
pub struct ContactInfo {
    pub email: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub frequency: u32,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct FolderStatus {
    pub name: String,
    pub total: u32,
    pub unseen: u32,
    pub recent: u32,
}

impl ImapClient {
    pub fn new(
        auth: Arc<dyn AuthProvider>,
        host: String,
        port: u16,
        smtp_host: String,
        smtp_port: u16,
    ) -> Self {
        Self {
            auth,
            host,
            port,
            smtp_host,
            smtp_port,
            cache: Arc::new(EmailCache::new()),
        }
    }

    /// Connect and authenticate to IMAP (blocking, call via spawn_blocking)
    fn connect_sync(
        host: &str,
        port: u16,
        credentials: ImapCredentials,
    ) -> Result<imap::Session<native_tls::TlsStream<std::net::TcpStream>>> {
        let tls = native_tls::TlsConnector::new()?;
        let client = imap::connect((host, port), host, &tls)
            .context("Failed to connect to IMAP server")?;

        client
            .login(&credentials.username, &credentials.password)
            .map_err(|(e, _)| e)
            .context("IMAP login failed")
    }

    /// List all mailbox folders
    pub async fn list_folders(&self) -> Result<Vec<FolderInfo>> {
        // Check cache
        if let Some(cached) = self.cache.get_folders() {
            return Ok(cached);
        }

        let credentials = self.auth.get_credentials().await?;
        let host = self.host.clone();
        let port = self.port;
        let cache = self.cache.clone();

        tokio::task::spawn_blocking(move || {
            let mut session = Self::connect_sync(&host, port, credentials)?;
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
            cache.set_folders(folders.clone());
            Ok(folders)
        })
        .await?
    }

    /// List emails in a folder
    pub async fn list_emails(
        &self,
        folder: &str,
        limit: Option<u32>,
        include_preview: bool,
    ) -> Result<Vec<EmailSummary>> {
        let limit_val = limit.unwrap_or(20);

        let credentials = self.auth.get_credentials().await?;
        let host = self.host.clone();
        let port = self.port;
        let folder = folder.to_string();
        let cache = self.cache.clone();

        tokio::task::spawn_blocking(move || {
            let mut session = Self::connect_sync(&host, port, credentials)?;

            // Resync: check folder status via STATUS before using cache
            let status = session.status(&folder, "(MESSAGES UIDNEXT)")?;
            let exists = status.exists;
            let uidnext = status.uid_next;

            // If folder unchanged and cache available, return cached data
            if !include_preview && cache.check_fingerprint(&folder, exists, uidnext) {
                if let Some(cached) = cache.get_summaries(&folder, limit_val) {
                    session.logout()?;
                    return Ok(cached);
                }
            }

            let mailbox = session.select(&folder)?;
            let total = mailbox.exists;

            if total == 0 {
                session.logout()?;
                cache.set_fingerprint(&folder, exists, uidnext);
                return Ok(vec![]);
            }

            let start = if total > limit_val { total - limit_val + 1 } else { 1 };
            let range = format!("{start}:{total}");

            let fetch_items = if include_preview {
                "(UID FLAGS ENVELOPE RFC822.SIZE BODY.PEEK[TEXT]<0.512>)"
            } else {
                "(UID FLAGS ENVELOPE RFC822.SIZE)"
            };

            let messages = session.fetch(&range, fetch_items)?;

            let summaries: Vec<EmailSummary> = messages
                .iter()
                .filter_map(|msg| {
                    let mut summary = parse::parse_email_summary(msg)?;
                    if include_preview {
                        summary.snippet = parse::extract_snippet(msg, 200);
                    }
                    Some(summary)
                })
                .collect();

            session.logout()?;
            cache.set_fingerprint(&folder, exists, uidnext);
            cache.set_summaries(&folder, limit_val, summaries.clone());
            Ok(summaries)
        })
        .await?
    }

    /// Read a specific email by UID (uses BODY.PEEK[] to avoid marking as read)
    pub async fn read_email(&self, folder: &str, uid: u32) -> Result<EmailDetail> {
        // Check cache
        if let Some(cached) = self.cache.get_detail(folder, uid) {
            return Ok(cached);
        }

        let credentials = self.auth.get_credentials().await?;
        let host = self.host.clone();
        let port = self.port;
        let folder = folder.to_string();
        let cache = self.cache.clone();

        tokio::task::spawn_blocking(move || {
            let mut session = Self::connect_sync(&host, port, credentials)?;
            session.select(&folder)?;

            let messages = session.uid_fetch(uid.to_string(), "(UID FLAGS ENVELOPE BODY.PEEK[])")?;

            let msg = messages.iter().next().context("Email not found")?;
            let detail = parse::parse_email_detail(msg)?;

            session.logout()?;
            cache.set_detail(&folder, uid, detail.clone());
            Ok(detail)
        })
        .await?
    }

    /// Read multiple emails by UIDs in a single IMAP connection
    pub async fn read_emails(&self, folder: &str, uids: &[u32]) -> Result<Vec<EmailDetail>> {
        if uids.is_empty() {
            return Ok(vec![]);
        }

        // Check cache for all UIDs, find which ones we need to fetch
        let mut cached_details: Vec<(u32, EmailDetail)> = Vec::new();
        let mut missing_uids: Vec<u32> = Vec::new();

        for &uid in uids {
            if let Some(detail) = self.cache.get_detail(folder, uid) {
                cached_details.push((uid, detail));
            } else {
                missing_uids.push(uid);
            }
        }

        // If all are cached, return them in order
        if missing_uids.is_empty() {
            let mut result: Vec<EmailDetail> = Vec::with_capacity(uids.len());
            for &uid in uids {
                if let Some((_, detail)) = cached_details.iter().find(|(u, _)| *u == uid) {
                    result.push(detail.clone());
                }
            }
            return Ok(result);
        }

        let credentials = self.auth.get_credentials().await?;
        let host = self.host.clone();
        let port = self.port;
        let folder_owned = folder.to_string();
        let cache = self.cache.clone();

        let fetched = tokio::task::spawn_blocking(move || {
            let mut session = Self::connect_sync(&host, port, credentials)?;
            session.select(&folder_owned)?;

            let uid_range: String = missing_uids
                .iter()
                .map(|u| u.to_string())
                .collect::<Vec<_>>()
                .join(",");

            let messages =
                session.uid_fetch(&uid_range, "(UID FLAGS ENVELOPE BODY.PEEK[])")?;

            let mut details: Vec<EmailDetail> = Vec::new();
            for msg in messages.iter() {
                match parse::parse_email_detail(msg) {
                    Ok(detail) => {
                        cache.set_detail(&folder_owned, detail.uid, detail.clone());
                        details.push(detail);
                    }
                    Err(e) => {
                        tracing::warn!("Failed to parse email: {e}");
                    }
                }
            }

            session.logout()?;
            Ok::<_, anyhow::Error>(details)
        })
        .await??;

        // Merge cached + fetched, return in original UID order
        let mut all_details = std::collections::HashMap::new();
        for (uid, detail) in cached_details {
            all_details.insert(uid, detail);
        }
        for detail in fetched {
            all_details.insert(detail.uid, detail);
        }

        let mut result = Vec::with_capacity(uids.len());
        for &uid in uids {
            if let Some(detail) = all_details.remove(&uid) {
                result.push(detail);
            }
        }

        Ok(result)
    }

    /// Search emails in a folder
    pub async fn search_emails(
        &self,
        folder: &str,
        query: &str,
        limit: Option<u32>,
        include_preview: bool,
    ) -> Result<Vec<EmailSummary>> {
        let limit_val = limit.unwrap_or(20);

        let credentials = self.auth.get_credentials().await?;
        let host = self.host.clone();
        let port = self.port;
        let folder = folder.to_string();
        let query = query.to_string();
        let cache = self.cache.clone();

        tokio::task::spawn_blocking(move || {
            let mut session = Self::connect_sync(&host, port, credentials)?;

            // Resync: check folder status via STATUS before using cache
            let status = session.status(&folder, "(MESSAGES UIDNEXT)")?;
            let exists = status.exists;
            let uidnext = status.uid_next;

            // If folder unchanged and cache available, return cached data
            if !include_preview && cache.check_fingerprint(&folder, exists, uidnext) {
                if let Some(cached) = cache.get_search(&folder, &query, limit_val) {
                    session.logout()?;
                    return Ok(cached);
                }
            }

            session.select(&folder)?;

            let uids = session.uid_search(&query)?;

            if uids.is_empty() {
                session.logout()?;
                cache.set_fingerprint(&folder, exists, uidnext);
                return Ok(vec![]);
            }

            let mut uid_vec: Vec<u32> = uids.into_iter().collect();
            uid_vec.sort_unstable();
            uid_vec.reverse();
            uid_vec.truncate(limit_val as usize);

            let uid_range: String = uid_vec
                .iter()
                .map(|u| u.to_string())
                .collect::<Vec<_>>()
                .join(",");

            let fetch_items = if include_preview {
                "(UID FLAGS ENVELOPE RFC822.SIZE BODY.PEEK[TEXT]<0.512>)"
            } else {
                "(UID FLAGS ENVELOPE RFC822.SIZE)"
            };

            let messages = session.uid_fetch(&uid_range, fetch_items)?;

            let summaries: Vec<EmailSummary> = messages
                .iter()
                .filter_map(|msg| {
                    let mut summary = parse::parse_email_summary(msg)?;
                    if include_preview {
                        summary.snippet = parse::extract_snippet(msg, 200);
                    }
                    Some(summary)
                })
                .collect();

            session.logout()?;
            cache.set_fingerprint(&folder, exists, uidnext);
            cache.set_search(&folder, &query, limit_val, summaries.clone());
            Ok(summaries)
        })
        .await?
    }

    /// Get folder status (total, unseen, recent)
    pub async fn get_folder_status(&self, folder: &str) -> Result<FolderStatus> {
        let credentials = self.auth.get_credentials().await?;
        let host = self.host.clone();
        let port = self.port;
        let folder = folder.to_string();
        let cache = self.cache.clone();

        tokio::task::spawn_blocking(move || {
            let mut session = Self::connect_sync(&host, port, credentials)?;

            // Resync: check folder status via STATUS before using cache
            let quick_status = session.status(&folder, "(MESSAGES UIDNEXT)")?;
            let exists = quick_status.exists;
            let uidnext = quick_status.uid_next;

            if cache.check_fingerprint(&folder, exists, uidnext) {
                if let Some(cached) = cache.get_status(&folder) {
                    session.logout()?;
                    return Ok(cached);
                }
            }

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
            cache.set_fingerprint(&status.name, exists, uidnext);
            cache.set_status(&status.name, status.clone());
            Ok(status)
        })
        .await?
    }

    /// Create a new mailbox folder
    pub async fn create_folder(&self, folder: &str) -> Result<()> {
        let credentials = self.auth.get_credentials().await?;
        let host = self.host.clone();
        let port = self.port;
        let folder = folder.to_string();

        tokio::task::spawn_blocking(move || {
            let mut session = Self::connect_sync(&host, port, credentials)?;
            session.create(&folder)?;
            session.logout()?;
            Ok::<(), anyhow::Error>(())
        })
        .await??;

        self.cache.invalidate_folders_list();
        Ok(())
    }

    /// Rename (move) a mailbox folder
    pub async fn rename_folder(&self, folder: &str, new_name: &str) -> Result<()> {
        let credentials = self.auth.get_credentials().await?;
        let host = self.host.clone();
        let port = self.port;
        let folder_owned = folder.to_string();
        let new_name = new_name.to_string();

        tokio::task::spawn_blocking(move || {
            let mut session = Self::connect_sync(&host, port, credentials)?;
            session.rename(&folder_owned, &new_name)?;
            session.logout()?;
            Ok::<(), anyhow::Error>(())
        })
        .await??;

        self.cache.invalidate_folders_list();
        self.cache.invalidate_folder(folder);
        Ok(())
    }

    /// Delete a mailbox folder
    pub async fn delete_folder(&self, folder: &str) -> Result<()> {
        let credentials = self.auth.get_credentials().await?;
        let host = self.host.clone();
        let port = self.port;
        let folder_owned = folder.to_string();

        tokio::task::spawn_blocking(move || {
            let mut session = Self::connect_sync(&host, port, credentials)?;
            session.delete(&folder_owned)?;
            session.logout()?;
            Ok::<(), anyhow::Error>(())
        })
        .await??;

        self.cache.invalidate_folders_list();
        self.cache.invalidate_folder(folder);
        Ok(())
    }

    /// Mark an email as read
    pub async fn mark_as_read(&self, folder: &str, uid: u32) -> Result<()> {
        self.store_flag(folder, uid, "+FLAGS (\\Seen)").await?;
        self.cache.invalidate_detail(folder, uid);
        self.cache.invalidate_folder(folder);
        Ok(())
    }

    /// Mark an email as unread
    pub async fn mark_as_unread(&self, folder: &str, uid: u32) -> Result<()> {
        self.store_flag(folder, uid, "-FLAGS (\\Seen)").await?;
        self.cache.invalidate_detail(folder, uid);
        self.cache.invalidate_folder(folder);
        Ok(())
    }

    /// Move an email to another folder
    pub async fn move_email(&self, folder: &str, uid: u32, target_folder: &str) -> Result<()> {
        let credentials = self.auth.get_credentials().await?;
        let host = self.host.clone();
        let port = self.port;
        let folder_owned = folder.to_string();
        let target_owned = target_folder.to_string();

        tokio::task::spawn_blocking(move || {
            let mut session = Self::connect_sync(&host, port, credentials)?;
            session.select(&folder_owned)?;

            session.uid_copy(uid.to_string(), &target_owned)?;
            session.uid_store(uid.to_string(), "+FLAGS (\\Deleted)")?;
            session.expunge()?;

            session.logout()?;
            Ok::<(), anyhow::Error>(())
        })
        .await??;

        self.cache.invalidate_folder(folder);
        self.cache.invalidate_folder(target_folder);
        Ok(())
    }

    /// Delete an email (move to Deleted Items)
    pub async fn delete_email(&self, folder: &str, uid: u32) -> Result<()> {
        self.move_email(folder, uid, "Deleted Items").await
    }

    /// Set or remove an IMAP flag
    pub async fn set_flag(&self, folder: &str, uid: u32, flag: &str, add: bool) -> Result<()> {
        let op = if add { "+FLAGS" } else { "-FLAGS" };
        let query = format!("{op} ({flag})");
        self.store_flag(folder, uid, &query).await?;
        self.cache.invalidate_detail(folder, uid);
        self.cache.invalidate_folder(folder);
        Ok(())
    }

    async fn store_flag(&self, folder: &str, uid: u32, flag_cmd: &str) -> Result<()> {
        let credentials = self.auth.get_credentials().await?;
        let host = self.host.clone();
        let port = self.port;
        let folder = folder.to_string();
        let flag_cmd = flag_cmd.to_string();

        tokio::task::spawn_blocking(move || {
            let mut session = Self::connect_sync(&host, port, credentials)?;
            session.select(&folder)?;
            session.uid_store(uid.to_string(), &flag_cmd)?;
            session.logout()?;
            Ok(())
        })
        .await?
    }

    /// Get the UID of the last message in a folder (used after APPEND to retrieve the new UID).
    fn get_last_uid(
        session: &mut imap::Session<native_tls::TlsStream<std::net::TcpStream>>,
        folder: &str,
    ) -> Result<u32> {
        let mailbox = session.select(folder)?;
        let total = mailbox.exists;
        if total == 0 {
            anyhow::bail!("Folder is empty after append");
        }
        let range = format!("{total}:{total}");
        let messages = session.fetch(&range, "UID")?;
        let msg = messages.iter().next().context("Could not fetch last message UID")?;
        msg.uid.context("No UID for last message")
    }

    /// Build an RFC 822 message from components.
    fn build_message(
        from: &str,
        to: &[String],
        cc: &[String],
        subject: &str,
        body: &str,
        in_reply_to: Option<&str>,
        references: Option<&str>,
    ) -> Result<String> {
        use lettre::message::{header, Mailbox, MessageBuilder};

        let from_mailbox: Mailbox = from.parse().context("Invalid From address")?;
        let mut builder = MessageBuilder::new()
            .from(from_mailbox)
            .subject(subject);

        for addr in to {
            let mb: Mailbox = addr.trim().parse().context(format!("Invalid To address: {addr}"))?;
            builder = builder.to(mb);
        }
        for addr in cc {
            let mb: Mailbox = addr.trim().parse().context(format!("Invalid Cc address: {addr}"))?;
            builder = builder.cc(mb);
        }

        if let Some(msg_id) = in_reply_to {
            builder = builder.header(header::InReplyTo::from(msg_id.to_string()));
            if let Some(refs) = references {
                builder = builder.header(header::References::from(refs.to_string()));
            } else {
                builder = builder.header(header::References::from(msg_id.to_string()));
            }
        }

        let message = builder
            .header(header::ContentType::TEXT_PLAIN)
            .body(body.to_string())
            .context("Failed to build email message")?;

        Ok(String::from_utf8(message.formatted())
            .context("Message contains invalid UTF-8")?)
    }

    /// Send an email via SMTP and save a copy to "Sent Items" via IMAP APPEND.
    fn send_smtp_and_save(
        smtp_host: &str,
        smtp_port: u16,
        imap_host: &str,
        imap_port: u16,
        credentials: &ImapCredentials,
        rfc822: &[u8],
    ) -> Result<()> {
        use lettre::{SmtpTransport, Transport};
        use lettre::transport::smtp::authentication::Credentials as SmtpCreds;

        let creds = SmtpCreds::new(
            credentials.username.clone(),
            credentials.password.clone(),
        );

        let mailer = SmtpTransport::starttls_relay(smtp_host)
            .context("Failed to create SMTP transport")?
            .port(smtp_port)
            .credentials(creds)
            .build();

        let envelope = lettre::address::Envelope::new(
            {
                let addr: lettre::Address = credentials.username.parse()
                    .context("Invalid sender address for SMTP envelope")?;
                Some(addr)
            },
            {
                // Parse recipients from the RFC 822 message To and Cc headers
                let parsed = mailparse::parse_mail(rfc822).context("Failed to parse composed message")?;
                let mut recipients = Vec::new();
                for hdr in &parsed.headers {
                    let key = hdr.get_key().to_lowercase();
                    if key == "to" || key == "cc" || key == "bcc" {
                        let value = hdr.get_value();
                        for addr_str in value.split(',') {
                            let addr_str = addr_str.trim();
                            // Extract email from "Name <email>" or plain "email"
                            let email = if let Some(start) = addr_str.rfind('<') {
                                addr_str[start + 1..].trim_end_matches('>').trim()
                            } else {
                                addr_str
                            };
                            if let Ok(addr) = email.parse::<lettre::Address>() {
                                recipients.push(addr);
                            }
                        }
                    }
                }
                if recipients.is_empty() {
                    anyhow::bail!("No valid recipients found");
                }
                recipients
            },
        ).context("Failed to build SMTP envelope")?;

        mailer.send_raw(&envelope, rfc822).context("SMTP send failed")?;

        // Save to Sent Items via IMAP APPEND
        let mut session = Self::connect_sync(imap_host, imap_port, ImapCredentials {
            username: credentials.username.clone(),
            password: credentials.password.clone(),
        })?;
        let _ = session.append("Sent Items", rfc822);
        session.logout()?;

        Ok(())
    }

    /// Create a draft email (saved to Drafts folder via IMAP APPEND).
    /// Returns the UID of the created draft.
    pub async fn create_draft(
        &self,
        to: &[String],
        cc: &[String],
        subject: &str,
        body: &str,
    ) -> Result<String> {
        let credentials = self.auth.get_credentials().await?;
        let from = credentials.username.clone();
        let rfc822 = Self::build_message(&from, to, cc, subject, body, None, None)?;

        let host = self.host.clone();
        let port = self.port;
        let cache = self.cache.clone();

        tokio::task::spawn_blocking(move || {
            let mut session = Self::connect_sync(&host, port, ImapCredentials {
                username: credentials.username,
                password: credentials.password,
            })?;
            session
                .append_with_flags("Drafts", rfc822.as_bytes(), &[imap::types::Flag::Draft])
                .context("Failed to save draft")?;

            let uid = Self::get_last_uid(&mut session, "Drafts")?;

            session.logout()?;
            cache.invalidate_folder("Drafts");
            Ok(serde_json::json!({
                "message": "Draft saved to Drafts folder",
                "uid": uid,
                "folder": "Drafts"
            }).to_string())
        })
        .await?
    }

    /// Update an existing draft: fetch it, merge with new fields, delete old, append new.
    pub async fn update_draft(
        &self,
        uid: u32,
        to: Option<Vec<String>>,
        cc: Option<Vec<String>>,
        subject: Option<String>,
        body: Option<String>,
    ) -> Result<String> {
        let credentials = self.auth.get_credentials().await?;
        let host = self.host.clone();
        let port = self.port;
        let cache = self.cache.clone();

        tokio::task::spawn_blocking(move || {
            let mut session = Self::connect_sync(&host, port, ImapCredentials {
                username: credentials.username.clone(),
                password: credentials.password.clone(),
            })?;
            session.select("Drafts")?;

            // 1. Fetch original draft to extract current fields
            let messages = session.uid_fetch(uid.to_string(), "BODY.PEEK[]")?;
            let msg = messages.iter().next().context("Draft not found")?;
            let raw = msg.body().context("No message body")?;

            let parsed = mailparse::parse_mail(raw).context("Failed to parse draft")?;

            // Extract current values from headers
            let mut current_to = Vec::new();
            let mut current_cc = Vec::new();
            let mut current_subject = String::new();

            for hdr in &parsed.headers {
                let key = hdr.get_key().to_lowercase();
                match key.as_str() {
                    "to" => {
                        for addr in hdr.get_value().split(',') {
                            let addr = addr.trim().to_string();
                            if !addr.is_empty() {
                                current_to.push(addr);
                            }
                        }
                    }
                    "cc" => {
                        for addr in hdr.get_value().split(',') {
                            let addr = addr.trim().to_string();
                            if !addr.is_empty() {
                                current_cc.push(addr);
                            }
                        }
                    }
                    "subject" => {
                        current_subject = hdr.get_value();
                    }
                    _ => {}
                }
            }

            // Extract current body text
            let current_body = parsed.get_body().unwrap_or_default();

            // Merge: use provided values or fall back to current
            let final_to = to.unwrap_or(current_to);
            let final_cc = cc.unwrap_or(current_cc);
            let final_subject = subject.unwrap_or(current_subject);
            let final_body = body.unwrap_or(current_body);

            drop(messages);

            // 2. Delete old draft
            session.uid_store(uid.to_string(), "+FLAGS (\\Deleted)")?;
            session.expunge()?;

            // 3. Build and append new draft
            let from = credentials.username.clone();
            let rfc822 = Self::build_message(&from, &final_to, &final_cc, &final_subject, &final_body, None, None)?;

            session
                .append_with_flags("Drafts", rfc822.as_bytes(), &[imap::types::Flag::Draft])
                .context("Failed to save updated draft")?;

            let new_uid = Self::get_last_uid(&mut session, "Drafts")?;

            session.logout()?;
            cache.invalidate_folder("Drafts");

            Ok(serde_json::json!({
                "message": "Draft updated in Drafts folder",
                "uid": new_uid,
                "folder": "Drafts"
            }).to_string())
        })
        .await?
    }

    /// Send a draft email: fetch it from Drafts, send via SMTP, save to Sent Items,
    /// then delete the draft from the Drafts folder.
    pub async fn send_draft(&self, uid: u32) -> Result<String> {
        let credentials = self.auth.get_credentials().await?;
        let host = self.host.clone();
        let port = self.port;
        let smtp_host = self.smtp_host.clone();
        let smtp_port = self.smtp_port;
        let cache = self.cache.clone();

        tokio::task::spawn_blocking(move || {
            // 1. Fetch the draft's raw RFC822 content
            let mut session = Self::connect_sync(&host, port, ImapCredentials {
                username: credentials.username.clone(),
                password: credentials.password.clone(),
            })?;
            session.select("Drafts")?;
            let messages = session.uid_fetch(uid.to_string(), "BODY.PEEK[]")?;
            let msg = messages.iter().next().context("Draft not found")?;
            let rfc822 = msg.body().context("No message body")?.to_vec();
            drop(messages);

            // 2. Send via SMTP and save to Sent Items
            Self::send_smtp_and_save(
                &smtp_host, smtp_port,
                &host, port,
                &credentials,
                &rfc822,
            )?;

            // 3. Delete the draft
            session.uid_store(uid.to_string(), "+FLAGS (\\Deleted)")?;
            session.expunge()?;

            // 4. Get UID of saved copy in Sent Items
            let sent_uid = Self::get_last_uid(&mut session, "Sent Items").ok();

            session.logout()?;

            cache.invalidate_folder("Drafts");
            cache.invalidate_folder("Sent Items");

            Ok(serde_json::json!({
                "message": "Draft sent and removed from Drafts folder",
                "uid": sent_uid,
                "folder": "Sent Items"
            }).to_string())
        })
        .await?
    }

    /// Delete a draft email from the Drafts folder (moves to Deleted Items).
    pub async fn delete_draft(&self, uid: u32) -> Result<()> {
        self.move_email("Drafts", uid, "Deleted Items").await
    }

    /// Send an email via SMTP and save to Sent Items.
    pub async fn send_email(
        &self,
        to: &[String],
        cc: &[String],
        subject: &str,
        body: &str,
    ) -> Result<String> {
        let credentials = self.auth.get_credentials().await?;
        let from = credentials.username.clone();
        let rfc822 = Self::build_message(&from, to, cc, subject, body, None, None)?;

        let smtp_host = self.smtp_host.clone();
        let smtp_port = self.smtp_port;
        let imap_host = self.host.clone();
        let imap_port = self.port;
        let cache = self.cache.clone();

        tokio::task::spawn_blocking(move || {
            Self::send_smtp_and_save(
                &smtp_host, smtp_port,
                &imap_host, imap_port,
                &credentials,
                rfc822.as_bytes(),
            )?;

            // Get UID of saved copy in Sent Items
            let sent_uid = {
                let mut session = Self::connect_sync(&imap_host, imap_port, ImapCredentials {
                    username: credentials.username,
                    password: credentials.password,
                })?;
                let uid = Self::get_last_uid(&mut session, "Sent Items").ok();
                session.logout()?;
                uid
            };

            cache.invalidate_folder("Sent Items");
            Ok(serde_json::json!({
                "message": "Email sent successfully",
                "uid": sent_uid,
                "folder": "Sent Items"
            }).to_string())
        })
        .await?
    }

    /// Reply to an email. Reads the original, composes a reply, sends via SMTP.
    pub async fn reply_email(
        &self,
        folder: &str,
        uid: u32,
        body: &str,
        reply_all: bool,
        additional_cc: &[String],
        lang: &str,
    ) -> Result<String> {
        // Read original email for subject, from, message-id, etc.
        let original = self.read_email(folder, uid).await?;

        let credentials = self.auth.get_credentials().await?;
        let from = credentials.username.clone();

        // Build reply subject
        let subject = if original.subject.starts_with("Re:") || original.subject.starts_with("RE:") {
            original.subject.clone()
        } else {
            format!("Re: {}", original.subject)
        };

        // Determine recipients
        let to_addrs = vec![extract_email_address(&original.from)];
        let mut cc_addrs = Vec::new();
        if reply_all {
            // Add original To (minus ourselves) to CC
            for addr in parse_address_list(&original.to) {
                if addr.to_lowercase() != from.to_lowercase() {
                    cc_addrs.push(addr);
                }
            }
            // Add original CC (minus ourselves)
            for addr in parse_address_list(&original.cc) {
                if addr.to_lowercase() != from.to_lowercase() {
                    cc_addrs.push(addr);
                }
            }
        }
        // Add extra CC recipients (minus ourselves and duplicates)
        for addr in additional_cc {
            let addr_lower = addr.to_lowercase();
            if addr_lower != from.to_lowercase()
                && !cc_addrs.iter().any(|a| a.to_lowercase() == addr_lower)
                && !to_addrs.iter().any(|a| a.to_lowercase() == addr_lower)
            {
                cc_addrs.push(addr.clone());
            }
        }

        // Get Message-ID from original for In-Reply-To
        let message_id = self.get_message_id(folder, uid).await.ok();

        // Build quoted body — fallback to HTML-to-text if body_text is empty
        let original_text = if original.body_text.trim().is_empty() {
            original.body_html.as_deref()
                .map(super::html_to_text)
                .unwrap_or_default()
        } else {
            original.body_text.clone()
        };
        let quoted_original = original_text.lines()
            .map(|l| format!("> {l}"))
            .collect::<Vec<_>>()
            .join("\n");

        let wrote_label = match lang {
            "fr" => "a ecrit",
            "de" => "schrieb",
            "es" => "escribio",
            "it" => "ha scritto",
            "pt" => "escreveu",
            "nl" => "schreef",
            _ => "wrote",
        };

        let full_body = format!(
            "{body}\n\nOn {date}, {from_addr} {wrote}:\n{quoted}",
            date = original.date,
            from_addr = original.from,
            wrote = wrote_label,
            quoted = quoted_original,
        );

        let rfc822 = Self::build_message(
            &from,
            &to_addrs,
            &cc_addrs,
            &subject,
            &full_body,
            message_id.as_deref(),
            None,
        )?;

        let smtp_host = self.smtp_host.clone();
        let smtp_port = self.smtp_port;
        let imap_host = self.host.clone();
        let imap_port = self.port;
        let cache = self.cache.clone();
        let source_folder = folder.to_string();

        tokio::task::spawn_blocking(move || {
            Self::send_smtp_and_save(
                &smtp_host, smtp_port,
                &imap_host, imap_port,
                &credentials,
                rfc822.as_bytes(),
            )?;

            let sent_uid = {
                let mut session = Self::connect_sync(&imap_host, imap_port, ImapCredentials {
                    username: credentials.username,
                    password: credentials.password,
                })?;
                let uid = Self::get_last_uid(&mut session, "Sent Items").ok();
                session.logout()?;
                uid
            };

            cache.invalidate_folder("Sent Items");
            cache.invalidate_folder(&source_folder);
            Ok(serde_json::json!({
                "message": format!("Reply sent to {}", to_addrs.join(", ")),
                "uid": sent_uid,
                "folder": "Sent Items"
            }).to_string())
        })
        .await?
    }

    /// Forward an email. Reads the original, composes a forward, sends via SMTP.
    pub async fn forward_email(
        &self,
        folder: &str,
        uid: u32,
        to: &[String],
        cc: &[String],
        body: &str,
    ) -> Result<String> {
        let original = self.read_email(folder, uid).await?;

        let credentials = self.auth.get_credentials().await?;
        let from = credentials.username.clone();

        let subject = if original.subject.starts_with("Fwd:") || original.subject.starts_with("FW:") {
            original.subject.clone()
        } else {
            format!("Fwd: {}", original.subject)
        };

        // Fallback to HTML-to-text if body_text is empty
        let original_text = if original.body_text.trim().is_empty() {
            original.body_html.as_deref()
                .map(super::html_to_text)
                .unwrap_or_default()
        } else {
            original.body_text.clone()
        };

        let forwarded_body = format!(
            "{body}\n\n---------- Forwarded message ----------\n\
             From: {from_addr}\n\
             Date: {date}\n\
             Subject: {subj}\n\
             To: {to_addr}\n\n\
             {orig_body}",
            from_addr = original.from,
            date = original.date,
            subj = original.subject,
            to_addr = original.to,
            orig_body = original_text,
        );

        let rfc822 = Self::build_message(
            &from,
            to,
            cc,
            &subject,
            &forwarded_body,
            None,
            None,
        )?;

        let smtp_host = self.smtp_host.clone();
        let smtp_port = self.smtp_port;
        let imap_host = self.host.clone();
        let imap_port = self.port;
        let cache = self.cache.clone();
        let to_display: Vec<String> = to.to_vec();
        let source_folder = folder.to_string();

        tokio::task::spawn_blocking(move || {
            Self::send_smtp_and_save(
                &smtp_host, smtp_port,
                &imap_host, imap_port,
                &credentials,
                rfc822.as_bytes(),
            )?;

            let sent_uid = {
                let mut session = Self::connect_sync(&imap_host, imap_port, ImapCredentials {
                    username: credentials.username,
                    password: credentials.password,
                })?;
                let uid = Self::get_last_uid(&mut session, "Sent Items").ok();
                session.logout()?;
                uid
            };

            cache.invalidate_folder("Sent Items");
            cache.invalidate_folder(&source_folder);
            Ok(serde_json::json!({
                "message": format!("Email forwarded to {}", to_display.join(", ")),
                "uid": sent_uid,
                "folder": "Sent Items"
            }).to_string())
        })
        .await?
    }

    /// List contacts extracted from email headers (From, To, Cc) in specified folders.
    pub async fn list_contacts(
        &self,
        folders: &[String],
        scan_limit: u32,
        result_limit: u32,
    ) -> Result<Vec<ContactInfo>> {
        let credentials = self.auth.get_credentials().await?;
        let host = self.host.clone();
        let port = self.port;
        let own_email = credentials.username.clone().to_lowercase();
        let folders = folders.to_vec();

        tokio::task::spawn_blocking(move || {
            let mut session = Self::connect_sync(&host, port, ImapCredentials {
                username: credentials.username,
                password: credentials.password,
            })?;

            // Collect (email -> (name, count))
            let mut contacts: std::collections::HashMap<String, (Option<String>, u32)> =
                std::collections::HashMap::new();

            let folders_to_scan = if folders.len() == 1 && folders[0] == "ALL" {
                // List all folders
                let mailboxes = session.list(Some(""), Some("*"))?;
                mailboxes.iter().map(|mb| mb.name().to_string()).collect::<Vec<_>>()
            } else {
                folders
            };

            for folder in &folders_to_scan {
                let mailbox = match session.select(folder) {
                    Ok(mb) => mb,
                    Err(_) => continue, // skip inaccessible folders
                };

                let total = mailbox.exists;
                if total == 0 {
                    continue;
                }

                let start = if total > scan_limit { total - scan_limit + 1 } else { 1 };
                let range = format!("{start}:{total}");

                let messages = match session.fetch(&range, "(UID ENVELOPE)") {
                    Ok(m) => m,
                    Err(_) => continue,
                };

                for msg in messages.iter() {
                    if let Some(envelope) = msg.envelope() {
                        // Process all address fields
                        let address_lists = [
                            envelope.from.as_deref(),
                            envelope.to.as_deref(),
                            envelope.cc.as_deref(),
                        ];

                        for addrs_opt in &address_lists {
                            if let Some(addrs) = addrs_opt {
                                for addr in *addrs {
                                    let mailbox = addr.mailbox.as_ref()
                                        .map(|m| String::from_utf8_lossy(m).to_string())
                                        .unwrap_or_default();
                                    let host_part = addr.host.as_ref()
                                        .map(|h| String::from_utf8_lossy(h).to_string())
                                        .unwrap_or_default();

                                    if mailbox.is_empty() || host_part.is_empty() {
                                        continue;
                                    }

                                    let email = format!("{mailbox}@{host_part}").to_lowercase();

                                    // Skip own email
                                    if email == own_email {
                                        continue;
                                    }

                                    let name = addr.name.as_ref()
                                        .map(|n| {
                                            let s = String::from_utf8_lossy(n).to_string();
                                            if s.contains("=?") {
                                                // Decode RFC 2047 encoded names
                                                super::parse::decode_rfc2047_public(&s)
                                            } else {
                                                s
                                            }
                                        })
                                        .filter(|n| !n.is_empty());

                                    let entry = contacts.entry(email).or_insert((None, 0));
                                    // Keep the first non-empty name we find
                                    if entry.0.is_none() && name.is_some() {
                                        entry.0 = name;
                                    }
                                    entry.1 += 1;
                                }
                            }
                        }
                    }
                }
            }

            session.logout()?;

            // Sort by frequency descending
            let mut result: Vec<ContactInfo> = contacts
                .into_iter()
                .map(|(email, (name, frequency))| ContactInfo { email, name, frequency })
                .collect();
            result.sort_by(|a, b| b.frequency.cmp(&a.frequency));
            result.truncate(result_limit as usize);

            Ok(result)
        })
        .await?
    }

    // ---- Calendar operations ----

    /// Default calendar folder name for Exchange
    const DEFAULT_CALENDAR_FOLDER: &'static str = "Calendar";

    /// List calendar events from the Calendar folder.
    /// Optionally filter by date range using IMAP SEARCH (SINCE/BEFORE).
    pub async fn list_calendar_events(
        &self,
        folder: Option<&str>,
        start_date: Option<&str>,
        end_date: Option<&str>,
        limit: Option<u32>,
    ) -> Result<Vec<super::calendar::CalendarEvent>> {
        let credentials = self.auth.get_credentials().await?;
        let host = self.host.clone();
        let port = self.port;
        let folder = folder.unwrap_or(Self::DEFAULT_CALENDAR_FOLDER).to_string();
        let limit_val = limit.unwrap_or(50);
        let start_date = start_date.map(|s| s.to_string());
        let end_date = end_date.map(|s| s.to_string());

        tokio::task::spawn_blocking(move || {
            let mut session = Self::connect_sync(&host, port, credentials)?;
            session.select(&folder)?;

            // Build search query for date range
            let search_query = build_calendar_search_query(start_date.as_deref(), end_date.as_deref());

            let uids = session.uid_search(&search_query)?;

            if uids.is_empty() {
                session.logout()?;
                return Ok(vec![]);
            }

            let mut uid_vec: Vec<u32> = uids.into_iter().collect();
            uid_vec.sort_unstable();
            uid_vec.reverse();
            uid_vec.truncate(limit_val as usize);

            let uid_range: String = uid_vec
                .iter()
                .map(|u| u.to_string())
                .collect::<Vec<_>>()
                .join(",");

            let messages = session.uid_fetch(&uid_range, "(UID BODY.PEEK[])")?;

            let mut events: Vec<super::calendar::CalendarEvent> = Vec::new();
            for msg in messages.iter() {
                let uid = match msg.uid {
                    Some(u) => u,
                    None => continue,
                };
                if let Some(body) = msg.body() {
                    if let Some(ics) = super::calendar::extract_ics_from_mime(body) {
                        if let Some(event) = super::calendar::parse_calendar_event(uid, &ics) {
                            events.push(event);
                        }
                    }
                }
            }

            // Sort by start date
            events.sort_by(|a, b| a.start.cmp(&b.start));

            session.logout()?;
            Ok(events)
        })
        .await?
    }

    /// Read full details of a single calendar event by UID.
    pub async fn read_calendar_event(
        &self,
        folder: Option<&str>,
        uid: u32,
    ) -> Result<super::calendar::CalendarEventDetail> {
        let credentials = self.auth.get_credentials().await?;
        let host = self.host.clone();
        let port = self.port;
        let folder = folder.unwrap_or(Self::DEFAULT_CALENDAR_FOLDER).to_string();

        tokio::task::spawn_blocking(move || {
            let mut session = Self::connect_sync(&host, port, credentials)?;
            session.select(&folder)?;

            let messages = session.uid_fetch(uid.to_string(), "(UID BODY.PEEK[])")?;
            let msg = messages.iter().next().context("Calendar event not found")?;

            let body = msg.body().context("No message body")?;
            let ics = super::calendar::extract_ics_from_mime(body)
                .context("No calendar data found in message")?;
            let detail = super::calendar::parse_calendar_event_detail(uid, &ics)
                .context("Failed to parse calendar event")?;

            session.logout()?;
            Ok(detail)
        })
        .await?
    }

    /// Search calendar events using IMAP SEARCH with a text query.
    pub async fn search_calendar_events(
        &self,
        folder: Option<&str>,
        query: &str,
        limit: Option<u32>,
    ) -> Result<Vec<super::calendar::CalendarEvent>> {
        let credentials = self.auth.get_credentials().await?;
        let host = self.host.clone();
        let port = self.port;
        let folder = folder.unwrap_or(Self::DEFAULT_CALENDAR_FOLDER).to_string();
        let limit_val = limit.unwrap_or(20);
        let query = query.to_string();

        tokio::task::spawn_blocking(move || {
            let mut session = Self::connect_sync(&host, port, credentials)?;
            session.select(&folder)?;

            // Use TEXT search to match against the full message content (includes ICS data)
            let search_query = format!("TEXT \"{}\"", query.replace('"', "\\\""));
            let uids = session.uid_search(&search_query)?;

            if uids.is_empty() {
                session.logout()?;
                return Ok(vec![]);
            }

            let mut uid_vec: Vec<u32> = uids.into_iter().collect();
            uid_vec.sort_unstable();
            uid_vec.reverse();
            uid_vec.truncate(limit_val as usize);

            let uid_range: String = uid_vec
                .iter()
                .map(|u| u.to_string())
                .collect::<Vec<_>>()
                .join(",");

            let messages = session.uid_fetch(&uid_range, "(UID BODY.PEEK[])")?;

            let mut events: Vec<super::calendar::CalendarEvent> = Vec::new();
            for msg in messages.iter() {
                let uid = match msg.uid {
                    Some(u) => u,
                    None => continue,
                };
                if let Some(body) = msg.body() {
                    if let Some(ics) = super::calendar::extract_ics_from_mime(body) {
                        if let Some(event) = super::calendar::parse_calendar_event(uid, &ics) {
                            events.push(event);
                        }
                    }
                }
            }

            events.sort_by(|a, b| a.start.cmp(&b.start));

            session.logout()?;
            Ok(events)
        })
        .await?
    }

    /// Get the Message-ID header of an email by UID.
    async fn get_message_id(&self, folder: &str, uid: u32) -> Result<String> {
        let credentials = self.auth.get_credentials().await?;
        let host = self.host.clone();
        let port = self.port;
        let folder = folder.to_string();

        tokio::task::spawn_blocking(move || {
            let mut session = Self::connect_sync(&host, port, credentials)?;
            session.select(&folder)?;
            let messages = session.uid_fetch(uid.to_string(), "BODY.PEEK[HEADER.FIELDS (MESSAGE-ID)]")?;
            let msg = messages.iter().next().context("Email not found")?;
            let header_bytes = msg.body().context("No header data")?;
            let header_str = String::from_utf8_lossy(header_bytes);
            session.logout()?;

            // Parse "Message-ID: <xxx>\r\n"
            for line in header_str.lines() {
                let line = line.trim();
                if let Some(value) = line.strip_prefix("Message-ID:").or_else(|| line.strip_prefix("Message-Id:")).or_else(|| line.strip_prefix("message-id:")) {
                    return Ok(value.trim().to_string());
                }
            }
            anyhow::bail!("No Message-ID header found")
        })
        .await?
    }
}

/// Extract a bare email address from "Name <email@example.com>" or "email@example.com".
fn extract_email_address(addr: &str) -> String {
    if let Some(start) = addr.rfind('<') {
        addr[start + 1..].trim_end_matches('>').trim().to_string()
    } else {
        addr.trim().to_string()
    }
}

/// Parse a comma-separated address list into individual email addresses.
fn parse_address_list(addrs: &str) -> Vec<String> {
    if addrs.is_empty() {
        return Vec::new();
    }
    addrs
        .split(',')
        .map(|a| extract_email_address(a.trim()))
        .filter(|a| !a.is_empty())
        .collect()
}

/// Build an IMAP SEARCH query for calendar events with optional date range.
/// Dates should be in "dd-Mon-yyyy" format (e.g., "01-Jan-2024") or "yyyy-mm-dd" format.
fn build_calendar_search_query(start_date: Option<&str>, end_date: Option<&str>) -> String {
    let mut parts = Vec::new();
    parts.push("ALL".to_string());

    if let Some(start) = start_date {
        let formatted = normalize_imap_date(start);
        parts.push(format!("SINCE {formatted}"));
    }
    if let Some(end) = end_date {
        let formatted = normalize_imap_date(end);
        parts.push(format!("BEFORE {formatted}"));
    }

    parts.join(" ")
}

/// Normalize a date string to IMAP format "dd-Mon-yyyy".
/// Accepts "yyyy-mm-dd" or "dd-Mon-yyyy" formats.
fn normalize_imap_date(date: &str) -> String {
    // If already in IMAP format (contains month name), return as-is
    if date.len() == 11 && date.chars().nth(2) == Some('-') && date.chars().nth(6) == Some('-') {
        return date.to_string();
    }

    // Try to parse yyyy-mm-dd
    let parts: Vec<&str> = date.split('-').collect();
    if parts.len() == 3 && parts[0].len() == 4 {
        let month_names = [
            "Jan", "Feb", "Mar", "Apr", "May", "Jun",
            "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
        ];
        if let Ok(month_num) = parts[1].parse::<usize>() {
            if month_num >= 1 && month_num <= 12 {
                return format!("{}-{}-{}", parts[2], month_names[month_num - 1], parts[0]);
            }
        }
    }

    // Return as-is if we can't parse it
    date.to_string()
}
