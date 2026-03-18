# Exchange MCP Server — Guide de developpement

## Apercu du projet

Serveur MCP en Rust pour acceder aux emails et evenements de calendrier via IMAP et envoyer des emails via SMTP.
Multi-utilisateur, OAuth 2.1 + PKCE, sessions IMAP isolees. Transport Streamable HTTP uniquement.

## Stack technique

- **Rust 2021**, async avec **Tokio**
- **rmcp** v1.2 — SDK MCP (server, streamable HTTP)
- **axum** v0.8 — serveur HTTP
- **imap** v2 — client IMAP (sync, utilise via `spawn_blocking`)
- **lettre** v0.11 — client SMTP (envoi d'emails, STARTTLS)
- **rusqlite** v0.34 (bundled) — stockage OAuth2
- **tower** v0.5 — middleware de service
- **aes-gcm** v0.10 — chiffrement AES-256-GCM des credentials
- **subtle** v2.6 — comparaisons constant-time (secrets OAuth, PKCE)
- **zeroize** v1 — nettoyage securise des credentials en memoire
- **encoding_rs** v0.8 — conversion charset (RFC 2047 : iso-8859-1, windows-1252, etc.)

## Architecture des fichiers

```
Dockerfile              # Multi-stage build (builder + runtime debian-slim)
docker-compose.yml      # Stack complete avec volume persistant ./data
.env.example            # Variables d'environnement (a copier en .env)
src/
├── main.rs             # Point d'entree, demarrage serveur HTTP, tache de nettoyage periodique
├── config.rs           # Config JSON/env, constantes DEFAULT_IMAP_*, DEFAULT_SMTP_*
├── server.rs           # ExchangeMcpServer + 25 outils MCP + resources UI (MCP Apps)
├── auth.rs             # Trait AuthProvider, BasicAuthProvider
├── cache.rs            # EmailCache — cache en memoire avec TTL par type de donnee
├── crypto.rs           # Chiffrement AES-256-GCM des credentials SQLite
├── middleware.rs        # AuthMcpService (middleware Tower) + extraction Bearer token + security headers
├── session.rs          # SessionStore — HashMap<token, UserSession> avec timeout
├── oauth/
│   ├── mod.rs          # OAuth2State + re-exports
│   ├── endpoints.rs    # Handlers HTTP (metadata, register, authorize, token, revoke)
│   └── store.rs        # Store SQLite (clients, auth codes, tokens, sessions, CSRF tokens)
├── ui_resources/       # Fichiers HTML pour MCP Apps (embarques via include_str!)
│   ├── email_preview.html  # Preview d'un email avant/apres envoi
│   └── inbox_list.html     # Liste des emails interactive avec statut lu/non-lu
└── imap/
    ├── mod.rs          # Re-exports (ImapClient, html_to_text, strip_quoted_replies)
    ├── calendar.rs     # Parsing ICS/iCalendar (RFC 5545) — structures CalendarEvent/CalendarEventDetail, extraction MIME text/calendar
    ├── client.rs       # ImapClient — connexion, lecture, recherche batch, flags, cache, envoi SMTP, brouillons (create/update/send/delete), contacts, calendrier
    └── parse.rs        # Parsing email (MIME, RFC 2047 multi-charset, HTML-to-text, snippets)
```

## Flux de donnees

```
Client MCP
  → GET /mcp (pas de token)
  → 401 + WWW-Authenticate (RFC 9728)
  → Decouverte metadata + enregistrement client
  → GET /oauth/authorize (formulaire login IMAP)
  → POST /oauth/authorize (test IMAP → session + auth code)
  → POST /oauth/token (code + PKCE → access_token)
  → POST /oauth/revoke (revocation de token, RFC 7009)
  → GET /mcp + Authorization: Bearer <access_token>
      → AuthMcpService resout access_token → session_token
      → CURRENT_USER_TOKEN task-local
      → MCP factory lit le task-local → ImapClient de la session
      → ExchangeMcpServer traite la requete
```

## Conventions

- **Langue du code** : anglais (noms de variables, commentaires techniques)
- **Langue de l'UI** : francais (messages d'erreur utilisateur, formulaires HTML)
- **Gestion d'erreur** : `anyhow::Result` partout, pas de `unwrap()` sur du code faillible
- **IMAP** : toutes les operations IMAP passent par `tokio::task::spawn_blocking`
- **SMTP** : envoi via `lettre` (STARTTLS) dans `spawn_blocking`, copie automatique dans "Sent Items" via IMAP APPEND. Supporte les emails HTML via `body_html` (multipart/alternative text + HTML)
- **Tokens** : generes via `base64(random_bytes(32))` URL-safe sans padding (256 bits)
- **Sessions** : token aleatoire 256 bits comme cle, stockees dans un `RwLock<HashMap>` + persistees en SQLite (table `sessions`) pour survivre aux restarts. Timeout d'inactivite de 8h avec nettoyage periodique (toutes les 5 min)
- **Credentials** : mots de passe IMAP chiffres en AES-256-GCM avant stockage SQLite, zeroizes en memoire au drop (`ZeroizeOnDrop`). Cle dans `EXCHANGE_MCP_ENCRYPTION_KEY` ou generee automatiquement dans un fichier `.key`
- **Auth** : uniquement login/password IMAP (pas d'OAuth2 Microsoft cote IMAP)
- **Cache** : cache en memoire par utilisateur avec TTL (dossiers 5min, listes 2min, details 10min, statut 1min). Invalide automatiquement lors des operations d'ecriture
- **Securite** :
  - Comparaisons constant-time (`subtle::ConstantTimeEq`) pour client_secret et PKCE
  - Protection CSRF sur le formulaire d'autorisation (token unique consommable, 10 min d'expiration)
  - Headers HTTP securite : `X-Content-Type-Options`, `X-Frame-Options`, `CSP`, `HSTS`, `Referrer-Policy`, `X-XSS-Protection`
  - Validation SSRF : blocage des IPs internes/localhost pour le champ IMAP host personnalise
  - Validation des redirect URIs : seuls les schemes `http` et `https` sont acceptes
  - Messages d'erreur generiques cote client (pas de fuite d'information IMAP)
  - Transaction SQLite `IMMEDIATE` pour l'echange d'auth code (anti-replay)
  - Validation email et port IMAP cote serveur
  - Revocation de token via `POST /oauth/revoke` (RFC 7009)

