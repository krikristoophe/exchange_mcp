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
- Valeur par défaut : `./attachments/`
- Variable d'environnement : `EXCHANGE_MCP_ATTACHMENT_DIR`
- Le répertoire est créé automatiquement au démarrage via `std::fs::create_dir_all`

**`src/imap/client.rs`**
- Nouvelle méthode `download_attachment(folder: &str, uid: u32, filename: &str, attachment_dir: &Path) -> Result<DownloadedAttachment>`
- Logique :
  1. Fetch le body complet via IMAP (`BODY.PEEK[]`) dans `spawn_blocking`
  2. Parse le MIME avec `mailparse`
  3. Localise la partie dont le filename correspond (insensible à la casse)
  4. Sanitise le filename (suppression de `..`, `/`, `\`)
  5. Génère un chemin unique avec suffixe numérique si conflit (`nom_1.ext`, `nom_2.ext`...)
  6. Écrit le fichier sur disque
  7. Retourne `DownloadedAttachment { path, filename, size }`

**`src/server.rs`**
- Nouvel outil MCP `download_attachment`
- Paramètres : `folder: String`, `uid: u32`, `filename: String`
- Retour : `{ path: String, filename: String, size: u64 }`
- Annotations : `read_only_hint = false`, `destructive_hint = false`, `idempotent_hint = false`

## Interface MCP

```
download_attachment(
  folder: String,    // ex: "INBOX"
  uid: u32,          // UID IMAP de l'email (issu de read_email)
  filename: String   // nom exact de la pièce jointe (issu de read_email.attachments[].filename)
) -> {
  path: String,      // chemin absolu du fichier sauvegardé
  filename: String,  // nom final (avec suffixe si conflit)
  size: u64          // taille en octets
}
```

## Gestion des fichiers

### Structure du répertoire
```
<ATTACHMENT_DIR>/
  rapport.pdf          # premier téléchargement
  rapport_1.pdf        # conflit : même nom
  rapport_2.pdf        # conflit : encore
```

- Stockage **à plat** dans `ATTACHMENT_DIR` (pas de sous-dossier par email ou utilisateur)
- Résolution des conflits : le suffixe est inséré avant l'extension (`rapport.pdf` → `rapport_1.pdf`)
- Création automatique du répertoire au démarrage

### Sécurité
- Sanitisation du `filename` : suppression de `..`, `/`, `\` pour éviter les path traversal
- Si le filename sanitisé est vide après nettoyage, retourner une erreur

## Variables d'environnement

| Variable | Défaut | Description |
|---|---|---|
| `EXCHANGE_MCP_ATTACHMENT_DIR` | `./attachments/` | Répertoire de stockage des pièces jointes téléchargées |

## Cas d'erreur

| Cas | Comportement |
|---|---|
| Email introuvable (uid inexistant) | Erreur IMAP propagée |
| Pièce jointe non trouvée (filename inexistant) | Erreur explicite : "attachment not found: <filename>" |
| Répertoire inaccessible en écriture | Erreur IO propagée |
| Filename invalide (path traversal) | Erreur explicite : "invalid filename" |

## Flux de données

```
LLM → download_attachment(folder, uid, filename)
  → server.rs : outil MCP
  → imap/client.rs : download_attachment()
    → spawn_blocking
      → IMAP FETCH BODY.PEEK[]
      → mailparse : localiser la partie par filename
      → sanitiser le filename
      → résoudre les conflits
      → écrire sur disque
    → retourner DownloadedAttachment
  → retourner { path, filename, size }
```
