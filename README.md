# Exchange MCP Server

Serveur MCP (Model Context Protocol) pour acceder aux emails via IMAP. Deploiement multi-utilisateur avec OAuth 2.1 + PKCE, compatible Claude Web et tout client MCP.

## Fonctionnalites

- OAuth 2.1 Authorization Server integre (PKCE, Dynamic Client Registration)
- Multi-utilisateur avec sessions IMAP isolees
- Conversion HTML → texte, decodage MIME/RFC 2047 multi-charset, detection des pieces jointes
- Lecture sans marquage (BODY.PEEK) — lire un email ne le marque pas comme lu
- Cache en memoire avec TTL pour des reponses rapides
- Chiffrement AES-256-GCM des credentials IMAP en base de donnees
- Suppression automatique des reponses citees pour economiser des tokens
- Lecture batch (plusieurs emails en un seul appel)
- Snippets/previews dans les listes sans lire le contenu complet
- Compatible avec tout serveur IMAP (Exchange, Gmail, Dovecot, etc.)

### Outils MCP

| Outil | Description |
|-------|-------------|
| `list_folders` | Lister tous les dossiers (INBOX, Sent Items, etc.) |
| `list_emails` | Lister les emails recents d'un dossier (avec preview optionnel) |
| `read_email` | Lire un email (format text/html/both, suppression des quotes) |
| `read_emails` | Lire plusieurs emails en batch (un seul appel IMAP) |
| `search_emails` | Rechercher avec la syntaxe IMAP (avec preview optionnel) |
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

#### Securite

| Variable | Description | Defaut |
|----------|-------------|--------|
| `EXCHANGE_MCP_ENCRYPTION_KEY` | Cle AES-256 en base64 (32 octets) pour chiffrer les credentials | Auto-generee dans `oauth2.key` |

> Si aucune cle n'est fournie, une cle est generee automatiquement et stockee dans un fichier `.key` a cote de la base SQLite. Les mots de passe existants en clair sont migres automatiquement lors de la prochaine lecture.

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
| `include_preview` | bool | non | false | Inclure un snippet texte (~200 cars) pour chaque email |

**Retour :** liste de `{ uid, subject, from, date, flags, size, snippet? }`

### read_email

Lire le contenu complet d'un email. Ne marque PAS l'email comme lu.

| Parametre | Type | Requis | Defaut | Description |
|-----------|------|--------|--------|-------------|
| `folder` | string | oui | — | Dossier contenant l'email |
| `uid` | entier | oui | — | UID de l'email |
| `format` | string | non | `"text"` | `"text"` (defaut, pas de HTML), `"html"`, ou `"both"` |
| `strip_quotes` | bool | non | true | Supprimer les reponses citees (apres `---`, `De:`, `>`, etc.) |

**Retour :** `{ uid, subject, from, to, cc, date, flags, body_text, body_html?, attachments }`

### read_emails

Lire plusieurs emails en un seul appel (batch). Utilise une seule connexion IMAP.

| Parametre | Type | Requis | Defaut | Description |
|-----------|------|--------|--------|-------------|
| `folder` | string | oui | — | Dossier contenant les emails |
| `uids` | entier[] | oui | — | Liste des UIDs a lire |
| `format` | string | non | `"text"` | `"text"`, `"html"`, ou `"both"` |
| `strip_quotes` | bool | non | true | Supprimer les reponses citees |

**Retour :** liste de `{ uid, subject, from, to, cc, date, flags, body_text, body_html?, attachments }`

### search_emails

Rechercher des emails avec la syntaxe IMAP.

| Parametre | Type | Requis | Defaut | Description |
|-----------|------|--------|--------|-------------|
| `folder` | string | oui | — | Dossier dans lequel chercher |
| `query` | string | oui | — | Requete IMAP (ex: `UNSEEN`, `FROM "user@ex.com"`, `SUBJECT "reunion"`) |
| `limit` | entier | non | 20 | Nombre max de resultats |
| `include_preview` | bool | non | false | Inclure un snippet texte (~200 cars) pour chaque email |

**Retour :** liste de `{ uid, subject, from, date, flags, size, snippet? }`

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
├── main.rs             # Point d'entree, demarrage serveur HTTP + init crypto
├── config.rs           # Chargement configuration (fichier + env)
├── server.rs           # Definition des 11 outils MCP
├── auth.rs             # Trait AuthProvider + BasicAuthProvider
├── cache.rs            # Cache en memoire avec TTL (dossiers, listes, details, statuts)
├── crypto.rs           # Chiffrement AES-256-GCM des credentials
├── middleware.rs        # AuthMcpService (middleware Tower) + extraction Bearer token
├── session.rs          # Store de sessions multi-utilisateur
├── oauth/
│   ├── mod.rs          # OAuth2State + re-exports
│   ├── endpoints.rs    # Handlers HTTP (metadata, register, authorize, token)
│   └── store.rs        # Store SQLite (clients, auth codes, tokens, sessions chiffrees)
└── imap/
    ├── mod.rs          # Re-exports (ImapClient, html_to_text, strip_quoted_replies)
    ├── client.rs       # Operations IMAP (connexion, lecture, batch, recherche, flags, cache)
    └── parse.rs        # Parsing email (MIME, RFC 2047 multi-charset, HTML-to-text, snippets)
```

## Licence

MIT
