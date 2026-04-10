# Phase 2 — Watcher local et sync temps réel

## Objectif

oxidrive tourne **en continu** : il détecte les changements locaux instantanément via inotify/kqueue/ReadDirectoryChanges et poll le remote périodiquement. Le cycle de sync est déclenché automatiquement.

---

## Statut actuel : ✅ TERMINÉE (sauf P2-2 fallback polling)

| Composant | État | Détail |
|-----------|------|--------|
| Watcher local (`watch/local.rs`) | ✅ | notify + debounce, WatchEvent, détection inotify |
| Boucle principale (`daemon.rs`) | ✅ | `tokio::select!` : shutdown + timer + événements watcher |
| Exclusion mutuelle watcher/sync | ✅ | `Semaphore(1)` dans `daemon.rs` |
| Shutdown gracieux | ✅ | `CancellationToken` (`tokio-util`) dans `daemon.rs` |
| Commande `status` | ✅ | RedbStore : dernier sync, fichiers suivis, jeton de page, conversions, unité systemd |

---

## Prérequis

- Phase 1 complétée (sync fonctionnelle de bout en bout)
- `LocalWatcher` câblé dans le daemon

---

## Matrice de tâches

| ID | Tâche | Fichier(s) | Input | Output | Critère de complétion | Dépendances | Complexité | Statut |
|----|-------|-----------|-------|--------|----------------------|-------------|------------|--------|
| **P2-1** | Câbler LocalWatcher dans le daemon | `src/daemon.rs`, `src/watch/local.rs` | crate `notify`, config | `LocalWatcher` émet `WatchEvent` sur `tokio::mpsc` | Mode continu : événements FS déclenchent un cycle (sous verrou) | P0-1, P0-3 | Moyenne | ✅ |
| **P2-2** | Détection limite inotify | `src/watch/local.rs` | `/proc/sys/fs/inotify/max_user_watches` | Warning log + fallback polling si limite atteinte | Test avec mock de la valeur sysctl | P2-1 | Faible | ✅ détection, 🔴 fallback |
| **P2-3** | Boucle principale tokio::select! | `src/daemon.rs` | P2-1, P1-11 | `select!` sur : signal shutdown, timer sync périodique, événements watcher | Le binaire tourne, sync au démarrage, réagit aux events | P2-1, P1-11 | Moyenne | ✅ |
| **P2-4** | Exclusion mutuelle watcher/sync | `src/daemon.rs` | P2-3 | Un seul cycle de sync à la fois (`Semaphore(1)`) | Pas de sync concurrents sur le même store | P2-3 | Moyenne | ✅ |
| **P2-5** | Shutdown gracieux | `src/daemon.rs` | P2-3 | SIGINT/SIGTERM → `CancellationToken` → fin de boucle | Arrêt propre de la boucle daemon | P2-3 | Moyenne | ✅ |
| **P2-6** | Commande `status` enrichie | `src/main.rs` | P0-2, P0-8 | Affiche : dernier sync, fichiers suivis, page token, conversions, index, unité systemd | Données lues depuis `RedbStore` + config | P0-2, P0-8 | Faible | ✅ |

---

## Graphe de dépendances

```
P1-11 (engine complet) ──────────────────────┐
P2-1 (watcher câblé) ───→ P2-2 (inotify) ──┤
                                              ├──→ P2-3 (boucle select!)
                                              │        ├── P2-4 (exclusion mutuelle)
                                              │        └── P2-5 (shutdown gracieux)
P0-2, P0-8 ──────────────────────────────────→ P2-6 (status enrichi)
```

---

## Détail technique

### P2-3 : Boucle principale

L'architecture cible pour le mode daemon :

