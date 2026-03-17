# Exchange MCP Server

Serveur MCP (Model Context Protocol) pour acceder aux emails via IMAP. Deploiement multi-utilisateur avec OAuth 2.1 + PKCE, compatible Claude Web et tout client MCP.

## Fonctionnalites

- OAuth 2.1 Authorization Server integre (PKCE, Dynamic Client Registration)
- Multi-utilisateur avec sessions IMAP isolees
- Conversion HTML → texte, decodage MIME/RFC 2047, detection des pieces jointes
- Compatible avec tout serveur IMAP (Exchange, Gmail, Dovecot, etc.)

### Outils MCP

| Outil | Description |
|-------|-------------|
| `list_folders` | Lister tous les dossiers (INBOX, Sent Items, etc.) |
| `list_emails` | Lister les emails recents d'un dossier |
| `read_email` | Lire le contenu complet d'un email |
| `search_emails` | Rechercher avec la syntaxe IMAP |
| `mark_as_read` | Marquer un email comme lu |
| `mark_as_unread` | Marquer un email comme non lu |
| `move_email` | Deplacer un email vers un autre dossier |
| `delete_email` | Supprimer (deplace vers Deleted Items) |
| `set_flag` | Ajouter/retirer un flag IMAP |
| `folder_status` | Stats d'un dossier (total, non lus, recents) |

## Installation

### Avec Docker (recommande)

```bash
# Copier et editer les variables d'environnement
cp .env.example .env

# Lancer la stack
docker compose up -d
```

Le serveur est accessible sur `http://localhost:3000/mcp`.

La base SQLite OAuth2 est stockee dans `./data/oauth2.db` (persistant entre les redemarrages).

```bash
# Voir les logs
docker compose logs -f

# Arreter
docker compose down

# Rebuild apres une mise a jour du code
docker compose up -d --build
```

### Build natif

```bash
cargo build --release
```

## Configuration

La configuration se charge depuis un fichier JSON ou des variables d'environnement. Les variables d'environnement ont priorite sur le fichier.

### Fichier de configuration

Emplacement par defaut : `~/.config/exchange-mcp/config.json`

```json
{
  "imap_host": "outlook.office365.com",
  "imap_port": 993,
  "sse_host": "0.0.0.0",
  "sse_port": 3000
}
```

Override du chemin : `EXCHANGE_MCP_CONFIG=/chemin/vers/config.json`

### Variables d'environnement

#### Serveur IMAP

| Variable | Description | Defaut |
|----------|-------------|--------|
| `EXCHANGE_IMAP_HOST` | Serveur IMAP | `outlook.office365.com` |
| `EXCHANGE_IMAP_PORT` | Port IMAP | `993` |

#### Serveur HTTP

| Variable | Description | Defaut |
|----------|-------------|--------|
| `EXCHANGE_MCP_SSE_HOST` | Adresse d'ecoute | `127.0.0.1` |
| `EXCHANGE_MCP_SSE_PORT` | Port | `3000` |
| `EXCHANGE_MCP_ISSUER` | URL publique du serveur OAuth 2.1 (derriere un proxy) | `http://<host>:<port>` |

#### Chemins de fichiers

| Variable | Description | Defaut |
|----------|-------------|--------|
| `EXCHANGE_MCP_CONFIG` | Fichier de configuration | `~/.config/exchange-mcp/config.json` |
| `EXCHANGE_MCP_OAUTH_DB` | Base SQLite OAuth 2.1 | `~/.local/share/exchange-mcp/oauth2.db` |

#### Logging

| Variable | Description | Defaut |
|----------|-------------|--------|
| `RUST_LOG` | Niveau de log (`trace`, `debug`, `info`, `warn`, `error`) | `info` |

## Lancement

```bash
EXCHANGE_MCP_SSE_HOST=0.0.0.0 \
EXCHANGE_MCP_SSE_PORT=3000 \
exchange-mcp
```

Le serveur expose :

| Endpoint | Description |
|----------|-------------|
| `GET /.well-known/oauth-protected-resource` | Metadonnees ressource protegee (RFC 9728) |
| `GET /.well-known/oauth-authorization-server` | Metadonnees serveur d'autorisation (RFC 8414) |
| `POST /oauth/register` | Enregistrement dynamique de client (RFC 7591) |
| `GET\|POST /oauth/authorize` | Endpoint d'autorisation (formulaire de login IMAP) |
| `POST /oauth/token` | Endpoint de token (echange de code + refresh) |
| `/mcp` | Endpoint MCP (`Authorization: Bearer <token>` requis) |

