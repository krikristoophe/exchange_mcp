//! Tower Service middleware for MCP authentication.
//!
//! Extracts the Bearer token from the request, resolves OAuth2 access tokens
//! to session tokens, and sets the CURRENT_USER_TOKEN task-local before
//! delegating to the inner MCP service.

use std::sync::Arc;

use crate::oauth::store::OAuth2Store;
use crate::CURRENT_USER_TOKEN;

/// Extract Bearer token from an HTTP request's Authorization header.
pub fn extract_bearer_token<B>(req: &http::Request<B>) -> Option<String> {
    let auth = req.headers().get("authorization")?;
    let val = auth.to_str().ok()?;
    val.strip_prefix("Bearer ").map(|t| t.to_string())
}

/// GET /favicon.ico — Return empty response to avoid 404
pub async fn favicon() -> impl axum::response::IntoResponse {
    let mut headers = axum::http::HeaderMap::new();
    headers.insert(
        axum::http::header::CONTENT_TYPE,
        "image/x-icon".parse().unwrap(),
    );
    (axum::http::StatusCode::OK, headers, &[] as &[u8])
}

/// Tower Service wrapper that extracts the Bearer token from the request,
/// resolves OAuth2 access tokens to session tokens, and sets the
/// CURRENT_USER_TOKEN task-local before delegating to the inner MCP service.
///
/// Returns HTTP 401 with `WWW-Authenticate` header (RFC 9728) when no valid token.
#[derive(Clone)]
pub struct AuthMcpService<S> {
    pub inner: S,
    pub oauth2_store: Arc<OAuth2Store>,
    pub issuer: String,
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
        let bearer_token = extract_bearer_token(&req);
        let oauth2_store = self.oauth2_store.clone();
        let issuer = self.issuer.clone();
        let mut inner = self.inner.clone();
        std::mem::swap(&mut self.inner, &mut inner);

        Box::pin(async move {
            // Resolve Bearer token -> OAuth2 access token -> session token
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
pub trait IntoMcpResponse {
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
