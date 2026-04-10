# Feuille de route oxidrive

Vision complète du développement d'**oxidrive** — synchronisation bidirectionnelle Google Drive en Rust.

Les durées sont **indicatives** pour un contributeur principal ; elles dépendent du temps disponible et de la complexité des cas Drive réels.

---

## Vue d'ensemble

```
Phase 0 ✅   Phase 1 🟡   Phase 2 ✅    Phase 3 🟡    Phase 4 ✅    Phase 5 🟡
Scaffold     Sync base    Watcher      Workspace    Index MD     Polish
  CLI          Engine       inotify      Export       .docx→md     systemd
  Config       Decision     select!      Import       .xlsx→md     CI/CD
  Auth         Executor     Shutdown     Conversions  .pdf→md      musl
  Store        Drive API    Status       Table conv   Intégration  Releases
  Types        Persistance               Drawings     txt/binaires Progress
```

---

## Progression par phase

| Phase | Description | Tâches | Faites | Restantes | Statut |
|-------|------------|--------|--------|-----------|--------|
| **0** | Scaffold, CLI, Config, Auth | 11 | 11 | 0 | ✅ Terminée |
| **1** | Sync bidirectionnelle de base | 13 | 13 | 0 | 🟡 Terminée côté code (tests wiremock en cours) |
| **2** | Watcher local + sync temps réel | 6 | 6 | 0 | ✅ Terminée |
| **3** | Conversion Google Workspace | 8 | 7 | 1 | 🟡 Presque complète (intégration executor / engine) |
| **4** | Index Markdown | 10 | 10 | 0 | ✅ Terminée (helper export MD API optionnel) |
| **5** | Polish, service, cross-compilation | 9 | 6 | 3 | 🟡 En cours (logging, cleanup, Windows schtasks) |
| **Total** | | **57** | **53** | **4** | **~93% fait** |

---

## Phase 0 — Échafaudage, CLI, configuration, authentification ✅

**Contenu** : structure du crate, CLI clap, chargement config TOML/JSON, tracing avec config.log_level, OAuth2 Google (loopback PKCE), affichage infos compte, types partagés, module store (redb + session), utilitaires (retry, fs, hash).

**Durée réelle** : ~1 session (scaffold + revue + corrections).

**Détail** : [phase0-scaffold.md](phase0-scaffold.md)

---

## Phase 1 — Synchronisation bidirectionnelle de base 🟡

**Contenu** :
- Client Drive complet : listing récursif, Changes API, download, upload, déduplication noms
- Scan local avec MD5 et ignore patterns
- Matrice de décision exhaustive (12 cas) avec ConflictPolicy appliquée
- Exécuteur parallèle (JoinSet + Semaphore) avec résolution de conflits
- Engine de sync : orchestration scan → list → decide → execute → persist
- Sync incrémentale via Changes API + page token persisté
- Persistance RedbStore ↔ Session (bincode)
- Dry-run fonctionnel
- Gestion des dossiers : `create_folder`, `trash_folder`, `ensure_folder_hierarchy` câblés dans l’engine

**Reste à faire** :
- Tests d’intégration wiremock end-to-end (en cours)

**Durée estimée restante** : quelques jours à 1 semaine (tests mock)

**Détail** : [phase1-sync.md](phase1-sync.md)

---

## Phase 2 — Watcher local et sync temps réel ✅

**Contenu** :
- Câblage de `LocalWatcher` (notify + debounce) dans le daemon (`daemon.rs`)
- Boucle `tokio::select!` : shutdown + timer périodique + événements watcher
- Exclusion mutuelle via `Semaphore(1)` (un seul cycle de sync à la fois)
- Shutdown gracieux avec `CancellationToken` (`tokio-util`)
- Commande `status` enrichie : lecture RedbStore (dernier sync, fichiers suivis, jeton de page, conversions, unité systemd)

**Note** : avertissement limite inotify côté `LocalWatcher` ; fallback polling explicite encore à finaliser (voir phase2-watcher.md, P2-2).

**Durée estimée** : maintenance / polish mineur

**Détail** : [phase2-watcher.md](phase2-watcher.md)

---

## Phase 3 — Conversion Google Workspace ↔ formats ouverts 🟡

**Contenu** :
- Export Google Docs/Sheets/Slides via `files.export` avec MIME **OOXML** (`export_format_sync` / `export_format`)
- Fallback **`export_file_with_fallback`** (`exportLinks`) pour les exports trop volumineux
- Import avec conversion : `upload_with_conversion()` (.docx → Google Doc, etc.)
- Table **CONVERSIONS** en redb (CRUD) ; utilisée dans **`executor.rs`** (upload / export Google)
- Branche **`determine_action_converted`** dans `decision.rs` (tests unitaires)
- Google Drawings → **SVG** (`image/svg+xml`)
- Executor : **`export_file_with_fallback`** + OOXML ; **engine** : encore **`determine_action`** — bascule vers **`determine_action_converted`** : **en cours**
- Documentation des limites (perte de métadonnées collaboratives)

**Durée estimée** : 1-3 semaines (intégration + docs + tests)

**Détail** : [phase3-workspace.md](phase3-workspace.md)

---

