# Attachment Download URL — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Expose downloaded email attachments via temporary HTTP URLs so users can retrieve files from a remote MCP server.

**Architecture:** New `AttachmentStore` (in-memory `RwLock<HashMap>`) holds temporary download tokens with 24h TTL. The existing `download_attachment` MCP tool registers tokens and returns download URLs. A new axum endpoint `GET /attachments/:token/:filename` serves files. The existing cleanup task purges expired tokens and deletes files from disk.

**Tech Stack:** Rust, axum 0.8, tokio, rand, base64, percent-encoding (via `url` crate)

**Spec:** `docs/superpowers/specs/2026-03-27-attachment-download-url-design.md`

---

## File Structure

| File | Action | Responsibility |
|------|--------|---------------|
| `src/attachment_store.rs` | Create | `AttachmentStore` + `AttachmentMeta` + MIME allowlist |
| `src/main.rs` | Modify | `mod` declaration, pass store to factory, add endpoint, extend cleanup |
| `src/server.rs` | Modify | Add `attachment_store` + `issuer` fields, return `download_url` |

---

### Task 1: Create `AttachmentStore` module

**Files:**
- Create: `src/attachment_store.rs`

- [ ] **Step 1: Create `src/attachment_store.rs` with structs and constants**

```rust
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::RwLock;
use std::time::{Duration, Instant};

use rand::RngCore;

/// Download tokens expire after 24 hours.
const ATTACHMENT_TOKEN_TTL: Duration = Duration::from_secs(24 * 3600);

/// Grace period before deleting files (avoids race with in-flight downloads).
const CLEANUP_GRACE: Duration = Duration::from_secs(5 * 60);

/// MIME types safe to serve as-is. Everything else becomes application/octet-stream.
const SAFE_MIME_PREFIXES: &[&str] = &[
    "image/",
    "audio/",
    "video/",
    "text/plain",
    "application/pdf",
    "application/zip",
    "application/vnd.openxmlformats-",
    "application/vnd.ms-",
];

#[derive(Debug, Clone)]
pub struct AttachmentMeta {
    pub path: PathBuf,
    pub filename: String,
    pub content_type: String,
    pub size: u64,
    pub expires_at: Instant,
}

pub struct AttachmentStore {
    entries: RwLock<HashMap<String, AttachmentMeta>>,
}
```

- [ ] **Step 2: Implement `AttachmentStore` methods**

```rust
impl AttachmentStore {
    pub fn new() -> Self {
        Self {
            entries: RwLock::new(HashMap::new()),
        }
    }

    /// Sanitize a MIME type: only serve safe types as-is, everything else
    /// becomes application/octet-stream to prevent XSS.
    pub fn sanitize_content_type(raw: &str) -> String {
        let lower = raw.to_lowercase();
        for prefix in SAFE_MIME_PREFIXES {
            if lower.starts_with(prefix) {
                return raw.to_string();
            }
        }
        "application/octet-stream".to_string()
    }

    /// Insert attachment metadata and return a unique download token.
    pub fn insert(&self, meta: AttachmentMeta) -> String {
        let token = generate_token();
        self.entries.write().expect("attachment store lock poisoned").insert(token.clone(), meta);
        token
    }

    /// Look up a token. Returns None if missing or expired.
    pub fn get(&self, token: &str) -> Option<AttachmentMeta> {
        let entries = self.entries.read().expect("attachment store lock poisoned");
        let meta = entries.get(token)?;
        if Instant::now() > meta.expires_at {
            return None;
        }
        Some(meta.clone())
    }

    /// Remove expired entries (with grace period). Returns paths of files to delete.
    pub fn cleanup_expired(&self) -> Vec<PathBuf> {
        let now = Instant::now();
        let mut entries = self.entries.write().expect("attachment store lock poisoned");
        let mut paths = Vec::new();
        entries.retain(|_token, meta| {
            if now > meta.expires_at + CLEANUP_GRACE {
                paths.push(meta.path.clone());
                false
            } else {
                true
            }
        });
        paths
    }
}

/// Generate a URL-safe base64 token from 32 random bytes (256 bits).
fn generate_token() -> String {
    use base64::Engine;
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}
```

- [ ] **Step 3: Register the module in `main.rs`**

Add `mod attachment_store;` after the existing module declarations in `src/main.rs` (after line 10, alongside the other `mod` statements):

```rust
mod attachment_store;
```

- [ ] **Step 4: Verify it compiles**

Run: `cargo check 2>&1`
Expected: No errors (warnings OK at this stage)

- [ ] **Step 5: Commit**

```bash
git add src/attachment_store.rs src/main.rs
git commit -m "feat: add AttachmentStore module with token management and MIME sanitization"
```

