use std::sync::Arc;

use rmcp::{
    ServerHandler,
    handler::server::{router::tool::ToolRouter, tool::ToolCallContext, wrapper::Parameters},
    model::{
        AnnotateAble, CallToolRequestParams, CallToolResult, Content,
        ErrorData, ExtensionCapabilities, ListResourcesResult, ListToolsResult,
        Meta, PaginatedRequestParams, RawResource, ReadResourceRequestParams,
        ReadResourceResult, ResourceContents, ServerCapabilities, ServerInfo,
    },
    service::RequestContext, RoleServer,
    schemars, tool, tool_router,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::ews::EwsClient;
use crate::imap::ImapClient;

// UI resource constants
const UI_MIME_TYPE: &str = "text/html;profile=mcp-app";
const EMAIL_PREVIEW_URI: &str = "ui://exchange/email-preview";
const INBOX_LIST_URI: &str = "ui://exchange/inbox-list";
const EMAIL_PREVIEW_HTML: &str = include_str!("ui_resources/email_preview.html");
const INBOX_LIST_HTML: &str = include_str!("ui_resources/inbox_list.html");

fn ui_meta(resource_uri: &str) -> Meta {
    let mut meta = Meta::new();
    meta.insert(
        "ui".to_string(),
        json!({ "resourceUri": resource_uri }),
    );
    meta
}

fn ui_resources_list() -> ListResourcesResult {
    ListResourcesResult {
        resources: vec![
            RawResource::new(EMAIL_PREVIEW_URI, "Email Preview")
                .with_mime_type(UI_MIME_TYPE)
                .no_annotation(),
            RawResource::new(INBOX_LIST_URI, "Inbox List")
                .with_mime_type(UI_MIME_TYPE)
                .no_annotation(),
        ],
        ..Default::default()
    }
}

fn read_ui_resource(uri: &str) -> Option<ResourceContents> {
    let html = match uri {
        EMAIL_PREVIEW_URI => EMAIL_PREVIEW_HTML,
        INBOX_LIST_URI => INBOX_LIST_HTML,
        _ => return None,
    };
    Some(
        ResourceContents::text(html, uri)
            .with_mime_type(UI_MIME_TYPE),
    )
}

fn result_with_structured(text: String, structured: Value) -> Result<CallToolResult, ErrorData> {
    let mut result = CallToolResult::success(vec![Content::text(text)]);
    result.structured_content = Some(structured);
    Ok(result)
}

#[derive(Clone)]
pub struct ExchangeMcpServer {
    imap: Arc<ImapClient>,
    ews: Arc<EwsClient>,
    tool_router: ToolRouter<Self>,
}

impl ExchangeMcpServer {
    pub fn new(imap: Arc<ImapClient>, ews: Arc<EwsClient>) -> Self {
        Self {
            imap,
            ews,
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
    /// IMAP search query (e.g., "FROM \"john@example.com\"", "SUBJECT \"meeting\"", "SINCE 01-Jan-2024")
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
pub struct CreateFolderParams {
    /// Name of the folder to create (e.g., "Projects", "INBOX/Subfolder")
    pub folder: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct RenameFolderParams {
    /// Current folder name
    pub folder: String,
    /// New folder name
    pub new_name: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct DeleteFolderParams {
    /// Name of the folder to delete
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
    /// Email body in HTML format (optional). When provided, the email is sent as multipart/alternative with both plain text and HTML parts. Use this for formatted emails and signatures.
    #[serde(default)]
    pub body_html: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct UpdateDraftParams {
    /// UID of the draft to update (from the Drafts folder)
    pub uid: u32,
    /// New recipient email addresses (if omitted, keeps original recipients)
    #[serde(default)]
    pub to: Option<Vec<String>>,
    /// New CC recipients (if omitted, keeps original CC)
    #[serde(default)]
    pub cc: Option<Vec<String>>,
    /// New subject (if omitted, keeps original subject)
    #[serde(default)]
    pub subject: Option<String>,
    /// New body in plain text (if omitted, keeps original body)
    #[serde(default)]
    pub body: Option<String>,
    /// New body in HTML format (if omitted, keeps original HTML body). When provided, the email uses multipart/alternative with both plain text and HTML parts.
    #[serde(default)]
    pub body_html: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SendDraftParams {
    /// UID of the draft to send (from the Drafts folder)
    pub uid: u32,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct DeleteDraftParams {
    /// UID of the draft to delete (from the Drafts folder)
    pub uid: u32,
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
    /// Email body in HTML format (optional). When provided, the email is sent as multipart/alternative with both plain text and HTML parts. Use this for formatted emails and signatures.
    #[serde(default)]
    pub body_html: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ReplyParams {
    /// Folder containing the original email
    pub folder: String,
    /// UID of the email to reply to
    pub uid: u32,
    /// Reply body (plain text)
    pub body: String,
    /// Reply body in HTML format (optional). When provided, the reply is sent as multipart/alternative with both plain text and HTML parts. Use this for formatted replies and signatures.
    #[serde(default)]
    pub body_html: Option<String>,
    /// Reply to all recipients (default: false)
    #[serde(default)]
    pub reply_all: Option<bool>,
    /// Additional CC recipients to add to the reply
    #[serde(default)]
    pub additional_cc: Vec<String>,
    /// Language for the reply header (e.g. "fr" for "a ecrit", "en" for "wrote"). Default: "en"
    #[serde(default)]
    pub lang: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ListContactsParams {
    /// Maximum number of contacts to return (default: 50)
    #[serde(default)]
    pub limit: Option<u32>,
    /// Folders to scan for contacts. Default: ["INBOX", "Sent Items"]. Use ["ALL"] to scan all folders.
    #[serde(default)]
    pub folders: Option<Vec<String>>,
    /// Number of recent emails to scan per folder (default: 100)
    #[serde(default)]
    pub scan_limit: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ListCalendarEventsParams {
    /// Calendar folder name (default: "Calendar")
    #[serde(default)]
    pub folder: Option<String>,
    /// Start date filter (inclusive). Accepts "yyyy-mm-dd" or "dd-Mon-yyyy" format (e.g., "2024-01-15" or "15-Jan-2024")
    #[serde(default)]
    pub start_date: Option<String>,
    /// End date filter (exclusive). Accepts "yyyy-mm-dd" or "dd-Mon-yyyy" format
    #[serde(default)]
    pub end_date: Option<String>,
    /// Maximum number of events to return (default: 50)
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ReadCalendarEventParams {
    /// Calendar folder name (default: "Calendar")
    #[serde(default)]
    pub folder: Option<String>,
    /// Event ID to read. Use the ical_uid/item_id returned by list_calendar_events or search_calendar_events.
    pub event_id: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SearchCalendarEventsParams {
    /// Text to search for in calendar events (matches subject, description, location, attendees)
    pub query: String,
    /// Calendar folder name (default: "Calendar")
    #[serde(default)]
    pub folder: Option<String>,
    /// Maximum number of results (default: 20)
    #[serde(default)]
    pub limit: Option<u32>,
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
    /// Additional message in HTML format to include before the forwarded content (optional). When provided, the forward is sent as multipart/alternative with both plain text and HTML parts.
    #[serde(default)]
    pub body_html: Option<String>,
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
            // "text" (default) -- remove HTML to save tokens
            email.body_html = None;
        }
    }

    email
}

#[tool_router]
impl ExchangeMcpServer {
    #[tool(
        description = "List all mailbox folders (INBOX, Sent Items, Drafts, etc.)",
        annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn list_folders(&self, Parameters(_params): Parameters<ListFoldersParams>) -> String {
        match self.imap.list_folders().await {
            Ok(folders) => serde_json::to_string_pretty(&folders).unwrap_or_else(|e| e.to_string()),
            Err(e) => format!("Error listing folders: {e}"),
        }
    }

    #[tool(
        description = "List recent emails in a folder. Returns subject, from, date, flags, size for each email. Use include_preview=true to also get a short text snippet (first ~200 chars) without reading the full email.",
        annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false),
        meta = ui_meta(INBOX_LIST_URI)
    )]
    async fn list_emails(&self, Parameters(params): Parameters<ListEmailsParams>) -> Result<CallToolResult, ErrorData> {
        let include_preview = params.include_preview.unwrap_or(false);
        match self.imap.list_emails(&params.folder, params.limit, include_preview).await {
            Ok(emails) => {
                let text = serde_json::to_string_pretty(&emails).unwrap_or_else(|e| e.to_string());
                let structured = json!({
                    "folder": params.folder,
                    "emails": serde_json::to_value(&emails).unwrap_or(Value::Null),
                });
                result_with_structured(text, structured)
            }
            Err(e) => Ok(CallToolResult::error(vec![Content::text(format!("Error listing emails: {e}"))])),
        }
    }

    #[tool(
        description = "Read the full content of a single email. By default returns text only (no HTML) with quoted replies stripped to minimize token usage. Use format=\"both\" to include HTML, and strip_quotes=false to keep full thread. Does NOT mark the email as read.",
        annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false),
        meta = ui_meta(EMAIL_PREVIEW_URI)
    )]
    async fn read_email(&self, Parameters(params): Parameters<ReadEmailParams>) -> Result<CallToolResult, ErrorData> {
        match self.imap.read_email(&params.folder, params.uid).await {
            Ok(email) => {
                let strip = params.strip_quotes.unwrap_or(true);
                let email = process_email_detail(email, params.format.as_deref(), strip);
                let text = serde_json::to_string_pretty(&email).unwrap_or_else(|e| e.to_string());
                let structured = json!({
                    "uid": email.uid,
                    "subject": email.subject,
                    "from": email.from,
                    "to": email.to,
                    "cc": email.cc,
                    "date": email.date,
                    "body_text": email.body_text,
                    "attachments": serde_json::to_value(&email.attachments).unwrap_or(Value::Null),
                    "status": "read",
                });
                result_with_structured(text, structured)
            }
            Err(e) => Ok(CallToolResult::error(vec![Content::text(format!("Error reading email: {e}"))])),
        }
    }

    #[tool(
        description = "Read multiple emails at once by their UIDs (batch). More efficient than calling read_email multiple times. Same format and strip_quotes options apply. Does NOT mark emails as read.",
        annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
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

    #[tool(
        description = "Search emails in a folder using IMAP search criteria. Examples: UNSEEN, FROM \"user@example.com\", SUBJECT \"meeting\", SINCE 01-Jan-2024. Use include_preview=true for text snippets.",
        annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false),
        meta = ui_meta(INBOX_LIST_URI)
    )]
    async fn search_emails(&self, Parameters(params): Parameters<SearchEmailsParams>) -> Result<CallToolResult, ErrorData> {
        let include_preview = params.include_preview.unwrap_or(false);
        match self
            .imap
            .search_emails(&params.folder, &params.query, params.limit, include_preview)
            .await
        {
            Ok(emails) => {
                let text = serde_json::to_string_pretty(&emails).unwrap_or_else(|e| e.to_string());
                let structured = json!({
                    "folder": params.folder,
                    "query": params.query,
                    "emails": serde_json::to_value(&emails).unwrap_or(Value::Null),
                });
                result_with_structured(text, structured)
            }
            Err(e) => Ok(CallToolResult::error(vec![Content::text(format!("Error searching emails: {e}"))])),
        }
    }

    #[tool(
        description = "Mark an email as read (add \\Seen flag)",
        annotations(read_only_hint = false, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn mark_as_read(&self, Parameters(params): Parameters<MarkReadParams>) -> String {
        match self.imap.mark_as_read(&params.folder, params.uid).await {
            Ok(()) => "Email marked as read".to_string(),
            Err(e) => format!("Error: {e}"),
        }
    }

    #[tool(
        description = "Mark an email as unread (remove \\Seen flag)",
        annotations(read_only_hint = false, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn mark_as_unread(&self, Parameters(params): Parameters<MarkReadParams>) -> String {
        match self.imap.mark_as_unread(&params.folder, params.uid).await {
            Ok(()) => "Email marked as unread".to_string(),
            Err(e) => format!("Error: {e}"),
        }
    }

    #[tool(
        description = "Move an email from one folder to another",
        annotations(read_only_hint = false, destructive_hint = true, idempotent_hint = true, open_world_hint = false)
    )]
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

    #[tool(
        description = "Delete an email (moves it to the Deleted Items folder)",
        annotations(read_only_hint = false, destructive_hint = true, idempotent_hint = true, open_world_hint = false)
    )]
    async fn delete_email(&self, Parameters(params): Parameters<DeleteEmailParams>) -> String {
        match self.imap.delete_email(&params.folder, params.uid).await {
            Ok(()) => "Email deleted (moved to Deleted Items)".to_string(),
            Err(e) => format!("Error: {e}"),
        }
    }

    #[tool(
        description = "Set or remove an IMAP flag on an email. Common flags: \\Flagged, \\Seen, \\Answered, \\Draft",
        annotations(read_only_hint = false, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
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

    #[tool(
        description = "Get folder status: total messages, unseen count, and recent count",
        annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn folder_status(&self, Parameters(params): Parameters<FolderStatusParams>) -> String {
        match self.imap.get_folder_status(&params.folder).await {
            Ok(status) => serde_json::to_string_pretty(&status).unwrap_or_else(|e| e.to_string()),
            Err(e) => format!("Error: {e}"),
        }
    }

    #[tool(
        description = "Create a new mailbox folder. Use a path separator (usually '/') to create subfolders (e.g., 'INBOX/Projects').",
        annotations(read_only_hint = false, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn create_folder(&self, Parameters(params): Parameters<CreateFolderParams>) -> String {
        match self.imap.create_folder(&params.folder).await {
            Ok(()) => format!("Folder '{}' created", params.folder),
            Err(e) => format!("Error creating folder: {e}"),
        }
    }

    #[tool(
        description = "Rename a mailbox folder. Can also be used to move a folder by changing its path.",
        annotations(read_only_hint = false, destructive_hint = true, idempotent_hint = false, open_world_hint = false)
    )]
    async fn rename_folder(&self, Parameters(params): Parameters<RenameFolderParams>) -> String {
        match self.imap.rename_folder(&params.folder, &params.new_name).await {
            Ok(()) => format!("Folder '{}' renamed to '{}'", params.folder, params.new_name),
            Err(e) => format!("Error renaming folder: {e}"),
        }
    }

    #[tool(
        description = "Delete a mailbox folder. The folder must be empty or the server must support recursive deletion. Use with caution.",
        annotations(read_only_hint = false, destructive_hint = true, idempotent_hint = true, open_world_hint = false)
    )]
    async fn delete_folder(&self, Parameters(params): Parameters<DeleteFolderParams>) -> String {
        match self.imap.delete_folder(&params.folder).await {
            Ok(()) => format!("Folder '{}' deleted", params.folder),
            Err(e) => format!("Error deleting folder: {e}"),
        }
    }

    #[tool(
        description = "Create a draft email and save it to the Drafts folder. The email is NOT sent. Use send_draft to send it later, or delete_draft to discard it. Supports optional body_html for formatted emails with signatures.",
        annotations(read_only_hint = false, destructive_hint = false, idempotent_hint = false, open_world_hint = false),
        meta = ui_meta(EMAIL_PREVIEW_URI)
    )]
    async fn create_draft(&self, Parameters(params): Parameters<CreateDraftParams>) -> Result<CallToolResult, ErrorData> {
        match self.imap.create_draft(&params.to, &params.cc, &params.subject, &params.body, params.body_html.as_deref()).await {
            Ok(msg) => {
                let structured = json!({
                    "to": params.to.join(", "),
                    "cc": params.cc.join(", "),
                    "subject": params.subject,
                    "body_text": params.body,
                    "body_html": params.body_html,
                    "status": "draft",
                });
                result_with_structured(msg, structured)
            }
            Err(e) => Ok(CallToolResult::error(vec![Content::text(format!("Error creating draft: {e}"))])),
        }
    }

    #[tool(
        description = "Update an existing draft email in the Drafts folder. Fetches the current draft, replaces only the provided fields (to, cc, subject, body), and saves the updated version. The old draft is deleted. Returns the new UID. Use list_emails with folder=\"Drafts\" to find the draft UID.",
        annotations(read_only_hint = false, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn update_draft(&self, Parameters(params): Parameters<UpdateDraftParams>) -> String {
        match self.imap.update_draft(params.uid, params.to, params.cc, params.subject, params.body, params.body_html).await {
            Ok(msg) => msg,
            Err(e) => format!("Error updating draft: {e}"),
        }
    }

    #[tool(
        description = "Send a draft email from the Drafts folder. Fetches the draft by UID, sends it via SMTP, saves to Sent Items, and removes it from Drafts. Use list_emails with folder=\"Drafts\" to find the draft UID.",
        annotations(read_only_hint = false, destructive_hint = false, idempotent_hint = false, open_world_hint = true),
        meta = ui_meta(EMAIL_PREVIEW_URI)
    )]
    async fn send_draft(&self, Parameters(params): Parameters<SendDraftParams>) -> Result<CallToolResult, ErrorData> {
        match self.imap.send_draft(params.uid).await {
            Ok(msg) => {
                let structured = json!({
                    "status": "sent",
                });
                result_with_structured(msg, structured)
            }
            Err(e) => Ok(CallToolResult::error(vec![Content::text(format!("Error sending draft: {e}"))])),
        }
    }

    #[tool(
        description = "Delete a draft email from the Drafts folder (moves it to Deleted Items).",
        annotations(read_only_hint = false, destructive_hint = true, idempotent_hint = true, open_world_hint = false)
    )]
    async fn delete_draft(&self, Parameters(params): Parameters<DeleteDraftParams>) -> String {
        match self.imap.delete_draft(params.uid).await {
            Ok(()) => "Draft deleted (moved to Deleted Items)".to_string(),
            Err(e) => format!("Error deleting draft: {e}"),
        }
    }

    #[tool(
        description = "Send an email via SMTP. The sent message is saved to the Sent Items folder. Supports optional body_html for formatted emails with signatures.",
        annotations(read_only_hint = false, destructive_hint = false, idempotent_hint = false, open_world_hint = true),
        meta = ui_meta(EMAIL_PREVIEW_URI)
    )]
    async fn send_email(&self, Parameters(params): Parameters<SendEmailParams>) -> Result<CallToolResult, ErrorData> {
        match self.imap.send_email(&params.to, &params.cc, &params.subject, &params.body, params.body_html.as_deref()).await {
            Ok(msg) => {
                let structured = json!({
                    "to": params.to.join(", "),
                    "cc": params.cc.join(", "),
                    "subject": params.subject,
                    "body_text": params.body,
                    "body_html": params.body_html,
                    "status": "sent",
                });
                result_with_structured(msg, structured)
            }
            Err(e) => Ok(CallToolResult::error(vec![Content::text(format!("Error sending email: {e}"))])),
        }
    }

    #[tool(
        description = "Reply to an email. Reads the original message, quotes it, and sends the reply via SMTP. Use reply_all=true to reply to all recipients. Use additional_cc to add extra CC recipients. Use lang to set the language of the reply header (e.g. 'fr' for French, 'de' for German, default: 'en').",
        annotations(read_only_hint = false, destructive_hint = false, idempotent_hint = false, open_world_hint = true),
        meta = ui_meta(EMAIL_PREVIEW_URI)
    )]
    async fn reply_email(&self, Parameters(params): Parameters<ReplyParams>) -> Result<CallToolResult, ErrorData> {
        let reply_all = params.reply_all.unwrap_or(false);
        let lang = params.lang.as_deref().unwrap_or("en");
        match self.imap.reply_email(&params.folder, params.uid, &params.body, params.body_html.as_deref(), reply_all, &params.additional_cc, lang).await {
            Ok(msg) => {
                let structured = json!({
                    "reply_body": params.body,
                    "status": "sent",
                });
                result_with_structured(msg, structured)
            }
            Err(e) => Ok(CallToolResult::error(vec![Content::text(format!("Error sending reply: {e}"))])),
        }
    }

    #[tool(
        description = "List contacts extracted from recent emails. Scans From, To, and Cc headers in the specified folders (default: INBOX + Sent Items) to build a contact list with name and email. Contacts are deduplicated by email and sorted by frequency (most contacted first).",
        annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn list_contacts(&self, Parameters(params): Parameters<ListContactsParams>) -> String {
        let limit = params.limit.unwrap_or(50);
        let scan_limit = params.scan_limit.unwrap_or(100);
        let folders = params.folders.unwrap_or_else(|| vec!["INBOX".to_string(), "Sent Items".to_string()]);
        match self.imap.list_contacts(&folders, scan_limit, limit).await {
            Ok(contacts) => serde_json::to_string_pretty(&contacts).unwrap_or_else(|e| e.to_string()),
            Err(e) => format!("Error listing contacts: {e}"),
        }
    }

    #[tool(
        description = "Forward an email to new recipients. Reads the original message, includes it in the body, and sends via SMTP.",
        annotations(read_only_hint = false, destructive_hint = false, idempotent_hint = false, open_world_hint = true),
        meta = ui_meta(EMAIL_PREVIEW_URI)
    )]
    async fn forward_email(&self, Parameters(params): Parameters<ForwardParams>) -> Result<CallToolResult, ErrorData> {
        match self.imap.forward_email(&params.folder, params.uid, &params.to, &params.cc, &params.body, params.body_html.as_deref()).await {
            Ok(msg) => {
                let structured = json!({
                    "to": params.to.join(", "),
                    "cc": params.cc.join(", "),
                    "forward_body": params.body,
                    "status": "sent",
                });
                result_with_structured(msg, structured)
            }
            Err(e) => Ok(CallToolResult::error(vec![Content::text(format!("Error forwarding email: {e}"))])),
        }
    }

    #[tool(
        description = "List calendar events from the Exchange Calendar. Uses EWS (Exchange Web Services) for reliable access. Optionally filter by date range (start_date/end_date in yyyy-mm-dd format). Returns subject, start/end times, location, organizer, and recurrence info. Use the ical_uid from results to read full event details.",
        annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn list_calendar_events(&self, Parameters(params): Parameters<ListCalendarEventsParams>) -> String {
        // Try EWS first (proper calendar access), fall back to IMAP
        match self
            .ews
            .list_calendar_events(
                params.folder.as_deref(),
                params.start_date.as_deref(),
                params.end_date.as_deref(),
                params.limit,
            )
            .await
        {
            Ok(events) => serde_json::to_string_pretty(&events).unwrap_or_else(|e| e.to_string()),
            Err(ews_err) => {
                tracing::warn!("EWS list_calendar_events failed, trying IMAP fallback: {ews_err}");
                match self
                    .imap
                    .list_calendar_events(
                        params.folder.as_deref(),
                        params.start_date.as_deref(),
                        params.end_date.as_deref(),
                        params.limit,
                    )
                    .await
                {
                    Ok(events) => serde_json::to_string_pretty(&events).unwrap_or_else(|e| e.to_string()),
                    Err(e) => format!("Error listing calendar events: {e} (EWS also failed: {ews_err})"),
                }
            }
        }
    }

    #[tool(
        description = "Read the full details of a single calendar event by its event_id (the ical_uid returned by list_calendar_events). Returns subject, start/end times, location, organizer, attendees, description, categories, and more.",
        annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn read_calendar_event(&self, Parameters(params): Parameters<ReadCalendarEventParams>) -> String {
        // Try EWS first
        match self.ews.read_calendar_event(&params.event_id).await {
            Ok(detail) => serde_json::to_string_pretty(&detail).unwrap_or_else(|e| e.to_string()),
            Err(ews_err) => {
                tracing::warn!("EWS read_calendar_event failed, trying IMAP fallback: {ews_err}");
                // IMAP fallback: try to parse event_id as numeric UID
                match params.event_id.parse::<u32>() {
                    Ok(uid) => {
                        match self.imap.read_calendar_event(params.folder.as_deref(), uid).await {
                            Ok(detail) => serde_json::to_string_pretty(&detail).unwrap_or_else(|e| e.to_string()),
                            Err(e) => format!("Error reading calendar event: {e}"),
                        }
                    }
                    Err(_) => format!("Error reading calendar event via EWS: {ews_err}"),
                }
            }
        }
    }

    #[tool(
        description = "Search calendar events by text query. Searches across event subject, description, location, and attendees. Returns matching events sorted by start date.",
        annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn search_calendar_events(&self, Parameters(params): Parameters<SearchCalendarEventsParams>) -> String {
        // Try EWS first, fall back to IMAP
        match self
            .ews
            .search_calendar_events(params.folder.as_deref(), &params.query, params.limit)
            .await
        {
            Ok(events) => serde_json::to_string_pretty(&events).unwrap_or_else(|e| e.to_string()),
            Err(ews_err) => {
                tracing::warn!("EWS search_calendar_events failed, trying IMAP fallback: {ews_err}");
                match self
                    .imap
                    .search_calendar_events(params.folder.as_deref(), &params.query, params.limit)
                    .await
                {
                    Ok(events) => serde_json::to_string_pretty(&events).unwrap_or_else(|e| e.to_string()),
                    Err(e) => format!("Error searching calendar events: {e} (EWS also failed: {ews_err})"),
                }
            }
        }
    }
}

