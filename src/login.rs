use std::sync::Arc;

use axum::{
    Json,
    extract::State,
    http::StatusCode,
    response::{Html, IntoResponse},
};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::auth::{AuthProvider, BasicAuthProvider};
use crate::config::Config;
use crate::imap_client::ImapClient;
use crate::session::{SessionStore, UserSession};

/// Shared application state for the multi-user HTTP server.
pub struct AppState {
    pub sessions: Arc<SessionStore>,
    pub config: RwLock<Config>,
}

#[derive(Deserialize)]
pub struct LoginRequest {
    pub email: String,
    pub password: String,
    #[serde(default = "default_imap_host")]
    pub imap_host: String,
    #[serde(default = "default_imap_port")]
    pub imap_port: u16,
}

fn default_imap_host() -> String {
    "outlook.office365.com".to_string()
}

fn default_imap_port() -> u16 {
    993
}

#[derive(Serialize)]
pub struct LoginResponse {
    pub success: bool,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mcp_url: Option<String>,
}

#[derive(Serialize)]
pub struct StatusResponse {
    pub authenticated: bool,
    pub email: Option<String>,
}

#[derive(Deserialize)]
pub struct LogoutRequest {
    pub token: String,
}

#[derive(Serialize)]
pub struct LogoutResponse {
    pub success: bool,
    pub message: String,
}

#[derive(Serialize)]
pub struct SessionInfo {
    pub token: String,
    pub email: String,
}

/// GET / — Serve the login page
pub async fn login_page() -> impl IntoResponse {
    Html(LOGIN_HTML)
}

/// GET /api/status?token=xxx — Check authentication status for a given token
pub async fn api_status(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Json<StatusResponse> {
    if let Some(token) = params.get("token") {
        if let Some(email) = state.sessions.get_email(token).await {
            return Json(StatusResponse {
                authenticated: true,
                email: Some(email),
            });
        }
    }
    Json(StatusResponse {
        authenticated: false,
        email: None,
    })
}

/// GET /api/sessions — List all active sessions
pub async fn api_sessions(
    State(state): State<Arc<AppState>>,
) -> Json<Vec<SessionInfo>> {
    let sessions = state.sessions.list().await;
    Json(
        sessions
            .into_iter()
            .map(|(token, email)| SessionInfo {
                token: format!("{}...{}", &token[..8], &token[token.len() - 4..]),
                email,
            })
            .collect(),
    )
}

/// POST /api/login — Test credentials and create a user session
pub async fn api_login(
    State(state): State<Arc<AppState>>,
    Json(req): Json<LoginRequest>,
) -> (StatusCode, Json<LoginResponse>) {
    if req.email.is_empty() || req.password.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(LoginResponse {
                success: false,
                message: "Email et mot de passe requis.".to_string(),
                token: None,
                mcp_url: None,
            }),
        );
    }

    let imap_host = if req.imap_host.is_empty() {
        default_imap_host()
    } else {
        req.imap_host.clone()
    };
    let imap_port = if req.imap_port == 0 {
        default_imap_port()
    } else {
        req.imap_port
    };

    // Test IMAP connection
    let username = req.email.clone();
    let password = req.password.clone();
    let host = imap_host.clone();
    let port = imap_port;

    let test_result = tokio::task::spawn_blocking(move || {
        let tls = native_tls::TlsConnector::new()?;
        let client = imap::connect((&*host, port), &host, &tls)?;
        let mut session = client.login(&username, &password).map_err(|(e, _)| e)?;
        session.logout()?;
        Ok::<(), anyhow::Error>(())
    })
    .await;

    match test_result {
        Ok(Ok(())) => {
            // Connection successful — create user session
            let auth: Arc<dyn AuthProvider> = Arc::new(BasicAuthProvider::new(
                req.email.clone(),
                req.password.clone(),
                req.email.clone(),
            ));

            let imap_client = Arc::new(ImapClient::new(auth, imap_host.clone(), imap_port));

            let token = uuid::Uuid::new_v4().to_string();

            state
                .sessions
                .insert(
                    token.clone(),
                    UserSession {
                        email: req.email.clone(),
                        imap: imap_client,
                        imap_host,
                        imap_port,
                    },
                )
                .await;

            // Build the MCP URL
            let config = state.config.read().await;
            let base_host = if config.sse_host == "0.0.0.0" {
                "YOUR_SERVER_HOST".to_string()
            } else {
                config.sse_host.clone()
            };
            let mcp_url = format!(
                "http://{}:{}/mcp?token={}",
                base_host, config.sse_port, token
            );

            tracing::info!("User {} authenticated, token: {}...{}", req.email, &token[..8], &token[token.len()-4..]);

            (
                StatusCode::OK,
                Json(LoginResponse {
                    success: true,
                    message: format!(
                        "Connexion réussie pour {}.",
                        req.email
                    ),
                    token: Some(token),
                    mcp_url: Some(mcp_url),
                }),
            )
        }
        Ok(Err(e)) => {
            tracing::warn!("Login failed for {}: {e}", req.email);
            (
                StatusCode::UNAUTHORIZED,
                Json(LoginResponse {
                    success: false,
                    message: format!("Échec de connexion : {e}"),
                    token: None,
                    mcp_url: None,
                }),
            )
        }
        Err(e) => {
            tracing::error!("Login task panicked: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(LoginResponse {
                    success: false,
                    message: "Erreur interne du serveur.".to_string(),
                    token: None,
                    mcp_url: None,
                }),
            )
        }
    }
}

