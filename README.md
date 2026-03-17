# Exchange MCP Server

Serveur MCP (Model Context Protocol) pour acceder aux emails via IMAP. Supporte Microsoft Exchange (OAuth2 Device Code Flow) et tout serveur IMAP standard (login/password).

Deux modes de deploiement :
- **stdio** — usage local avec Claude Code CLI / Desktop
- **HTTP** — usage distant multi-utilisateur avec OAuth 2.1 + PKCE (compatible Claude Web)

## Fonctionnalites

- Authentification OAuth2 (Microsoft 365) ou basique (login/password)
- OAuth 2.1 Authorization Server integre (PKCE, Dynamic Client Registration)
- Transport dual : stdio ou Streamable HTTP
- Token caching avec refresh automatique
- Multi-utilisateur en mode HTTP (sessions isolees)
- Conversion HTML → texte, decodage MIME/RFC 2047, detection des pieces jointes

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

```bash
cargo build --release
```

Ou installation directe :

```bash
cargo install --path .
```

## Configuration

La configuration se charge depuis un fichier JSON ou des variables d'environnement. Les variables d'environnement ont priorite sur le fichier.

### Fichier de configuration

Emplacement par defaut : `~/.config/exchange-mcp/config.json`

Override avec : `EXCHANGE_MCP_CONFIG=/chemin/vers/config.json`

### Variables d'environnement

#### Authentification

| Variable | Description | Defaut |
|----------|-------------|--------|
| `EXCHANGE_AUTH_METHOD` | Methode d'auth : `oauth2` ou `basic` | `oauth2` |
| `EXCHANGE_TENANT_ID` | Azure AD Tenant ID (requis si oauth2) | — |
| `EXCHANGE_CLIENT_ID` | Azure App Client ID (requis si oauth2) | — |
| `EXCHANGE_CLIENT_SECRET` | Client secret (optionnel, apps confidentielles) | — |
| `EXCHANGE_USERNAME` | Nom d'utilisateur IMAP (requis si basic) | — |
| `EXCHANGE_PASSWORD` | Mot de passe IMAP (requis si basic) | — |
| `EXCHANGE_EMAIL` | Adresse email de l'utilisateur | — |

#### Serveur IMAP/SMTP

| Variable | Description | Defaut |
|----------|-------------|--------|
| `EXCHANGE_IMAP_HOST` | Serveur IMAP | `outlook.office365.com` |
| `EXCHANGE_IMAP_PORT` | Port IMAP | `993` |
| `EXCHANGE_SMTP_HOST` | Serveur SMTP | `smtp.office365.com` |
| `EXCHANGE_SMTP_PORT` | Port SMTP | `587` |

#### Transport et serveur HTTP

| Variable | Description | Defaut |
|----------|-------------|--------|
| `EXCHANGE_MCP_TRANSPORT` | Mode de transport : `stdio` ou `http` | `stdio` |
| `EXCHANGE_MCP_SSE_HOST` | Adresse d'ecoute HTTP | `127.0.0.1` |
| `EXCHANGE_MCP_SSE_PORT` | Port HTTP | `3000` |
| `EXCHANGE_MCP_ISSUER` | URL publique du serveur OAuth 2.1 | `http://<host>:<port>` |

#### Chemins de fichiers

| Variable | Description | Defaut |
|----------|-------------|--------|
| `EXCHANGE_MCP_CONFIG` | Fichier de configuration | `~/.config/exchange-mcp/config.json` |
| `EXCHANGE_MCP_TOKEN_CACHE` | Cache de tokens OAuth2 Microsoft | `~/.cache/exchange-mcp/token_cache.json` |
| `EXCHANGE_MCP_OAUTH_DB` | Base SQLite OAuth 2.1 (mode HTTP) | `~/.local/share/exchange-mcp/oauth2.db` |

#### Logging

| Variable | Description | Defaut |
|----------|-------------|--------|
| `RUST_LOG` | Niveau de log (`trace`, `debug`, `info`, `warn`, `error`) | `info` |

## Usage

### Mode stdio — Claude Code CLI / Desktop

Configuration MCP client :

```json
{
  "mcpServers": {
    "exchange": {
      "command": "exchange-mcp",
      "env": {
        "EXCHANGE_TENANT_ID": "votre-tenant-id",
        "EXCHANGE_CLIENT_ID": "votre-client-id",
        "EXCHANGE_EMAIL": "vous@entreprise.com"
      }
    }
  }
}
```

Pour un serveur IMAP standard (non Microsoft) :

```json
{
  "mcpServers": {
    "exchange": {
      "command": "exchange-mcp",
      "env": {
        "EXCHANGE_AUTH_METHOD": "basic",
        "EXCHANGE_USERNAME": "vous@exemple.com",
        "EXCHANGE_PASSWORD": "motdepasse",
        "EXCHANGE_EMAIL": "vous@exemple.com",
        "EXCHANGE_IMAP_HOST": "imap.exemple.com",
        "EXCHANGE_IMAP_PORT": "993"
      }
    }
  }
}
```

#### Premiere connexion OAuth2

Au premier lancement en mode OAuth2, le serveur affiche un lien d'authentification Microsoft sur stderr :

