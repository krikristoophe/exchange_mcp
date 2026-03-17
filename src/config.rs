use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const DEFAULT_IMAP_HOST: &str = "outlook.office365.com";
pub const DEFAULT_IMAP_PORT: u16 = 993;
pub const DEFAULT_SMTP_HOST: &str = "smtp.office365.com";
pub const DEFAULT_SMTP_PORT: u16 = 587;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// IMAP server hostname
    #[serde(default = "default_imap_host")]
    pub imap_host: String,
    /// IMAP server port
    #[serde(default = "default_imap_port")]
    pub imap_port: u16,
    /// SMTP server hostname
    #[serde(default = "default_smtp_host")]
    pub smtp_host: String,
    /// SMTP server port
    #[serde(default = "default_smtp_port")]
    pub smtp_port: u16,
    /// HTTP server bind address
    #[serde(default = "default_sse_host")]
    pub sse_host: String,
    /// HTTP server port
    #[serde(default = "default_sse_port")]
    pub sse_port: u16,
}

fn default_imap_host() -> String {
    DEFAULT_IMAP_HOST.to_string()
}

fn default_imap_port() -> u16 {
    DEFAULT_IMAP_PORT
}

fn default_smtp_host() -> String {
    DEFAULT_SMTP_HOST.to_string()
}

fn default_smtp_port() -> u16 {
    DEFAULT_SMTP_PORT
}

fn default_sse_host() -> String {
    "127.0.0.1".to_string()
}

fn default_sse_port() -> u16 {
    3000
}

impl Config {
    pub fn load() -> anyhow::Result<Self> {
        let config_path = Self::config_path();

        if config_path.exists() {
            let content = std::fs::read_to_string(&config_path)?;
            let mut config: Config = serde_json::from_str(&content)?;
            config.apply_env_overrides();
            Ok(config)
        } else {
            Self::from_env()
        }
    }

    pub fn config_path() -> PathBuf {
        if let Ok(path) = std::env::var("EXCHANGE_MCP_CONFIG") {
            PathBuf::from(path)
        } else {
            dirs::config_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("exchange-mcp")
                .join("config.json")
        }
    }

    fn from_env() -> anyhow::Result<Self> {
        Ok(Config {
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
            sse_host: std::env::var("EXCHANGE_MCP_SSE_HOST")
                .unwrap_or_else(|_| default_sse_host()),
            sse_port: std::env::var("EXCHANGE_MCP_SSE_PORT")
                .ok()
                .and_then(|p| p.parse().ok())
                .unwrap_or_else(default_sse_port),
        })
    }

    /// The OAuth 2.1 issuer URL. Override with EXCHANGE_MCP_ISSUER env var.
    pub fn issuer_url(&self) -> String {
        if let Ok(url) = std::env::var("EXCHANGE_MCP_ISSUER") {
            return url.trim_end_matches('/').to_string();
        }
        let host = if self.sse_host == "0.0.0.0" {
            "localhost"
        } else {
            &self.sse_host
        };
        format!("http://{}:{}", host, self.sse_port)
    }

    fn apply_env_overrides(&mut self) {
        if let Ok(v) = std::env::var("EXCHANGE_IMAP_HOST") {
            self.imap_host = v;
        }
        if let Ok(v) = std::env::var("EXCHANGE_IMAP_PORT") {
            if let Ok(p) = v.parse() {
                self.imap_port = p;
            }
        }
        if let Ok(v) = std::env::var("EXCHANGE_SMTP_HOST") {
            self.smtp_host = v;
        }
        if let Ok(v) = std::env::var("EXCHANGE_SMTP_PORT") {
            if let Ok(p) = v.parse() {
                self.smtp_port = p;
            }
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
