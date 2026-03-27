# Design : Téléchargement de pièces jointes

**Date :** 2026-03-27
**Branche :** feature/download-attachments
**Statut :** Approuvé

## Contexte

Le serveur MCP expose déjà les métadonnées des pièces jointes (`filename`, `content_type`, `size`) via l'outil `read_email`. Il n'existe pas encore de moyen de télécharger le contenu binaire d'une pièce jointe en local.

## Objectif

Ajouter un outil MCP `download_attachment` permettant au LLM de télécharger une pièce jointe d'un email et de la sauvegarder localement.

## Architecture

### Fichiers modifiés

**`src/config.rs`**
- Ajout du champ `attachment_dir: PathBuf`
- Valeur par défaut : `./attachments/` (relative au répertoire de travail du processus — chemin absolu recommandé en production via `EXCHANGE_MCP_ATTACHMENT_DIR`)
- Variable d'environnement : `EXCHANGE_MCP_ATTACHMENT_DIR`

**`src/imap/client.rs`**
- Ajout du champ `attachment_dir: PathBuf` dans `ImapClient`
- Modification de `ImapClient::new` pour accepter `attachment_dir: PathBuf` (impacte les deux call sites : `src/main.rs` et `src/oauth/endpoints.rs`)
- Nouvelle méthode `download_attachment(folder: &str, uid: u32, filename: &str) -> Result<DownloadedAttachment>`
- Nouveau struct : `DownloadedAttachment { path: PathBuf, filename: String, size: u64, content_type: String }`
- Logique de `download_attachment` :
  1. Validation du `folder` : mêmes règles que les autres appels IMAP du codebase
  2. Fetch le body complet via IMAP (`BODY.PEEK[]`) dans `spawn_blocking`
  3. Parse le MIME avec `mailparse::parse_mail()`
  4. Localise la **première** partie MIME dont le filename correspond :
     - Comparaison insensible à la casse
     - Le filename de chaque partie est extrait via `ctype.params["name"]` puis `content-disposition` filename (même logique que `collect_attachments` existant)
     - Les filenames RFC 2047-encodés (ex: `=?utf-8?b?...?=`) sont décodés via la fonction `decode_rfc2047` existante dans `parse.rs` avant la comparaison
  5. Extraction du contenu binaire via `get_body_raw()` (retourne `Vec<u8>` avec décodage base64/quoted-printable automatique par mailparse — **ne pas utiliser `get_body()` qui retourne un `String` UTF-8 et corrompt les fichiers binaires**)
  6. Sanitisation du filename :
     - Extraction du composant final uniquement via `Path::new(filename).file_name()`
     - Longueur max : 255 caractères
     - Si le résultat est vide ou None : erreur `"invalid filename"`
  7. Création du répertoire : `create_dir_all(attachment_dir)` — **obligatoire avant la canonicalisation** car `fs::canonicalize` échoue si le chemin n'existe pas encore
  8. Canonicalisation du répertoire cible : `fs::canonicalize(attachment_dir)` (résout les chemins relatifs et les symlinks)
  9. Construction du chemin cible dans `attachment_dir`
  10. Vérification de confinement : `resolved_path.starts_with(canonical_attachment_dir)` — erreur si violation
  11. Boucle atomique : pour chaque candidat (`nom.ext`, `nom_1.ext`, `nom_2.ext`..., schéma : insérer `_N` avant l'extension, ou à la fin si pas d'extension), tenter l'ouverture avec `OpenOptions::new().write(true).create_new(true)` — `create_new(true)` correspond à `O_EXCL` et garantit l'atomicité. En cas de succès, écrire immédiatement dans ce handle. Max 100 tentatives, puis erreur `"too many conflicts"`.
  12. Écriture du fichier
  13. Retourne `DownloadedAttachment { path, filename, size, content_type }`

**`src/oauth/endpoints.rs`** et **`src/main.rs`**
- Mise à jour des call sites de `ImapClient::new` pour passer `config.attachment_dir.clone()`

