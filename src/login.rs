use std::sync::Arc;

use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::{Html, IntoResponse},
};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::auth::{AuthProvider, BasicAuthProvider};
use crate::config::Config;
use crate::imap_client::ImapClient;

/// Shared application state for the HTTP server.
pub struct AppState {
    pub imap: RwLock<Option<Arc<ImapClient>>>,
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
}

#[derive(Serialize)]
pub struct StatusResponse {
    pub authenticated: bool,
    pub email: Option<String>,
}

/// GET / — Serve the login page
pub async fn login_page(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let imap = state.imap.read().await;
    if imap.is_some() {
        // Already authenticated, show status
        return Html(LOGIN_HTML.replace(
            "<!--STATUS-->",
            r#"<div class="status success">Connecté avec succès. Le serveur MCP est prêt sur <code>/mcp</code>.</div>"#,
        ));
    }
    Html(LOGIN_HTML.replace("<!--STATUS-->", ""))
}

/// GET /api/status — Check authentication status
pub async fn api_status(State(state): State<Arc<AppState>>) -> Json<StatusResponse> {
    let imap = state.imap.read().await;
    if imap.is_some() {
        let config = state.config.read().await;
        Json(StatusResponse {
            authenticated: true,
            email: Some(config.email.clone()),
        })
    } else {
        Json(StatusResponse {
            authenticated: false,
            email: None,
        })
    }
}

/// POST /api/login — Test credentials and configure the server
pub async fn api_login(
    State(state): State<Arc<AppState>>,
    Json(req): Json<LoginRequest>,
) -> (StatusCode, Json<LoginResponse>) {
    // Validate input
    if req.email.is_empty() || req.password.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(LoginResponse {
                success: false,
                message: "Email et mot de passe requis.".to_string(),
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
        let mut session = client
            .login(&username, &password)
            .map_err(|(e, _)| e)?;
        session.logout()?;
        Ok::<(), anyhow::Error>(())
    })
    .await;

    match test_result {
        Ok(Ok(())) => {
            // Connection successful — update state
            let auth: Arc<dyn AuthProvider> = Arc::new(BasicAuthProvider::new(
                req.email.clone(),
                req.password.clone(),
                req.email.clone(),
            ));

            let imap_client = Arc::new(ImapClient::new(auth, imap_host.clone(), imap_port));

            {
                let mut imap_lock = state.imap.write().await;
                *imap_lock = Some(imap_client);
            }

            // Update config
            {
                let mut config = state.config.write().await;
                config.auth_method = "basic".to_string();
                config.email = req.email.clone();
                config.username = Some(req.email.clone());
                config.password = Some(req.password.clone());
                config.imap_host = imap_host;
                config.imap_port = imap_port;

                // Save config to disk
                if let Err(e) = save_config(&config) {
                    tracing::warn!("Could not save config to disk: {e}");
                }
            }

            tracing::info!("User {} authenticated via login page", req.email);

            (
                StatusCode::OK,
                Json(LoginResponse {
                    success: true,
                    message: format!("Connexion réussie pour {}. Le serveur MCP est prêt.", req.email),
                }),
            )
        }
        Ok(Err(e)) => {
            tracing::warn!("Login failed: {e}");
            (
                StatusCode::UNAUTHORIZED,
                Json(LoginResponse {
                    success: false,
                    message: format!("Échec de connexion : {e}"),
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
                }),
            )
        }
    }
}

/// GET /favicon.ico — Return empty response to avoid 404
pub async fn favicon() -> impl IntoResponse {
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, "image/x-icon".parse().unwrap());
    (StatusCode::OK, headers, &[] as &[u8])
}

fn save_config(config: &Config) -> anyhow::Result<()> {
    let config_path = Config::config_path();
    if let Some(parent) = config_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(config)?;
    std::fs::write(&config_path, json)?;
    tracing::info!("Config saved to {}", config_path.display());
    Ok(())
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
            max-width: 420px;
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
    </style>
</head>
<body>
    <div class="container">
        <div class="card">
            <div class="logo">
                <h1>Exchange MCP</h1>
                <p>Connexion au serveur de messagerie</p>
            </div>

            <!--STATUS-->

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
                    <span class="arrow" id="advArrow">▶</span> Paramètres avancés
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
        </div>
    </div>

    <script>
        function toggleAdvanced() {
            const fields = document.getElementById('advFields');
            const arrow = document.getElementById('advArrow');
            fields.classList.toggle('open');
            arrow.classList.toggle('open');
        }

        document.getElementById('loginForm').addEventListener('submit', async (e) => {
            e.preventDefault();
            const btn = document.getElementById('submitBtn');
            const result = document.getElementById('result');

            btn.disabled = true;
            btn.innerHTML = '<span class="spinner"></span>Connexion en cours…';
            result.innerHTML = '';

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
                    result.innerHTML = '<div class="status success">' + data.message + '</div>';
                } else {
                    result.innerHTML = '<div class="status error">' + data.message + '</div>';
                }
            } catch (err) {
                result.innerHTML = '<div class="status error">Erreur réseau : ' + err.message + '</div>';
            }

            btn.disabled = false;
            btn.textContent = 'Se connecter';
        });
    </script>
</body>
</html>
"##;