## Flow OAuth 2.1

1. Le client MCP envoie une requete a `/mcp` sans token
2. Le serveur repond **401** avec `WWW-Authenticate: Bearer resource_metadata="..."`
3. Le client decouvre les metadonnees via `/.well-known/oauth-protected-resource`
4. Le client recupere la config du serveur d'auth via `/.well-known/oauth-authorization-server`
5. Le client s'enregistre via `POST /oauth/register` (recoit un `client_id`)
6. Le client redirige l'utilisateur vers `/oauth/authorize` avec les parametres PKCE
7. L'utilisateur entre ses identifiants IMAP sur le formulaire
8. Le serveur teste la connexion IMAP, cree une session, genere un code d'autorisation
9. Le serveur redirige vers le client avec le code
10. Le client echange le code + `code_verifier` contre un `access_token` via `POST /oauth/token`
11. Le client utilise `Authorization: Bearer <access_token>` sur `/mcp`

### Deploiement derriere un reverse proxy

```bash
EXCHANGE_MCP_ISSUER=https://mcp.exemple.com exchange-mcp
```

## Outils MCP — Details

### list_folders

Lister tous les dossiers de la boite mail.

**Parametres :** aucun

**Retour :** liste de `{ name, attributes, delimiter }`

### list_emails

Lister les emails recents d'un dossier.

| Parametre | Type | Requis | Defaut | Description |
|-----------|------|--------|--------|-------------|
| `folder` | string | oui | — | Nom du dossier (ex: `"INBOX"`, `"Sent Items"`) |
| `limit` | entier | non | 20 | Nombre max d'emails |

**Retour :** liste de `{ uid, subject, from, date, flags, size }`

### read_email

Lire le contenu complet d'un email.

| Parametre | Type | Requis | Description |
|-----------|------|--------|-------------|
| `folder` | string | oui | Dossier contenant l'email |
| `uid` | entier | oui | UID de l'email |

**Retour :** `{ uid, subject, from, to, cc, date, flags, body_text, body_html, attachments }`

### search_emails

Rechercher des emails avec la syntaxe IMAP.

| Parametre | Type | Requis | Defaut | Description |
|-----------|------|--------|--------|-------------|
| `folder` | string | oui | — | Dossier dans lequel chercher |
| `query` | string | oui | — | Requete IMAP (ex: `UNSEEN`, `FROM "user@ex.com"`, `SUBJECT "reunion"`) |
| `limit` | entier | non | 20 | Nombre max de resultats |

**Retour :** liste de `{ uid, subject, from, date, flags, size }`

### mark_as_read / mark_as_unread

| Parametre | Type | Requis | Description |
|-----------|------|--------|-------------|
| `folder` | string | oui | Dossier |
| `uid` | entier | oui | UID de l'email |

### move_email

| Parametre | Type | Requis | Description |
|-----------|------|--------|-------------|
| `folder` | string | oui | Dossier source |
| `uid` | entier | oui | UID de l'email |
| `target_folder` | string | oui | Dossier de destination |

### delete_email

| Parametre | Type | Requis | Description |
|-----------|------|--------|-------------|
| `folder` | string | oui | Dossier |
| `uid` | entier | oui | UID de l'email |

### set_flag

| Parametre | Type | Requis | Description |
|-----------|------|--------|-------------|
| `folder` | string | oui | Dossier |
| `uid` | entier | oui | UID de l'email |
| `flag` | string | oui | Flag IMAP (`\Flagged`, `\Seen`, `\Answered`, `\Draft`) |
| `add` | bool | oui | `true` pour ajouter, `false` pour retirer |

### folder_status

| Parametre | Type | Requis | Description |
|-----------|------|--------|-------------|
| `folder` | string | oui | Nom du dossier |

**Retour :** `{ name, total, unseen, recent }`

## Architecture

```
src/
├── main.rs             # Point d'entree, AuthMcpService (middleware Tower)
├── config.rs           # Chargement configuration (fichier + env)
├── server.rs           # Definition des outils MCP
├── auth.rs             # Trait AuthProvider + BasicAuthProvider
├── oauth2_server.rs    # Serveur d'autorisation OAuth 2.1
├── oauth2_store.rs     # Store SQLite pour OAuth 2.1
├── imap_client.rs      # Operations IMAP et parsing email
├── session.rs          # Store de sessions multi-utilisateur
└── login.rs            # Extraction Bearer token + favicon
```

## Licence

MIT
