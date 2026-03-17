pub mod endpoints;
pub mod store;

use std::sync::Arc;

use crate::session::SessionStore;
use self::store::OAuth2Store;

/// Shared state for OAuth2 endpoints.
pub struct OAuth2State {
    pub store: Arc<OAuth2Store>,
    pub sessions: Arc<SessionStore>,
    pub issuer: String,
    /// Default IMAP host from config (env or config file).
    pub default_imap_host: String,
    /// Default IMAP port from config (env or config file).
    pub default_imap_port: u16,
}