/// POST /api/logout — Remove a user session
pub async fn api_logout(
    State(state): State<Arc<AppState>>,
    Json(req): Json<LogoutRequest>,
) -> Json<LogoutResponse> {
    if let Some(session) = state.sessions.remove(&req.token).await {
        tracing::info!("User {} logged out", session.email);
        Json(LogoutResponse {
            success: true,
            message: "Déconnexion réussie.".to_string(),
        })
    } else {
        Json(LogoutResponse {
            success: false,
            message: "Session introuvable.".to_string(),
        })
    }
}

/// GET /favicon.ico — Return empty response to avoid 404
pub async fn favicon() -> impl IntoResponse {
    let mut headers = axum::http::HeaderMap::new();
    headers.insert(
        axum::http::header::CONTENT_TYPE,
        "image/x-icon".parse().unwrap(),
    );
    (StatusCode::OK, headers, &[] as &[u8])
}

/// Extract authentication token from an HTTP request (query param or Authorization header).
pub fn extract_token<B>(req: &http::Request<B>) -> Option<String> {
    // 1. Try Authorization: Bearer <token>
    if let Some(auth) = req.headers().get("authorization") {
        if let Ok(val) = auth.to_str() {
            if let Some(token) = val.strip_prefix("Bearer ") {
                return Some(token.to_string());
            }
        }
    }

    // 2. Try query parameter ?token=<token>
    if let Some(query) = req.uri().query() {
        for pair in query.split('&') {
            if let Some(token) = pair.strip_prefix("token=") {
                return Some(token.to_string());
            }
        }
    }

    None
}

