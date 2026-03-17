use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use crate::imap::ImapClient;

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

    pub fn insert(&self, token: String, session: UserSession) {
        self.sessions.write().unwrap().insert(token, session);
    }

    pub fn contains(&self, token: &str) -> bool {
        self.sessions.read().unwrap().contains_key(token)
    }

    /// Read access for sync contexts (e.g. MCP factory).
    pub fn sessions_read(
        &self,
    ) -> std::sync::RwLockReadGuard<'_, HashMap<String, UserSession>> {
        self.sessions.read().unwrap()
    }
}
