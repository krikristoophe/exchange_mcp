# Exchange MCP Server — Guide de developpement

## Apercu du projet

Serveur MCP en Rust pour acceder aux emails via IMAP. Multi-utilisateur,
OAuth 2.1 + PKCE, sessions IMAP isolees. Transport Streamable HTTP uniquement.

## Stack technique

- **Rust 2021**, async avec **Tokio**
- **rmcp** v1.2 — SDK MCP (server, streamable HTTP)
- **axum** v0.8 — serveur HTTP
- **imap** v2 — client IMAP (sync, utilise via `spawn_blocking`)
- **rusqlite** v0.34 (bundled) — stockage OAuth2
- **tower** v0.5 — middleware de service

## Architecture des fichiers

```
Dockerfile              # Multi-stage build (builder + runtime debian-slim)
docker-compose.yml      # Stack complete avec volume persistant ./data
.env.example            # Variables d'environnement (a copier en .env)
src/
├── main.rs             # Point d'entree, demarrage serveur HTTP
├── config.rs           # Config JSON/env, constantes DEFAULT_IMAP_*
├── server.rs           # ExchangeMcpServer + 10 outils MCP
├── auth.rs             # Trait AuthProvider, BasicAuthProvider
├── middleware.rs        # AuthMcpService (middleware Tower) + extraction Bearer token
├── session.rs          # SessionStore — HashMap<token, UserSession>
├── oauth/
│   ├── mod.rs          # OAuth2State + re-exports
│   ├── endpoints.rs    # Handlers HTTP (metadata, register, authorize, token)
│   └── store.rs        # Store SQLite (clients, auth codes, tokens)
└── imap/
    ├── mod.rs          # Re-exports (ImapClient, html_to_text)
    ├── client.rs       # ImapClient — connexion, lecture, recherche, flags
    └── parse.rs        # Parsing email (MIME, RFC 2047, HTML-to-text)
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
- **Sessions** : UUID v4 comme cle, stockees dans un `RwLock<HashMap>` + persistees en SQLite (table `sessions`) pour survivre aux restarts
- **Auth** : uniquement login/password IMAP (pas d'OAuth2 Microsoft cote IMAP)

## Points d'attention

- Le crate `imap` est synchrone — ne jamais l'appeler directement dans un contexte async
- `CURRENT_USER_TOKEN` est un `task_local!` — doit etre scope dans la future avant d'appeler le service MCP
- `SessionStore::sessions_blocking_read()` est utilise dans la factory MCP (contexte sync) — ne pas remplacer par la version async
- Les auth codes OAuth expirent en 10 minutes, les access tokens en 1 heure
- Au demarrage, les sessions sont restaurees depuis SQLite et les tokens orphelins sont nettoyes

## Variables d'environnement

Voir la section complete dans le README.md. Les plus importantes :

- `EXCHANGE_IMAP_HOST` / `EXCHANGE_IMAP_PORT` — serveur IMAP cible
- `EXCHANGE_MCP_SSE_HOST` / `EXCHANGE_MCP_SSE_PORT` — adresse d'ecoute HTTP
- `EXCHANGE_MCP_ISSUER` — URL publique du serveur (derriere un proxy)
- `EXCHANGE_MCP_OAUTH_DB` — chemin de la base SQLite OAuth2
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
