# Download Attachment Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ajouter un outil MCP `download_attachment` qui télécharge une pièce jointe d'un email IMAP et la sauvegarde localement.

**Architecture:** Un nouveau champ `attachment_dir` est ajouté à `Config` et à `ImapClient`. Une nouvelle méthode `download_attachment` dans `ImapClient` fait le fetch IMAP, parse le MIME, extrait le binaire avec `get_body_raw()`, sanitise le filename, et écrit le fichier atomiquement via `OpenOptions::create_new(true)`. Un nouvel outil MCP dans `server.rs` expose cette fonctionnalité au LLM.

**Tech Stack:** Rust 2021, `mailparse` (parse MIME + `get_body_raw()`), `std::fs` (écriture), `tokio::task::spawn_blocking` (IMAP sync), `rmcp` (outil MCP)

---

## Fichiers modifiés

| Fichier | Action | Responsabilité |
|---|---|---|
| `src/config.rs` | Modifier | Ajouter `attachment_dir: PathBuf` + env var |
| `src/imap/parse.rs` | Modifier | Exposer `find_attachment_part` (nouvelle fonction pub) |
| `src/imap/client.rs` | Modifier | Ajouter `attachment_dir` field, `DownloadedAttachment` struct, méthode `download_attachment` |
| `src/main.rs` | Modifier | Passer `config.attachment_dir.clone()` à `ImapClient::new` |
| `src/oauth/endpoints.rs` | Modifier | Passer `attachment_dir` à `ImapClient::new` |
| `src/server.rs` | Modifier | Ajouter l'outil MCP `download_attachment`, `create_dir_all` au démarrage |
| `.env.example` | Modifier | Documenter `EXCHANGE_MCP_ATTACHMENT_DIR` |
| `CLAUDE.md` | Modifier | Documenter la nouvelle variable d'env et l'outil MCP |

---

## Task 1: Ajouter `attachment_dir` à `Config`

**Fichiers:**
- Modifier: `src/config.rs`

- [ ] **Step 1.1: Écrire le test**

Dans `src/config.rs`, à la fin du fichier, ajouter :

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_attachment_dir_default() {
        std::env::remove_var("EXCHANGE_MCP_ATTACHMENT_DIR");
        let config = Config::from_env().unwrap();
        assert_eq!(config.attachment_dir, std::path::PathBuf::from("./attachments"));
    }

    #[test]
    fn test_attachment_dir_from_env() {
        std::env::set_var("EXCHANGE_MCP_ATTACHMENT_DIR", "/tmp/my-attachments");
        let config = Config::from_env().unwrap();
        assert_eq!(config.attachment_dir, std::path::PathBuf::from("/tmp/my-attachments"));
        std::env::remove_var("EXCHANGE_MCP_ATTACHMENT_DIR");
    }
}
```

- [ ] **Step 1.2: Vérifier que le test échoue**

```bash
cargo test test_attachment_dir 2>&1 | tail -10
```
Expected: erreur de compilation — `attachment_dir` n'existe pas encore.

- [ ] **Step 1.3: Implémenter**

Dans `src/config.rs`, ajouter à la struct `Config` :

```rust
/// Directory where downloaded attachments are stored
#[serde(default = "default_attachment_dir")]
pub attachment_dir: std::path::PathBuf,
```

Ajouter la fonction default et la constante :

```rust
pub const DEFAULT_ATTACHMENT_DIR: &str = "./attachments";

fn default_attachment_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(DEFAULT_ATTACHMENT_DIR)
}
```

Dans `from_env()`, ajouter le champ :

```rust
attachment_dir: std::env::var("EXCHANGE_MCP_ATTACHMENT_DIR")
    .map(std::path::PathBuf::from)
    .unwrap_or_else(|_| default_attachment_dir()),
```

Dans `apply_env_overrides()`, ajouter :

```rust
if let Ok(v) = std::env::var("EXCHANGE_MCP_ATTACHMENT_DIR") {
    self.attachment_dir = std::path::PathBuf::from(v);
}
```

- [ ] **Step 1.4: Vérifier que le test passe**

```bash
cargo test test_attachment_dir 2>&1 | tail -10
```
Expected: `test test_attachment_dir_default ... ok` et `test test_attachment_dir_from_env ... ok`

- [ ] **Step 1.5: Vérifier zero warning**

```bash
cargo check 2>&1 | grep "^warning" | grep -v "from external"
```
Expected: aucune ligne.

- [ ] **Step 1.6: Commit**

```bash
git add src/config.rs
git commit -m "feat(config): add attachment_dir field with EXCHANGE_MCP_ATTACHMENT_DIR env var"
```

---

## Task 2: Exposer `find_attachment_part` dans `parse.rs`

Le but est d'extraire la logique de recherche d'une partie MIME par filename dans une fonction réutilisable par `client.rs`.

**Fichiers:**
- Modifier: `src/imap/parse.rs`

- [ ] **Step 2.1: Écrire les tests**

À la fin de `src/imap/parse.rs`, dans le module `#[cfg(test)]` existant (ou en créer un), ajouter :

