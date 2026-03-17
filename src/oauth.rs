use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::config::Config;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TokenCache {
    access_token: String,
    refresh_token: Option<String>,
    expires_at: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct DeviceCodeResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    #[serde(default)]
    verification_uri_complete: Option<String>,
    expires_in: u64,
    interval: u64,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<u64>,
    #[serde(default)]
    token_type: String,
}

#[derive(Debug, Deserialize)]
struct TokenErrorResponse {
    error: String,
    error_description: Option<String>,
}

pub struct OAuthManager {
    tenant_id: String,
    client_id: String,
    client_secret: Option<String>,
    email: String,
    token_cache_path: PathBuf,
    cached_token: Arc<RwLock<Option<TokenCache>>>,
    http_client: reqwest::Client,
}

impl OAuthManager {
    pub fn new(config: &Config) -> Result<Self> {
        let token_cache_path = Config::token_cache_path();

        Ok(Self {
            tenant_id: config.tenant_id.clone(),
            client_id: config.client_id.clone(),
            client_secret: config.client_secret.clone(),
            email: config.email.clone(),
            token_cache_path,
            cached_token: Arc::new(RwLock::new(None)),
            http_client: reqwest::Client::new(),
        })
    }

    pub fn email(&self) -> &str {
        &self.email
    }

    fn token_url(&self) -> String {
        format!(
            "https://login.microsoftonline.com/{}/oauth2/v2.0/token",
            self.tenant_id
        )
    }

    fn device_code_url(&self) -> String {
        format!(
            "https://login.microsoftonline.com/{}/oauth2/v2.0/devicecode",
            self.tenant_id
        )
    }

    /// Load cached token from disk
    pub async fn load_cached_token(&self) -> Result<()> {
        if self.token_cache_path.exists() {
            let content = tokio::fs::read_to_string(&self.token_cache_path).await?;
            match serde_json::from_str::<TokenCache>(&content) {
                Ok(cache) => {
                    tracing::info!("Loaded cached OAuth token");
                    *self.cached_token.write().await = Some(cache);
                }
                Err(e) => {
                    tracing::warn!("Failed to parse token cache: {e}");
                }
            }
        }
        Ok(())
    }

    /// Save token to disk
    async fn save_token(&self, cache: &TokenCache) -> Result<()> {
        if let Some(parent) = self.token_cache_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let content = serde_json::to_string_pretty(cache)?;
        tokio::fs::write(&self.token_cache_path, content).await?;
        Ok(())
    }

    /// Get a valid access token, refreshing or re-authenticating as needed
    pub async fn get_access_token(&self) -> Result<String> {
        // Check if we have a valid cached token
        {
            let cache = self.cached_token.read().await;
            if let Some(ref token) = *cache {
                if let Some(expires_at) = token.expires_at {
                    let now = chrono::Utc::now().timestamp();
                    if now < expires_at - 60 {
                        return Ok(token.access_token.clone());
                    }
                }
            }
        }

        // Try refresh token
        {
            let cache = self.cached_token.read().await;
            if let Some(ref token) = *cache {
                if let Some(ref refresh_token) = token.refresh_token {
                    let rt = refresh_token.clone();
                    drop(cache);
                    match self.refresh_access_token(&rt).await {
                        Ok(new_token) => return Ok(new_token),
                        Err(e) => {
                            tracing::warn!("Token refresh failed, will re-authenticate: {e}");
                        }
                    }
                }
            }
        }

        // Full device code auth
        self.device_code_auth().await
    }

