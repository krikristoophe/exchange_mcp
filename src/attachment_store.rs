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