```rust
#[cfg(test)]
mod tests {
    use super::*;

    // Email MIME minimal avec une pièce jointe text/plain nommée "hello.txt"
    fn make_email_with_attachment(filename: &str, content: &str) -> Vec<u8> {
        format!(
            "From: test@example.com\r\n\
             To: dest@example.com\r\n\
             Subject: Test\r\n\
             MIME-Version: 1.0\r\n\
             Content-Type: multipart/mixed; boundary=\"bound\"\r\n\
             \r\n\
             --bound\r\n\
             Content-Type: text/plain\r\n\
             \r\n\
             Body text\r\n\
             --bound\r\n\
             Content-Type: application/octet-stream; name=\"{filename}\"\r\n\
             Content-Disposition: attachment; filename=\"{filename}\"\r\n\
             Content-Transfer-Encoding: base64\r\n\
             \r\n\
             {}\r\n\
             --bound--\r\n",
            base64::engine::general_purpose::STANDARD.encode(content.as_bytes())
        ).into_bytes()
    }

    #[test]
    fn test_find_attachment_part_found() {
        let raw = make_email_with_attachment("report.pdf", "PDF content");
        let parsed = mailparse::parse_mail(&raw).unwrap();
        let part = find_attachment_part(&parsed, "report.pdf");
        assert!(part.is_some());
    }

    #[test]
    fn test_find_attachment_part_case_insensitive() {
        let raw = make_email_with_attachment("Report.PDF", "PDF content");
        let parsed = mailparse::parse_mail(&raw).unwrap();
        let part = find_attachment_part(&parsed, "report.pdf");
        assert!(part.is_some());
    }

    #[test]
    fn test_find_attachment_part_not_found() {
        let raw = make_email_with_attachment("other.pdf", "PDF content");
        let parsed = mailparse::parse_mail(&raw).unwrap();
        let part = find_attachment_part(&parsed, "missing.pdf");
        assert!(part.is_none());
    }
}
```

Note: `base64` est déjà une dépendance transitive via `mailparse`. Si le module `base64` n'est pas accessible directement, utiliser un contenu en clair avec `Content-Transfer-Encoding: 7bit` dans le test.

- [ ] **Step 2.2: Vérifier que le test échoue**

```bash
cargo test test_find_attachment 2>&1 | tail -10
```
Expected: erreur de compilation — `find_attachment_part` n'existe pas.

- [ ] **Step 2.3: Implémenter `find_attachment_part`**

Dans `src/imap/parse.rs`, ajouter après `extract_disposition_filename` :

```rust
/// Find the first MIME part whose filename matches (case-insensitive).
/// Filenames are decoded from RFC 2047 encoding before comparison.
pub fn find_attachment_part<'a>(
    mail: &'a mailparse::ParsedMail<'a>,
    target_filename: &str,
) -> Option<&'a mailparse::ParsedMail<'a>> {
    let target = target_filename.to_lowercase();
    find_part_recursive(mail, &target)
}

fn find_part_recursive<'a>(
    mail: &'a mailparse::ParsedMail<'a>,
    target: &str,
) -> Option<&'a mailparse::ParsedMail<'a>> {
    if mail.subparts.is_empty() {
        let disposition = mail
            .headers
            .iter()
            .find(|h| h.get_key().eq_ignore_ascii_case("content-disposition"))
            .map(|h| h.get_value())
            .unwrap_or_default();

        let raw_name = mail
            .ctype
            .params
            .get("name")
            .cloned()
            .or_else(|| extract_disposition_filename(&disposition));

        if let Some(raw) = raw_name {
            let decoded = decode_rfc2047_public(&raw);
            if decoded.to_lowercase() == target {
                return Some(mail);
            }
        }
        None
    } else {
        for part in &mail.subparts {
            if let Some(found) = find_part_recursive(part, target) {
                return Some(found);
            }
        }
        None
    }
}
```

