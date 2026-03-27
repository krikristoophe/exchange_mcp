# Attachment Download URL — Design Spec

## Problem

The MCP server runs on a remote host. The current `download_attachment` tool saves files to the server's filesystem and returns a local path — useless for the end user who needs the file on their machine.

## Solution

Expose downloaded attachments via temporary HTTP URLs. When `download_attachment` is called, the server saves the file to disk, generates a temporary token, and returns a download URL. The user can then `wget`/`curl`/open the URL in a browser to retrieve the file.

## Design

### New module: `src/attachment_store.rs`

In-memory store for attachment download tokens, following the same pattern as `SessionStore`.

**Struct `AttachmentMeta`** (derives `Clone`):

| Field          | Type       | Description                              |
|----------------|------------|------------------------------------------|
| `path`         | `PathBuf`  | Absolute path to the file on disk        |
| `filename`     | `String`   | Sanitized filename (for Content-Disposition) |
| `content_type` | `String`   | Sanitized MIME type (see Security)       |
| `size`         | `u64`      | File size in bytes                       |
| `expires_at`   | `Instant`  | Expiration time (creation + 24h)         |

**Struct `AttachmentStore`:**

- Inner: `RwLock<HashMap<String, AttachmentMeta>>`
- `insert(meta: AttachmentMeta) -> String` — generates a token (`base64(random_bytes(32))` URL-safe no padding), inserts entry, returns token
- `get(token: &str) -> Option<AttachmentMeta>` — returns meta if token exists and not expired
- `cleanup_expired() -> Vec<PathBuf>` — removes entries expired by more than 5 minutes (grace period for in-flight downloads), returns list of file paths to delete

Token format: same as existing tokens in the project — `base64(random_bytes(32))` URL-safe without padding (256 bits).

TTL: 24 hours (constant `ATTACHMENT_TOKEN_TTL`).

### Modified: `src/server.rs`

**`ExchangeMcpServer` struct** gets a new field:

```
attachment_store: Arc<AttachmentStore>
issuer: String
```

**`download_attachment` tool** — after downloading the file:

1. Inserts an entry in `AttachmentStore`
2. Builds URL: `{issuer}/attachments/{token}/{filename}`
3. Returns JSON with existing fields (`path`, `filename`, `size`, `content_type`) plus new field `download_url`

### New endpoint: `GET /attachments/:token/:filename`

Added to the axum router in `main.rs`.

**Flow:**

1. Extract `:token` and `:filename` from path
2. Look up token in `AttachmentStore`
3. If not found or expired → 404
4. If `:filename` doesn't match stored filename → 404 (prevent URL manipulation)
5. Open file at stored `path`
6. Serve with headers:
   - `Content-Type: {content_type}` (sanitized, see Security)
   - `Content-Disposition: attachment; filename="{ascii_fallback}"; filename*=UTF-8''{percent_encoded}` (RFC 5987 for non-ASCII)
   - `Content-Length: {size}`
   - `X-Content-Type-Options: nosniff`
7. Stream file body

**No Bearer token required** — the random token in the URL is the sole auth mechanism.

The route is added alongside existing routes in the axum router (above the security headers layer). The security headers middleware applies but is harmless for file downloads.

### Modified: cleanup task in `main.rs`

Add to the existing 5-minute periodic cleanup loop:

```
let expired_paths = attachment_store.cleanup_expired();
for path in expired_paths {
    let _ = tokio::fs::remove_file(&path).await;
}
```

Log the count of cleaned attachments (same pattern as session cleanup).

### Modified: `src/config.rs`

No changes needed. The existing `attachment_dir` config is reused.

### Modified: MCP factory in `main.rs`

The factory closure (which creates `ExchangeMcpServer` per request) must capture `attachment_store.clone()` and `issuer.clone()` and pass them to `ExchangeMcpServer::new()`. Same pattern as the existing `sessions_for_mcp` capture.

## Data Flow

```
MCP Client → download_attachment(folder, uid, filename)
  → ImapClient fetches email, extracts attachment, saves to disk
  → AttachmentStore.insert(meta) → token
  → Returns { path, filename, size, content_type, download_url }

User → GET /attachments/{token}/{filename}
  → AttachmentStore.get(token) → meta
  → Serve file with correct headers
  → (file stays on disk until token expires)

Cleanup task (every 5 min):
  → AttachmentStore.cleanup_expired() → expired paths
  → Delete files from disk
```

## Security Considerations

- **Token entropy**: 256-bit random tokens (same as session tokens) — not guessable
- **Filename validation**: endpoint checks that URL filename matches stored filename
- **Path traversal**: files are already written with path traversal protection in `download_attachment` (canonical path check)
- **No auth bypass**: the attachment endpoint is independent from the MCP auth chain — knowing a download token doesn't grant access to MCP tools
- **Expiration**: tokens and files are cleaned up after 24h (+5min grace), limiting exposure window
- **No directory listing**: the endpoint only serves files with valid tokens, not arbitrary paths
- **MIME type sanitization**: content types from emails are untrusted. Allowlist of safe types (`image/*`, `application/pdf`, `text/plain`, `application/zip`, `application/vnd.openxmlformats-*`, `application/vnd.ms-*`). Anything else is served as `application/octet-stream` to prevent XSS via `text/html` content type injection
- **Cleanup race condition**: `cleanup_expired()` uses a 5-minute grace period after token expiry before deleting files, avoiding races with in-flight downloads

## Files Changed

| File                        | Change                                          |
|-----------------------------|------------------------------------------------|
| `src/attachment_store.rs`   | **New** — AttachmentStore + AttachmentMeta      |
| `src/server.rs`             | Add store/issuer to struct, return download_url |
| `src/main.rs`               | New endpoint, pass store to factory, cleanup    |
| `src/main.rs`               | `mod attachment_store;` declaration             |

## Not in Scope

- Persisting tokens in SQLite (lost on restart, acceptable for 24h tokens)
- One-shot tokens (can be added later if needed)
- Authentication on the download endpoint (token-based access is sufficient)
- UI resource for attachment browsing
