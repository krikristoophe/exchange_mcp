use anyhow::{Context, Result};
use std::sync::Arc;

use crate::auth::{AuthProvider, ImapCredentials};
use super::parse;

pub struct ImapClient {
    auth: Arc<dyn AuthProvider>,
    host: String,
    port: u16,
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
    pub fn new(auth: Arc<dyn AuthProvider>, host: String, port: u16) -> Self {
        Self { auth, host, port }
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
        let credentials = self.auth.get_credentials().await?;
        let host = self.host.clone();
        let port = self.port;

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
        let credentials = self.auth.get_credentials().await?;
        let host = self.host.clone();
        let port = self.port;
        let folder = folder.to_string();

        tokio::task::spawn_blocking(move || {
            let mut session = Self::connect_sync(&host, port, credentials)?;
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
                .filter_map(|msg| parse::parse_email_summary(msg))
                .collect();

            session.logout()?;
            Ok(summaries)
        })
        .await?
    }

    /// Read a specific email by UID
    pub async fn read_email(&self, folder: &str, uid: u32) -> Result<EmailDetail> {
        let credentials = self.auth.get_credentials().await?;
        let host = self.host.clone();
        let port = self.port;
        let folder = folder.to_string();

        tokio::task::spawn_blocking(move || {
            let mut session = Self::connect_sync(&host, port, credentials)?;
            session.select(&folder)?;

            let messages = session.uid_fetch(uid.to_string(), "(UID FLAGS ENVELOPE BODY[])")?;

            let msg = messages.iter().next().context("Email not found")?;
            let detail = parse::parse_email_detail(msg)?;

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
        let credentials = self.auth.get_credentials().await?;
        let host = self.host.clone();
        let port = self.port;
        let folder = folder.to_string();
        let query = query.to_string();

        tokio::task::spawn_blocking(move || {
            let mut session = Self::connect_sync(&host, port, credentials)?;
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
                .filter_map(|msg| parse::parse_email_summary(msg))
                .collect();

            session.logout()?;
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
        let credentials = self.auth.get_credentials().await?;
        let host = self.host.clone();
        let port = self.port;
        let folder = folder.to_string();
        let target_folder = target_folder.to_string();

        tokio::task::spawn_blocking(move || {
            let mut session = Self::connect_sync(&host, port, credentials)?;
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
