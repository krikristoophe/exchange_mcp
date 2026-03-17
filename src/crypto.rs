//! AES-256-GCM encryption for IMAP credentials stored in SQLite.

use aes_gcm::{
    Aes256Gcm, KeyInit, Nonce,
    aead::Aead,
};
use anyhow::{Context, Result};
use base64::Engine;
use std::path::PathBuf;
use std::sync::OnceLock;

const KEY_LEN: usize = 32; // AES-256
const NONCE_LEN: usize = 12; // GCM standard nonce

static CIPHER: OnceLock<Aes256Gcm> = OnceLock::new();

/// Initialize the encryption key. Must be called once at startup.
/// Reads from EXCHANGE_MCP_ENCRYPTION_KEY env var (base64), or generates
/// and persists a key file next to the OAuth2 database.
pub fn init_cipher() -> Result<()> {
    let key_bytes = load_or_generate_key()?;
    let cipher = Aes256Gcm::new_from_slice(&key_bytes)
        .map_err(|e| anyhow::anyhow!("Invalid encryption key: {e}"))?;
    CIPHER
        .set(cipher)
        .map_err(|_| anyhow::anyhow!("Cipher already initialized"))?;
    Ok(())
}

fn load_or_generate_key() -> Result<[u8; KEY_LEN]> {
    // 1. Try env var
    if let Ok(b64) = std::env::var("EXCHANGE_MCP_ENCRYPTION_KEY") {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(b64.trim())
            .context("EXCHANGE_MCP_ENCRYPTION_KEY is not valid base64")?;
        if bytes.len() != KEY_LEN {
            anyhow::bail!(
                "EXCHANGE_MCP_ENCRYPTION_KEY must be {KEY_LEN} bytes (got {})",
                bytes.len()
            );
        }
        let mut key = [0u8; KEY_LEN];
        key.copy_from_slice(&bytes);
        return Ok(key);
    }

    // 2. Try keyfile next to the DB
    let key_path = key_file_path();
    if key_path.exists() {
        let content = std::fs::read_to_string(&key_path)
            .context("Failed to read encryption key file")?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(content.trim())
            .context("Key file is not valid base64")?;
        if bytes.len() != KEY_LEN {
            anyhow::bail!("Key file has wrong length ({} bytes)", bytes.len());
        }
        let mut key = [0u8; KEY_LEN];
        key.copy_from_slice(&bytes);
        return Ok(key);
    }

    // 3. Generate a new key and save it
    let mut key = [0u8; KEY_LEN];
    use rand::TryRngCore;
    rand::rngs::OsRng.try_fill_bytes(&mut key[..])
        .map_err(|e| anyhow::anyhow!("Failed to generate random key: {e}"))?;

    if let Some(parent) = key_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let b64 = base64::engine::general_purpose::STANDARD.encode(key);
    std::fs::write(&key_path, &b64).context("Failed to write encryption key file")?;

    // Restrict permissions on Unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600))?;
    }

    tracing::info!("Generated new encryption key at {}", key_path.display());
    Ok(key)
}

fn key_file_path() -> PathBuf {
    if let Ok(path) = std::env::var("EXCHANGE_MCP_OAUTH_DB") {
        let mut p = PathBuf::from(path);
        p.set_extension("key");
        p
    } else {
        dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("exchange-mcp")
            .join("oauth2.key")
    }
}

fn get_cipher() -> Result<&'static Aes256Gcm> {
    CIPHER.get().context("Encryption not initialized — call crypto::init_cipher() first")
}

/// Encrypt a plaintext string. Returns base64(nonce || ciphertext).
pub fn encrypt(plaintext: &str) -> Result<String> {
    let cipher = get_cipher()?;

    let mut nonce_bytes = [0u8; NONCE_LEN];
    use rand::TryRngCore;
    rand::rngs::OsRng.try_fill_bytes(&mut nonce_bytes[..])
        .map_err(|e| anyhow::anyhow!("Failed to generate nonce: {e}"))?;
    let nonce = Nonce::from_slice(&nonce_bytes);

    let ciphertext = cipher
        .encrypt(nonce, plaintext.as_bytes())
        .map_err(|e| anyhow::anyhow!("Encryption failed: {e}"))?;

    // nonce || ciphertext
    let mut combined = Vec::with_capacity(NONCE_LEN + ciphertext.len());
    combined.extend_from_slice(&nonce_bytes);
    combined.extend_from_slice(&ciphertext);

    Ok(base64::engine::general_purpose::STANDARD.encode(&combined))
}

/// Decrypt a base64(nonce || ciphertext) string back to plaintext.
pub fn decrypt(encrypted_b64: &str) -> Result<String> {
    let cipher = get_cipher()?;

    let combined = base64::engine::general_purpose::STANDARD
        .decode(encrypted_b64)
        .context("Invalid base64 in encrypted data")?;

    if combined.len() < NONCE_LEN + 1 {
        anyhow::bail!("Encrypted data too short");
    }

    let nonce = Nonce::from_slice(&combined[..NONCE_LEN]);
    let ciphertext = &combined[NONCE_LEN..];

    let plaintext = cipher
        .decrypt(nonce, ciphertext)
        .map_err(|e| anyhow::anyhow!("Decryption failed: {e}"))?;

    String::from_utf8(plaintext).context("Decrypted data is not valid UTF-8")
}

/// Check if a stored password looks encrypted (base64 with nonce prefix)
/// vs plaintext (for migration of existing data).
pub fn is_encrypted(stored: &str) -> bool {
    if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(stored) {
        // Encrypted data: 12 bytes nonce + at least 16 bytes GCM tag + 1 byte data
        bytes.len() >= NONCE_LEN + 17
    } else {
        false
    }
}

/// Decrypt if encrypted, return as-is if plaintext (migration helper).
pub fn decrypt_or_plaintext(stored: &str) -> Result<String> {
    if is_encrypted(stored) {
        decrypt(stored)
    } else {
        Ok(stored.to_string())
    }
}
