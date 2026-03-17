mod auth;
mod config;
mod imap_client;
mod login;
mod oauth;
mod server;
mod session;

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

    let config = Config::load()?;

    match config.transport.as_str() {
        "http" => {
            start_http_server(config).await?;
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

async fn create_imap_client(config: &Config) -> Result<Arc<ImapClient>> {
    let auth_provider: Arc<dyn AuthProvider> = match config.auth_method.as_str() {
        "basic" => {
            let username = config.username.clone().unwrap_or_else(|| config.email.clone());
            let password = config
                .password
                .clone()
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

async fn start_http_server(config: Config) -> Result<()> {
    use rmcp::transport::streamable_http_server::{
        StreamableHttpService,
        session::local::LocalSessionManager,
        StreamableHttpServerConfig,
    };

    let addr = format!("{}:{}", config.sse_host, config.sse_port);
    tracing::info!("Starting MCP Streamable HTTP server on {addr} (multi-user mode)");

    let session_store = Arc::new(SessionStore::new());

    let app_state = Arc::new(AppState {
        sessions: session_store.clone(),
        config: RwLock::new(config),
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
                    "Not authenticated. Add ?token=YOUR_TOKEN to the MCP URL.",
                )
            })?;

            let sessions = sessions_for_mcp.clone();
            // Use blocking_read since we're in a sync context inside the factory
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

    // Wrap MCP service with auth middleware that sets the task-local token
    let auth_mcp = AuthMcpService {
        inner: mcp_service,
        sessions: session_store,
    };

    let router = axum::Router::new()
        .route("/", axum::routing::get(login::login_page))
        .route("/api/login", axum::routing::post(login::api_login))
        .route("/api/logout", axum::routing::post(login::api_logout))
        .route("/api/status", axum::routing::get(login::api_status))
        .route("/api/sessions", axum::routing::get(login::api_sessions))
        .route("/favicon.ico", axum::routing::get(login::favicon))
        .with_state(app_state)
        .nest_service("/mcp", auth_mcp);

    let tcp_listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("Login page:   http://{addr}/");
    tracing::info!("MCP endpoint: http://{addr}/mcp?token=YOUR_TOKEN");

    axum::serve(tcp_listener, router).await?;

    Ok(())
}

/// Tower Service wrapper that extracts the auth token from the request
/// and sets the CURRENT_USER_TOKEN task-local before delegating to the inner MCP service.
#[derive(Clone)]
struct AuthMcpService<S> {
    inner: S,
    sessions: Arc<SessionStore>,
}

impl<S, B> tower::Service<http::Request<B>> for AuthMcpService<S>
where
    S: tower::Service<http::Request<B>> + Clone + Send + 'static,
    S::Response: IntoMcpResponse + Send + 'static,
    S::Error: Send + 'static,
    S::Future: Send + 'static,
    B: Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Self::Response, Self::Error>> + Send>,
    >;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: http::Request<B>) -> Self::Future {
        let token = login::extract_token(&req);
        let mut inner = self.inner.clone();
        // Swap the ready service into inner (standard tower pattern)
        std::mem::swap(&mut self.inner, &mut inner);

        match token {
            Some(t) => {
                Box::pin(CURRENT_USER_TOKEN.scope(t, async move { inner.call(req).await }))
            }
            None => {
                // No token — still call the service, it will fail in the factory
                // with a clear error message
                Box::pin(async move { inner.call(req).await })
            }
        }
    }
}

/// Marker trait — we need S::Response to be the correct type.
/// In practice, StreamableHttpService returns http::Response<Body>.
trait IntoMcpResponse {}
impl<T> IntoMcpResponse for http::Response<T> {}
