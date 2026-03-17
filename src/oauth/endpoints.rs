//! OAuth 2.1 HTTP handlers: metadata, registration, authorization, and token exchange.

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::{header, StatusCode};
use axum::response::{Html, IntoResponse, Redirect};
use axum::Json;
use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use subtle::ConstantTimeEq;

use crate::auth::{AuthProvider, BasicAuthProvider};
use crate::imap::ImapClient;
use crate::session::UserSession;
use super::store::{AuthCode, PersistedSession, RegisteredClient, StoredToken};
use super::OAuth2State;

/// Allowed redirect URI schemes. Blocks javascript:, data:, etc.
fn is_valid_redirect_scheme(uri: &str) -> bool {
    match url::Url::parse(uri) {
        Ok(u) => matches!(u.scheme(), "https" | "http"),
        Err(_) => false,
    }
}

/// Validate IMAP host to prevent SSRF attacks.
/// Blocks internal IPs, localhost, and non-hostname values.
fn is_safe_imap_host(host: &str) -> bool {
    // Block empty
    if host.is_empty() {
        return false;
    }
    // Block obvious internal hosts
    let lower = host.to_lowercase();
    if lower == "localhost"
        || lower == "127.0.0.1"
        || lower == "::1"
        || lower == "0.0.0.0"
        || lower.starts_with("10.")
        || lower.starts_with("192.168.")
        || lower.starts_with("172.16.")
        || lower.starts_with("172.17.")
        || lower.starts_with("172.18.")
        || lower.starts_with("172.19.")
        || lower.starts_with("172.2")
        || lower.starts_with("172.30.")
        || lower.starts_with("172.31.")
        || lower.starts_with("169.254.")
        || lower.starts_with("[")
        || lower.ends_with(".local")
        || lower.ends_with(".internal")
    {
        return false;
    }
    // Must contain at least one dot (looks like a hostname)
    if !host.contains('.') {
        return false;
    }
    true
}

/// Validate IMAP port (only standard IMAP ports or high ports).
fn is_valid_imap_port(port: u16) -> bool {
    matches!(port, 143 | 993 | 995 | 1024..=65535)
}

/// Basic email format validation (server-side).
fn is_valid_email(email: &str) -> bool {
    let parts: Vec<&str> = email.splitn(2, '@').collect();
    if parts.len() != 2 {
        return false;
    }
    let (local, domain) = (parts[0], parts[1]);
    !local.is_empty() && !domain.is_empty() && domain.contains('.')
}

/// Generate a CSRF token and store it in the OAuth2 store.
fn generate_csrf_token(store: &super::store::OAuth2Store) -> String {
    let token = random_token(32);
    let _ = store.store_csrf_token(&token);
    token
}

/// Verify and consume a CSRF token.
fn verify_csrf_token(store: &super::store::OAuth2Store, token: &str) -> bool {
    store.consume_csrf_token(token).unwrap_or(false)
}

// -- Helper: generate a random URL-safe token --

fn random_token(len: usize) -> String {
    use rand::Rng;
    let bytes: Vec<u8> = (0..len).map(|_| rand::rng().random::<u8>()).collect();
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&bytes)
}

// -- Protected Resource Metadata (RFC 9728) --

#[derive(Serialize)]
struct ProtectedResourceMetadata {
    resource: String,
    authorization_servers: Vec<String>,
    bearer_methods_supported: Vec<String>,
    scopes_supported: Vec<String>,
}

pub async fn protected_resource_metadata(
    State(state): State<Arc<OAuth2State>>,
) -> impl IntoResponse {
    let resource = format!("{}/mcp", state.issuer);
    let meta = ProtectedResourceMetadata {
        resource,
        authorization_servers: vec![state.issuer.clone()],
        bearer_methods_supported: vec!["header".to_string()],
        scopes_supported: vec!["email".to_string()],
    };
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        serde_json::to_string(&meta).unwrap(),
    )
}

// -- Authorization Server Metadata (RFC 8414) --

#[derive(Serialize)]
struct AuthServerMetadata {
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    registration_endpoint: String,
    revocation_endpoint: String,
    response_types_supported: Vec<String>,
    grant_types_supported: Vec<String>,
    token_endpoint_auth_methods_supported: Vec<String>,
    code_challenge_methods_supported: Vec<String>,
    scopes_supported: Vec<String>,
}

