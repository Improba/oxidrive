# Phase 0 — Échafaudage, CLI, configuration, authentification

## Objectif

Livrable minimal : un **binaire oxidrive** capable de :

1. Charger la configuration (`config.toml` / `config.json` / chemins par défaut).
2. Exposer une **CLI** cohérente (`setup`, `sync`, `status`, `service`).
3. Réaliser l'**authentification OAuth2** Google (loopback + PKCE) et afficher les informations du compte.
4. Poser les fondations de tous les modules (types, erreurs, store, utils).

---

## Statut actuel : ✅ TERMINÉE

Toutes les tâches P0 sont implémentées et vérifiées.

---

## Matrice de tâches

| ID | Tâche | Fichier(s) | Input | Output | Critère de complétion | Dépendances | Complexité | Statut |
|----|-------|-----------|-------|--------|----------------------|-------------|------------|--------|
| **P0-1** | Scaffold Cargo + structure dossiers | `Cargo.toml`, `src/**/*.rs` | `PLAN.md` | Arborescence `src/` avec modules et stubs | `cargo check` passe | — | Faible | ✅ |
| **P0-2** | Module CLI | `src/cli.rs` | Spec commandes | Parsing clap avec sous-commandes | `cargo run -- --help` affiche l'aide | P0-1 | Faible | ✅ |
| **P0-3** | Module config | `src/config.rs` | Format TOML | Struct `Config` + chargement + validation | Tests : TOML valide, JSON valide, invalide rejeté | P0-1 | Faible | ✅ |
| **P0-4** | Module auth | `src/auth.rs` | OAuth2 Google | `AuthManager` : loopback PKCE, stockage token, refresh | `setup` ouvre le navigateur et stocke le token | P0-1 | Moyenne | ✅ |
| **P0-5** | Utilitaire retry | `src/utils/retry.rs` | — | `retry_with_backoff` générique | Tests : retry N fois, succès au 3e, abandon max | P0-1 | Faible | ✅ |
| **P0-6** | Utilitaire fs | `src/utils/fs.rs` | — | `atomic_write`, `move_to_trash`, `cleanup_part_files` | Tests : écriture atomique, pas de `.part` résiduel | P0-1 | Faible | ✅ |
| **P0-7** | Utilitaire hash | `src/utils/hash.rs` | — | `compute_md5` avec cache `(size, mtime)` | Tests : hash correct, cache hit | P0-1 | Faible | ✅ |
| **P0-8** | Module store | `src/store/db.rs`, `src/store/session.rs` | Schéma redb | CRUD 4 tables, transactions atomiques, session in-memory | Tests : write+read, round-trip bincode | P0-1 | Moyenne | ✅ |
| **P0-9** | Types partagés | `src/types.rs`, `src/error.rs` | PLAN.md | `RelativePath`, `SyncAction`, `OxidriveError`, etc. | Compile, tests serde round-trip | P0-1 | Faible | ✅ |
| **P0-10** | Tracing | `src/main.rs` | — | Setup `tracing-subscriber` + `EnvFilter` + config.log_level | Logs visibles avec `RUST_LOG=debug` | P0-1 | Faible | ✅ |
| **P0-11** | Intégration setup → auth → test connexion | `src/main.rs` | P0-2..P0-4 | `oxidrive setup` + affichage user info | Auth complète fonctionnelle | P0-2..P0-4, P0-10 | Faible | ✅ |

---

## Dépendances entre tâches

```
P0-1 (scaffold) ─────────────────────────────────────────┐
  ├── P0-2 (CLI) ─────────────────┐                      │
  ├── P0-3 (config) ──────────────┤                      │
  ├── P0-5 (retry) ──────────────┤                      │
  ├── P0-6 (fs) ─────────────────┤                      │
  ├── P0-7 (hash) ───────────────┤                      │
  ├── P0-8 (store) ──────────────┤                      │
  ├── P0-9 (types/error) ────────┤                      │
  └── P0-10 (tracing) ───────────┤                      │
                                  ├── P0-4 (auth) ──────┤
                                  └────────────────────── P0-11 (intégration)
```

**Parallélisables** : P0-2 à P0-10 sont tous indépendants une fois P0-1 terminé.

---

## Critères de complétion de la phase

- [x] `cargo build --release` passe sans erreur
- [x] `cargo test` — 71 tests passent
- [x] `cargo clippy` — aucune erreur
- [x] `oxidrive setup` câblé à `AuthManager` réel
- [x] `oxidrive sync` câblé à `run_sync` réel (+ dry-run)
- [x] `oxidrive status` affiche l'état depuis RedbStore
- [x] Tokens persistés et réutilisables
- [x] `config.example.toml` documenté avec section OAuth
- [x] Documentation : `docs/architecture/`, `docs/conventions/`

---

## Leçons apprises

1. **L'intégration main.rs est critique** — les modules étaient complets mais le binaire ne faisait rien. Toujours câbler les vrais handlers, même partiels.
2. **Le store nécessite un pont** — RedbStore (persistance) et Store (session) doivent être connectés via `load_from_redb`/`persist_to_redb`.
3. **Le token bucket doit être async-safe** — `std::sync::Mutex` + sleep hors du lock = race condition. Utiliser `tokio::sync::Mutex` avec réservation atomique de slot.

→ Suivant : [Phase 1 — Sync bidirectionnelle](phase1-sync.md)
