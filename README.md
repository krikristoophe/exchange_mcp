# Exchange MCP Server

Serveur MCP (Model Context Protocol) pour accéder aux emails Microsoft Exchange via IMAP avec authentification OAuth2. Permet à Claude de lire et gérer les emails.

## Fonctionnalités

- **Authentification OAuth2** via Microsoft Entra (Azure AD) avec Device Code Flow — fonctionne en local, mobile, web et cowork
- **Token caching** avec refresh automatique
- **Transport dual** : stdio (local) ou Streamable HTTP (distant)
- **Outils MCP disponibles** :
  - `list_folders` — Lister tous les dossiers
  - `list_emails` — Lister les emails récents d'un dossier
  - `read_email` — Lire le contenu complet d'un email
  - `search_emails` — Rechercher avec la syntaxe IMAP (FROM, SUBJECT, UNSEEN, SINCE, etc.)
  - `mark_as_read` / `mark_as_unread` — Marquer lu/non lu
  - `move_email` — Déplacer un email vers un autre dossier
  - `delete_email` — Supprimer (déplace vers Deleted Items)
  - `set_flag` — Ajouter/retirer un flag IMAP

## Prérequis

### Azure App Registration

1. Aller sur [Azure Portal](https://portal.azure.com) → Azure Active Directory → App registrations
2. Créer une nouvelle application :
   - **Name** : Exchange MCP
   - **Supported account types** : Single tenant (ou multi-tenant selon vos besoins)
   - **Redirect URI** : laisser vide (device code flow)
3. Dans **API permissions**, ajouter :
   - `https://outlook.office365.com/IMAP.AccessAsUser.All` (Delegated)
   - `https://outlook.office365.com/SMTP.Send` (Delegated)
4. Dans **Authentication** → **Allow public client flows** : **Yes** (nécessaire pour le device code flow)
5. Noter le **Application (client) ID** et le **Directory (tenant) ID**

## Installation

```bash
cargo install --path .
```

Ou build depuis les sources :

```bash
cargo build --release
```

## Configuration

### Option 1 : Fichier de configuration

Créer `~/.config/exchange-mcp/config.json` :

```json
{
  "tenant_id": "VOTRE_TENANT_ID",
  "client_id": "VOTRE_CLIENT_ID",
  "email": "votre.email@entreprise.com"
}
```

### Option 2 : Variables d'environnement

```bash
export EXCHANGE_TENANT_ID="votre-tenant-id"
export EXCHANGE_CLIENT_ID="votre-client-id"
export EXCHANGE_EMAIL="votre.email@entreprise.com"
```

## Utilisation

### Mode stdio (local — Claude Code CLI / Desktop)

```json
{
  "mcpServers": {
    "exchange": {
      "command": "exchange-mcp",
      "env": {
        "EXCHANGE_TENANT_ID": "...",
        "EXCHANGE_CLIENT_ID": "...",
        "EXCHANGE_EMAIL": "..."
      }
    }
  }
}
```

### Mode HTTP (distant — mobile, web, cowork)

```bash
EXCHANGE_MCP_TRANSPORT=http \
EXCHANGE_MCP_SSE_HOST=0.0.0.0 \
EXCHANGE_MCP_SSE_PORT=3000 \
exchange-mcp
```

Le serveur écoute sur `http://0.0.0.0:3000/mcp` et accepte les connexions MCP via Streamable HTTP.

### Première connexion

Au premier lancement, le serveur affiche un lien d'authentification Microsoft :

```
========================================
  Microsoft Exchange Authentication
========================================
Open this URL in your browser:
  https://microsoft.com/devicelogin
Enter code: ABCD1234
========================================
```

Ouvrez le lien, entrez le code, et autorisez l'accès. Le token est ensuite mis en cache dans `~/.cache/exchange-mcp/token_cache.json` et sera rafraîchi automatiquement.

## Variables d'environnement

| Variable | Description | Défaut |
|----------|-------------|--------|
| `EXCHANGE_TENANT_ID` | Azure AD Tenant ID | requis |
| `EXCHANGE_CLIENT_ID` | Azure App Client ID | requis |
| `EXCHANGE_CLIENT_SECRET` | Client secret (optionnel) | - |
| `EXCHANGE_EMAIL` | Adresse email | requis |
| `EXCHANGE_IMAP_HOST` | Serveur IMAP | `outlook.office365.com` |
| `EXCHANGE_IMAP_PORT` | Port IMAP | `993` |
| `EXCHANGE_MCP_TRANSPORT` | Transport : `stdio` ou `http` | `stdio` |
| `EXCHANGE_MCP_SSE_HOST` | Adresse d'écoute HTTP | `127.0.0.1` |
| `EXCHANGE_MCP_SSE_PORT` | Port HTTP | `3000` |
| `EXCHANGE_MCP_CONFIG` | Chemin du fichier de config | `~/.config/exchange-mcp/config.json` |
| `EXCHANGE_MCP_TOKEN_CACHE` | Chemin du cache de tokens | `~/.cache/exchange-mcp/token_cache.json` |
| `RUST_LOG` | Niveau de log | `info` |

## Licence

MIT