---

### Task 2: Add download endpoint to axum router

**Files:**
- Modify: `src/main.rs` (router setup at ~line 188, add handler function)

- [ ] **Step 1: Add the download handler function in `main.rs`**

Add at the bottom of the file (before the closing of the module or after `start_http_server`). This is a standalone async function used as an axum handler:

```rust
/// Serve a previously downloaded attachment via its temporary token.
async fn serve_attachment(
    axum::extract::Path((token, filename)): axum::extract::Path<(String, String)>,
    axum::extract::State(store): axum::extract::State<Arc<attachment_store::AttachmentStore>>,
) -> impl axum::response::IntoResponse {
    use axum::http::{StatusCode, header};

    let meta = match store.get(&token) {
        Some(m) => m,
        None => return Err(StatusCode::NOT_FOUND),
    };

    // Filename in URL must match stored filename
    if meta.filename != filename {
        return Err(StatusCode::NOT_FOUND);
    }

    let body = match tokio::fs::read(&meta.path).await {
        Ok(b) => b,
        Err(_) => return Err(StatusCode::NOT_FOUND),
    };

    // RFC 5987 Content-Disposition with UTF-8 filename
    use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
    let ascii_fallback: String = meta.filename.chars()
        .map(|c| if c.is_ascii() && c != '"' { c } else { '_' })
        .collect();
    let percent_encoded = utf8_percent_encode(&meta.filename, NON_ALPHANUMERIC);
    let disposition = format!(
        "attachment; filename=\"{}\"; filename*=UTF-8''{}",
        ascii_fallback, percent_encoded
    );

    // Content-Length from actual body (not stored meta, which could be stale)
    let content_length = body.len().to_string();

    // Note: X-Content-Type-Options: nosniff is already set by the security_headers middleware
    Ok((
        [
            (header::CONTENT_TYPE, meta.content_type.clone()),
            (header::CONTENT_DISPOSITION, disposition),
            (header::CONTENT_LENGTH, content_length),
        ],
        body,
    ))
}
```

- [ ] **Step 2: Create the `AttachmentStore` instance in `start_http_server`**

Add after the `session_store` creation (~line 65):

```rust
    let attachment_store = Arc::new(attachment_store::AttachmentStore::new());
```

- [ ] **Step 3: Add the route to the axum router**

In the router chain (~line 188), add the attachment route before `.with_state(oauth2_state)`. The route needs its own state (the `AttachmentStore`), so use a nested router:

```rust
        // Attachment download endpoint (token-based, no OAuth required)
        .route(
            "/attachments/{token}/{filename}",
            axum::routing::get(serve_attachment)
                .with_state(attachment_store.clone()),
        )
```

Add this just before the `.route("/oauth/revoke", ...)` line.

- [ ] **Step 4: Add `percent-encoding` dependency**

Run: `cargo add percent-encoding`

- [ ] **Step 5: Verify it compiles**

Run: `cargo check 2>&1`
Expected: No errors

- [ ] **Step 6: Commit**

```bash
git add src/main.rs Cargo.toml Cargo.lock
git commit -m "feat: add GET /attachments/:token/:filename endpoint for file downloads"
```

---

### Task 3: Wire `AttachmentStore` into `ExchangeMcpServer` and return download URLs

**Files:**
- Modify: `src/server.rs` (struct ~line 69, new() ~line 78, download_attachment ~line 873)
- Modify: `src/main.rs` (factory closure ~line 155)

- [ ] **Step 1: Add fields to `ExchangeMcpServer` struct**

In `src/server.rs` (~line 70), add two fields:

```rust
#[derive(Clone)]
pub struct ExchangeMcpServer {
    imap: Arc<ImapClient>,
    ews: Arc<EwsClient>,
    attachment_store: Arc<crate::attachment_store::AttachmentStore>,
    issuer: String,
    tool_router: ToolRouter<Self>,
}
```

- [ ] **Step 2: Update `new()` to accept the new fields**

```rust
    pub fn new(
        imap: Arc<ImapClient>,
        ews: Arc<EwsClient>,
        attachment_store: Arc<crate::attachment_store::AttachmentStore>,
        issuer: String,
    ) -> Self {
        Self {
            imap,
            ews,
            attachment_store,
            issuer,
            tool_router: Self::tool_router(),
        }
    }
```

- [ ] **Step 3: Update `download_attachment` tool to register token and return URL**

Replace the tool handler (~line 873):

