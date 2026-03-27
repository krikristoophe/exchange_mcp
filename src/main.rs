mod attachment_store;
mod auth;
mod cache;
mod config;
mod crypto;
mod ews;
mod imap;
mod middleware;
mod oauth;
mod server;
mod session;

use std::sync::Arc;

use anyhow::Result;
use tracing_subscriber::EnvFilter;

use crate::auth::{AuthProvider, BasicAuthProvider};
use crate::ews::EwsClient;
use crate::imap::ImapClient;
use crate::middleware::AuthMcpService;
use crate::oauth::OAuth2State;
use crate::oauth::store::OAuth2Store;
use crate::server::ExchangeMcpServer;
use crate::session::{SessionStore, UserSession};

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

    // Initialize encryption for credential storage
    crypto::init_cipher()?;
    tracing::info!("Credential encryption initialized");

    let config = config::Config::load()?;
    start_http_server(config).await
}

async fn start_http_server(config: config::Config) -> Result<()> {
    use rmcp::transport::streamable_http_server::{
        StreamableHttpService,
        session::local::LocalSessionManager,
        StreamableHttpServerConfig,
    };

    // Best-effort: create attachment directory at startup
    if let Err(e) = std::fs::create_dir_all(&config.attachment_dir) {
        tracing::warn!("Could not create attachment dir {:?}: {}", config.attachment_dir, e);
    }

    let addr = format!("{}:{}", config.sse_host, config.sse_port);
    tracing::info!("Starting MCP Streamable HTTP server on {addr}");

    let session_store = Arc::new(SessionStore::new());

    let issuer = config.issuer_url();

    let oauth2_store = Arc::new(OAuth2Store::open(Some(OAuth2Store::db_path()))?);
    tracing::info!("OAuth2 store: {:?}", OAuth2Store::db_path());

    // Restore persisted sessions from SQLite (survives server restarts)
    let mut restored_tokens = Vec::new();
    match oauth2_store.load_all_sessions() {
        Ok(persisted) => {
            for ps in persisted {
                let auth: Arc<dyn AuthProvider> =
                    Arc::new(BasicAuthProvider::new(ps.email.clone(), ps.password));
                let imap_client = Arc::new(ImapClient::new(
                    auth.clone(),
                    ps.imap_host.clone(),
                    ps.imap_port,
                    config.smtp_host.clone(),
                    config.smtp_port,
                    config.attachment_dir.clone(),
                ));
                let ews_url = EwsClient::ews_url_from_host(&ps.imap_host);
                let ews_client = Arc::new(EwsClient::new(auth, ews_url));
                session_store.insert(
                    ps.session_token.clone(),
                    UserSession {
                        email: ps.email.clone(),
                        imap: imap_client,
                        ews: ews_client,
                        imap_host: ps.imap_host,
                        imap_port: ps.imap_port,
                        last_activity: chrono::Utc::now().timestamp(),
                    },
                );
                restored_tokens.push(ps.session_token);
            }
            if !restored_tokens.is_empty() {
                tracing::info!("Restored {} session(s) from database", restored_tokens.len());
            }
        }
        Err(e) => {
            tracing::warn!("Failed to load persisted sessions: {e}");
        }
    }

    // Clean up OAuth tokens referencing sessions that no longer exist
    let _ = oauth2_store.cleanup_orphaned_tokens(&restored_tokens);

    let oauth2_state = Arc::new(OAuth2State {
        store: oauth2_store.clone(),
        sessions: session_store.clone(),
        issuer: issuer.clone(),
        default_imap_host: config.imap_host.clone(),
        default_imap_port: config.imap_port,
        default_smtp_host: config.smtp_host.clone(),
        default_smtp_port: config.smtp_port,
        attachment_dir: config.attachment_dir.clone(),
    });

    // Periodic cleanup task — runs every 5 minutes
    {
        let sessions = session_store.clone();
        let store = oauth2_store.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(300));
            loop {
                interval.tick().await;
                // Clean expired sessions
                let expired = sessions.cleanup_expired();
                if !expired.is_empty() {
                    tracing::info!("Cleaned up {} expired session(s)", expired.len());
                    // Clean associated tokens and persisted sessions
                    for token in &expired {
                        let _ = store.delete_session(token);
                    }
                }
                // Clean expired tokens and codes
                let valid = sessions.session_tokens();
                let _ = store.cleanup_orphaned_tokens(&valid);
            }
        });
    }

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
            // Touch session to update last activity
            sessions.touch(&token);
            let guard = sessions.sessions_read();
            match guard.get(&token) {
                Some(session) => Ok(ExchangeMcpServer::new(session.imap.clone(), session.ews.clone())),
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
        sessions: session_store.clone(),
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
        .route(
            "/oauth/revoke",
            axum::routing::post(oauth::endpoints::revoke_token),
        )
        .with_state(oauth2_state)
        // Security headers middleware
        .layer(axum::middleware::from_fn(middleware::security_headers))
        // MCP endpoint (with auth middleware)
        .nest_service("/mcp", auth_mcp);

    let tcp_listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("MCP endpoint: http://{addr}/mcp (OAuth 2.1)");
    tracing::info!("OAuth metadata: http://{addr}/.well-known/oauth-authorization-server");

    axum::serve(tcp_listener, router).await?;

    Ok(())
}