    /// Refresh the access token using a refresh token
    async fn refresh_access_token(&self, refresh_token: &str) -> Result<String> {
        let mut params = vec![
            ("client_id", self.client_id.as_str()),
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            (
                "scope",
                "https://outlook.office365.com/IMAP.AccessAsUser.All https://outlook.office365.com/SMTP.Send offline_access",
            ),
        ];

        let secret_val;
        if let Some(ref secret) = self.client_secret {
            secret_val = secret.clone();
            params.push(("client_secret", &secret_val));
        }

        let resp = self
            .http_client
            .post(&self.token_url())
            .form(&params)
            .send()
            .await?;

        let status = resp.status();
        let body = resp.text().await?;

        if !status.is_success() {
            anyhow::bail!("Token refresh failed ({status}): {body}");
        }

        let token_resp: TokenResponse =
            serde_json::from_str(&body).context("Failed to parse token response")?;

        let cache = TokenCache {
            access_token: token_resp.access_token.clone(),
            refresh_token: token_resp
                .refresh_token
                .or_else(|| Some(refresh_token.to_string())),
            expires_at: token_resp
                .expires_in
                .map(|d| chrono::Utc::now().timestamp() + d as i64),
        };

        self.save_token(&cache).await?;
        let access_token = cache.access_token.clone();
        *self.cached_token.write().await = Some(cache);

        tracing::info!("Token refreshed successfully");
        Ok(access_token)
    }

    /// Perform device code flow authentication
    async fn device_code_auth(&self) -> Result<String> {
        // Step 1: Request device code
        let resp = self
            .http_client
            .post(&self.device_code_url())
            .form(&[
                ("client_id", self.client_id.as_str()),
                (
                    "scope",
                    "https://outlook.office365.com/IMAP.AccessAsUser.All https://outlook.office365.com/SMTP.Send offline_access",
                ),
            ])
            .send()
            .await?;

        let status = resp.status();
        let body = resp.text().await?;

        if !status.is_success() {
            anyhow::bail!("Device code request failed ({status}): {body}");
        }

        let device_resp: DeviceCodeResponse =
            serde_json::from_str(&body).context("Failed to parse device code response")?;

        // Step 2: Display instructions (via stderr, stdout is for MCP)
        eprintln!();
        eprintln!("========================================");
        eprintln!("  Microsoft Exchange Authentication");
        eprintln!("========================================");
        if let Some(ref uri) = device_resp.verification_uri_complete {
            eprintln!("Open this URL in your browser:");
            eprintln!("  {uri}");
        } else {
            eprintln!("Go to: {}", device_resp.verification_uri);
            eprintln!("Enter code: {}", device_resp.user_code);
        }
        eprintln!("========================================");
        eprintln!();

        // Step 3: Poll for token
        let interval = std::time::Duration::from_secs(device_resp.interval.max(5));
        let deadline =
            std::time::Instant::now() + std::time::Duration::from_secs(device_resp.expires_in);

        loop {
            tokio::time::sleep(interval).await;

            if std::time::Instant::now() > deadline {
                anyhow::bail!("Device code authentication timed out");
            }

            let mut params = vec![
                ("client_id", self.client_id.as_str()),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("device_code", device_resp.device_code.as_str()),
            ];

            let secret_val;
            if let Some(ref secret) = self.client_secret {
                secret_val = secret.clone();
                params.push(("client_secret", &secret_val));
            }

            let resp = self
                .http_client
                .post(&self.token_url())
                .form(&params)
                .send()
                .await?;

            let status = resp.status();
            let body = resp.text().await?;

            if status.is_success() {
                let token_resp: TokenResponse =
                    serde_json::from_str(&body).context("Failed to parse token response")?;

                let cache = TokenCache {
                    access_token: token_resp.access_token.clone(),
                    refresh_token: token_resp.refresh_token,
                    expires_at: token_resp
                        .expires_in
                        .map(|d| chrono::Utc::now().timestamp() + d as i64),
                };

                self.save_token(&cache).await?;
                let access_token = cache.access_token.clone();
                *self.cached_token.write().await = Some(cache);

                eprintln!("Authentication successful!");
                return Ok(access_token);
            }

            // Check if we should keep polling
            if let Ok(err_resp) = serde_json::from_str::<TokenErrorResponse>(&body) {
                match err_resp.error.as_str() {
                    "authorization_pending" => continue,
                    "slow_down" => {
                        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                        continue;
                    }
                    "expired_token" => {
                        anyhow::bail!("Device code expired. Please try again.");
                    }
                    "access_denied" => {
                        anyhow::bail!("Access denied by user.");
                    }
                    _ => {
                        anyhow::bail!(
                            "Authentication error: {} - {}",
                            err_resp.error,
                            err_resp.error_description.unwrap_or_default()
                        );
                    }
                }
            } else {
                anyhow::bail!("Unexpected token response ({status}): {body}");
            }
        }
    }
}