- **MCP Apps** (SEP-1865) :
  - Les fichiers HTML sont dans `src/ui_resources/` et embarques au compile-time via `include_str!()`
  - Les UI resources utilisent le scheme `ui://` et le mime type `text/html;profile=mcp-app`
  - Les tools avec UI declarent `_meta.ui.resourceUri` via le helper `ui_meta()`
  - Les tool results incluent `structuredContent` (donnees pour l'UI, pas envoyees au LLM) + `content` texte (pour le LLM)
  - Le SDK JS `@modelcontextprotocol/ext-apps` est charge via CDN (esm.sh) dans les HTML
  - L'extension `io.modelcontextprotocol/ui` est declaree dans les capabilities du serveur

## Points d'attention

- Le crate `imap` est synchrone — ne jamais l'appeler directement dans un contexte async
- `CURRENT_USER_TOKEN` est un `task_local!` — doit etre scope dans la future avant d'appeler le service MCP
- `SessionStore::sessions_blocking_read()` est utilise dans la factory MCP (contexte sync) — ne pas remplacer par la version async
- Les auth codes OAuth expirent en 10 minutes, les access tokens en 1 heure, les sessions en 8h d'inactivite
- Au demarrage, les sessions sont restaurees depuis SQLite et les tokens orphelins sont nettoyes
- Une tache periodique (toutes les 5 min) nettoie les sessions expirees et les tokens/codes orphelins
- `read_email` utilise `BODY.PEEK[]` pour ne pas marquer les emails comme lus
- Le cache est invalide automatiquement apres chaque operation d'ecriture (move, delete, set_flag, mark_as_read/unread, create_draft, update_draft, send, reply, forward, create_folder, rename_folder, delete_folder)
- Les outils create_draft, update_draft, send_draft, send_email, reply, forward retournent l'UID du message cree/envoye (JSON avec message + uid + folder)
- `crypto::init_cipher()` doit etre appele au demarrage avant toute operation sur les sessions
- Les mots de passe existants en clair sont migres automatiquement (detection via `is_encrypted()`) lors de la lecture

## Variables d'environnement

Voir la section complete dans le README.md. Les plus importantes :

- `EXCHANGE_IMAP_HOST` / `EXCHANGE_IMAP_PORT` — serveur IMAP cible
- `EXCHANGE_SMTP_HOST` / `EXCHANGE_SMTP_PORT` — serveur SMTP cible (defaut: smtp.office365.com:587)
- `EXCHANGE_MCP_SSE_HOST` / `EXCHANGE_MCP_SSE_PORT` — adresse d'ecoute HTTP
- `EXCHANGE_MCP_ISSUER` — URL publique du serveur (derriere un proxy)
- `EXCHANGE_MCP_OAUTH_DB` — chemin de la base SQLite OAuth2
- `EXCHANGE_MCP_ENCRYPTION_KEY` — cle AES-256 en base64 (optionnel, generee auto si absente)
- `RUST_LOG` — niveau de log

## Docker

Le projet inclut un `Dockerfile` (multi-stage build) et un `docker-compose.yml`.

- **`.env`** : variables d'environnement (copier `.env.example`). Contient les variables configurables (IMAP host/port, issuer, log level).
- **`./data/`** : volume monte pour la persistance SQLite (`oauth2.db`). Ce dossier est cree automatiquement a cote du `docker-compose.yml`.
- Les variables statiques non sensibles (`SSE_HOST=0.0.0.0`, `SSE_PORT=3000`, `OAUTH_DB=/data/oauth2.db`) sont fixees dans le Dockerfile et le compose, pas dans le `.env`.

## Commandes utiles

```bash
# Build natif
cargo build --release

# Lancer natif
EXCHANGE_MCP_SSE_HOST=0.0.0.0 cargo run

# Logs debug
RUST_LOG=debug cargo run

# Docker — lancer la stack
cp .env.example .env   # puis editer .env
docker compose up -d

# Docker — rebuild + relancer
docker compose up -d --build

# Docker — logs
docker compose logs -f

# Docker — arreter
docker compose down
```

## Regles importantes

- **Garder la documentation a jour** : toute modification d'un outil MCP, d'une variable d'environnement, d'un endpoint OAuth, ou de l'architecture doit etre refletee dans le README.md et ce fichier CLAUDE.md.
- **Pas d'accents dans le code source** (eviter les problemes d'encodage dans les string literals HTML). Les accents sont OK dans le README et CLAUDE.md.
- **Pas de mode stdio** — le serveur fonctionne uniquement en HTTP multi-utilisateur.
- **Pas d'auth OAuth2 Microsoft cote IMAP** — l'auth IMAP est toujours login/password. L'OAuth 2.1 sert uniquement a authentifier les clients MCP.
- **Tester la compilation** (`cargo build`) avant de commit.
- **Zero warning** : `cargo check` ne doit produire aucun warning sur le code du projet (les warnings des dependances externes sont acceptables). Utiliser `#[allow(dead_code)]` sur les methodes publiques utilitaires non encore appelees plutot que de les supprimer.
