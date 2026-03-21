use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use crate::imap::ImapClient;

/// Session timeout: 7 days of inactivity.
const SESSION_TIMEOUT_SECS: i64 = 7 * 24 * 3600;

/// Represents an authenticated user session.
#[allow(dead_code)]
pub struct UserSession {
    pub email: String,
    pub imap: Arc<ImapClient>,
    pub imap_host: String,
    pub imap_port: u16,
    /// Timestamp of last activity (epoch seconds).
    pub last_activity: i64,
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
        let sessions = self.sessions.read().unwrap();
        if let Some(session) = sessions.get(token) {
            let now = chrono::Utc::now().timestamp();
            now - session.last_activity < SESSION_TIMEOUT_SECS
        } else {
            false
        }
    }

    /// Touch the session to update last activity timestamp.
    pub fn touch(&self, token: &str) {
        let mut sessions = self.sessions.write().unwrap();
        if let Some(session) = sessions.get_mut(token) {
            session.last_activity = chrono::Utc::now().timestamp();
        }
    }

    /// Remove a session by token.
    #[allow(dead_code)]
    pub fn remove(&self, token: &str) {
        self.sessions.write().unwrap().remove(token);
    }

    /// Remove expired sessions and return their tokens.
    pub fn cleanup_expired(&self) -> Vec<String> {
        let now = chrono::Utc::now().timestamp();
        let mut sessions = self.sessions.write().unwrap();
        let expired: Vec<String> = sessions
            .iter()
            .filter(|(_, s)| now - s.last_activity >= SESSION_TIMEOUT_SECS)
            .map(|(k, _)| k.clone())
            .collect();
        for token in &expired {
            sessions.remove(token);
        }
        expired
    }

    /// Read access for sync contexts (e.g. MCP factory).
    pub fn sessions_read(
        &self,
    ) -> std::sync::RwLockReadGuard<'_, HashMap<String, UserSession>> {
        self.sessions.read().unwrap()
    }

    /// Get all current session tokens.
    pub fn session_tokens(&self) -> Vec<String> {
        self.sessions.read().unwrap().keys().cloned().collect()
    }
}
