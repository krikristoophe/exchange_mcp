use std::path::PathBuf;
use std::sync::Mutex;

use rusqlite::{Connection, params};

/// SQLite-backed store for OAuth 2.1 server data:
/// dynamic client registrations, authorization codes, and access/refresh tokens.
pub struct OAuth2Store {
    conn: Mutex<Connection>,
}

#[derive(Debug, Clone)]
pub struct RegisteredClient {
    pub client_id: String,
    pub client_secret: Option<String>,
    pub redirect_uris: Vec<String>,
    pub client_name: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AuthCode {
    pub code: String,
    pub client_id: String,
    pub redirect_uri: String,
    pub code_challenge: String,
    pub code_challenge_method: String,
    pub session_token: String,
    pub expires_at: i64,
}

#[derive(Debug, Clone)]
pub struct StoredToken {
    pub access_token: String,
    pub refresh_token: String,
    pub client_id: String,
    pub session_token: String,
    pub expires_at: i64,
}

impl OAuth2Store {
    pub fn open(path: Option<PathBuf>) -> anyhow::Result<Self> {
        let conn = match path {
            Some(p) => {
                if let Some(parent) = p.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                Connection::open(p)?
            }
            None => Connection::open_in_memory()?,
        };

        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS oauth_clients (
                client_id       TEXT PRIMARY KEY,
                client_secret   TEXT,
                redirect_uris   TEXT NOT NULL,
                client_name     TEXT,
                created_at      INTEGER NOT NULL DEFAULT (strftime('%s','now'))
            );

            CREATE TABLE IF NOT EXISTS oauth_auth_codes (
                code                  TEXT PRIMARY KEY,
                client_id             TEXT NOT NULL,
                redirect_uri          TEXT NOT NULL,
                code_challenge        TEXT NOT NULL,
                code_challenge_method TEXT NOT NULL DEFAULT 'S256',
                session_token         TEXT NOT NULL,
                expires_at            INTEGER NOT NULL,
                used                  INTEGER NOT NULL DEFAULT 0
            );

            CREATE TABLE IF NOT EXISTS oauth_tokens (
                access_token    TEXT PRIMARY KEY,
                refresh_token   TEXT NOT NULL UNIQUE,
                client_id       TEXT NOT NULL,
                session_token   TEXT NOT NULL,
                expires_at      INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS sessions (
                session_token   TEXT PRIMARY KEY,
                email           TEXT NOT NULL,
                password        TEXT NOT NULL,
                imap_host       TEXT NOT NULL,
                imap_port       INTEGER NOT NULL,
                created_at      INTEGER NOT NULL DEFAULT (strftime('%s','now'))
            );",
        )?;

        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub fn db_path() -> PathBuf {
        if let Ok(path) = std::env::var("EXCHANGE_MCP_OAUTH_DB") {
            PathBuf::from(path)
        } else {
            dirs::data_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("exchange-mcp")
                .join("oauth2.db")
        }
    }

    // -- Client registration --

    pub fn register_client(&self, client: &RegisteredClient) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        let uris_json = serde_json::to_string(&client.redirect_uris)?;
        conn.execute(
            "INSERT INTO oauth_clients (client_id, client_secret, redirect_uris, client_name)
             VALUES (?1, ?2, ?3, ?4)",
            params![client.client_id, client.client_secret, uris_json, client.client_name],
        )?;
        Ok(())
    }

