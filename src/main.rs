mod auth;
mod config;
mod imap;
mod middleware;
mod oauth;
mod server;
mod session;

use std::sync::Arc;

use anyhow::Result;
use tracing_subscriber::EnvFilter;

use crate::middleware::AuthMcpService;
use crate::oauth::OAuth2State;
use crate::oauth::store::OAuth2Store;
use crate::server::ExchangeMcpServer;
use crate::session::SessionStore;

tokio::task_local! {
    /// The current user's session token, set by the MCP auth middleware.
    pub static CURRENT_USER_TOKEN: String;
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    tracing::info!("Starting Exchange MCP Server");

    let config = config::Config::load()?;
    start_http_server(config).await
}

async fn start_http_server(config: config::Config) -> Result<()> {
    use rmcp::transport::streamable_http_server::{
        StreamableHttpService,
        session::local::LocalSessionManager,
        StreamableHttpServerConfig,
    };

    let addr = format!("{}:{}", config.sse_host, config.sse_port);
    tracing::info!("Starting MCP Streamable HTTP server on {addr}");

    let session_store = Arc::new(SessionStore::new());

    let issuer = config.issuer_url();

    let oauth2_store = Arc::new(OAuth2Store::open(Some(OAuth2Store::db_path()))?);
    tracing::info!("OAuth2 store: {:?}", OAuth2Store::db_path());

    let _ = oauth2_store.cleanup_expired();

    let oauth2_state = Arc::new(OAuth2State {
        store: oauth2_store.clone(),
        sessions: session_store.clone(),
        issuer: issuer.clone(),
        default_imap_host: config.imap_host.clone(),
        default_imap_port: config.imap_port,
    });

    // MCP service — the factory reads CURRENT_USER_TOKEN task-local
    // to determine which user's IMAP client to use.
    let sessions_for_mcp = session_store.clone();
    let session_manager = Arc::new(LocalSessionManager::default());
    let server_config = StreamableHttpServerConfig::default();

    let mcp_service = StreamableHttpService::new(
        move || {
            let token = CURRENT_USER_TOKEN.try_with(|t| t.clone()).map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "Not authenticated.",
                )
            })?;

            let sessions = sessions_for_mcp.clone();
            let guard = sessions.sessions_blocking_read();
            match guard.get(&token) {
                Some(session) => Ok(ExchangeMcpServer::new(session.imap.clone())),
                None => Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "Invalid or expired session token.",
                )),
            }
        },
        session_manager,
        server_config,
    );

    // Wrap MCP service with auth middleware
    let auth_mcp = AuthMcpService {
        inner: mcp_service,
        oauth2_store,
        issuer: issuer.clone(),
    };

    let router = axum::Router::new()
        .route("/favicon.ico", axum::routing::get(middleware::favicon))
        // OAuth 2.1 well-known endpoints
        .route(
            "/.well-known/oauth-protected-resource",
            axum::routing::get(oauth::endpoints::protected_resource_metadata),
        )
        .route(
            "/.well-known/oauth-authorization-server",
            axum::routing::get(oauth::endpoints::authorization_server_metadata),
        )
        // OAuth 2.1 endpoints
        .route(
            "/oauth/register",
            axum::routing::post(oauth::endpoints::register_client),
        )
        .route(
            "/oauth/authorize",
            axum::routing::get(oauth::endpoints::authorize_get)
                .post(oauth::endpoints::authorize_post),
        )
        .route(
            "/oauth/token",
            axum::routing::post(oauth::endpoints::token_endpoint),
        )
        .with_state(oauth2_state)
        // MCP endpoint (with auth middleware)
        .nest_service("/mcp", auth_mcp);

    let tcp_listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("MCP endpoint: http://{addr}/mcp (OAuth 2.1)");
    tracing::info!("OAuth metadata: http://{addr}/.well-known/oauth-authorization-server");

    axum::serve(tcp_listener, router).await?;

    Ok(())
}
