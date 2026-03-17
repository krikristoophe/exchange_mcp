mod auth;
mod config;
mod imap_client;
mod login;
mod oauth2_server;
mod oauth2_store;
mod server;
mod session;

use std::sync::Arc;

use anyhow::Result;
use tracing_subscriber::EnvFilter;

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

    // Wrap MCP service with auth middleware that:
    // 1. Extracts Bearer token from the Authorization header
    // 2. Resolves OAuth2 access tokens to session tokens
    // 3. Returns 401 with RFC 9728 metadata hint when no valid token
    let auth_mcp = AuthMcpService {
        inner: mcp_service,
        oauth2_store,
        issuer: issuer.clone(),
    };

    let router = axum::Router::new()
        .route("/favicon.ico", axum::routing::get(login::favicon))
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
    tracing::info!("MCP endpoint: http://{addr}/mcp (OAuth 2.1)");
    tracing::info!("OAuth metadata: http://{addr}/.well-known/oauth-authorization-server");

    axum::serve(tcp_listener, router).await?;

    Ok(())
}

/// Tower Service wrapper that extracts the Bearer token from the request,
/// resolves OAuth2 access tokens to session tokens, and sets the
/// CURRENT_USER_TOKEN task-local before delegating to the inner MCP service.
///
/// Returns HTTP 401 with `WWW-Authenticate` header (RFC 9728) when no valid token.
#[derive(Clone)]
struct AuthMcpService<S> {
    inner: S,
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
        match self.inner.poll_ready(cx) {
            std::task::Poll::Ready(_) => std::task::Poll::Ready(Ok(())),
            std::task::Poll::Pending => std::task::Poll::Pending,
        }
    }

    fn call(&mut self, req: http::Request<B>) -> Self::Future {
        let bearer_token = login::extract_bearer_token(&req);
        let oauth2_store = self.oauth2_store.clone();
        let issuer = self.issuer.clone();
        let mut inner = self.inner.clone();
        std::mem::swap(&mut self.inner, &mut inner);

        Box::pin(async move {
            // Resolve Bearer token → OAuth2 access token → session token
            let session_token = bearer_token.and_then(|token| {
                oauth2_store
                    .get_token(&token)
                    .ok()
                    .flatten()
                    .map(|stored| stored.session_token)
            });

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
                    // 401 with WWW-Authenticate header (RFC 9728)
                    let www_auth = format!(
                        r#"Bearer resource_metadata="{}/.well-known/oauth-protected-resource""#,
                        issuer
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

/// Converts inner service response to http::Response<axum::body::Body>.
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