pub async fn authorization_server_metadata(
    State(state): State<Arc<OAuth2State>>,
) -> impl IntoResponse {
    let meta = AuthServerMetadata {
        issuer: state.issuer.clone(),
        authorization_endpoint: format!("{}/oauth/authorize", state.issuer),
        token_endpoint: format!("{}/oauth/token", state.issuer),
        registration_endpoint: format!("{}/oauth/register", state.issuer),
        revocation_endpoint: format!("{}/oauth/revoke", state.issuer),
        response_types_supported: vec!["code".to_string()],
        grant_types_supported: vec![
            "authorization_code".to_string(),
            "refresh_token".to_string(),
        ],
        token_endpoint_auth_methods_supported: vec![
            "client_secret_post".to_string(),
            "none".to_string(),
        ],
        code_challenge_methods_supported: vec!["S256".to_string()],
        scopes_supported: vec!["email".to_string()],
    };
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        serde_json::to_string(&meta).unwrap(),
    )
}

// -- Dynamic Client Registration (RFC 7591) --

#[derive(Deserialize)]
pub struct RegisterRequest {
    pub redirect_uris: Vec<String>,
    #[serde(default)]
    pub client_name: Option<String>,
    #[serde(default)]
    pub token_endpoint_auth_method: Option<String>,
}

#[derive(Serialize)]
pub struct RegisterResponse {
    pub client_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<String>,
    pub redirect_uris: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_name: Option<String>,
    pub grant_types: Vec<String>,
    pub response_types: Vec<String>,
    pub token_endpoint_auth_method: String,
}

pub async fn register_client(
    State(state): State<Arc<OAuth2State>>,
    Json(req): Json<RegisterRequest>,
) -> impl IntoResponse {
    if req.redirect_uris.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "invalid_client_metadata", "error_description": "redirect_uris is required"})),
        );
    }

    // Validate redirect URIs — must be valid URLs with http(s) scheme only
    for uri in &req.redirect_uris {
        if !is_valid_redirect_scheme(uri) {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "invalid_redirect_uri", "error_description": "Redirect URIs must use http or https scheme"})),
            );
        }
    }

    let client_id = format!("client_{}", random_token(24));
    let auth_method = req
        .token_endpoint_auth_method
        .as_deref()
        .unwrap_or("none");

    let client_secret = if auth_method == "client_secret_post" || auth_method == "client_secret_basic" {
        Some(random_token(32))
    } else {
        None
    };

    let client = RegisteredClient {
        client_id: client_id.clone(),
        client_secret: client_secret.clone(),
        redirect_uris: req.redirect_uris.clone(),
        client_name: req.client_name.clone(),
    };

    if let Err(e) = state.store.register_client(&client) {
        tracing::error!("Failed to register client: {e}");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": "server_error"})),
        );
    }

    tracing::info!("Registered OAuth client: {client_id}");

    (
        StatusCode::CREATED,
        Json(serde_json::json!(RegisterResponse {
            client_id,
            client_secret,
            redirect_uris: req.redirect_uris,
            client_name: req.client_name,
            grant_types: vec!["authorization_code".to_string(), "refresh_token".to_string()],
            response_types: vec!["code".to_string()],
            token_endpoint_auth_method: auth_method.to_string(),
        })),
    )
}

// -- Authorization endpoint --

#[derive(Deserialize)]
pub struct AuthorizeParams {
    pub response_type: String,
    pub client_id: String,
    pub redirect_uri: String,
    #[serde(default)]
    pub state: Option<String>,
    pub code_challenge: String,
    #[serde(default = "default_s256")]
    pub code_challenge_method: String,
}

fn default_s256() -> String {
    "S256".to_string()
}