const LOGIN_HTML: &str = r##"<!DOCTYPE html>
<html lang="fr">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>Exchange MCP — Connexion</title>
    <style>
        * { box-sizing: border-box; margin: 0; padding: 0; }
        body {
            font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif;
            background: #0f172a;
            color: #e2e8f0;
            min-height: 100vh;
            display: flex;
            align-items: center;
            justify-content: center;
        }
        .container {
            width: 100%;
            max-width: 520px;
            padding: 2rem;
        }
        .card {
            background: #1e293b;
            border: 1px solid #334155;
            border-radius: 12px;
            padding: 2rem;
            box-shadow: 0 4px 24px rgba(0,0,0,0.3);
        }
        .logo {
            text-align: center;
            margin-bottom: 1.5rem;
        }
        .logo h1 {
            font-size: 1.5rem;
            font-weight: 600;
            color: #f1f5f9;
        }
        .logo p {
            font-size: 0.875rem;
            color: #94a3b8;
            margin-top: 0.25rem;
        }
        .form-group {
            margin-bottom: 1rem;
        }
        label {
            display: block;
            font-size: 0.875rem;
            font-weight: 500;
            color: #cbd5e1;
            margin-bottom: 0.375rem;
        }
        input {
            width: 100%;
            padding: 0.625rem 0.75rem;
            background: #0f172a;
            border: 1px solid #475569;
            border-radius: 8px;
            color: #f1f5f9;
            font-size: 0.9375rem;
            transition: border-color 0.2s;
        }
        input:focus {
            outline: none;
            border-color: #3b82f6;
            box-shadow: 0 0 0 3px rgba(59,130,246,0.15);
        }
        input::placeholder { color: #64748b; }
        .advanced-toggle {
            font-size: 0.8125rem;
            color: #64748b;
            cursor: pointer;
            user-select: none;
            margin-bottom: 1rem;
            display: flex;
            align-items: center;
            gap: 0.375rem;
        }
        .advanced-toggle:hover { color: #94a3b8; }
        .advanced-toggle .arrow { transition: transform 0.2s; display: inline-block; }
        .advanced-toggle .arrow.open { transform: rotate(90deg); }
        .advanced-fields { display: none; }
        .advanced-fields.open { display: block; }
        button {
            width: 100%;
            padding: 0.75rem;
            background: #3b82f6;
            color: white;
            border: none;
            border-radius: 8px;
            font-size: 1rem;
            font-weight: 500;
            cursor: pointer;
            transition: background 0.2s;
        }
        button:hover { background: #2563eb; }
        button:disabled {
            background: #475569;
            cursor: not-allowed;
        }
        .btn-secondary {
            background: #475569;
            margin-top: 0.5rem;
        }
        .btn-secondary:hover { background: #64748b; }
        .btn-danger {
            background: #dc2626;
            margin-top: 0.5rem;
        }
        .btn-danger:hover { background: #b91c1c; }
        .status {
            margin-top: 1rem;
            padding: 0.75rem;
            border-radius: 8px;
            font-size: 0.875rem;
            line-height: 1.4;
        }
        .status.success {
            background: #064e3b;
            border: 1px solid #059669;
            color: #6ee7b7;
        }
        .status.error {
            background: #450a0a;
            border: 1px solid #dc2626;
            color: #fca5a5;
        }
        .status code {
            background: rgba(255,255,255,0.1);
            padding: 0.125rem 0.375rem;
            border-radius: 4px;
            font-size: 0.8125rem;
        }
        .token-box {
            margin-top: 1rem;
            padding: 1rem;
            background: #0f172a;
            border: 1px solid #334155;
            border-radius: 8px;
        }
        .token-box h3 {
            font-size: 0.875rem;
            color: #94a3b8;
            margin-bottom: 0.5rem;
        }
        .token-box .url-field {
            display: flex;
            gap: 0.5rem;
            margin-bottom: 0.75rem;
        }
        .token-box input[readonly] {
            flex: 1;
            background: #1e293b;
            border-color: #475569;
            font-family: monospace;
            font-size: 0.8125rem;
        }
        .copy-btn {
            width: auto;
            padding: 0.625rem 1rem;
            font-size: 0.8125rem;
        }
        .spinner {
            display: inline-block;
            width: 1rem;
            height: 1rem;
            border: 2px solid rgba(255,255,255,0.3);
            border-top-color: white;
            border-radius: 50%;
            animation: spin 0.6s linear infinite;
            vertical-align: middle;
            margin-right: 0.5rem;
        }
        @keyframes spin { to { transform: rotate(360deg); } }
        .sessions-list {
            margin-top: 1.5rem;
            padding-top: 1rem;
            border-top: 1px solid #334155;
        }
        .sessions-list h3 {
            font-size: 0.875rem;
            color: #94a3b8;
            margin-bottom: 0.5rem;
        }
        .session-item {
            display: flex;
            justify-content: space-between;
            align-items: center;
            padding: 0.5rem 0;
            font-size: 0.8125rem;
            color: #cbd5e1;
            border-bottom: 1px solid #1e293b;
        }
        .session-item code {
            color: #64748b;
            font-size: 0.75rem;
        }
    </style>
</head>
<body>
    <div class="container">
        <div class="card">
            <div class="logo">
                <h1>Exchange MCP</h1>
                <p>Connexion multi-utilisateur au serveur de messagerie</p>
            </div>

            <form id="loginForm">
                <div class="form-group">
                    <label for="email">Adresse email</label>
                    <input type="email" id="email" name="email" placeholder="prenom.nom@entreprise.com" required autocomplete="email">
                </div>
                <div class="form-group">
                    <label for="password">Mot de passe</label>
                    <input type="password" id="password" name="password" placeholder="Mot de passe" required autocomplete="current-password">
                </div>

                <div class="advanced-toggle" onclick="toggleAdvanced()">
                    <span class="arrow" id="advArrow">&#9654;</span> Parametres avances
                </div>
                <div class="advanced-fields" id="advFields">
                    <div class="form-group">
                        <label for="imap_host">Serveur IMAP</label>
                        <input type="text" id="imap_host" name="imap_host" placeholder="outlook.office365.com" autocomplete="off">
                    </div>
                    <div class="form-group">
                        <label for="imap_port">Port IMAP</label>
                        <input type="number" id="imap_port" name="imap_port" placeholder="993" autocomplete="off">
                    </div>
                </div>

                <button type="submit" id="submitBtn">Se connecter</button>
            </form>

            <div id="result"></div>
            <div id="tokenSection" style="display: none;"></div>

            <div class="sessions-list" id="sessionsSection">
                <h3>Sessions actives</h3>
                <div id="sessionsList"><em style="color: #64748b; font-size: 0.8125rem;">Aucune session active</em></div>
            </div>
        </div>
    </div>

    <script>
        let currentToken = null;

        function toggleAdvanced() {
            const fields = document.getElementById('advFields');
            const arrow = document.getElementById('advArrow');
            fields.classList.toggle('open');
            arrow.classList.toggle('open');
        }

        async function copyToClipboard(text) {
            try {
                await navigator.clipboard.writeText(text);
            } catch {
                const ta = document.createElement('textarea');
                ta.value = text;
                document.body.appendChild(ta);
                ta.select();
                document.execCommand('copy');
                document.body.removeChild(ta);
            }
        }

        async function loadSessions() {
            try {
                const res = await fetch('/api/sessions');
                const sessions = await res.json();
                const list = document.getElementById('sessionsList');
                if (sessions.length === 0) {
                    list.innerHTML = '<em style="color: #64748b; font-size: 0.8125rem;">Aucune session active</em>';
                } else {
                    list.innerHTML = sessions.map(s =>
                        '<div class="session-item"><span>' + s.email + '</span> <code>' + s.token + '</code></div>'
                    ).join('');
                }
            } catch {}
        }

        async function logout(token) {
            try {
                await fetch('/api/logout', {
                    method: 'POST',
                    headers: { 'Content-Type': 'application/json' },
                    body: JSON.stringify({ token }),
                });
                currentToken = null;
                document.getElementById('tokenSection').style.display = 'none';
                document.getElementById('result').innerHTML = '<div class="status success">Deconnexion reussie.</div>';
                loadSessions();
            } catch (err) {
                document.getElementById('result').innerHTML = '<div class="status error">Erreur : ' + err.message + '</div>';
            }
        }

        document.getElementById('loginForm').addEventListener('submit', async (e) => {
            e.preventDefault();
            const btn = document.getElementById('submitBtn');
            const result = document.getElementById('result');
            const tokenSection = document.getElementById('tokenSection');

            btn.disabled = true;
            btn.innerHTML = '<span class="spinner"></span>Connexion en cours...';
            result.innerHTML = '';
            tokenSection.style.display = 'none';

            const body = {
                email: document.getElementById('email').value,
                password: document.getElementById('password').value,
                imap_host: document.getElementById('imap_host').value || 'outlook.office365.com',
                imap_port: parseInt(document.getElementById('imap_port').value) || 993,
            };

            try {
                const res = await fetch('/api/login', {
                    method: 'POST',
                    headers: { 'Content-Type': 'application/json' },
                    body: JSON.stringify(body),
                });
                const data = await res.json();

                if (data.success) {
                    currentToken = data.token;
                    result.innerHTML = '<div class="status success">' + data.message + '</div>';
                    tokenSection.style.display = 'block';
                    tokenSection.innerHTML = `
                        <div class="token-box">
                            <h3>URL MCP (a copier dans votre client IA)</h3>
                            <div class="url-field">
                                <input type="text" id="mcpUrl" readonly value="${data.mcp_url}">
                                <button class="copy-btn" onclick="copyToClipboard(document.getElementById('mcpUrl').value)">Copier</button>
                            </div>
                            <h3>Token d'authentification</h3>
                            <div class="url-field">
                                <input type="text" id="tokenValue" readonly value="${data.token}">
                                <button class="copy-btn" onclick="copyToClipboard(document.getElementById('tokenValue').value)">Copier</button>
                            </div>
                            <p style="font-size: 0.75rem; color: #94a3b8; margin-top: 0.5rem;">
                                Configurez votre client MCP avec cette URL. Le token identifie votre session.
                            </p>
                            <button class="btn-danger" onclick="logout('${data.token}')">Se deconnecter</button>
                        </div>
                    `;
                    loadSessions();
                } else {
                    result.innerHTML = '<div class="status error">' + data.message + '</div>';
                }
            } catch (err) {
                result.innerHTML = '<div class="status error">Erreur reseau : ' + err.message + '</div>';
            }

            btn.disabled = false;
            btn.textContent = 'Se connecter';
        });

        // Load sessions on page load
        loadSessions();
    </script>
</body>
</html>
"##;