**`src/server.rs`**
- Création du répertoire `attachment_dir` au démarrage du serveur (`create_dir_all`) — best-effort, log d'avertissement si échec
- Nouvel outil MCP `download_attachment`
- Paramètres : `folder: String`, `uid: u32`, `filename: String`
- Retour : `{ path: String, filename: String, size: u64, content_type: String }`
- Annotations : `read_only_hint = false`, `destructive_hint = false` (crée un nouveau fichier sans supprimer ni modifier de données existantes), `idempotent_hint = false`

## Interface MCP

```
download_attachment(
  folder: String,    // ex: "INBOX" — validé comme les autres outils IMAP
  uid: u32,          // UID IMAP de l'email (entier JSON 32-bit non signé)
  filename: String   // nom exact de la pièce jointe (issu de read_email.attachments[].filename)
) -> {
  path: String,        // chemin absolu du fichier sauvegardé sur le serveur
  filename: String,    // nom final (avec suffixe si conflit)
  size: u64,           // taille en octets
  content_type: String // type MIME de la pièce jointe
}
```

## Gestion des fichiers

### Structure du répertoire
```
<ATTACHMENT_DIR>/
  rapport.pdf          # premier téléchargement
  rapport_1.pdf        # conflit (créé atomiquement via O_EXCL)
  rapport_2.pdf        # conflit encore
```

- Stockage **à plat** dans `ATTACHMENT_DIR` (pas de sous-dossier par utilisateur)
  - **Implication privacy** : dans un contexte multi-utilisateur, les fichiers de tous les utilisateurs coexistent. Acceptable pour usage local/dev.
- Résolution des conflits : atomique via `O_EXCL`, max 100 tentatives, puis erreur
- `create_dir_all` avant la boucle O_EXCL (non atomique avec l'écriture — risque négligeable en pratique)
- Pas de limite de taille imposée sur les pièces jointes — limitation connue (contrainte mémoire/disque)

### Sécurité
- Sanitisation : `Path::file_name()` pour extraire le composant final uniquement, longueur max 255 chars
- Confinement : `fs::canonicalize(attachment_dir)` puis `resolved_path.starts_with(canonical_dir)` — obligatoire
- Validation du paramètre `folder` : mêmes règles que les autres outils IMAP

## Variables d'environnement

| Variable | Défaut | Description |
|---|---|---|
| `EXCHANGE_MCP_ATTACHMENT_DIR` | `./attachments/` | Répertoire de stockage. Chemin absolu recommandé en production. |

## Cas d'erreur

| Cas | Comportement |
|---|---|
| Email introuvable (uid inexistant) | Erreur IMAP propagée |
| Pièce jointe non trouvée dans l'email | Erreur : `"attachment not found: <filename>"` |
| Répertoire inaccessible en écriture | Erreur IO propagée |
| Filename invalide après sanitisation | Erreur : `"invalid filename"` |
| Path traversal détecté (confinement) | Erreur : `"invalid filename"` |
| Trop de conflits de noms (> 100) | Erreur : `"too many conflicts for filename: <filename>"` |

## Flux de données

```
LLM → download_attachment(folder, uid, filename)
  → server.rs : validation des params, appel imap
  → imap/client.rs : download_attachment()
    → spawn_blocking
      → IMAP FETCH BODY.PEEK[]
      → mailparse : localiser la première partie par filename (decode RFC 2047)
      → get_body_raw() : extraction binaire (base64/qp décodé par mailparse)
      → sanitiser le filename (Path::file_name, max 255 chars)
      → create_dir_all(attachment_dir) — obligatoire avant canonicalize
      → canonicalize(attachment_dir) pour le confinement check
      → confinement check : starts_with(canonical_dir)
      → O_EXCL loop : nom.ext, nom_1.ext, nom_2.ext... (max 100 tentatives)
      → écrire sur disque
    → retourner DownloadedAttachment { path, filename, size, content_type }
  → retourner { path, filename, size, content_type }
```
