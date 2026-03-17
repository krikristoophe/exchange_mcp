mod auth;
mod config;
mod imap_client;
mod login;
mod oauth;
mod oauth2_server;
mod oauth2_store;
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
use crate::oauth2_server::OAuth2State;
use crate::oauth2_store::OAuth2Store;
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

    // Compute issuer URL from config
    let issuer = config.issuer_url();

    // Open OAuth2 SQLite store
    let oauth2_store = Arc::new(OAuth2Store::open(Some(OAuth2Store::db_path()))?);
    tracing::info!("OAuth2 store: {:?}", OAuth2Store::db_path());

    // Cleanup expired entries on startup
    let _ = oauth2_store.cleanup_expired();

    let oauth2_state = Arc::new(OAuth2State {
        store: oauth2_store.clone(),
        sessions: session_store.clone(),
        issuer: issuer.clone(),
    });

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
                    "Not authenticated.",
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

    // Wrap MCP service with auth middleware that:
    // 1. Extracts Bearer token from the request
    // 2. Resolves OAuth2 access tokens → session tokens
    // 3. Falls back to raw session tokens (legacy mode)
    // 4. Returns 401 with RFC 9728 metadata hint when no valid token
    let auth_mcp = AuthMcpService {
        inner: mcp_service,
        sessions: session_store.clone(),
        oauth2_store: oauth2_store.clone(),
        issuer: issuer.clone(),
    };

    let router = axum::Router::new()
        // Existing login UI & API
        .route("/", axum::routing::get(login::login_page))
        .route("/api/login", axum::routing::post(login::api_login))
        .route("/api/logout", axum::routing::post(login::api_logout))
        .route("/api/status", axum::routing::get(login::api_status))
        .route("/api/sessions", axum::routing::get(login::api_sessions))
        .route("/favicon.ico", axum::routing::get(login::favicon))
        .with_state(app_state)
        // OAuth 2.1 well-known endpoints
        .route(
            "/.well-known/oauth-protected-resource",
            axum::routing::get(oauth2_server::protected_resource_metadata),
        )
        .route(
            "/.well-known/oauth-authorization-server",
            axum::routing::get(oauth2_server::authorization_server_metadata),
        )
        // OAuth 2.1 endpoints
        .route(
            "/oauth/register",
            axum::routing::post(oauth2_server::register_client),
        )
        .route(
            "/oauth/authorize",
            axum::routing::get(oauth2_server::authorize_get)
                .post(oauth2_server::authorize_post),
        )
        .route(
            "/oauth/token",
            axum::routing::post(oauth2_server::token_endpoint),
        )
        .with_state(oauth2_state)
        // MCP endpoint (with auth middleware)
        .nest_service("/mcp", auth_mcp);

    let tcp_listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("Login page:   http://{addr}/");
    tracing::info!("MCP endpoint: http://{addr}/mcp (OAuth 2.1 or ?token=)");
    tracing::info!("OAuth metadata: http://{addr}/.well-known/oauth-authorization-server");

    axum::serve(tcp_listener, router).await?;

    Ok(())
}

/// Tower Service wrapper that extracts the auth token from the request,
/// resolves OAuth2 access tokens to session tokens, and sets the
/// CURRENT_USER_TOKEN task-local before delegating to the inner MCP service.
///
/// If no valid token is provided, returns HTTP 401 with a `WWW-Authenticate`
/// header pointing to the protected resource metadata (RFC 9728).
#[derive(Clone)]
struct AuthMcpService<S> {
    inner: S,
    sessions: Arc<SessionStore>,
    oauth2_store: Arc<OAuth2Store>,
    issuer: String,
}

impl<S, B> tower::Service<http::Request<B>> for AuthMcpService<S>
where
    S: tower::Service<http::Request<B>> + Clone + Send + 'static,
    S::Response: IntoMcpResponse + Send + 'static,
    S::Error: Into<Box<dyn std::error::Error + Send + Sync>> + Send + 'static,
    S::Future: Send + 'static,
    B: Send + 'static,
{
    type Response = http::Response<axum::body::Body>;
    type Error = std::convert::Infallible;
    type Future = std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Self::Response, Self::Error>> + Send>,
    >;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        // We always wrap errors ourselves, so we're always ready
        match self.inner.poll_ready(cx) {
            std::task::Poll::Ready(_) => std::task::Poll::Ready(Ok(())),
            std::task::Poll::Pending => std::task::Poll::Pending,
        }
    }

    fn call(&mut self, req: http::Request<B>) -> Self::Future {
        let bearer_token = login::extract_token(&req);
        let oauth2_store = self.oauth2_store.clone();
        let sessions = self.sessions.clone();
        let issuer = self.issuer.clone();
        let mut inner = self.inner.clone();
        std::mem::swap(&mut self.inner, &mut inner);

        Box::pin(async move {
            // Resolve the bearer token to a session token:
            // 1. Try as OAuth2 access token → get session_token
            // 2. Fall back to using it directly as a session token (legacy)
            // 3. If neither works, return 401
            let session_token = match bearer_token {
                Some(token) => {
                    // Try OAuth2 access token first
                    if let Ok(Some(stored)) = oauth2_store.get_token(&token) {
                        // Verify the underlying session still exists
                        if sessions.contains(&stored.session_token).await {
                            Some(stored.session_token)
                        } else {
                            None
                        }
                    } else if sessions.contains(&token).await {
                        // Legacy: direct session token
                        Some(token)
                    } else {
                        None
                    }
                }
                None => None,
            };

            match session_token {
                Some(st) => {
                    let result =
                        CURRENT_USER_TOKEN.scope(st, async move { inner.call(req).await }).await;
                    match result {
                        Ok(resp) => Ok(resp.into_mcp_response()),
                        Err(e) => {
                            let err_msg = format!("{}", e.into());
                            Ok(http::Response::builder()
                                .status(http::StatusCode::INTERNAL_SERVER_ERROR)
                                .body(axum::body::Body::from(err_msg))
                                .unwrap())
                        }
                    }
                }
                None => {
                    // Return 401 with WWW-Authenticate header (RFC 9728)
                    let resource_meta_url =
                        format!("{}/.well-known/oauth-protected-resource", issuer);
                    let www_auth = format!(
                        r#"Bearer resource_metadata="{}""#,
                        resource_meta_url
                    );
                    Ok(http::Response::builder()
                        .status(http::StatusCode::UNAUTHORIZED)
                        .header("WWW-Authenticate", www_auth)
                        .header("Content-Type", "application/json")
                        .body(axum::body::Body::from(
                            r#"{"error":"unauthorized","error_description":"Bearer token required. See WWW-Authenticate header for OAuth metadata."}"#,
                        ))
                        .unwrap())
                }
            }
        })
    }
}

/// Marker trait — we need S::Response to be convertible to http::Response<Body>.
trait IntoMcpResponse {
    fn into_mcp_response(self) -> http::Response<axum::body::Body>;
}

impl IntoMcpResponse for http::Response<axum::body::Body> {
    fn into_mcp_response(self) -> http::Response<axum::body::Body> {
        self
    }
}

impl IntoMcpResponse
    for http::Response<http_body_util::combinators::BoxBody<bytes::Bytes, std::convert::Infallible>>
{
    fn into_mcp_response(self) -> http::Response<axum::body::Body> {
        self.map(axum::body::Body::new)
    }
}
