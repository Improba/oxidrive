# Vue d’ensemble de l’architecture

Ce document décrit l’organisation logicielle d’**oxidrive** : modules, flux de données entre eux et principaux choix techniques.

---

## Diagramme ASCII (modules et interactions)

```
                    ┌─────────────────────────────────────────┐
                    │              CLI (main / cli)            │
                    │  setup │ sync │ status │ service        │
                    └───────────────┬─────────────────────────┘
                                    │
                    ┌───────────────┼───────────────┐
                    ▼               ▼               ▼
             ┌──────────┐   ┌──────────┐   ┌──────────────┐
             │  config  │   │   auth   │   │    error     │
             └──────────┘   └──────────┘   └──────────────┘
                                    │
        ┌───────────────────────────┼───────────────────────────┐
        ▼                           ▼                           ▼
 ┌─────────────┐            ┌─────────────┐              ┌─────────────┐
 │   watch     │───events──▶│    sync     │◀───scan────▶│   store     │
 │  (notify)   │            │ scan/decision│              │   (redb)    │
 └─────────────┘            │  /executor   │              └──────▲──────┘
        │                   └──────┬───────┘                     │
        │                          │ read/write metadata         │
        │                          ▼                             │
        │                   ┌─────────────┐                     │
        └──────────────────▶│   drive     │─────────────────────┘
                            │ client API  │    (persist ids, hashes)
                            └──────┬──────┘
                                   │ HTTPS (reqwest + rustls)
                                   ▼
                            ┌─────────────┐
                            │ Google Drive│
                            └─────────────┘

 ┌─────────────┐
 │   index     │◀── fichiers Markdown / exportés (lecture depuis sync_dir)
 │  (futur)    │──▶ artefacts dans index_dir (optionnel)
 └─────────────┘

 ┌─────────────┐
 │   utils     │── hash, fs, retry (utilisé par sync, drive, store)
 └─────────────┘
```

---

## Description des modules

### `drive/`

Couche **API Google Drive** : authentification des requêtes, listage des fichiers, téléchargement, upload, gestion des **changes** (incrémental). Les types distants (`DriveFile`, MIME, parents) sont isolés ici pour limiter la dépendance du reste du code aux détails de l’API.

### `sync/`

Cœur de la **réconciliation** :

- **scan** : inventaire local et distant (chemins relatifs, tailles, MD5 quand disponible).
- **decision** : fonction pure `(local, remote, métadonnées persistées) → action` (upload, download, skip, conflit, suppressions, nettoyage).
- **executor** : orchestration des opérations concrètes (appels `drive`, mises à jour `store`).
- **conflict** : application de la `ConflictPolicy` configurée lorsque la matrice de décision produit un conflit.

### `watch/`

**Surveillance du dossier de sync** via la bibliothèque `notify` (avec debounce configurable). Les événements déclenchent des cycles de sync ou des mises en file d’attente côté runtime async (`tokio`).

### `store/`

**Persistance locale** de l’état de synchronisation (fichiers vus, derniers MD5/mtime locaux et distants, identifiants Drive). Implémentation basée sur **redb** (base clé-valeur embarquée, fichier unique). Les accès bloquants sont typiquement isolés dans `spawn_blocking` pour ne pas bloquer le runtime async.

### `index/`

**Indexation Markdown** (et éventuellement recherche) sur les fichiers présents dans `sync_dir` ou générés par export Workspace. Écrit dans `index_dir` si configuré.

### `utils/`

Fonctions transverses : **hachage** (MD5 pour les binaires exportables), helpers **filesystem**, **retry** avec backoff pour les appels réseau fragiles.

### Modules transverses

- **`config`** : désérialisation TOML/JSON, valeurs par défaut.
- **`auth`** : flux OAuth2 Google (setup, rafraîchissement des jetons).
- **`types`** : types partagés (`SyncAction`, `SyncRecord`, chemins relatifs, etc.).
- **`error`** : erreurs typées (`thiserror`), conversion en codes de sortie CLI.

---

## Flux de données : un cycle de synchronisation

1. **Chargement** : lecture de `Config`, ouverture de la base `store`, client `drive` authentifié.
2. **Scan** : énumération des fichiers locaux (hors `ignore_patterns`) et récupération de l’arborescence / métadonnées Drive dans le périmètre (`drive_folder_id` si défini).
3. **Jointure** : pour chaque chemin relatif connu d’au moins un côté, association avec le dernier `SyncRecord` en base.
4. **Décision** : `determine_action` produit une `SyncAction` (skip, upload, download, conflit, delete local/remote, cleanup metadata).
5. **Résolution de conflits** : si action `Conflict`, application de `ConflictPolicy` (local, remote, renommage).
6. **Exécution** : transferts et suppressions ; mise à jour des fichiers locaux ; appels API Drive.
7. **Persistance** : écriture des nouveaux `SyncRecord` / nettoyage des entrées obsolètes dans `store`.
8. **Index** (optionnel) : si activé, mise à jour incrémentale de l’index après les changements stables.

Ce pipeline peut être déclenché manuellement (`sync`), par timer (`service`) ou par le **watcher** après debounce.

---

## Choix techniques

| Domaine | Choix | Motivation |
|--------|--------|------------|
| Async / concurrence | **Tokio** | Runtime mature pour I/O réseau et tâches parallèles (uploads/downloads). |
| HTTP / TLS | **reqwest** + **rustls** | Client HTTP sans OpenSSL système ; TLS en pur Rust, adapté aux binaires statiques. |
| Stockage local | **redb** | Base embarquée, transactionnelle, fichier unique ; pas de serveur externe. |
| CLI | **clap** | Sous-commandes, flags globaux, messages d’aide cohérents. |
| Config | **serde** + **toml** | Format lisible, schéma évolutif. |
| Erreurs | **thiserror** | Erreurs typées et messages stables pour la CLI. |
| Logs | **tracing** + **tracing-subscriber** | Niveaux, filtres `RUST_LOG`, sortie structurée possible (JSON). |
| FS watch | **notify** + **notify-debouncer-full** | Agrégation d’événements pour éviter les tempêtes de sync. |
| OAuth2 | **oauth2** | Flux standard pour l’API Google. |

Pour le détail de la logique de décision, voir [decision-tree.md](decision-tree.md).
