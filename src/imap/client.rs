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

        // Check cache (only for non-preview requests to avoid stale snippets)
        if !include_preview {
            if let Some(cached) = self.cache.get_summaries(folder, limit_val) {
                return Ok(cached);
            }
        }

        let credentials = self.auth.get_credentials().await?;
        let host = self.host.clone();
        let port = self.port;
        let folder = folder.to_string();
        let cache = self.cache.clone();

        tokio::task::spawn_blocking(move || {
            let mut session = Self::connect_sync(&host, port, credentials)?;
            let mailbox = session.select(&folder)?;

            let total = mailbox.exists;

            if total == 0 {
                session.logout()?;
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

        // Check cache
        if !include_preview {
            if let Some(cached) = self.cache.get_search(folder, query, limit_val) {
                return Ok(cached);
            }
        }

        let credentials = self.auth.get_credentials().await?;
        let host = self.host.clone();
        let port = self.port;
        let folder = folder.to_string();
        let query = query.to_string();
        let cache = self.cache.clone();

        tokio::task::spawn_blocking(move || {
            let mut session = Self::connect_sync(&host, port, credentials)?;
            session.select(&folder)?;

            let uids = session.uid_search(&query)?;

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
            cache.set_search(&folder, &query, limit_val, summaries.clone());
            Ok(summaries)
        })
        .await?
    }

    /// Get folder status (total, unseen, recent)
    pub async fn get_folder_status(&self, folder: &str) -> Result<FolderStatus> {
        // Check cache
        if let Some(cached) = self.cache.get_status(folder) {
            return Ok(cached);
        }

        let credentials = self.auth.get_credentials().await?;
        let host = self.host.clone();
        let port = self.port;
        let folder = folder.to_string();
        let cache = self.cache.clone();

        tokio::task::spawn_blocking(move || {
            let mut session = Self::connect_sync(&host, port, credentials)?;
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
            cache.set_status(&status.name, status.clone());
            Ok(status)
        })
        .await?
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
            session.logout()?;
            cache.invalidate_folder("Drafts");
            Ok("Draft saved to Drafts folder".to_string())
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
            session.logout()?;

            cache.invalidate_folder("Drafts");
            cache.invalidate_folder("Sent Items");

            Ok("Draft sent and removed from Drafts folder".to_string())
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
            cache.invalidate_folder("Sent Items");
            Ok("Email sent successfully".to_string())
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

        // Get Message-ID from original for In-Reply-To
        let message_id = self.get_message_id(folder, uid).await.ok();

        // Build quoted body
        let quoted_original = original.body_text.lines()
            .map(|l| format!("> {l}"))
            .collect::<Vec<_>>()
            .join("\n");

        let full_body = format!(
            "{body}\n\nOn {date}, {from_addr} wrote:\n{quoted}",
            date = original.date,
            from_addr = original.from,
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

        tokio::task::spawn_blocking(move || {
            Self::send_smtp_and_save(
                &smtp_host, smtp_port,
                &imap_host, imap_port,
                &credentials,
                rfc822.as_bytes(),
            )?;
            cache.invalidate_folder("Sent Items");
            Ok(format!(
                "Reply sent to {}",
                to_addrs.join(", ")
            ))
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
            orig_body = original.body_text,
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

        tokio::task::spawn_blocking(move || {
            Self::send_smtp_and_save(
                &smtp_host, smtp_port,
                &imap_host, imap_port,
                &credentials,
                rfc822.as_bytes(),
            )?;
            cache.invalidate_folder("Sent Items");
            Ok(format!(
                "Email forwarded to {}",
                to_display.join(", ")
            ))
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
