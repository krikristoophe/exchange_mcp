use std::sync::Arc;

use rmcp::{
    ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{ServerCapabilities, ServerInfo},
    schemars, tool, tool_router,
};
use serde::Deserialize;

use crate::imap_client::ImapClient;

#[derive(Clone)]
pub struct ExchangeMcpServer {
    imap: Arc<ImapClient>,
    tool_router: ToolRouter<Self>,
}

impl ExchangeMcpServer {
    pub fn new(imap: Arc<ImapClient>) -> Self {
        Self {
            imap,
            tool_router: Self::tool_router(),
        }
    }

    pub fn imap_ref(&self) -> &Arc<ImapClient> {
        &self.imap
    }
}

// Tool parameter types
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ListFoldersParams {}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ListEmailsParams {
    /// Folder name (e.g., "INBOX", "Sent Items", "Drafts")
    pub folder: String,
    /// Maximum number of emails to return (default: 20)
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ReadEmailParams {
    /// Folder containing the email
    pub folder: String,
    /// UID of the email to read
    pub uid: u32,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SearchEmailsParams {
    /// Folder to search in
    pub folder: String,
    /// IMAP search query (e.g., "FROM \"john@example.com\"", "SUBJECT \"meeting\"", "UNSEEN", "SINCE 01-Jan-2024")
    pub query: String,
    /// Maximum number of results (default: 20)
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct MarkReadParams {
    /// Folder containing the email
    pub folder: String,
    /// UID of the email
    pub uid: u32,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct MoveEmailParams {
    /// Source folder
    pub folder: String,
    /// UID of the email to move
    pub uid: u32,
    /// Target folder to move the email to
    pub target_folder: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct DeleteEmailParams {
    /// Folder containing the email
    pub folder: String,
    /// UID of the email to delete
    pub uid: u32,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SetFlagParams {
    /// Folder containing the email
    pub folder: String,
    /// UID of the email
    pub uid: u32,
    /// IMAP flag (e.g., "\\Flagged", "\\Seen", "\\Answered")
    pub flag: String,
    /// true to add the flag, false to remove it
    pub add: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct FolderStatusParams {
    /// Folder name to get status for
    pub folder: String,
}

#[tool_router]
impl ExchangeMcpServer {
    #[tool(description = "List all mailbox folders (INBOX, Sent Items, Drafts, etc.)")]
    async fn list_folders(&self, Parameters(_params): Parameters<ListFoldersParams>) -> String {
        match self.imap.list_folders().await {
            Ok(folders) => serde_json::to_string_pretty(&folders).unwrap_or_else(|e| e.to_string()),
            Err(e) => format!("Error listing folders: {e}"),
        }
    }

    #[tool(description = "List recent emails in a folder. Returns subject, from, date, flags for each email.")]
    async fn list_emails(&self, Parameters(params): Parameters<ListEmailsParams>) -> String {
        match self.imap.list_emails(&params.folder, params.limit).await {
            Ok(emails) => serde_json::to_string_pretty(&emails).unwrap_or_else(|e| e.to_string()),
            Err(e) => format!("Error listing emails: {e}"),
        }
    }

    #[tool(description = "Read the full content of an email including body, headers, attachments info. HTML is auto-converted to readable text if no plain text version exists.")]
    async fn read_email(&self, Parameters(params): Parameters<ReadEmailParams>) -> String {
        match self.imap.read_email(&params.folder, params.uid).await {
            Ok(mut email) => {
                // If no plain text body but HTML exists, convert HTML to text
                if email.body_text.is_empty() || email.body_text == "(no body)" {
                    if let Some(ref html) = email.body_html {
                        email.body_text = crate::imap_client::html_to_text(html);
                    }
                }
                serde_json::to_string_pretty(&email).unwrap_or_else(|e| e.to_string())
            }
            Err(e) => format!("Error reading email: {e}"),
        }
    }

    #[tool(description = "Search emails in a folder using IMAP search criteria. Examples: UNSEEN, FROM \"user@example.com\", SUBJECT \"meeting\", SINCE 01-Jan-2024")]
    async fn search_emails(&self, Parameters(params): Parameters<SearchEmailsParams>) -> String {
        match self
            .imap
            .search_emails(&params.folder, &params.query, params.limit)
            .await
        {
            Ok(emails) => serde_json::to_string_pretty(&emails).unwrap_or_else(|e| e.to_string()),
            Err(e) => format!("Error searching emails: {e}"),
        }
    }

    #[tool(description = "Mark an email as read (add \\Seen flag)")]
    async fn mark_as_read(&self, Parameters(params): Parameters<MarkReadParams>) -> String {
        match self.imap.mark_as_read(&params.folder, params.uid).await {
            Ok(()) => "Email marked as read".to_string(),
            Err(e) => format!("Error: {e}"),
        }
    }

    #[tool(description = "Mark an email as unread (remove \\Seen flag)")]
    async fn mark_as_unread(&self, Parameters(params): Parameters<MarkReadParams>) -> String {
        match self.imap.mark_as_unread(&params.folder, params.uid).await {
            Ok(()) => "Email marked as unread".to_string(),
            Err(e) => format!("Error: {e}"),
        }
    }

    #[tool(description = "Move an email from one folder to another")]
    async fn move_email(&self, Parameters(params): Parameters<MoveEmailParams>) -> String {
        match self
            .imap
            .move_email(&params.folder, params.uid, &params.target_folder)
            .await
        {
            Ok(()) => format!("Email moved to '{}'", params.target_folder),
            Err(e) => format!("Error: {e}"),
        }
    }

    #[tool(description = "Delete an email (moves it to the Deleted Items folder)")]
    async fn delete_email(&self, Parameters(params): Parameters<DeleteEmailParams>) -> String {
        match self.imap.delete_email(&params.folder, params.uid).await {
            Ok(()) => "Email deleted (moved to Deleted Items)".to_string(),
            Err(e) => format!("Error: {e}"),
        }
    }

    #[tool(description = "Set or remove an IMAP flag on an email. Common flags: \\Flagged, \\Seen, \\Answered, \\Draft")]
    async fn set_flag(&self, Parameters(params): Parameters<SetFlagParams>) -> String {
        let action = if params.add { "added" } else { "removed" };
        match self
            .imap
            .set_flag(&params.folder, params.uid, &params.flag, params.add)
            .await
        {
            Ok(()) => format!("Flag '{}' {action}", params.flag),
            Err(e) => format!("Error: {e}"),
        }
    }

    #[tool(description = "Get folder status: total messages, unseen count, and recent count")]
    async fn folder_status(&self, Parameters(params): Parameters<FolderStatusParams>) -> String {
        match self.imap.get_folder_status(&params.folder).await {
            Ok(status) => serde_json::to_string_pretty(&status).unwrap_or_else(|e| e.to_string()),
            Err(e) => format!("Error: {e}"),
        }
    }
}

impl ServerHandler for ExchangeMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions(
                "Exchange MCP Server - Access Microsoft Exchange emails via IMAP. \
                 Use list_folders to discover available folders, list_emails to browse, \
                 read_email to read full content, and search_emails to find specific messages.",
            )
    }
}
