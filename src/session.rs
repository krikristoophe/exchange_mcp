use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;

use crate::imap_client::ImapClient;

/// Represents an authenticated user session.
#[allow(dead_code)]
pub struct UserSession {
    pub email: String,
    pub imap: Arc<ImapClient>,
    pub imap_host: String,
    pub imap_port: u16,
}

/// Thread-safe store for multi-user sessions, keyed by session token.
pub struct SessionStore {
    sessions: RwLock<HashMap<String, UserSession>>,
}

impl SessionStore {
    pub fn new() -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
        }
    }

    pub async fn insert(&self, token: String, session: UserSession) {
        self.sessions.write().await.insert(token, session);
    }

    pub async fn contains(&self, token: &str) -> bool {
        self.sessions.read().await.contains_key(token)
    }

    /// Blocking read access (for use in sync contexts like MCP factory).
    pub fn sessions_blocking_read(
        &self,
    ) -> tokio::sync::RwLockReadGuard<'_, HashMap<String, UserSession>> {
        self.sessions.blocking_read()
    }
}
