# Phase 1 — Synchronisation bidirectionnelle de base

## Objectif

`cargo run -- sync` exécute un **cycle complet** : scan local + listing remote + décisions + exécution parallèle + persistance des métadonnées.

C'est le cœur fonctionnel d'oxidrive. À la fin de cette phase, le binaire synchronise réellement un dossier local avec un dossier Google Drive.

---

## Statut actuel : 🟡 CODE COMPLET — TESTS WIREMOCK EN COURS

| Composant | État | Détail |
|-----------|------|--------|
| Types Drive (`drive/types.rs`) | ✅ | DriveFile, DriveChange, MIME, export formats |
| Client HTTP (`drive/client.rs`) | ✅ | Bearer, retry 429/503, token bucket async-safe |
| Listing récursif (`drive/list.rs`) | ✅ | Pagination, BFS dossiers, déduplication noms |
| Changes API (`drive/changes.rs`) | ✅ | Pagination, `get_start_page_token` |
| Download (`drive/download.rs`) | ✅ | `alt=media` + export, atomic write |
| Upload (`drive/upload.rs`) | ✅ | Multipart create, PATCH update, conversion |
| Scan local (`sync/scan.rs`) | ✅ | Walk récursif, ignore patterns, MD5, tests |
| Arbre de décision (`sync/decision.rs`) | ✅ | 12 cas implémentés, ConflictPolicy appliquée, tests exhaustifs |
| Conflits (`sync/conflict.rs`) | ✅ | 3 politiques, timestamp suffix |
| Exécuteur (`sync/executor.rs`) | ✅ | JoinSet + Semaphore, résolution de conflits exécutée |
| Engine (`sync/engine.rs`) | ✅ | Orchestration complète, sync incrémentale via Changes API |
| Persistance RedbStore ↔ Session | ✅ | `load_from_redb` / `persist_to_redb` bincode |
| Gestion des dossiers | ✅ | `create_folder`, `trash_folder`, `ensure_folder_hierarchy` dans `drive/folders.rs`, câblés dans `engine.rs` |
| Tests d'intégration avec mock Drive | 🟡 | Dépendance `wiremock` ; scénarios E2E en cours d’implémentation |
| Dry-run | ✅ | Scan + décisions sans exécution |

---

## Matrice de tâches

| ID | Tâche | Fichier(s) | Input | Output | Critère de complétion | Dépendances | Complexité | Statut |
|----|-------|-----------|-------|--------|----------------------|-------------|------------|--------|
| **P1-1** | Types Drive | `src/drive/types.rs` | API Drive v3 | `DriveFile`, `DriveChange`, constantes MIME | Compile, serde round-trip test | P0-1 | Faible | ✅ |
| **P1-2** | Client HTTP Drive | `src/drive/client.rs` | P0-4 (auth), P0-5 (retry) | `DriveClient` : reqwest + auth + retry + rate-limit | Test avec mock HTTP (wiremock) | P0-4, P0-5 | Moyenne | ✅ code, 🟡 tests wiremock |
| **P1-3** | Listing récursif | `src/drive/list.rs` | P1-2 | `list_all_files(folder_id)` → HashMap | Test mock : pagination, sous-dossiers, doublons | P1-2 | Moyenne | ✅ code, 🟡 tests wiremock |
| **P1-4** | Changes API | `src/drive/changes.rs` | P1-2 | `fetch_changes(page_token)` → `(Vec<DriveChange>, new_token)` | Test mock : pagination, token mis à jour | P1-2 | Moyenne | ✅ code, 🟡 tests wiremock |
| **P1-5** | Download fichiers | `src/drive/download.rs` | P1-2, P0-6 | `download_file` avec `.part` + rename | Test : fichier intact, `.part` nettoyé | P1-2, P0-6 | Faible | ✅ |
| **P1-6** | Upload fichiers | `src/drive/upload.rs` | P1-2 | `upload_file`, `update_file`, `upload_with_conversion` | Test mock : create + update + conversion | P1-2 | Moyenne | ✅ |
| **P1-7** | Scan local | `src/sync/scan.rs` | P0-7 (hash) | `scan_local(root)` → HashMap avec MD5 | Test : arborescence temp, ignore patterns, cache | P0-7 | Faible | ✅ |
| **P1-8** | Arbre de décision | `src/sync/decision.rs` | Matrice de décision | `determine_action(...)` → `SyncAction` | **Test exhaustif 12 cas + edge cases** | P0-9 | Haute | ✅ |
| **P1-9** | Stratégies conflit | `src/sync/conflict.rs` | P1-8 | `resolve_conflict(...)` → `ConflictResolution` | Test : chaque politique | P1-8 | Faible | ✅ |
| **P1-10** | Exécuteur parallèle | `src/sync/executor.rs` | P1-5, P1-6, P0-8 | `execute(actions, client, store)` → `SyncReport` | Test : N tâches en parallèle, erreurs agrégées | P1-5, P1-6, P0-8 | Moyenne | ✅ |
| **P1-11** | Engine sync | `src/sync/engine.rs` | P1-3..P1-10, P0-8 | `run_sync(config, client, store)` → `SyncReport` | Test d'intégration avec mock Drive | Tout P1 | Haute | ✅ code, 🟡 test E2E |
| **P1-12** | Gestion des dossiers | `src/drive/folders.rs`, `src/sync/engine.rs` | P1-3, P1-6, P0-8 | Création hiérarchie, corbeille dossier, mapping parents | Test : mkdir remote, rmdir local | P1-3, P1-6 | Moyenne | ✅ |
| **P1-13** | Intégration CLI sync | `src/main.rs` | P1-11, P0-2, P0-3 | `oxidrive sync` exécute un cycle complet | Test manuel E2E | P1-11 | Faible | ✅ |

