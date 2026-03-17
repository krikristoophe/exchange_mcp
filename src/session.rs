use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;

use crate::imap_client::ImapClient;

/// Represents an authenticated user session.
pub struct UserSession {
    pub email: String,
    pub imap: Arc<ImapClient>,
    pub imap_host: String,
    pub imap_port: u16,
}

/// Thread-safe store for multi-user sessions, keyed by token.
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

    pub async fn get_imap(&self, token: &str) -> Option<Arc<ImapClient>> {
        self.sessions.read().await.get(token).map(|s| s.imap.clone())
    }

    pub async fn get_email(&self, token: &str) -> Option<String> {
        self.sessions.read().await.get(token).map(|s| s.email.clone())
    }

    pub async fn remove(&self, token: &str) -> Option<UserSession> {
        self.sessions.write().await.remove(token)
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

    /// List all active sessions (token, email).
    pub async fn list(&self) -> Vec<(String, String)> {
        self.sessions
            .read()
            .await
            .iter()
            .map(|(token, s)| (token.clone(), s.email.clone()))
            .collect()
    }
}
