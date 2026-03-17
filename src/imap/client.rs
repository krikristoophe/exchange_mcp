use anyhow::{Context, Result};
use std::sync::Arc;

use crate::auth::{AuthProvider, ImapCredentials};
use crate::cache::EmailCache;
use super::parse;

pub struct ImapClient {
    auth: Arc<dyn AuthProvider>,
    host: String,
    port: u16,
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
    pub fn new(auth: Arc<dyn AuthProvider>, host: String, port: u16) -> Self {
        Self {
            auth,
            host,
            port,
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
}