```rust
    #[tool(
        description = "Download an attachment from an email. Returns a temporary download URL (valid 24h) that can be opened in a browser or used with wget/curl. Use read_email first to get the list of attachment filenames.",
        annotations(read_only_hint = false, destructive_hint = false, idempotent_hint = false, open_world_hint = false)
    )]
    async fn download_attachment(&self, Parameters(params): Parameters<DownloadAttachmentParams>) -> Result<CallToolResult, ErrorData> {
        match self.imap.download_attachment(&params.folder, params.uid, &params.filename).await {
            Ok(result) => {
                use crate::attachment_store::{AttachmentStore, AttachmentMeta};
                use std::time::{Duration, Instant};

                let meta = AttachmentMeta {
                    path: result.path.clone(),
                    filename: result.filename.clone(),
                    content_type: AttachmentStore::sanitize_content_type(&result.content_type),
                    size: result.size,
                    expires_at: Instant::now() + Duration::from_secs(24 * 3600),
                };
                let token = self.attachment_store.insert(meta);
                let encoded_filename = percent_encoding::utf8_percent_encode(
                    &result.filename,
                    percent_encoding::NON_ALPHANUMERIC,
                );
                let download_url = format!(
                    "{}/attachments/{}/{}",
                    self.issuer, token, encoded_filename
                );

                let text = serde_json::to_string_pretty(&json!({
                    "download_url": download_url,
                    "filename": result.filename,
                    "size": result.size,
                    "content_type": result.content_type,
                }))
                .unwrap_or_else(|e| e.to_string());
                Ok(CallToolResult::success(vec![Content::text(text)]))
            }
            Err(e) => Ok(CallToolResult::error(vec![Content::text(format!("Error downloading attachment: {e}"))])),
        }
    }
```

Note: the `path` field is removed from the JSON response (server-local path useless to remote users). The `download_url` replaces it. Spec updated to match.

- [ ] **Step 4: Update the MCP factory closure in `main.rs`**

In `src/main.rs`, the factory closure (~line 155) must capture and pass the new fields. Update:

Before the `mcp_service` creation, clone the values for the closure:

```rust
    let attachment_store_for_mcp = attachment_store.clone();
    let issuer_for_mcp = issuer.clone();
```

Then inside the closure, change the `ExchangeMcpServer::new` call:

```rust
                Some(session) => Ok(ExchangeMcpServer::new(
                    session.imap.clone(),
                    session.ews.clone(),
                    attachment_store_for_mcp.clone(),
                    issuer_for_mcp.clone(),
                )),
```

- [ ] **Step 5: Verify it compiles**

Run: `cargo check 2>&1`
Expected: No errors, no warnings on project code

- [ ] **Step 6: Commit**

```bash
git add src/server.rs src/main.rs
git commit -m "feat: return download URL from download_attachment tool"
```

---

### Task 4: Extend cleanup task to purge expired attachments

**Files:**
- Modify: `src/main.rs` (cleanup task ~line 125)

- [ ] **Step 1: Add attachment cleanup to the periodic task**

In the cleanup task block (~line 125), capture the attachment store:

```rust
    {
        let sessions = session_store.clone();
        let store = oauth2_store.clone();
        let attachments = attachment_store.clone();
        tokio::spawn(async move {
```

Then after the orphaned tokens cleanup (~line 144), add:

```rust
                // Clean expired attachment download tokens and delete files
                let expired_paths = attachments.cleanup_expired();
                if !expired_paths.is_empty() {
                    tracing::info!("Cleaning up {} expired attachment(s)", expired_paths.len());
                    for path in expired_paths {
                        if let Err(e) = tokio::fs::remove_file(&path).await {
                            tracing::warn!("Failed to delete expired attachment {:?}: {e}", path);
                        }
                    }
                }
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo check 2>&1`
Expected: No errors, no warnings on project code

- [ ] **Step 3: Commit**

```bash
git add src/main.rs
git commit -m "feat: purge expired attachment tokens and files in periodic cleanup"
```

---

### Task 5: Update documentation

**Files:**
- Modify: `CLAUDE.md`
- Modify: `README.md` (if it documents the download_attachment tool)

- [ ] **Step 1: Update CLAUDE.md**

Add `attachment_store.rs` to the file tree:

```
├── attachment_store.rs  # Store tokens temporaires pour telechargement HTTP des pieces jointes
```

Update the `download_attachment` description in the tools section if present. Add the new endpoint `/attachments/:token/:filename` to the data flow section.

- [ ] **Step 2: Update README.md**

Document the new behavior of `download_attachment` (returns `download_url` instead of `path`). Document the `GET /attachments/:token/:filename` endpoint.

- [ ] **Step 3: Verify it compiles (final check)**

Run: `cargo build --release 2>&1`
Expected: Successful build

- [ ] **Step 4: Commit**

```bash
git add CLAUDE.md README.md
git commit -m "docs: document attachment download URL feature and new HTTP endpoint"
```