impl ServerHandler for ExchangeMcpServer {
    fn get_info(&self) -> ServerInfo {
        let mut extensions = ExtensionCapabilities::new();
        extensions.insert(
            "io.modelcontextprotocol/ui".to_string(),
            serde_json::from_value(json!({})).unwrap(),
        );

        let capabilities = ServerCapabilities::builder()
            .enable_tools()
            .enable_resources()
            .enable_extensions_with(extensions)
            .build();

        ServerInfo::new(capabilities)
            .with_instructions(
                "Exchange MCP Server - Access Microsoft Exchange emails and calendar via IMAP/SMTP. \
                 Use list_folders to discover available folders, list_emails to browse, \
                 read_email to read full content, read_emails to read multiple at once, \
                 and search_emails to find specific messages. \
                 Reading emails does NOT mark them as read. \
                 Use include_preview=true on list/search to get text snippets without reading full emails. \
                 Use create_draft to save a draft, update_draft to modify it, \
                 send_draft to send it later, delete_draft to discard it. \
                 Use send_email to send a new email, reply_email to respond to an email, \
                 forward_email to forward an email, and list_contacts to discover contacts \
                 from recent emails. \
                 Use create_folder, rename_folder, delete_folder to manage mailbox folders. \
                 Use list_calendar_events to browse calendar events (with optional date range filter), \
                 read_calendar_event to get full event details (attendees, description, recurrence), \
                 and search_calendar_events to find events by text query.",
            )
    }

    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
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

    fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl std::future::Future<Output = Result<ListResourcesResult, rmcp::model::ErrorData>> + Send + '_ {
        std::future::ready(Ok(ui_resources_list()))
    }

    fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl std::future::Future<Output = Result<ReadResourceResult, rmcp::model::ErrorData>> + Send + '_ {
        let result = match read_ui_resource(&request.uri) {
            Some(contents) => Ok(ReadResourceResult::new(vec![contents])),
            None => Err(ErrorData::resource_not_found(
                format!("Resource not found: {}", request.uri),
                None,
            )),
        };
        std::future::ready(result)
    }
}