---

## Graphe de dépendances

```
P0-9 (types) ──────────────────────→─┐
P1-1 ──→ P1-2 ──→ P1-3 ──→──────────┤
                   P1-4 ──→──────────┤
                   P1-5 ──→──────────┤
                   P1-6 ──→──────────┤
P1-7 ────────────────────────────→───┤
P1-8 (decision) → P1-9 ─────────→───┤
                                     ├──→ P1-10 ──→ P1-11 ──→ P1-13
P0-8 ────────────────────────────→───┤
                                     │
P1-12 ───────────────────────────→───┘ (requis pour sync complète, pas pour compiler P1-10)
```

**Parallélisables** : P1-1 / P1-7 / P1-8 dès le début. P1-3 / P1-4 / P1-5 / P1-6 une fois P1-2 terminé.

> **Note** : P1-12 (dossiers) n'est pas un prérequis pour construire l'executor (P1-10) ni l'engine (P1-11), mais il est nécessaire pour que la sync fonctionne avec des arborescences réelles. L'engine tourne sans P1-12 pour les fichiers à plat.

---

## Tâches restantes prioritaires

### P1-12 : Gestion des dossiers — ✅ fait

Implémenté dans `src/drive/folders.rs` (`create_folder`, `trash_folder`, `ensure_folder_hierarchy`) et appelé depuis `src/sync/engine.rs` avant les uploads.

### Tests d'intégration wiremock

Créer `tests/sync_integration.rs` avec :
- Un serveur wiremock simulant les endpoints Drive v3
- Scénarios : premier sync (upload tout), second sync (skip unchanged), modification locale → upload, modification remote → download, conflit → résolution, suppression
- Vérification du SyncReport et de l'état du store après chaque scénario

---

## Critères de complétion de la phase

- [x] `oxidrive sync` câblé à `run_sync` réel (scan → list → decide → execute → persist)
- [x] `oxidrive sync --dry-run` affiche les actions sans les exécuter
- [x] Les 12 cas de décision sont testés et fonctionnels (matrix_1..matrix_12)
- [x] ConflictPolicy appliquée (LocalWins, RemoteWins, Rename)
- [x] La persistance survit au redémarrage (RedbStore ↔ Session via bincode)
- [x] La sync incrémentale (Changes API) fonctionne quand un page_token existe
- [x] Déduplication des noms de fichiers dans un même dossier Drive
- [x] Les dossiers sont gérés : création remote (`create_folder` / `ensure_folder_hierarchy`), corbeille (`trash_folder`), ordre parent → enfant
- [ ] Tests d'intégration wiremock : premier sync, skip inchangé, modif locale→upload, modif remote→download, conflit, suppression (🟡 en cours)
- [x] Gestion d'arborescences profondes : création parent avant enfant (`ensure_folder_hierarchy`)
- [x] `cargo test` passe (71 tests)
- [x] `cargo clippy` passe sans erreur

→ Suivant : [Phase 2 — Watcher local](phase2-watcher.md)
