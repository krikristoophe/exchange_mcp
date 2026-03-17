use std::sync::Arc;

use rmcp::{
    ServerHandler,
    handler::server::{router::tool::ToolRouter, tool::ToolCallContext, wrapper::Parameters},
    model::{
        CallToolRequestParams, CallToolResult, ListToolsResult,
        ServerCapabilities, ServerInfo,
    },
    service::RequestContext, RoleServer,
    schemars, tool, tool_router,
};
use serde::Deserialize;

use crate::imap::ImapClient;

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

    #[allow(dead_code)]
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
    /// Include a short text preview/snippet for each email (default: false). Slightly slower but avoids needing to read each email individually.
    #[serde(default)]
    pub include_preview: Option<bool>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ReadEmailParams {
    /// Folder containing the email
    pub folder: String,
    /// UID of the email to read
    pub uid: u32,
    /// Response format: "text" (default, text only), "html" (HTML only), "both" (text + HTML)
    #[serde(default)]
    pub format: Option<String>,
    /// Strip quoted replies (previous messages in the thread). Default: true
    #[serde(default)]
    pub strip_quotes: Option<bool>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ReadEmailsParams {
    /// Folder containing the emails
    pub folder: String,
    /// List of UIDs to read
    pub uids: Vec<u32>,
    /// Response format: "text" (default, text only), "html" (HTML only), "both" (text + HTML)
    #[serde(default)]
    pub format: Option<String>,
    /// Strip quoted replies (previous messages in the thread). Default: true
    #[serde(default)]
    pub strip_quotes: Option<bool>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SearchEmailsParams {
    /// Folder to search in
    pub folder: String,
    /// IMAP search query (e.g., "FROM \"john@example.com\"", "SUBJECT \"meeting\"", "UNSEEN", "SINCE 01-Jan-2024")
    pub query: String,
    /// Maximum number of results (default: 20)
    pub limit: Option<u32>,
    /// Include a short text preview/snippet for each email (default: false)
    #[serde(default)]
    pub include_preview: Option<bool>,
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

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CreateDraftParams {
    /// Recipient email addresses
    pub to: Vec<String>,
    /// CC recipients (optional)
    #[serde(default)]
    pub cc: Vec<String>,
    /// Email subject
    pub subject: String,
    /// Email body (plain text)
    pub body: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SendEmailParams {
    /// Recipient email addresses
    pub to: Vec<String>,
    /// CC recipients (optional)
    #[serde(default)]
    pub cc: Vec<String>,
    /// Email subject
    pub subject: String,
    /// Email body (plain text)
    pub body: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ReplyParams {
    /// Folder containing the original email
    pub folder: String,
    /// UID of the email to reply to
    pub uid: u32,
    /// Reply body (plain text)
    pub body: String,
    /// Reply to all recipients (default: false)
    #[serde(default)]
    pub reply_all: Option<bool>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ForwardParams {
    /// Folder containing the email to forward
    pub folder: String,
    /// UID of the email to forward
    pub uid: u32,
    /// Recipient email addresses
    pub to: Vec<String>,
    /// CC recipients (optional)
    #[serde(default)]
    pub cc: Vec<String>,
    /// Additional message to include before the forwarded content (optional)
    #[serde(default)]
    pub body: String,
}

/// Post-process an email detail according to format and strip_quotes options.
fn process_email_detail(
    mut email: crate::imap::client::EmailDetail,
    format: Option<&str>,
    strip_quotes: bool,
) -> crate::imap::client::EmailDetail {
    let format = format.unwrap_or("text");

    // If no plain text body but HTML exists, convert HTML to text
    if email.body_text.is_empty() || email.body_text == "(no body)" {
        if let Some(ref html) = email.body_html {
            email.body_text = crate::imap::html_to_text(html);
        }
    }

    // Strip quoted replies if requested
    if strip_quotes {
        email.body_text = crate::imap::strip_quoted_replies(&email.body_text);
    }

    // Apply format filter
    match format {
        "html" => {
            email.body_text = String::new();
        }
        "both" => {
            // Keep both as-is
        }
        _ => {
            // "text" (default) — remove HTML to save tokens
            email.body_html = None;
        }
    }

    email
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

    #[tool(description = "List recent emails in a folder. Returns subject, from, date, flags, size for each email. Use include_preview=true to also get a short text snippet (first ~200 chars) without reading the full email.")]
    async fn list_emails(&self, Parameters(params): Parameters<ListEmailsParams>) -> String {
        let include_preview = params.include_preview.unwrap_or(false);
        match self.imap.list_emails(&params.folder, params.limit, include_preview).await {
            Ok(emails) => serde_json::to_string_pretty(&emails).unwrap_or_else(|e| e.to_string()),
            Err(e) => format!("Error listing emails: {e}"),
        }
    }

    #[tool(description = "Read the full content of a single email. By default returns text only (no HTML) with quoted replies stripped to minimize token usage. Use format=\"both\" to include HTML, and strip_quotes=false to keep full thread. Does NOT mark the email as read.")]
    async fn read_email(&self, Parameters(params): Parameters<ReadEmailParams>) -> String {
        match self.imap.read_email(&params.folder, params.uid).await {
            Ok(email) => {
                let strip = params.strip_quotes.unwrap_or(true);
                let email = process_email_detail(email, params.format.as_deref(), strip);
                serde_json::to_string_pretty(&email).unwrap_or_else(|e| e.to_string())
            }
            Err(e) => format!("Error reading email: {e}"),
        }
    }

    #[tool(description = "Read multiple emails at once by their UIDs (batch). More efficient than calling read_email multiple times. Same format and strip_quotes options apply. Does NOT mark emails as read.")]
    async fn read_emails(&self, Parameters(params): Parameters<ReadEmailsParams>) -> String {
        match self.imap.read_emails(&params.folder, &params.uids).await {
            Ok(emails) => {
                let strip = params.strip_quotes.unwrap_or(true);
                let processed: Vec<_> = emails
                    .into_iter()
                    .map(|e| process_email_detail(e, params.format.as_deref(), strip))
                    .collect();
                serde_json::to_string_pretty(&processed).unwrap_or_else(|e| e.to_string())
            }
            Err(e) => format!("Error reading emails: {e}"),
        }
    }

    #[tool(description = "Search emails in a folder using IMAP search criteria. Examples: UNSEEN, FROM \"user@example.com\", SUBJECT \"meeting\", SINCE 01-Jan-2024. Use include_preview=true for text snippets.")]
    async fn search_emails(&self, Parameters(params): Parameters<SearchEmailsParams>) -> String {
        let include_preview = params.include_preview.unwrap_or(false);
        match self
            .imap
            .search_emails(&params.folder, &params.query, params.limit, include_preview)
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

    #[tool(description = "Create a draft email and save it to the Drafts folder. The email is NOT sent.")]
    async fn create_draft(&self, Parameters(params): Parameters<CreateDraftParams>) -> String {
        match self.imap.create_draft(&params.to, &params.cc, &params.subject, &params.body).await {
            Ok(msg) => msg,
            Err(e) => format!("Error creating draft: {e}"),
        }
    }

    #[tool(description = "Send an email via SMTP. The sent message is saved to the Sent Items folder.")]
    async fn send_email(&self, Parameters(params): Parameters<SendEmailParams>) -> String {
        match self.imap.send_email(&params.to, &params.cc, &params.subject, &params.body).await {
            Ok(msg) => msg,
            Err(e) => format!("Error sending email: {e}"),
        }
    }

    #[tool(description = "Reply to an email. Reads the original message, quotes it, and sends the reply via SMTP. Use reply_all=true to reply to all recipients.")]
    async fn reply(&self, Parameters(params): Parameters<ReplyParams>) -> String {
        let reply_all = params.reply_all.unwrap_or(false);
        match self.imap.reply_email(&params.folder, params.uid, &params.body, reply_all).await {
            Ok(msg) => msg,
            Err(e) => format!("Error sending reply: {e}"),
        }
    }

    #[tool(description = "Forward an email to new recipients. Reads the original message, includes it in the body, and sends via SMTP.")]
    async fn forward(&self, Parameters(params): Parameters<ForwardParams>) -> String {
        match self.imap.forward_email(&params.folder, params.uid, &params.to, &params.cc, &params.body).await {
            Ok(msg) => msg,
            Err(e) => format!("Error forwarding email: {e}"),
        }
    }
}

impl ServerHandler for ExchangeMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions(
                "Exchange MCP Server - Access Microsoft Exchange emails via IMAP/SMTP. \
                 Use list_folders to discover available folders, list_emails to browse, \
                 read_email to read full content, read_emails to read multiple at once, \
                 and search_emails to find specific messages. \
                 Reading emails does NOT mark them as read. \
                 Use include_preview=true on list/search to get text snippets without reading full emails. \
                 Use create_draft to save a draft, send_email to send a new email, \
                 reply to respond to an email, and forward to forward an email.",
            )
    }

    fn list_tools(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl std::future::Future<Output = Result<ListToolsResult, rmcp::model::ErrorData>> + Send + '_ {
        std::future::ready(Ok(ListToolsResult {
            tools: self.tool_router.list_all(),
            ..Default::default()
        }))
    }

    fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> impl std::future::Future<Output = Result<CallToolResult, rmcp::model::ErrorData>> + Send + '_ {
        let tool_context = ToolCallContext::new(self, request, context);
        self.tool_router.call(tool_context)
    }
}