/// GET /oauth/authorize — Show the login form
pub async fn authorize_get(
    State(state): State<Arc<OAuth2State>>,
    Query(params): Query<AuthorizeParams>,
) -> impl IntoResponse {
    // Validate basics
    if params.response_type != "code" {
        return error_redirect(
            &params.redirect_uri,
            "unsupported_response_type",
            "Only 'code' is supported",
            params.state.as_deref(),
        )
        .into_response();
    }

    if params.code_challenge_method != "S256" {
        return error_redirect(
            &params.redirect_uri,
            "invalid_request",
            "Only S256 code_challenge_method is supported",
            params.state.as_deref(),
        )
        .into_response();
    }

    // Verify client exists
    match state.store.get_client(&params.client_id) {
        Ok(Some(client)) => {
            if !client.redirect_uris.contains(&params.redirect_uri) {
                return (
                    StatusCode::BAD_REQUEST,
                    "redirect_uri does not match registered URIs",
                )
                    .into_response();
            }
        }
        Ok(None) => {
            return (StatusCode::BAD_REQUEST, "Unknown client_id").into_response();
        }
        Err(_) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, "Database error").into_response();
        }
    }

    // Generate CSRF token for form protection
    let csrf_token = generate_csrf_token(&state.store);

    // Render login page with hidden OAuth params
    let html = authorize_html(
        &params.client_id,
        &params.redirect_uri,
        &params.code_challenge,
        &params.code_challenge_method,
        params.state.as_deref().unwrap_or(""),
        &csrf_token,
        None,
    );
    Html(html).into_response()
}

/// POST /oauth/authorize — Process the login form and redirect with auth code
pub async fn authorize_post(
    State(state): State<Arc<OAuth2State>>,
    axum::extract::Form(form): axum::extract::Form<AuthorizeFormData>,
) -> impl IntoResponse {
    // Verify CSRF token
    if !verify_csrf_token(&state.store, &form.csrf_token) {
        return (StatusCode::BAD_REQUEST, "Invalid or expired form submission. Please try again.").into_response();
    }

    // Validate client
    let client = match state.store.get_client(&form.client_id) {
        Ok(Some(c)) => c,
        _ => {
            return (StatusCode::BAD_REQUEST, "Unknown client_id").into_response();
        }
    };

    if !client.redirect_uris.contains(&form.redirect_uri) {
        return (StatusCode::BAD_REQUEST, "Invalid redirect_uri").into_response();
    }

    // Validate email format server-side
    if !is_valid_email(&form.email) {
        let csrf_token = generate_csrf_token(&state.store);
        let html = authorize_html(
            &form.client_id, &form.redirect_uri, &form.code_challenge,
            &form.code_challenge_method, &form.state, &csrf_token,
            Some("Adresse email invalide"),
        );
        return Html(html).into_response();
    }

    // Test IMAP credentials — use configured defaults (env/config) when not provided
    let email = form.email.clone();
    let password = form.password.clone();
    let imap_host = if form.imap_host.is_empty() {
        state.default_imap_host.clone()
    } else {
        form.imap_host.clone()
    };
    let imap_port = if form.imap_port == 0 { state.default_imap_port } else { form.imap_port };

    // Validate IMAP host (SSRF protection) — only for user-supplied hosts
    if !form.imap_host.is_empty() && !is_safe_imap_host(&imap_host) {
        let csrf_token = generate_csrf_token(&state.store);
        let html = authorize_html(
            &form.client_id, &form.redirect_uri, &form.code_challenge,
            &form.code_challenge_method, &form.state, &csrf_token,
            Some("Serveur IMAP non autorise"),
        );
        return Html(html).into_response();
    }

    // Validate IMAP port
    if form.imap_port != 0 && !is_valid_imap_port(imap_port) {
        let csrf_token = generate_csrf_token(&state.store);
        let html = authorize_html(
            &form.client_id, &form.redirect_uri, &form.code_challenge,
            &form.code_challenge_method, &form.state, &csrf_token,
            Some("Port IMAP non autorise"),
        );
        return Html(html).into_response();
    }

    let host = imap_host.clone();
    let port = imap_port;
    let test_user = email.clone();
    let test_pass = password.clone();

    let test_result = tokio::task::spawn_blocking(move || {
        let tls = native_tls::TlsConnector::new()?;
        let client = imap::connect((&*host, port), &host, &tls)?;
        let mut session = client.login(&test_user, &test_pass).map_err(|(e, _)| e)?;
        session.logout()?;
        Ok::<(), anyhow::Error>(())
    })
    .await;

    match test_result {
        Ok(Ok(())) => {
            // IMAP OK — create session
            let auth: Arc<dyn AuthProvider> = Arc::new(BasicAuthProvider::new(
                email.clone(),
                password.clone(),
            ));
            let imap_client = Arc::new(ImapClient::new(auth, imap_host.clone(), imap_port));
            let session_token = random_token(32);

            state
                .sessions
                .insert(
                    session_token.clone(),
                    UserSession {
                        email: email.clone(),
                        imap: imap_client,
                        imap_host: imap_host.clone(),
                        imap_port,
                        last_activity: chrono::Utc::now().timestamp(),
                    },
                );

            // Persist session to SQLite for restart survival
            if let Err(e) = state.store.persist_session(&PersistedSession {
                session_token: session_token.clone(),
                email: email.clone(),
                password: password.clone(),
                imap_host,
                imap_port,
            }) {
                tracing::warn!("Failed to persist session: {e}");
            }

            // Generate auth code
            let code = random_token(32);
            let expires_at = chrono::Utc::now().timestamp() + 600; // 10 minutes

            let auth_code = AuthCode {
                code: code.clone(),
                client_id: form.client_id.clone(),
                redirect_uri: form.redirect_uri.clone(),
                code_challenge: form.code_challenge.clone(),
                code_challenge_method: form.code_challenge_method.clone(),
                session_token,
                expires_at,
            };

            if let Err(e) = state.store.store_auth_code(&auth_code) {
                tracing::error!("Failed to store auth code: {e}");
                return (StatusCode::INTERNAL_SERVER_ERROR, "Server error").into_response();
            }

            tracing::info!("OAuth authorize success for {email}, redirecting with code");

            // Redirect back to client with code
            let mut redirect_url = form.redirect_uri.clone();
            redirect_url.push_str(if redirect_url.contains('?') { "&" } else { "?" });
            redirect_url.push_str(&format!("code={}", urlencod(&code)));
            if !form.state.is_empty() {
                redirect_url.push_str(&format!("&state={}", urlencod(&form.state)));
            }

            Redirect::to(&redirect_url).into_response()
        }
        Ok(Err(e)) => {
            // Log detailed error server-side only — do not leak to user
            tracing::warn!("OAuth authorize IMAP login failed for {}: {e}", form.email);
            let csrf_token = generate_csrf_token(&state.store);
            let html = authorize_html(
                &form.client_id,
                &form.redirect_uri,
                &form.code_challenge,
                &form.code_challenge_method,
                &form.state,
                &csrf_token,
                Some("Echec de connexion : identifiants incorrects ou serveur injoignable"),
            );
            Html(html).into_response()
        }
        Err(e) => {
            tracing::error!("OAuth authorize task panicked: {e}");
            (StatusCode::INTERNAL_SERVER_ERROR, "Server error").into_response()
        }
    }
}

