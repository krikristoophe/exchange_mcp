use anyhow::Result;
use async_trait::async_trait;

/// Credentials needed to authenticate an IMAP/SMTP session.
pub enum ImapCredentials {
    /// OAuth2 XOAUTH2 authentication (Microsoft 365)
    OAuth2 {
        email: String,
        access_token: String,
    },
    /// Plain LOGIN authentication (self-hosted Exchange, standard IMAP)
    Basic {
        username: String,
        password: String,
    },
}

/// Trait for providing authentication credentials to IMAP/SMTP clients.
#[async_trait]
pub trait AuthProvider: Send + Sync {
    /// Returns credentials for the current session, refreshing tokens if needed.
    async fn get_credentials(&self) -> Result<ImapCredentials>;

    /// The email address of the authenticated user.
    fn email(&self) -> &str;
}

/// Basic (username/password) auth provider for self-hosted Exchange / standard IMAP servers.
pub struct BasicAuthProvider {
    username: String,
    password: String,
    email: String,
}

impl BasicAuthProvider {
    pub fn new(username: String, password: String, email: String) -> Self {
        Self {
            username,
            password,
            email,
        }
    }
}

#[async_trait]
impl AuthProvider for BasicAuthProvider {
    async fn get_credentials(&self) -> Result<ImapCredentials> {
        Ok(ImapCredentials::Basic {
            username: self.username.clone(),
            password: self.password.clone(),
        })
    }

    fn email(&self) -> &str {
        &self.email
    }
}
