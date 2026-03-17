use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Microsoft Entra (Azure AD) tenant ID
    pub tenant_id: String,
    /// Application (client) ID from Azure app registration
    pub client_id: String,
    /// Optional client secret (for confidential apps)
    pub client_secret: Option<String>,
    /// Email address of the user
    pub email: String,
    /// IMAP server hostname (default: outlook.office365.com)
    #[serde(default = "default_imap_host")]
    pub imap_host: String,
    /// IMAP server port (default: 993)
    #[serde(default = "default_imap_port")]
    pub imap_port: u16,
    /// SMTP server hostname (default: smtp.office365.com)
    #[serde(default = "default_smtp_host")]
    pub smtp_host: String,
    /// SMTP server port (default: 587)
    #[serde(default = "default_smtp_port")]
    pub smtp_port: u16,
    /// Transport mode: "stdio" or "sse"
    #[serde(default = "default_transport")]
    pub transport: String,
    /// SSE server bind address (default: 127.0.0.1)
    #[serde(default = "default_sse_host")]
    pub sse_host: String,
    /// SSE server port (default: 3000)
    #[serde(default = "default_sse_port")]
    pub sse_port: u16,
}

fn default_imap_host() -> String {
    "outlook.office365.com".to_string()
}

fn default_imap_port() -> u16 {
    993
}

fn default_smtp_host() -> String {
    "smtp.office365.com".to_string()
}

fn default_smtp_port() -> u16 {
    587
}

fn default_transport() -> String {
    "stdio".to_string()
}

fn default_sse_host() -> String {
    "127.0.0.1".to_string()
}

fn default_sse_port() -> u16 {
    3000
}

impl Config {
    pub fn load() -> anyhow::Result<Self> {
        // Try loading from config file, then env vars
        let config_path = Self::config_path();

        if config_path.exists() {
            let content = std::fs::read_to_string(&config_path)?;
            let mut config: Config = serde_json::from_str(&content)?;
            // Override with env vars if present
            config.apply_env_overrides();
            Ok(config)
        } else {
            Self::from_env()
        }
    }

    fn config_path() -> PathBuf {
        if let Ok(path) = std::env::var("EXCHANGE_MCP_CONFIG") {
            PathBuf::from(path)
        } else {
            dirs::config_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("exchange-mcp")
                .join("config.json")
        }
    }

    pub fn token_cache_path() -> PathBuf {
        if let Ok(path) = std::env::var("EXCHANGE_MCP_TOKEN_CACHE") {
            PathBuf::from(path)
        } else {
            dirs::cache_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("exchange-mcp")
                .join("token_cache.json")
        }
    }

    fn from_env() -> anyhow::Result<Self> {
        Ok(Config {
            tenant_id: std::env::var("EXCHANGE_TENANT_ID")
                .map_err(|_| anyhow::anyhow!("EXCHANGE_TENANT_ID not set"))?,
            client_id: std::env::var("EXCHANGE_CLIENT_ID")
                .map_err(|_| anyhow::anyhow!("EXCHANGE_CLIENT_ID not set"))?,
            client_secret: std::env::var("EXCHANGE_CLIENT_SECRET").ok(),
            email: std::env::var("EXCHANGE_EMAIL")
                .map_err(|_| anyhow::anyhow!("EXCHANGE_EMAIL not set"))?,
            imap_host: std::env::var("EXCHANGE_IMAP_HOST")
                .unwrap_or_else(|_| default_imap_host()),
            imap_port: std::env::var("EXCHANGE_IMAP_PORT")
                .ok()
                .and_then(|p| p.parse().ok())
                .unwrap_or_else(default_imap_port),
            smtp_host: std::env::var("EXCHANGE_SMTP_HOST")
                .unwrap_or_else(|_| default_smtp_host()),
            smtp_port: std::env::var("EXCHANGE_SMTP_PORT")
                .ok()
                .and_then(|p| p.parse().ok())
                .unwrap_or_else(default_smtp_port),
            transport: std::env::var("EXCHANGE_MCP_TRANSPORT")
                .unwrap_or_else(|_| default_transport()),
            sse_host: std::env::var("EXCHANGE_MCP_SSE_HOST")
                .unwrap_or_else(|_| default_sse_host()),
            sse_port: std::env::var("EXCHANGE_MCP_SSE_PORT")
                .ok()
                .and_then(|p| p.parse().ok())
                .unwrap_or_else(default_sse_port),
        })
    }

    fn apply_env_overrides(&mut self) {
        if let Ok(v) = std::env::var("EXCHANGE_TENANT_ID") {
            self.tenant_id = v;
        }
        if let Ok(v) = std::env::var("EXCHANGE_CLIENT_ID") {
            self.client_id = v;
        }
        if let Ok(v) = std::env::var("EXCHANGE_CLIENT_SECRET") {
            self.client_secret = Some(v);
        }
        if let Ok(v) = std::env::var("EXCHANGE_EMAIL") {
            self.email = v;
        }
        if let Ok(v) = std::env::var("EXCHANGE_MCP_TRANSPORT") {
            self.transport = v;
        }
        if let Ok(v) = std::env::var("EXCHANGE_MCP_SSE_HOST") {
            self.sse_host = v;
        }
        if let Ok(v) = std::env::var("EXCHANGE_MCP_SSE_PORT") {
            if let Ok(p) = v.parse() {
                self.sse_port = p;
            }
        }
    }
}