#[derive(Deserialize)]
pub struct AuthorizeFormData {
    pub email: String,
    pub password: String,
    #[serde(default)]
    pub imap_host: String,
    #[serde(default)]
    pub imap_port: u16,
    // OAuth params passed through hidden fields
    pub client_id: String,
    pub redirect_uri: String,
    pub code_challenge: String,
    pub code_challenge_method: String,
    #[serde(default)]
    pub state: String,
    /// CSRF token for form submission protection
    #[serde(default)]
    pub csrf_token: String,
}

// -- Token endpoint --

#[derive(Deserialize)]
pub struct TokenRequest {
    pub grant_type: String,
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub redirect_uri: Option<String>,
    #[serde(default)]
    pub code_verifier: Option<String>,
    #[serde(default)]
    pub client_id: Option<String>,
    #[serde(default)]
    pub client_secret: Option<String>,
    #[serde(default)]
    pub refresh_token: Option<String>,
}

#[derive(Serialize)]
struct TokenResponse {
    access_token: String,
    token_type: String,
    expires_in: i64,
    refresh_token: String,
    scope: String,
}

pub async fn token_endpoint(
    State(state): State<Arc<OAuth2State>>,
    axum::extract::Form(req): axum::extract::Form<TokenRequest>,
) -> impl IntoResponse {
    match req.grant_type.as_str() {
        "authorization_code" => handle_code_exchange(state, req).await,
        "refresh_token" => handle_refresh(state, req).await,
        _ => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "unsupported_grant_type"})),
        )
            .into_response(),
    }
}