    pub fn get_client(&self, client_id: &str) -> anyhow::Result<Option<RegisteredClient>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT client_id, client_secret, redirect_uris, client_name FROM oauth_clients WHERE client_id = ?1",
        )?;
        let mut rows = stmt.query(params![client_id])?;
        match rows.next()? {
            Some(row) => {
                let uris_json: String = row.get(2)?;
                let redirect_uris: Vec<String> = serde_json::from_str(&uris_json)?;
                Ok(Some(RegisteredClient {
                    client_id: row.get(0)?,
                    client_secret: row.get(1)?,
                    redirect_uris,
                    client_name: row.get(3)?,
                }))
            }
            None => Ok(None),
        }
    }

    // -- Authorization codes --

    pub fn store_auth_code(&self, code: &AuthCode) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO oauth_auth_codes (code, client_id, redirect_uri, code_challenge, code_challenge_method, session_token, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                code.code,
                code.client_id,
                code.redirect_uri,
                code.code_challenge,
                code.code_challenge_method,
                code.session_token,
                code.expires_at,
            ],
        )?;
        Ok(())
    }

    /// Consume an auth code (mark as used). Returns None if already used, expired, or not found.
    pub fn consume_auth_code(&self, code: &str) -> anyhow::Result<Option<AuthCode>> {
        let conn = self.conn.lock().unwrap();
        let now = chrono::Utc::now().timestamp();

        let mut stmt = conn.prepare(
            "SELECT code, client_id, redirect_uri, code_challenge, code_challenge_method, session_token, expires_at
             FROM oauth_auth_codes WHERE code = ?1 AND used = 0 AND expires_at > ?2",
        )?;
        let mut rows = stmt.query(params![code, now])?;

        match rows.next()? {
            Some(row) => {
                let auth_code = AuthCode {
                    code: row.get(0)?,
                    client_id: row.get(1)?,
                    redirect_uri: row.get(2)?,
                    code_challenge: row.get(3)?,
                    code_challenge_method: row.get(4)?,
                    session_token: row.get(5)?,
                    expires_at: row.get(6)?,
                };
                drop(rows);
                drop(stmt);
                // Mark as used
                conn.execute(
                    "UPDATE oauth_auth_codes SET used = 1 WHERE code = ?1",
                    params![code],
                )?;
                Ok(Some(auth_code))
            }
            None => Ok(None),
        }
    }

    // -- Tokens --

    pub fn store_token(&self, token: &StoredToken) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO oauth_tokens (access_token, refresh_token, client_id, session_token, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                token.access_token,
                token.refresh_token,
                token.client_id,
                token.session_token,
                token.expires_at,
            ],
        )?;
        Ok(())
    }

    /// Look up an access token. Returns None if expired or not found.
    pub fn get_token(&self, access_token: &str) -> anyhow::Result<Option<StoredToken>> {
        let conn = self.conn.lock().unwrap();
        let now = chrono::Utc::now().timestamp();
        let mut stmt = conn.prepare(
            "SELECT access_token, refresh_token, client_id, session_token, expires_at
             FROM oauth_tokens WHERE access_token = ?1 AND expires_at > ?2",
        )?;
        let mut rows = stmt.query(params![access_token, now])?;
        match rows.next()? {
            Some(row) => Ok(Some(StoredToken {
                access_token: row.get(0)?,
                refresh_token: row.get(1)?,
                client_id: row.get(2)?,
                session_token: row.get(3)?,
                expires_at: row.get(4)?,
            })),
            None => Ok(None),
        }
    }

    /// Look up a refresh token and return the associated stored token (regardless of access token expiry).
    pub fn get_by_refresh_token(&self, refresh_token: &str) -> anyhow::Result<Option<StoredToken>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT access_token, refresh_token, client_id, session_token, expires_at
             FROM oauth_tokens WHERE refresh_token = ?1",
        )?;
        let mut rows = stmt.query(params![refresh_token])?;
        match rows.next()? {
            Some(row) => Ok(Some(StoredToken {
                access_token: row.get(0)?,
                refresh_token: row.get(1)?,
                client_id: row.get(2)?,
                session_token: row.get(3)?,
                expires_at: row.get(4)?,
            })),
            None => Ok(None),
        }
    }

    /// Delete old token row (used during refresh rotation).
    pub fn delete_token(&self, access_token: &str) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM oauth_tokens WHERE access_token = ?1",
            params![access_token],
        )?;
        Ok(())
    }

    /// Cleanup expired codes and tokens.
    #[allow(dead_code)]
    pub fn cleanup_expired(&self) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        let now = chrono::Utc::now().timestamp();
        conn.execute(
            "DELETE FROM oauth_auth_codes WHERE expires_at <= ?1 OR used = 1",
            params![now],
        )?;
        conn.execute(
            "DELETE FROM oauth_tokens WHERE expires_at <= ?1",
            params![now],
        )?;
        Ok(())
    }

    /// Remove tokens and auth codes that reference sessions no longer in the store.
    pub fn cleanup_orphaned_tokens(&self, valid_session_tokens: &[String]) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        let now = chrono::Utc::now().timestamp();

        // Clean expired first
        conn.execute(
            "DELETE FROM oauth_auth_codes WHERE expires_at <= ?1 OR used = 1",
            params![now],
        )?;
        conn.execute(
            "DELETE FROM oauth_tokens WHERE expires_at <= ?1",
            params![now],
        )?;

        if valid_session_tokens.is_empty() {
            let deleted_tokens = conn.execute("DELETE FROM oauth_tokens", [])?;
            let deleted_codes = conn.execute("DELETE FROM oauth_auth_codes", [])?;
            if deleted_tokens > 0 || deleted_codes > 0 {
                tracing::info!(
                    "Cleared {deleted_tokens} orphaned token(s) and {deleted_codes} auth code(s)"
                );
            }
        } else {
            // Build a comma-separated list of placeholders
            let placeholders: String = valid_session_tokens
                .iter()
                .enumerate()
                .map(|(i, _)| format!("?{}", i + 1))
                .collect::<Vec<_>>()
                .join(",");

            let sql_tokens = format!(
                "DELETE FROM oauth_tokens WHERE session_token NOT IN ({placeholders})"
            );
            let sql_codes = format!(
                "DELETE FROM oauth_auth_codes WHERE session_token NOT IN ({placeholders})"
            );

            let sql_params: Vec<&dyn rusqlite::types::ToSql> = valid_session_tokens
                .iter()
                .map(|s| s as &dyn rusqlite::types::ToSql)
                .collect();

            let deleted_tokens = conn.execute(&sql_tokens, sql_params.as_slice())?;
            let deleted_codes = conn.execute(&sql_codes, sql_params.as_slice())?;
            if deleted_tokens > 0 || deleted_codes > 0 {
                tracing::info!(
                    "Cleared {deleted_tokens} orphaned token(s) and {deleted_codes} auth code(s)"
                );
            }
        }

        Ok(())
    }

    // -- Session persistence --

    pub fn persist_session(&self, session: &PersistedSession) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        // Encrypt the password before storing
        let encrypted_password = crate::crypto::encrypt(&session.password)?;
        conn.execute(
            "INSERT OR REPLACE INTO sessions (session_token, email, password, imap_host, imap_port)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                session.session_token,
                session.email,
                encrypted_password,
                session.imap_host,
                session.imap_port,
            ],
        )?;
        Ok(())
    }

    pub fn load_all_sessions(&self) -> anyhow::Result<Vec<PersistedSession>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT session_token, email, password, imap_host, imap_port FROM sessions",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(PersistedSession {
                session_token: row.get(0)?,
                email: row.get(1)?,
                password: row.get(2)?,
                imap_host: row.get(3)?,
                imap_port: row.get(4)?,
            })
        })?;
        let mut sessions = Vec::new();
        for row in rows {
            let mut session = row?;
            // Decrypt password (handles both encrypted and legacy plaintext)
            match crate::crypto::decrypt_or_plaintext(&session.password) {
                Ok(decrypted) => session.password = decrypted,
                Err(e) => {
                    tracing::warn!(
                        "Failed to decrypt password for session {}: {e}",
                        session.session_token
                    );
                    continue; // Skip sessions with corrupt credentials
                }
            }
            sessions.push(session);
        }
        Ok(sessions)
    }

    #[allow(dead_code)]
    pub fn delete_session(&self, session_token: &str) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM sessions WHERE session_token = ?1",
            params![session_token],
        )?;
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct PersistedSession {
    pub session_token: String,
    pub email: String,
    pub password: String,
    pub imap_host: String,
    pub imap_port: u16,
}