## Phase 4 — Index Markdown ✅

**Contenu** :
- Extracteurs : .docx, .xlsx, .pptx, .csv, **.pdf** (`pdf-extract`), .txt/.md (lecture / passthrough), binaires → fiche métadonnées (`generator.rs`)
- Export Markdown pour l’index : via `export_format_index` / flux Drive existants (pas de helper dédié `export_as_markdown`, voir P4-1 dans phase4-index.md)
- Générateur `update_index` avec dispatch par extension
- **Intégration post-sync** : `update_index` appelé depuis `engine.rs` lorsque `index_dir` est configuré
- Exclusion `.index/` du scan local

**Durée estimée** : polish (P4-1 helper, tests de référence supplémentaires)

**Détail** : [phase4-index.md](phase4-index.md)

---

## Phase 5 — Polish, service, cross-compilation 🟡

**Contenu** :
- Service systemd user (`service.rs`) : install / uninstall / start / stop
- Tâche planifiée Windows (`schtasks`) : encore à faire (P5-1b)
- Logging avancé (fichier JSON, rotation, niveaux par module) : **en cours**
- Barres de progression **`indicatif`** dans l’executor
- **CI** `.github/workflows/ci.yml` (Linux, macOS, Windows)
- **Release** `.github/workflows/release.yml` sur tags `v*` (musl, macOS x86_64/aarch64, Windows), archives + SHA256
- Cross-compilation musl validée via le workflow release (P5-5 : voir phase5-polish.md)
- Nettoyage warnings et code mort : **en cours**
- Documentation opérationnelle : README + `docs/` ; approfondissements possibles (FAQ, dépannage)

**Durée estimée** : 2-4 semaines (Windows service, logging fichier, cleanup)

**Détail** : [phase5-polish.md](phase5-polish.md)

---

## Timeline globale

```
         Mois 1         Mois 2         Mois 3         Mois 4         Mois 5
    ┌──────────────┬──────────────┬──────────────┬──────────────┬──────────────┐
    │  Phase 0 ✅  │              │              │              │              │
    │              │  Phase 1 🟡  │              │              │              │
    │              │              │  Phase 2     │              │              │
    │              │              │  Phase 3     │  Phase 3     │              │
    │              │              │              │  Phase 4     │  Phase 4     │
    │              │              │              │              │  Phase 5     │
    └──────────────┴──────────────┴──────────────┴──────────────┴──────────────┘
```

| Phase | Durée estimée | Cumulé |
|-------|--------------|--------|
| 0 — Scaffold + auth | ✅ Fait | — |
| 1 — Sync de base | 1-2 semaines (reste) | ~1 mois |
| 2 — Watcher | 2-4 semaines | ~2 mois |
| 3 — Workspace | 4-6 semaines | ~3.5 mois |
| 4 — Index MD | 3-5 semaines | ~4.5 mois |
| 5 — Polish + releases | 3-5 semaines | ~5.5 mois |

**Total estimé** : **4 à 6 mois** en développement continu, **3 à 4 mois** à temps plein concentré.

---

## Métriques actuelles du projet

| Métrique | Valeur |
|----------|--------|
| Fichiers source (.rs) | 39 |
| Lignes de code (src/) | ~6 800 |
| Tests unitaires | 71 |
| Dépendances directes | 22 |
| Dev-dépendances | 4 |
| Compilation (`cargo check`) | ✅ |
| Tests (`cargo test`) | ✅ 71/71 |
| Clippy (`cargo clippy`) | ✅ (`-D warnings` en CI) |

---

## Risques et mitigations

| Risque | Impact | Mitigation |
|--------|--------|------------|
| Complexité bisync sous-estimée | Élevé | Matrice formalisée (12 cas), tests exhaustifs |
| Rate limiting Google sur gros Drive | Moyen | Token bucket async, respect Retry-After, pagination |
| Limite inotify sur gros arbres | Moyen | Détection + fallback polling |
| redb synchrone dans tokio | Moyen | `spawn_blocking` systématique |
| Conversion OOXML complexe | Moyen | Best-effort, extraction texte seulement |
| Export Markdown API variable | Faible | Fallback text/plain |
| Symlinks dans l'arbre sync | Faible | Détection + skip + log warning |
| OAuth token refresh / révocation | Moyen | Gestion proactive de l'expiration, re-auth claire |
| Gros fichiers : OOM sur upload/download | Moyen | Streaming (reqwest stream feature) au lieu de `read` complet en mémoire |
| Interruption réseau mid-sync | Moyen | Atomic writes (.part), retry, SyncReport agrège les erreurs partielles |
| Multi-poste sur le même compte | Faible | Documenter la limitation (un seul client par dossier sync) |
| Évolutions API Google Drive | Faible | Abstraction client, version v3 stable |
| Sécurité du token.json sur disque | Faible | Permissions fichier restrictives (0600), documentation |

---

## Documents connexes

- Architecture : [../architecture/overview.md](../architecture/overview.md)
- Arbre de décision : [../architecture/decision-tree.md](../architecture/decision-tree.md)
- Conventions : [../conventions/code-style.md](../conventions/code-style.md)
- Git workflow : [../conventions/git-workflow.md](../conventions/git-workflow.md)
