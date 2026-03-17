# Exchange MCP Server

Serveur MCP (Model Context Protocol) pour acceder aux emails via IMAP. Deploiement multi-utilisateur avec OAuth 2.1 + PKCE, compatible Claude Web et tout client MCP.

## Fonctionnalites

- OAuth 2.1 Authorization Server integre (PKCE, Dynamic Client Registration)
- Multi-utilisateur avec sessions IMAP isolees
- Conversion HTML → texte, decodage MIME/RFC 2047 multi-charset, detection des pieces jointes
- Lecture sans marquage (BODY.PEEK) — lire un email ne le marque pas comme lu
- Cache en memoire avec TTL pour des reponses rapides
- Chiffrement AES-256-GCM des credentials IMAP en base de donnees (zeroize en memoire)
- Protection CSRF sur le formulaire d'autorisation
- Headers de securite HTTP (CSP, HSTS, X-Frame-Options, etc.)
- Validation SSRF des serveurs IMAP (blocage des IPs internes)
- Comparaisons constant-time pour les secrets OAuth
- Timeout de session avec nettoyage automatique
- Revocation de token (RFC 7009)
- Suppression automatique des reponses citees pour economiser des tokens
- Lecture batch (plusieurs emails en un seul appel)
- Snippets/previews dans les listes sans lire le contenu complet
- Compatible avec tout serveur IMAP (Exchange, Gmail, Dovecot, etc.)
- MCP Apps (SEP-1865) : interfaces HTML interactives dans le chat (preview email, liste inbox)

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
| `create_folder` | Creer un nouveau dossier |
| `rename_folder` | Renommer un dossier |
| `delete_folder` | Supprimer un dossier |
| `create_draft` | Creer un brouillon dans le dossier Drafts (retourne l'UID) |
| `update_draft` | Modifier un brouillon existant (retourne le nouvel UID) |
| `send_draft` | Envoyer un brouillon existant (supprime le brouillon apres envoi, retourne l'UID dans Sent Items) |
| `delete_draft` | Supprimer un brouillon (deplace vers Deleted Items) |
| `send_email` | Envoyer un email via SMTP (copie dans Sent Items, retourne l'UID) |
| `reply_email` | Repondre a un email (avec citation, retourne l'UID dans Sent Items) |
| `forward_email` | Transferer un email (retourne l'UID dans Sent Items) |
| `list_contacts` | Lister les contacts extraits des emails recents |

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
| `POST /oauth/revoke` | Revocation de token (RFC 7009) |
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

### create_folder

Creer un nouveau dossier. Utiliser le separateur de chemin (generalement '/') pour creer des sous-dossiers.

| Parametre | Type | Requis | Description |
|-----------|------|--------|-------------|
| `folder` | string | oui | Nom du dossier a creer (ex: "Projects", "INBOX/Subfolder") |

### rename_folder

Renommer un dossier existant. Peut aussi etre utilise pour deplacer un dossier en changeant son chemin.

| Parametre | Type | Requis | Description |
|-----------|------|--------|-------------|
| `folder` | string | oui | Nom actuel du dossier |
| `new_name` | string | oui | Nouveau nom du dossier |

### delete_folder

Supprimer un dossier. Le dossier doit etre vide ou le serveur doit supporter la suppression recursive.

| Parametre | Type | Requis | Description |
|-----------|------|--------|-------------|
| `folder` | string | oui | Nom du dossier a supprimer |

### create_draft

Creer un brouillon et le sauvegarder dans le dossier Drafts. L'email n'est PAS envoye.

| Parametre | Type | Requis | Description |
|-----------|------|--------|-------------|
| `to` | string[] | oui | Destinataires |
| `cc` | string[] | non | Copie carbone |
| `subject` | string | oui | Objet |
| `body` | string | oui | Corps (texte brut) |

**Retour :** `{ message, uid, folder }`

### update_draft

Modifier un brouillon existant. Seuls les champs fournis sont mis a jour, les autres conservent leur valeur actuelle.

| Parametre | Type | Requis | Description |
|-----------|------|--------|-------------|
| `uid` | entier | oui | UID du brouillon a modifier |
| `to` | string[] | non | Nouveaux destinataires (si omis, conserve les actuels) |
| `cc` | string[] | non | Nouvelle copie carbone (si omis, conserve les actuels) |
| `subject` | string | non | Nouvel objet (si omis, conserve l'actuel) |
| `body` | string | non | Nouveau corps (si omis, conserve l'actuel) |

**Retour :** `{ message, uid, folder }` (le UID change car IMAP ne permet pas l'edition en place)

### send_draft

Envoyer un brouillon existant. Recupere le brouillon par UID, l'envoie via SMTP, le sauvegarde dans Sent Items, puis le supprime du dossier Drafts.

| Parametre | Type | Requis | Description |
|-----------|------|--------|-------------|
| `uid` | entier | oui | UID du brouillon (dans le dossier Drafts) |

### delete_draft

Supprimer un brouillon du dossier Drafts (deplace vers Deleted Items).

| Parametre | Type | Requis | Description |
|-----------|------|--------|-------------|
| `uid` | entier | oui | UID du brouillon (dans le dossier Drafts) |

### send_email

Envoyer un email via SMTP. Une copie est sauvegardee dans Sent Items.

| Parametre | Type | Requis | Description |
|-----------|------|--------|-------------|
| `to` | string[] | oui | Destinataires |
| `cc` | string[] | non | Copie carbone |
| `subject` | string | oui | Objet |
| `body` | string | oui | Corps (texte brut) |

### reply_email

Repondre a un email. Lit l'original, le cite, et envoie la reponse via SMTP.

| Parametre | Type | Requis | Defaut | Description |
|-----------|------|--------|--------|-------------|
| `folder` | string | oui | — | Dossier de l'email original |
| `uid` | entier | oui | — | UID de l'email |
| `body` | string | oui | — | Corps de la reponse |
| `reply_all` | bool | non | false | Repondre a tous |

### forward_email

Transferer un email a de nouveaux destinataires.

| Parametre | Type | Requis | Description |
|-----------|------|--------|-------------|
| `folder` | string | oui | Dossier de l'email original |
| `uid` | entier | oui | UID de l'email |
| `to` | string[] | oui | Nouveaux destinataires |
| `cc` | string[] | non | Copie carbone |
| `body` | string | non | Message additionnel |

### list_contacts

Lister les contacts extraits des en-tetes (From, To, Cc) des emails recents. Les contacts sont dedupliques par email et tries par frequence.

| Parametre | Type | Requis | Defaut | Description |
|-----------|------|--------|--------|-------------|
| `limit` | entier | non | 50 | Nombre max de contacts a retourner |
| `folders` | string[] | non | `["INBOX", "Sent Items"]` | Dossiers a scanner. `["ALL"]` pour tous. |
| `scan_limit` | entier | non | 100 | Nombre d'emails recents a scanner par dossier |

**Retour :** liste de `{ email, name?, frequency }`

## MCP Apps (interfaces interactives)

Le serveur supporte l'extension MCP Apps (SEP-1865), qui permet d'afficher des interfaces HTML interactives directement dans le chat des clients compatibles (Claude.ai, Claude Desktop, Cowork, VS Code, etc.).

### UI Resources

| URI | Description |
|-----|-------------|
| `ui://exchange/email-preview` | Preview d'un email avec zone de reponse/transfert |
| `ui://exchange/inbox-list` | Liste des emails avec statut lu/non-lu, clic pour ouvrir |

### Outils avec UI

Les outils suivants declararent `_meta.ui.resourceUri` et retournent `structuredContent` pour l'UI :

- `list_emails`, `search_emails` → `inbox-list` (liste interactive)
- `read_email`, `reply_email`, `forward_email`, `send_email`, `create_draft`, `send_draft` → `email-preview` (preview email)

Le `structuredContent` est envoye uniquement a l'UI (pas au LLM), ce qui permet de passer des donnees riches sans consommer de tokens. Le `content` texte reste toujours informatif et suffisant seul pour les clients qui ne supportent pas MCP Apps.

### Capabilities

Le serveur declare les capabilities suivantes a l'initialisation :

```json
{
  "capabilities": {
    "tools": {},
    "resources": {},
    "extensions": {
      "io.modelcontextprotocol/ui": {}
    }
  }
}
```

## Architecture

```
src/
├── main.rs             # Point d'entree, demarrage serveur HTTP + init crypto
├── config.rs           # Chargement configuration (fichier + env)
├── server.rs           # Definition des 19 outils MCP + resources UI
├── auth.rs             # Trait AuthProvider + BasicAuthProvider
├── cache.rs            # Cache en memoire avec TTL (dossiers, listes, details, statuts)
├── crypto.rs           # Chiffrement AES-256-GCM des credentials
├── middleware.rs        # AuthMcpService (middleware Tower) + extraction Bearer token
├── session.rs          # Store de sessions multi-utilisateur
├── oauth/
│   ├── mod.rs          # OAuth2State + re-exports
│   ├── endpoints.rs    # Handlers HTTP (metadata, register, authorize, token)
│   └── store.rs        # Store SQLite (clients, auth codes, tokens, sessions chiffrees)
├── ui_resources/       # Fichiers HTML pour MCP Apps (embarques au compile-time)
│   ├── email_preview.html  # Preview d'un email avant/apres envoi
│   └── inbox_list.html     # Liste des emails avec statut lu/non-lu
└── imap/
    ├── mod.rs          # Re-exports (ImapClient, html_to_text, strip_quoted_replies)
    ├── client.rs       # Operations IMAP (connexion, lecture, batch, recherche, flags, cache)
    └── parse.rs        # Parsing email (MIME, RFC 2047 multi-charset, HTML-to-text, snippets)
```

## Licence

MIT