```
========================================
  Microsoft Exchange Authentication
========================================
Open this URL in your browser:
  https://microsoft.com/devicelogin
Enter code: ABCD1234
========================================
```

Le token est ensuite cache dans `~/.cache/exchange-mcp/token_cache.json` et rafraichi automatiquement.

### Mode HTTP — Claude Web / multi-utilisateur

Lancer le serveur :

```bash
EXCHANGE_MCP_TRANSPORT=http \
EXCHANGE_MCP_SSE_HOST=0.0.0.0 \
EXCHANGE_MCP_SSE_PORT=3000 \
exchange-mcp
```

Le serveur expose :
- `GET /.well-known/oauth-protected-resource` — Metadonnees de la ressource protegee (RFC 9728)
- `GET /.well-known/oauth-authorization-server` — Metadonnees du serveur d'autorisation (RFC 8414)
- `POST /oauth/register` — Enregistrement dynamique de client (RFC 7591)
- `GET|POST /oauth/authorize` — Endpoint d'autorisation (formulaire de login IMAP)
- `POST /oauth/token` — Endpoint de token (echange de code + refresh)
- `/mcp` — Endpoint MCP (necessite `Authorization: Bearer <token>`)

#### Flow OAuth 2.1 complet

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

#### Deploiement avec URL publique

Si le serveur est derriere un reverse proxy, configurer l'URL publique :

```bash
EXCHANGE_MCP_ISSUER=https://mcp.exemple.com exchange-mcp
```

Cela permet aux metadonnees OAuth de retourner les bonnes URLs.

## Azure App Registration (OAuth2)

Pour l'authentification Microsoft 365 (mode stdio avec Device Code Flow) :

1. Aller sur [Azure Portal](https://portal.azure.com) → Microsoft Entra ID → App registrations
2. Creer une nouvelle application :
   - **Name** : Exchange MCP
   - **Supported account types** : Single tenant
   - **Redirect URI** : laisser vide
3. Dans **API permissions**, ajouter :
   - `https://outlook.office365.com/IMAP.AccessAsUser.All` (Delegated)
   - `https://outlook.office365.com/SMTP.Send` (Delegated)
4. Dans **Authentication** → **Allow public client flows** : **Yes**
5. Noter le **Application (client) ID** et le **Directory (tenant) ID**

## Outils MCP — Details

### list_folders

Lister tous les dossiers de la boite mail.

**Parametres :** aucun

**Retour :** liste de `{ name, attributes, delimiter }`

### list_emails

Lister les emails recents d'un dossier.

**Parametres :**
- `folder` (string, requis) — nom du dossier (ex: `"INBOX"`, `"Sent Items"`)
- `limit` (entier, optionnel) — nombre max d'emails. Defaut: 20

**Retour :** liste de `{ uid, subject, from, date, flags, size }`

### read_email

Lire le contenu complet d'un email.

**Parametres :**
- `folder` (string, requis) — dossier contenant l'email
- `uid` (entier, requis) — UID de l'email

**Retour :** `{ uid, subject, from, to, cc, date, flags, body_text, body_html, attachments }`

### search_emails

Rechercher des emails avec la syntaxe IMAP.

**Parametres :**
- `folder` (string, requis) — dossier dans lequel chercher
- `query` (string, requis) — requete IMAP (ex: `UNSEEN`, `FROM "user@ex.com"`, `SUBJECT "reunion"`, `SINCE 01-Jan-2025`)
- `limit` (entier, optionnel) — nombre max de resultats. Defaut: 20

**Retour :** liste de `{ uid, subject, from, date, flags, size }`

### mark_as_read / mark_as_unread

Marquer un email comme lu ou non lu.

**Parametres :**
- `folder` (string, requis)
- `uid` (entier, requis)

### move_email

Deplacer un email vers un autre dossier.

**Parametres :**
- `folder` (string, requis) — dossier source
- `uid` (entier, requis)
- `target_folder` (string, requis) — dossier de destination

### delete_email

Supprimer un email (deplace vers Deleted Items).

**Parametres :**
- `folder` (string, requis)
- `uid` (entier, requis)

### set_flag

Ajouter ou retirer un flag IMAP.

**Parametres :**
- `folder` (string, requis)
- `uid` (entier, requis)
- `flag` (string, requis) — flag IMAP (ex: `\Flagged`, `\Seen`, `\Answered`, `\Draft`)
- `add` (bool, requis) — `true` pour ajouter, `false` pour retirer

### folder_status

Obtenir les statistiques d'un dossier.

**Parametres :**
- `folder` (string, requis)

**Retour :** `{ name, total, unseen, recent }`

## Architecture

```
src/
├── main.rs             # Point d'entree, initialisation des transports
├── config.rs           # Chargement configuration (fichier + env)
├── server.rs           # Definition des outils MCP
├── auth.rs             # Trait AuthProvider + BasicAuthProvider
├── oauth.rs            # OAuth2 Device Code Flow (Microsoft 365)
├── oauth2_server.rs    # Serveur d'autorisation OAuth 2.1 (mode HTTP)
├── oauth2_store.rs     # Store SQLite pour OAuth 2.1
├── imap_client.rs      # Operations IMAP et parsing email
├── session.rs          # Store de sessions multi-utilisateur
└── login.rs            # Extraction Bearer token + favicon
```

## Licence

MIT
