use anyhow::Result;
use async_trait::async_trait;

/// Credentials needed to authenticate an IMAP session.
pub struct ImapCredentials {
    pub username: String,
    pub password: String,
}

/// Trait for providing authentication credentials to IMAP clients.
#[async_trait]
pub trait AuthProvider: Send + Sync {
    async fn get_credentials(&self) -> Result<ImapCredentials>;
}

/// Basic (username/password) auth provider for IMAP servers.
pub struct BasicAuthProvider {
    username: String,
    password: String,
}

impl BasicAuthProvider {
    pub fn new(username: String, password: String) -> Self {
        Self { username, password }
    }
}

#[async_trait]
impl AuthProvider for BasicAuthProvider {
    async fn get_credentials(&self) -> Result<ImapCredentials> {
        Ok(ImapCredentials {
            username: self.username.clone(),
            password: self.password.clone(),
        })
    }
}
