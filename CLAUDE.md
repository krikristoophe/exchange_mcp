# Exchange MCP Server — Guide de developpement

## Apercu du projet

Serveur MCP en Rust pour acceder aux emails via IMAP. Deux modes :
- **stdio** : usage local, un seul utilisateur, auth configuree au demarrage
- **HTTP** : multi-utilisateur, OAuth 2.1 + PKCE, sessions isolees

## Stack technique

- **Rust 2021**, async avec **Tokio**
- **rmcp** v1.2 — SDK MCP (server, stdio, streamable HTTP)
- **axum** v0.8 — serveur HTTP
- **imap** v2 — client IMAP (sync, utilise via `spawn_blocking`)
- **rusqlite** v0.34 (bundled) — stockage OAuth2
- **tower** v0.5 — middleware de service

## Architecture des fichiers

```
src/
├── main.rs             # Point d'entree + AuthMcpService (middleware Tower)
├── config.rs           # Config JSON/env, constantes DEFAULT_IMAP_*
├── server.rs           # ExchangeMcpServer + 10 outils MCP
├── auth.rs             # Trait AuthProvider, BasicAuthProvider, ImapCredentials
├── oauth.rs            # OAuthManager — Device Code Flow Microsoft 365
├── oauth2_server.rs    # Serveur OAuth 2.1 (metadata, register, authorize, token)
├── oauth2_store.rs     # SQLite store (clients, auth codes, tokens)
├── imap_client.rs      # ImapClient — toutes les operations IMAP
├── session.rs          # SessionStore — HashMap<token, UserSession>
└── login.rs            # extract_bearer_token() + favicon handler
```

## Flux de donnees (mode HTTP)

```
Client MCP
  → GET /mcp (pas de token)
  → 401 + WWW-Authenticate (RFC 9728)
  → Decouverte metadata + enregistrement client
  → GET /oauth/authorize (formulaire login IMAP)
  → POST /oauth/authorize (test IMAP → session + auth code)
  → POST /oauth/token (code + PKCE → access_token)
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
- **Tokens** : generes via `base64(random_bytes)` URL-safe sans padding
- **Sessions** : UUID v4 comme cle, stockees dans un `RwLock<HashMap>`

## Points d'attention

- Le crate `imap` est synchrone — ne jamais l'appeler directement dans un contexte async
- `CURRENT_USER_TOKEN` est un `task_local!` — doit etre scope dans la future avant d'appeler le service MCP
- `SessionStore::sessions_blocking_read()` est utilise dans la factory MCP (contexte sync) — ne pas remplacer par la version async
- Les auth codes OAuth expirent en 10 minutes, les access tokens en 1 heure
- Le cleanup des tokens expires se fait au demarrage du serveur uniquement

## Variables d'environnement

Voir la section complete dans le README.md. Les plus importantes :
- `EXCHANGE_AUTH_METHOD` — `oauth2` ou `basic`
- `EXCHANGE_MCP_TRANSPORT` — `stdio` ou `http`
- `EXCHANGE_MCP_ISSUER` — URL publique du serveur (pour le mode HTTP derriere un proxy)
- `EXCHANGE_MCP_OAUTH_DB` — chemin de la base SQLite OAuth2
- `RUST_LOG` — niveau de log

## Commandes utiles

```bash
# Build
cargo build --release

# Lancer en mode stdio
cargo run

# Lancer en mode HTTP
EXCHANGE_MCP_TRANSPORT=http EXCHANGE_MCP_SSE_HOST=0.0.0.0 cargo run

# Logs debug
RUST_LOG=debug cargo run
```

## Regles importantes

- **Garder la documentation a jour** : toute modification d'un outil MCP, d'une variable d'environnement, d'un endpoint OAuth, ou de l'architecture doit etre refletee dans le README.md et ce fichier CLAUDE.md.
- **Pas d'accents dans le code source** (eviter les problemes d'encodage dans les string literals HTML). Les accents sont OK dans le README et CLAUDE.md.
- **Ne pas ajouter de endpoint legacy** (token en query param, page de login directe) — toute l'auth HTTP passe par OAuth 2.1.
- **Tester la compilation** (`cargo build`) avant de commit.