```rust
use tokio_util::sync::CancellationToken;

// Verrou logique : Semaphore(1) pour "un seul sync à la fois"
let sync_permit = Arc::new(tokio::sync::Semaphore::new(1));
let shutdown = CancellationToken::new();

// Enregistrer le handler SIGINT/SIGTERM
let shutdown_trigger = shutdown.clone();
tokio::spawn(async move {
    tokio::signal::ctrl_c().await.ok();
    shutdown_trigger.cancel();
});

loop {
    tokio::select! {
        // Signal de shutdown (SIGINT/SIGTERM)
        _ = shutdown.cancelled() => {
            tracing::info!("Shutdown requested, draining tasks...");
            break;
        }

        // Timer de sync périodique
        _ = sync_interval.tick() => {
            match sync_permit.clone().try_acquire_owned() {
                Ok(permit) => {
                    run_sync_cycle(&config, &client, &store, &redb).await?;
                    drop(permit);
                }
                Err(_) => {
                    tracing::debug!("Sync already in progress, skipping periodic trigger");
                }
            }
        }

        // Événements du watcher local
        Some(event) = watcher_rx.recv() => {
            event_buffer.push(event);
            // Déclencher un sync si pas déjà en cours
            if let Ok(permit) = sync_permit.clone().try_acquire_owned() {
                run_sync_cycle(&config, &client, &store, &redb).await?;
                event_buffer.clear();
                drop(permit);
            }
        }
    }
}
```

**Points clés** :
- Un seul sync à la fois via `Semaphore(1)` (pas `std::sync::Mutex` qui ne peut pas être `await`-ed)
- Les events watcher s'accumulent pendant un sync en cours
- Le timer se réarme après chaque tick, pas après chaque sync
- `CancellationToken` de `tokio_util::sync` propagé à tous les appels réseau et DB
- `tokio-util` est déjà dans `Cargo.toml` (la feature `sync` expose `CancellationToken`)

### P2-4 : Exclusion mutuelle

Le problème : le watcher peut détecter un changement local PENDANT que le sync engine est en train d'écrire (download). Cela provoquerait un faux positif (le fichier apparaît comme "modifié localement" alors que c'est le sync qui l'a écrit).

Solution :
1. Pendant l'exécution du sync, **ignorer les events watcher** pour les fichiers en cours de téléchargement
2. Le sync engine expose la liste des fichiers qu'il est en train de traiter
3. Après le sync, les events watcher restants sont filtrés pour exclure les fichiers que le sync vient de toucher

### P2-5 : Shutdown gracieux

```
SIGINT/SIGTERM reçu
    → CancellationToken.cancel()
    → Les tâches en vol vérifient le token et s'arrêtent proprement
    → Flush des writes en cours (atomic write = pas de corruption)
    → persist_to_redb() pour sauvegarder l'état
    → Exit 0
```

Le worst case (kill -9) est géré par la nature atomique des écritures :
- Les fichiers `.part` sont nettoyés au prochain démarrage (`cleanup_part_files`)
- redb est ACID : les transactions non commitées sont rollback

---

## Critères de complétion

- [x] `oxidrive sync` en mode continu : sync au démarrage + réaction aux changements locaux (`daemon.rs`)
- [x] Le timer de sync périodique fonctionne (`sync_interval_secs` de la config)
- [x] Un seul cycle de sync à la fois (pas de race condition côté daemon)
- [x] Shutdown gracieux : SIGINT → `CancellationToken` → sortie de boucle
- [x] La base de données reste cohérente après un shutdown pendant un sync (écritures atomiques + redb)
- [x] Le watcher détecte correctement création, modification, suppression, renommage
- [x] Sur Linux : warning si la limite inotify est trop basse, avec suggestion de commande sysctl
- [x] `oxidrive status` affiche l'état utile depuis RedbStore (sync, fichiers suivis, jeton, conversions) + config + unité systemd
- [ ] `oxidrive status` : état détaillé du watcher et file d'événements en attente (non exposé aujourd’hui)

→ Suivant : [Phase 3 — Conversion Google Workspace](phase3-workspace.md)