async fn handle_code_exchange(
    state: Arc<OAuth2State>,
    req: TokenRequest,
) -> axum::response::Response {
    let code = match &req.code {
        Some(c) => c,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "invalid_request", "error_description": "code is required"})),
            )
                .into_response();
        }
    };

    let code_verifier = match &req.code_verifier {
        Some(v) => v,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "invalid_request", "error_description": "code_verifier is required"})),
            )
                .into_response();
        }
    };

    let client_id = match &req.client_id {
        Some(c) => c,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "invalid_request", "error_description": "client_id is required"})),
            )
                .into_response();
        }
    };

    // Consume auth code
    let auth_code = match state.store.consume_auth_code(code) {
        Ok(Some(ac)) => ac,
        Ok(None) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "invalid_grant", "error_description": "Invalid, expired, or already-used authorization code"})),
            )
                .into_response();
        }
        Err(e) => {
            tracing::error!("consume_auth_code error: {e}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "server_error"})),
            )
                .into_response();
        }
    };

    // Verify client_id matches
    if auth_code.client_id != *client_id {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "invalid_grant", "error_description": "client_id mismatch"})),
        )
            .into_response();
    }

    // Verify redirect_uri matches
    if let Some(redirect_uri) = &req.redirect_uri {
        if auth_code.redirect_uri != *redirect_uri {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "invalid_grant", "error_description": "redirect_uri mismatch"})),
            )
                .into_response();
        }
    }

    // Verify PKCE: S256(code_verifier) == code_challenge
    if !verify_pkce(
        code_verifier,
        &auth_code.code_challenge,
        &auth_code.code_challenge_method,
    ) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "invalid_grant", "error_description": "PKCE verification failed"})),
        )
            .into_response();
    }

    // Verify client secret if applicable — constant-time comparison
    if let Some(secret) = &req.client_secret {
        if let Ok(Some(client)) = state.store.get_client(client_id) {
            if let Some(expected) = &client.client_secret {
                if secret.as_bytes().ct_eq(expected.as_bytes()).unwrap_u8() != 1 {
                    return (
                        StatusCode::UNAUTHORIZED,
                        Json(serde_json::json!({"error": "invalid_client"})),
                    )
                        .into_response();
                }
            }
        }
    }

    // Issue tokens
    let access_token = random_token(32);
    let refresh_token = random_token(32);
    let expires_in: i64 = 3600; // 1 hour
    let expires_at = chrono::Utc::now().timestamp() + expires_in;

    let stored = StoredToken {
        access_token: access_token.clone(),
        refresh_token: refresh_token.clone(),
        client_id: client_id.clone(),
        session_token: auth_code.session_token,
        expires_at,
    };

    if let Err(e) = state.store.store_token(&stored) {
        tracing::error!("Failed to store token: {e}");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": "server_error"})),
        )
            .into_response();
    }

    tracing::info!("Issued OAuth tokens for client {client_id}");

    let resp = TokenResponse {
        access_token,
        token_type: "Bearer".to_string(),
        expires_in,
        refresh_token,
        scope: "email".to_string(),
    };

    (StatusCode::OK, Json(serde_json::json!(resp))).into_response()
}

async fn handle_refresh(state: Arc<OAuth2State>, req: TokenRequest) -> axum::response::Response {
    let refresh_token = match &req.refresh_token {
        Some(rt) => rt,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "invalid_request", "error_description": "refresh_token is required"})),
            )
                .into_response();
        }
    };

    let old_token = match state.store.get_by_refresh_token(refresh_token) {
        Ok(Some(t)) => t,
        Ok(None) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "invalid_grant", "error_description": "Invalid refresh token"})),
            )
                .into_response();
        }
        Err(e) => {
            tracing::error!("get_by_refresh_token error: {e}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "server_error"})),
            )
                .into_response();
        }
    };

    // Verify session still exists
    if !state.sessions.contains(&old_token.session_token) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "invalid_grant", "error_description": "Session expired"})),
        )
            .into_response();
    }

    // Rotate tokens
    let _ = state.store.delete_token(&old_token.access_token);

    let new_access = random_token(32);
    let new_refresh = random_token(32);
    let expires_in: i64 = 3600;
    let expires_at = chrono::Utc::now().timestamp() + expires_in;

    let stored = StoredToken {
        access_token: new_access.clone(),
        refresh_token: new_refresh.clone(),
        client_id: old_token.client_id.clone(),
        session_token: old_token.session_token,
        expires_at,
    };

    if let Err(e) = state.store.store_token(&stored) {
        tracing::error!("Failed to store refreshed token: {e}");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": "server_error"})),
        )
            .into_response();
    }

    tracing::info!("Refreshed OAuth tokens for client {}", old_token.client_id);

    let resp = TokenResponse {
        access_token: new_access,
        token_type: "Bearer".to_string(),
        expires_in,
        refresh_token: new_refresh,
        scope: "email".to_string(),
    };

    (StatusCode::OK, Json(serde_json::json!(resp))).into_response()
}