- [ ] **Step 2.4: Vérifier que les tests passent**

```bash
cargo test test_find_attachment 2>&1 | tail -15
```
Expected: 3 tests `ok`.

- [ ] **Step 2.5: Vérifier zero warning**

```bash
cargo check 2>&1 | grep "^warning" | grep -v "from external"
```

- [ ] **Step 2.6: Commit**

```bash
git add src/imap/parse.rs
git commit -m "feat(parse): add find_attachment_part for MIME part lookup by filename"
```

---

## Task 3: Ajouter `DownloadedAttachment` et `download_attachment` dans `ImapClient`

**Fichiers:**
- Modifier: `src/imap/client.rs`

- [ ] **Step 3.1: Écrire les tests unitaires pour la sanitisation et la résolution de conflits**

Ajouter à la fin de `src/imap/client.rs` :

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    // Helper: crée un TempDir propre
    fn tmp() -> TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn test_sanitize_filename_simple() {
        assert_eq!(sanitize_filename("report.pdf").unwrap(), "report.pdf");
    }

    #[test]
    fn test_sanitize_filename_strips_path() {
        // Un attaquant passe "../../etc/passwd"
        assert_eq!(sanitize_filename("../../etc/passwd").unwrap(), "passwd");
    }

    #[test]
    fn test_sanitize_filename_empty_after_strip() {
        assert!(sanitize_filename("/").is_err());
        assert!(sanitize_filename("..").is_err());
        assert!(sanitize_filename("").is_err());
    }

    #[test]
    fn test_sanitize_filename_too_long() {
        let long = "a".repeat(256);
        assert!(sanitize_filename(&long).is_err());
    }

    #[test]
    fn test_resolve_conflict_no_conflict() {
        let dir = tmp();
        let (path, _file) = resolve_unique_path(dir.path(), "file.pdf").unwrap();
        assert_eq!(path.file_name().unwrap(), "file.pdf");
    }

    #[test]
    fn test_resolve_conflict_with_existing() {
        let dir = tmp();
        fs::write(dir.path().join("file.pdf"), b"existing").unwrap();
        let (path, _file) = resolve_unique_path(dir.path(), "file.pdf").unwrap();
        assert_eq!(path.file_name().unwrap(), "file_1.pdf");
    }

    #[test]
    fn test_resolve_conflict_no_extension() {
        let dir = tmp();
        fs::write(dir.path().join("notes"), b"existing").unwrap();
        let (path, _file) = resolve_unique_path(dir.path(), "notes").unwrap();
        assert_eq!(path.file_name().unwrap(), "notes_1");
    }

    #[test]
    fn test_resolve_conflict_multiple() {
        let dir = tmp();
        fs::write(dir.path().join("file.pdf"), b"1").unwrap();
        fs::write(dir.path().join("file_1.pdf"), b"2").unwrap();
        let (path, _file) = resolve_unique_path(dir.path(), "file.pdf").unwrap();
        assert_eq!(path.file_name().unwrap(), "file_2.pdf");
    }
}
```

Note: `tempfile` doit être ajouté en dev-dependency. Voir step suivant.

- [ ] **Step 3.2: Ajouter `tempfile` en dev-dependency**

Dans `Cargo.toml`, ajouter :

```toml
[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 3.3: Vérifier que les tests échouent**

```bash
cargo test test_sanitize test_resolve 2>&1 | tail -10
```
Expected: erreur de compilation — fonctions pas encore définies.

- [ ] **Step 3.4: Implémenter `attachment_dir` dans `ImapClient`**

Dans `src/imap/client.rs` :

**Ajouter le struct `DownloadedAttachment`** (après `AttachmentInfo`) :

```rust
#[derive(Debug, Clone, serde::Serialize)]
pub struct DownloadedAttachment {
    pub path: std::path::PathBuf,
    pub filename: String,
    pub size: u64,
    pub content_type: String,
}
```

**Ajouter `attachment_dir` dans la struct `ImapClient`** :

```rust
pub struct ImapClient {
    auth: Arc<dyn AuthProvider>,
    host: String,
    port: u16,
    smtp_host: String,
    smtp_port: u16,
    cache: Arc<EmailCache>,
    attachment_dir: std::path::PathBuf,  // <-- ajouter
}
```

**Modifier `ImapClient::new`** pour accepter le nouveau paramètre :

```rust
pub fn new(
    auth: Arc<dyn AuthProvider>,
    host: String,
    port: u16,
    smtp_host: String,
    smtp_port: u16,
    attachment_dir: std::path::PathBuf,  // <-- ajouter
) -> Self {
    Self {
        auth,
        host,
        port,
        smtp_host,
        smtp_port,
        cache: Arc::new(EmailCache::new()),
        attachment_dir,  // <-- ajouter
    }
}
```

- [ ] **Step 3.5: Implémenter les fonctions helper**

Ajouter dans `src/imap/client.rs`, avant `impl ImapClient` :

```rust
/// Sanitize a filename: keep only the final component, enforce max length.
fn sanitize_filename(filename: &str) -> Result<String> {
    let name = std::path::Path::new(filename)
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|n| !n.is_empty())
        .ok_or_else(|| anyhow::anyhow!("invalid filename"))?;
    if name.len() > 255 {
        return Err(anyhow::anyhow!("invalid filename"));
    }
    Ok(name.to_string())
}

/// Find an available path in `dir` for `filename`, appending _1, _2... on conflict.
/// Uses O_EXCL (create_new) to atomically create the file.
/// Returns (PathBuf, File) — the caller must write content into the returned File handle.
fn resolve_unique_path(
    dir: &std::path::Path,
    filename: &str,
) -> Result<(std::path::PathBuf, std::fs::File)> {
    let stem = std::path::Path::new(filename)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(filename);
    let ext = std::path::Path::new(filename)
        .extension()
        .and_then(|e| e.to_str());

    for attempt in 0..=100u32 {
        let candidate = if attempt == 0 {
            filename.to_string()
        } else if let Some(e) = ext {
            format!("{stem}_{attempt}.{e}")
        } else {
            format!("{stem}_{attempt}")
        };

        let candidate_path = dir.join(&candidate);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate_path)
        {
            Ok(file) => return Ok((candidate_path, file)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Err(anyhow::anyhow!("too many conflicts for filename: {filename}"))
}
```

- [ ] **Step 3.6: Implémenter `download_attachment`**

Ajouter dans `impl ImapClient` dans `src/imap/client.rs` :

```rust
/// Download an attachment from an email and save it to the attachment directory.
pub async fn download_attachment(
    &self,
    folder: &str,
    uid: u32,
    filename: &str,
) -> Result<DownloadedAttachment> {
    let safe_filename = sanitize_filename(filename)?;

    let credentials = self.auth.get_credentials().await?;
    let host = self.host.clone();
    let port = self.port;
    let folder = folder.to_string();
    let attachment_dir = self.attachment_dir.clone();

    tokio::task::spawn_blocking(move || {
        let mut session = ImapClient::connect_sync(&host, port, credentials)?;
        session.select(&folder)?;

        let messages = session.uid_fetch(uid.to_string(), "BODY.PEEK[]")?;
        let msg = messages.iter().next().context("Email not found")?;
        let body = msg.body().context("Email has no body")?;

        let parsed = mailparse::parse_mail(body)
            .map_err(|e| anyhow::anyhow!("MIME parse error: {e}"))?;

        let part = parse::find_attachment_part(&parsed, &safe_filename)
            .ok_or_else(|| anyhow::anyhow!("attachment not found: {safe_filename}"))?;

        let content_type = part.ctype.mimetype.clone();
        let raw = part.get_body_raw()
            .map_err(|e| anyhow::anyhow!("Failed to extract attachment body: {e}"))?;

        // create_dir_all BEFORE canonicalize (canonicalize fails on non-existent paths)
        std::fs::create_dir_all(&attachment_dir)?;
        let canonical_dir = std::fs::canonicalize(&attachment_dir)?;

        let (candidate_path, mut file) = resolve_unique_path(&canonical_dir, &safe_filename)?;

        // Confinement check (re-verify after resolve)
        if !candidate_path.starts_with(&canonical_dir) {
            return Err(anyhow::anyhow!("invalid filename"));
        }

        // Write directly into the atomically-created file handle (no TOCTOU)
        use std::io::Write;
        file.write_all(&raw)?;
        drop(file);

        let final_filename = candidate_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(&safe_filename)
            .to_string();

        session.logout()?;

        Ok(DownloadedAttachment {
            path: candidate_path,
            filename: final_filename,
            size: raw.len() as u64,
            content_type,
        })
    })
    .await?
}
```

**Note importante :** `resolve_unique_path` crée le fichier de manière atomique (O_EXCL), puis `std::fs::write` écrase ce fichier vide avec le contenu. Il n'y a pas de TOCTOU ici car le fichier existe déjà à ce stade. Pour éviter le double appel, on peut aussi écrire directement dans le handle de `resolve_unique_path` — si on veut optimiser, `resolve_unique_path` peut retourner le `File` handle ouvert. Mais pour la simplicité, l'approche `create_new` + `write` est correcte (le fichier réservé garantit l'unicité du nom).

- [ ] **Step 3.7: Vérifier que les tests passent**

```bash
cargo test test_sanitize test_resolve 2>&1 | tail -15
```
Expected: 7 tests `ok`.

- [ ] **Step 3.8: Vérifier zero warning et compilation**

```bash
cargo check 2>&1 | grep "^warning" | grep -v "from external"
```
Expected: aucune ligne (les call sites `main.rs` et `endpoints.rs` vont échouer — c'est normal, on les fixe dans la tâche suivante).

- [ ] **Step 3.9: Commit**

```bash
git add src/imap/client.rs Cargo.toml
git commit -m "feat(imap): add download_attachment method with safe filename handling"
```

---

## Task 4: Mettre à jour les call sites de `ImapClient::new`

**Fichiers:**
- Modifier: `src/main.rs`
- Modifier: `src/oauth/endpoints.rs`

- [ ] **Step 4.1: Mettre à jour `src/main.rs`**

Il y a **deux** appels à `ImapClient::new` dans `main.rs` :
1. Ligne ~74 : dans la boucle de restauration des sessions persistées (`load_all_sessions`)
2. Éventuellement un second appel lors de la construction de l'état initial — vérifier avec `grep -n "ImapClient::new" src/main.rs`

Pour chaque appel, ajouter `config.attachment_dir.clone()` comme dernier argument :
```rust
let imap_client = Arc::new(ImapClient::new(
    auth.clone(),
    ps.imap_host.clone(),
    ps.imap_port,
    config.smtp_host.clone(),
    config.smtp_port,
    config.attachment_dir.clone(),  // <-- ajouter
));
```

- [ ] **Step 4.2: Mettre à jour `src/oauth/endpoints.rs`**

Trouver l'appel (ligne ~424) :
```rust
let imap_client = Arc::new(ImapClient::new(
    auth.clone(),
    imap_host.clone(),
    imap_port,
    state.default_smtp_host.clone(),
    state.default_smtp_port,
));
```

`OAuth2State` ne dispose pas de `attachment_dir` — il faut l'y ajouter. Chercher la définition de `OAuth2State` dans `src/oauth/mod.rs` ou `src/oauth/endpoints.rs` et ajouter :

```rust
pub attachment_dir: std::path::PathBuf,
```

Puis lors de la construction de `OAuth2State` dans `src/main.rs`, passer `config.attachment_dir.clone()`.

Enfin, dans `endpoints.rs`, utiliser :
```rust
let imap_client = Arc::new(ImapClient::new(
    auth.clone(),
    imap_host.clone(),
    imap_port,
    state.default_smtp_host.clone(),
    state.default_smtp_port,
    state.attachment_dir.clone(),
));
```

- [ ] **Step 4.3: Vérifier compilation complète**

```bash
cargo build 2>&1 | tail -20
```
Expected: `Finished` sans erreur.

- [ ] **Step 4.4: Tous les tests passent**

```bash
cargo test 2>&1 | tail -10
```

- [ ] **Step 4.5: Commit**

```bash
git add src/main.rs src/oauth/endpoints.rs src/oauth/mod.rs
git commit -m "fix(oauth): pass attachment_dir to ImapClient::new at all call sites"
```

---

## Task 5: Ajouter l'outil MCP `download_attachment` dans `server.rs`

**Fichiers:**
- Modifier: `src/server.rs`

- [ ] **Step 5.1: Ajouter le struct de paramètres**

Dans `src/server.rs`, chercher les autres structs de paramètres (ex: `ReadEmailParams`) et ajouter à la suite :

```rust
#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct DownloadAttachmentParams {
    /// Folder name (e.g. "INBOX")
    folder: String,
    /// Email UID (from list_emails or read_email)
    uid: u32,
    /// Exact attachment filename (from read_email attachments list)
    filename: String,
}
```

- [ ] **Step 5.2: Ajouter le `create_dir_all` au démarrage**

Dans `src/server.rs`, trouver le constructeur de `ExchangeMcpServer` (la fonction `new` ou l'endroit où le serveur est instancié). Ajouter après la construction :

```rust
// Best-effort: create attachment directory at startup
if let Some(dir) = self.imap... // Note: attachment_dir est dans ImapClient, pas directement accessible ici
```

En fait, `ExchangeMcpServer` contient un `Arc<ImapClient>` mais pas directement `attachment_dir`. La solution la plus simple est d'ajouter `attachment_dir` dans `ExchangeMcpServer` également, ou d'appeler `create_dir_all` dans `main.rs` juste avant de démarrer le serveur.

**Approche recommandée :** dans `src/main.rs`, après le chargement de config et avant de démarrer le serveur :

```rust
// Create attachment directory at startup (best-effort)
if let Err(e) = std::fs::create_dir_all(&config.attachment_dir) {
    tracing::warn!("Could not create attachment dir {:?}: {}", config.attachment_dir, e);
}
```

- [ ] **Step 5.3: Ajouter l'outil MCP**

Dans `src/server.rs`, ajouter la méthode dans `impl ExchangeMcpServer` (avec le `#[tool]` attribute), en suivant le pattern de `read_email` :

```rust
#[tool(
    description = "Download an attachment from an email and save it to the local attachment directory. Use read_email first to get the list of attachment filenames. Returns the local file path, final filename, size in bytes, and content type.",
    annotations(read_only_hint = false, destructive_hint = false, idempotent_hint = false, open_world_hint = false)
)]
async fn download_attachment(
    &self,
    Parameters(params): Parameters<DownloadAttachmentParams>,
) -> Result<CallToolResult, ErrorData> {
    match self.imap.download_attachment(&params.folder, params.uid, &params.filename).await {
        Ok(result) => {
            let text = serde_json::to_string_pretty(&json!({
                "path": result.path.to_string_lossy(),
                "filename": result.filename,
                "size": result.size,
                "content_type": result.content_type,
            }))
            .unwrap_or_else(|e| e.to_string());
            Ok(CallToolResult::success(vec![Content::text(text)]))
        }
        Err(e) => Ok(CallToolResult::error(vec![Content::text(format!("Error downloading attachment: {e}"))])),
    }
}
```

Ne pas oublier d'ajouter `download_attachment` dans le `tool_router!` macro si le codebase l'utilise.

- [ ] **Step 5.4: Vérifier la compilation**

```bash
cargo build 2>&1 | tail -20
```
Expected: `Finished` sans erreur.

- [ ] **Step 5.5: Tous les tests passent**

```bash
cargo test 2>&1 | tail -10
```

- [ ] **Step 5.6: Vérifier zero warning**

```bash
cargo check 2>&1 | grep "^warning" | grep -v "from external"
```

- [ ] **Step 5.7: Commit**

```bash
git add src/server.rs src/main.rs
git commit -m "feat(server): expose download_attachment MCP tool"
```

---

## Task 6: Mettre à jour la documentation

**Fichiers:**
- Modifier: `.env.example`
- Modifier: `CLAUDE.md`

- [ ] **Step 6.1: Mettre à jour `.env.example`**

Ajouter dans `.env.example` :

```
# --- Pieces jointes ---
# Repertoire de stockage des pieces jointes telechargees (chemin absolu recommande en production)
# EXCHANGE_MCP_ATTACHMENT_DIR=./attachments
```

- [ ] **Step 6.2: Mettre à jour `CLAUDE.md`**

Dans la section **Variables d'environnement**, ajouter la ligne :
```
- `EXCHANGE_MCP_ATTACHMENT_DIR` — répertoire de stockage des pièces jointes (défaut: `./attachments`)
```

Dans la section **Architecture des fichiers**, mettre à jour le commentaire de `imap/client.rs` pour mentionner `DownloadedAttachment` et `download_attachment`.

- [ ] **Step 6.3: Commit final**

```bash
git add .env.example CLAUDE.md
git commit -m "docs: document EXCHANGE_MCP_ATTACHMENT_DIR and download_attachment tool"
```

---

## Vérification finale

- [ ] **Compilation propre**

```bash
cargo build --release 2>&1 | tail -5
```
Expected: `Finished release`.

- [ ] **Tous les tests passent**

```bash
cargo test 2>&1 | tail -10
```
Expected: tous verts.

- [ ] **Zero warning**

```bash
cargo check 2>&1 | grep "^warning" | grep -v "from external"
```
Expected: aucune ligne.
