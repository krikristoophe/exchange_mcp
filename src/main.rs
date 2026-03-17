mod auth;
mod config;
mod imap_client;
mod login;
mod oauth;
mod server;

use std::sync::Arc;

use anyhow::Result;
use rmcp::ServiceExt;
use tokio::sync::RwLock;
use tracing_subscriber::EnvFilter;

use crate::auth::{AuthProvider, BasicAuthProvider};
use crate::config::Config;
use crate::imap_client::ImapClient;
use crate::login::AppState;
use crate::oauth::OAuthManager;
use crate::server::ExchangeMcpServer;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging to stderr (stdout is used for MCP stdio transport)
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    tracing::info!("Starting Exchange MCP Server");

    // Load configuration
    let config = Config::load()?;

    match config.transport.as_str() {
        "http" => {
            // In HTTP mode, try to set up auth but allow starting without it
            // (login page will handle authentication)
            let imap = try_create_imap_client(&config).await;
            start_http_server(imap, config).await?;
        }
        _ => {
            // stdio mode requires auth to be configured upfront
            let imap = create_imap_client(&config).await?;
            let mcp_server = ExchangeMcpServer::new(imap);
            start_stdio_server(mcp_server).await?;
        }
    }

    Ok(())
}

/// Try to create an IMAP client from config. Returns None if auth is not configured.
async fn try_create_imap_client(config: &Config) -> Option<Arc<ImapClient>> {
    match create_imap_client(config).await {
        Ok(imap) => {
            tracing::info!("Auth pre-configured, IMAP client ready");
            Some(imap)
        }
        Err(e) => {
            tracing::info!("Auth not pre-configured ({e}), login page will be available at /");
            None
        }
    }
}

/// Create an IMAP client from config, returning an error if auth is not configured.
async fn create_imap_client(config: &Config) -> Result<Arc<ImapClient>> {
    let auth_provider: Arc<dyn AuthProvider> = match config.auth_method.as_str() {
        "basic" => {
            let username = config.username.clone().unwrap_or_else(|| config.email.clone());
            let password = config.password.clone()
                .ok_or_else(|| anyhow::anyhow!("password is required for basic auth"))?;
            tracing::info!("Using basic (login/password) authentication");
            Arc::new(BasicAuthProvider::new(username, password, config.email.clone()))
        }
        _ => {
            if config.client_id.is_empty() || config.tenant_id.is_empty() {
                anyhow::bail!("client_id and tenant_id are required for OAuth2 auth");
            }
            let oauth = Arc::new(OAuthManager::new(config)?);
            oauth.load_cached_token().await?;
            tracing::info!("Using OAuth2 (Microsoft 365) authentication");
            oauth
        }
    };

    Ok(Arc::new(ImapClient::new(
        auth_provider,
        config.imap_host.clone(),
        config.imap_port,
    )))
}

async fn start_stdio_server(server: ExchangeMcpServer) -> Result<()> {
    tracing::info!("Starting MCP server on stdio");

    let transport = rmcp::transport::io::stdio();
    let service = server.serve(transport).await?;
    service.waiting().await?;

    Ok(())
}

async fn start_http_server(imap: Option<Arc<ImapClient>>, config: Config) -> Result<()> {
    use rmcp::transport::streamable_http_server::{
        StreamableHttpService,
        session::local::LocalSessionManager,
        StreamableHttpServerConfig,
    };

    let addr = format!("{}:{}", config.sse_host, config.sse_port);
    tracing::info!("Starting MCP Streamable HTTP server on {addr}");

    let app_state = Arc::new(AppState {
        imap: RwLock::new(imap),
        config: RwLock::new(config),
    });

    // MCP service — uses a closure that reads the current IMAP client from state
    let state_for_mcp = app_state.clone();
    let session_manager = Arc::new(LocalSessionManager::default());
    let server_config = StreamableHttpServerConfig::default();

    let mcp_service = StreamableHttpService::new(
        move || {
            let state = state_for_mcp.clone();
            // We need to get the imap client synchronously here.
            // Use try_read to avoid blocking; if not available, return error.
            let imap_guard = state.imap.blocking_read();
            match imap_guard.as_ref() {
                Some(imap) => Ok(ExchangeMcpServer::new(imap.clone())),
                None => Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, "Not authenticated. Please visit the login page at / first.")),
            }
        },
        session_manager,
        server_config,
    );

    // Build the router with login routes + MCP
    let router = axum::Router::new()
        .route("/", axum::routing::get(login::login_page))
        .route("/api/login", axum::routing::post(login::api_login))
        .route("/api/status", axum::routing::get(login::api_status))
        .route("/favicon.ico", axum::routing::get(login::favicon))
        .with_state(app_state)
        .nest_service("/mcp", mcp_service);

    let tcp_listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("Login page:  http://{addr}/");
    tracing::info!("MCP endpoint: http://{addr}/mcp");

    axum::serve(tcp_listener, router).await?;

    Ok(())
}