// -- PKCE verification --

fn verify_pkce(code_verifier: &str, code_challenge: &str, method: &str) -> bool {
    match method {
        "S256" => {
            let mut hasher = Sha256::new();
            hasher.update(code_verifier.as_bytes());
            let digest = hasher.finalize();
            let computed = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest);
            // Constant-time comparison to prevent timing attacks
            computed.as_bytes().ct_eq(code_challenge.as_bytes()).unwrap_u8() == 1
        }
        _ => false,
    }
}

// -- Helpers --

fn urlencod(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

fn error_redirect(
    redirect_uri: &str,
    error: &str,
    description: &str,
    state: Option<&str>,
) -> Redirect {
    let mut url = redirect_uri.to_string();
    url.push_str(if url.contains('?') { "&" } else { "?" });
    url.push_str(&format!(
        "error={}&error_description={}",
        urlencod(error),
        urlencod(description)
    ));
    if let Some(s) = state {
        url.push_str(&format!("&state={}", urlencod(s)));
    }
    Redirect::to(&url)
}

// -- Authorize HTML form --

fn authorize_html(
    client_id: &str,
    redirect_uri: &str,
    code_challenge: &str,
    code_challenge_method: &str,
    state: &str,
    csrf_token: &str,
    error: Option<&str>,
) -> String {
    let error_html = match error {
        Some(msg) => format!(
            r#"<div class="status error">{}</div>"#,
            msg.replace('<', "&lt;").replace('>', "&gt;")
        ),
        None => String::new(),
    };

    format!(
        r##"<!DOCTYPE html>
<html lang="fr">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>Exchange MCP — Autorisation</title>
    <style>
        * {{ box-sizing: border-box; margin: 0; padding: 0; }}
        body {{
            font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif;
            background: #0f172a;
            color: #e2e8f0;
            min-height: 100vh;
            display: flex;
            align-items: center;
            justify-content: center;
        }}
        .container {{ width: 100%; max-width: 520px; padding: 2rem; }}
        .card {{
            background: #1e293b;
            border: 1px solid #334155;
            border-radius: 12px;
            padding: 2rem;
            box-shadow: 0 4px 24px rgba(0,0,0,0.3);
        }}
        .logo {{ text-align: center; margin-bottom: 1.5rem; }}
        .logo h1 {{ font-size: 1.5rem; font-weight: 600; color: #f1f5f9; }}
        .logo p {{ font-size: 0.875rem; color: #94a3b8; margin-top: 0.25rem; }}
        .form-group {{ margin-bottom: 1rem; }}
        label {{
            display: block; font-size: 0.875rem; font-weight: 500;
            color: #cbd5e1; margin-bottom: 0.375rem;
        }}
        input {{
            width: 100%; padding: 0.625rem 0.75rem;
            background: #0f172a; border: 1px solid #475569;
            border-radius: 8px; color: #f1f5f9; font-size: 0.9375rem;
            transition: border-color 0.2s;
        }}
        input:focus {{
            outline: none; border-color: #3b82f6;
            box-shadow: 0 0 0 3px rgba(59,130,246,0.15);
        }}
        input::placeholder {{ color: #64748b; }}
        .advanced-toggle {{
            font-size: 0.8125rem; color: #64748b; cursor: pointer;
            user-select: none; margin-bottom: 1rem;
            display: flex; align-items: center; gap: 0.375rem;
        }}
        .advanced-toggle:hover {{ color: #94a3b8; }}
        .advanced-toggle .arrow {{ transition: transform 0.2s; display: inline-block; }}
        .advanced-toggle .arrow.open {{ transform: rotate(90deg); }}
        .advanced-fields {{ display: none; }}
        .advanced-fields.open {{ display: block; }}
        button {{
            width: 100%; padding: 0.75rem;
            background: #3b82f6; color: white; border: none;
            border-radius: 8px; font-size: 1rem; font-weight: 500;
            cursor: pointer; transition: background 0.2s;
        }}
        button:hover {{ background: #2563eb; }}
        button:disabled {{ background: #475569; cursor: not-allowed; }}
        .status {{
            margin-bottom: 1rem; padding: 0.75rem; border-radius: 8px;
            font-size: 0.875rem; line-height: 1.4;
        }}
        .status.error {{
            background: #450a0a; border: 1px solid #dc2626; color: #fca5a5;
        }}
        .info {{
            margin-top: 1rem; padding: 0.75rem; border-radius: 8px;
            background: #172554; border: 1px solid #1e40af; color: #93c5fd;
            font-size: 0.8125rem;
        }}
    </style>
</head>
<body>
    <div class="container">
        <div class="card">
            <div class="logo">
                <h1>Exchange MCP</h1>
                <p>Connectez-vous pour autoriser l'acces a votre messagerie</p>
            </div>
            {error_html}
            <form method="POST" action="/oauth/authorize">
                <input type="hidden" name="client_id" value="{client_id}">
                <input type="hidden" name="redirect_uri" value="{redirect_uri}">
                <input type="hidden" name="code_challenge" value="{code_challenge}">
                <input type="hidden" name="code_challenge_method" value="{code_challenge_method}">
                <input type="hidden" name="state" value="{state}">
                <input type="hidden" name="csrf_token" value="{csrf_token}">
                <div class="form-group">
                    <label for="email">Adresse email</label>
                    <input type="email" id="email" name="email" placeholder="prenom.nom@entreprise.com" required autocomplete="email">
                </div>
                <div class="form-group">
                    <label for="password">Mot de passe</label>
                    <input type="password" id="password" name="password" placeholder="Mot de passe" required autocomplete="current-password">
                </div>
                <div class="advanced-toggle" onclick="document.getElementById('advFields').classList.toggle('open'); document.getElementById('advArrow').classList.toggle('open');">
                    <span class="arrow" id="advArrow">&#9654;</span> Parametres avances
                </div>
                <div class="advanced-fields" id="advFields">
                    <div class="form-group">
                        <label for="imap_host">Serveur IMAP</label>
                        <input type="text" id="imap_host" name="imap_host" placeholder="outlook.office365.com">
                    </div>
                    <div class="form-group">
                        <label for="imap_port">Port IMAP</label>
                        <input type="number" id="imap_port" name="imap_port" placeholder="993" value="0">
                    </div>
                </div>
                <button type="submit">Autoriser l'acces</button>
            </form>
            <div class="info">
                Une application demande l'acces a votre messagerie via le protocole MCP.
                Vos identifiants sont utilises uniquement pour etablir la connexion IMAP.
            </div>
        </div>
    </div>
</body>
</html>"##,
        error_html = error_html,
        client_id = client_id.replace('"', "&quot;"),
        redirect_uri = redirect_uri.replace('"', "&quot;"),
        code_challenge = code_challenge.replace('"', "&quot;"),
        code_challenge_method = code_challenge_method.replace('"', "&quot;"),
        state = state.replace('"', "&quot;"),
        csrf_token = csrf_token.replace('"', "&quot;"),
    )
}

// -- Token Revocation (RFC 7009) --

#[derive(Deserialize)]
pub struct RevokeRequest {
    pub token: String,
    #[serde(default)]
    pub token_type_hint: Option<String>,
}

/// POST /oauth/revoke — Revoke an access or refresh token
pub async fn revoke_token(
    State(state): State<Arc<OAuth2State>>,
    axum::extract::Form(req): axum::extract::Form<RevokeRequest>,
) -> impl IntoResponse {
    // Try as access token first
    let deleted = match req.token_type_hint.as_deref() {
        Some("refresh_token") => {
            // Try refresh token first
            if let Ok(Some(stored)) = state.store.get_by_refresh_token(&req.token) {
                let _ = state.store.delete_token(&stored.access_token);
                true
            } else if let Ok(Some(_)) = state.store.get_token(&req.token) {
                let _ = state.store.delete_token(&req.token);
                true
            } else {
                false
            }
        }
        _ => {
            // Try access token first
            if let Ok(Some(_)) = state.store.get_token(&req.token) {
                let _ = state.store.delete_token(&req.token);
                true
            } else if let Ok(Some(stored)) = state.store.get_by_refresh_token(&req.token) {
                let _ = state.store.delete_token(&stored.access_token);
                true
            } else {
                false
            }
        }
    };

    if deleted {
        tracing::info!("Token revoked successfully");
    }

    // RFC 7009: always return 200, even if token was not found
    StatusCode::OK
}
