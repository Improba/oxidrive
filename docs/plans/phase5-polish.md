# Phase 5 — Polish, service, cross-compilation

## Objectif

Transformer oxidrive en un **outil prêt pour l'utilisation quotidienne** : service systemd, barres de progression, logging avancé, CI/CD, binaires statiques multi-plateforme, documentation opérationnelle.

---

## Statut actuel : 🟡 EN COURS (Windows schtasks, logging fichier, cleanup)

| Composant | État | Détail |
|-----------|------|--------|
| Service systemd | ✅ | `service.rs` : install / uninstall / start / stop (user unit) |
| Tâche planifiée Windows | 🔴 | `schtasks` non implémenté (P5-1b) |
| Logging avancé | 🟡 | tracing console + env-filter + `config.log_level` ; rotation fichier JSON à finaliser |
| Progress reporting | ✅ | `indicatif` dans `executor.rs` (mode interactif) |
| CI GitHub Actions | ✅ | `.github/workflows/ci.yml` (fmt, clippy, test, build release) |
| Cross-compilation musl | 🟡 | Validée via `.github/workflows/release.yml` (`x86_64-unknown-linux-musl`) |
| Release packaging | ✅ | `.github/workflows/release.yml` sur tags `v*` (archives + SHA256) |
| Documentation README | ✅ | README.md |
| Documentation docs/ | ✅ | architecture/, conventions/, plans/ |
| Nettoyage warnings | 🟡 | En cours |

---

## Prérequis

- Phases 1-4 substantiellement complétées
- Tests d'intégration couvrant les scénarios principaux

---

## Matrice de tâches

| ID | Tâche | Fichier(s) | Input | Output | Critère de complétion | Dépendances | Complexité | Statut |
|----|-------|-----------|-------|--------|----------------------|-------------|------------|--------|
| **P5-1** | Service systemd | `src/service.rs`, `src/main.rs` | — | `oxidrive service install/uninstall/start/stop` | Service user systemd fonctionnel | P2-3 | Faible–Moyenne | ✅ |
| **P5-1b** | Tâche planifiée Windows | `src/main.rs` | API `schtasks` | `oxidrive service install/uninstall` Windows | Tâche au login fonctionnelle | P2-3 | Faible–Moyenne | 🔴 |
| **P5-2** | Logging avancé | `src/main.rs` | P0-10 | Rotation fichier JSON, niveaux par module, format compact | Logs fichier parseable, console lisible | P0-10 | Faible | 🟡 |
| **P5-3** | Progress reporting | `src/sync/executor.rs` | crate `indicatif` | Barre de progression / compteurs sync | UX en mode interactif | P2-3 | Faible | ✅ |
| **P5-4** | CI GitHub Actions | `.github/workflows/ci.yml` | — | Build + test + clippy + fmt sur Linux/macOS/Windows | CI verte sur 3 OS | P0-1 | Faible | ✅ |
| **P5-5** | Cross-compilation musl | `Cargo.toml`, `.github/workflows/release.yml` | — | Build release `x86_64-unknown-linux-musl` | Binaire musl dans les releases | P0-1 | Faible | 🟡 |
| **P5-6** | Release packaging | `.github/workflows/release.yml` | P5-4, P5-5 | GitHub Releases : Linux/macOS/Windows + checksums | Téléchargeables | P5-4, P5-5 | Faible | ✅ |
| **P5-7** | Documentation opérationnelle | `docs/`, `README.md` | — | Guide dépannage, FAQ, instructions credentials Google | Lisible et utile | Tout | Faible | 🟡 |
| **P5-8** | Nettoyage warnings | `src/**/*.rs` | — | Suppression des `#[allow(unused)]` et code mort | `cargo clippy` sans avertissements | Tout | Faible | 🟡 |

---

## Graphe de dépendances

```
P2-3 (boucle principale) ──→ P5-1 (systemd)
                              P5-1b (Windows)
                              P5-3 (progress)

P0-10 (tracing) ────────→ P5-2 (logging avancé)

P0-1 ──→ P5-4 (CI) ──→ P5-5 (musl) ──→ P5-6 (releases)

Tout ──→ P5-7 (docs) + P5-8 (cleanup)
```

**Parallélisables** : P5-1/P5-1b, P5-2, P5-3, P5-4 sont indépendants.

---

## Détail technique

### P5-1 : Service systemd

Template unit à générer dans `~/.config/systemd/user/oxidrive.service` :

```ini
[Unit]
Description=oxidrive - Google Drive bidirectional sync
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart=%h/.cargo/bin/oxidrive sync --config %h/.config/oxidrive/config.toml
Restart=on-failure
RestartSec=30
Environment=RUST_LOG=info

[Install]
WantedBy=default.target
```

> **Note** : `ExecStart=%h/.cargo/bin/oxidrive` suppose une installation via `cargo install`. Pour les binaires depuis GitHub Releases, le chemin sera différent. La commande `service install` devrait détecter le chemin du binaire courant via `std::env::current_exe()`.

Commandes :
- `oxidrive service install` → écrire le fichier + `systemctl --user daemon-reload` + `systemctl --user enable oxidrive`
- `oxidrive service start` → `systemctl --user start oxidrive`
- `oxidrive service stop` → `systemctl --user stop oxidrive`
- `oxidrive service uninstall` → `systemctl --user disable oxidrive` + supprimer le fichier + daemon-reload

### P5-3 : Progress reporting avec indicatif

```rust
use indicatif::{ProgressBar, ProgressStyle, MultiProgress};

// Pendant le scan local
let pb = ProgressBar::new_spinner();
pb.set_style(ProgressStyle::default_spinner().template("{spinner:.green} Scanning {msg}")?);
pb.set_message(format!("{} files scanned", count));

// Pendant l'exécution des actions
let pb = ProgressBar::new(actions.len() as u64);
pb.set_style(ProgressStyle::default_bar()
    .template("{spinner:.green} [{bar:40.cyan/blue}] {pos}/{len} {msg}")?);

// En mode non-interactif (pipe, fichier), désactiver automatiquement
if !std::io::stdout().is_terminal() {
    pb.set_draw_target(ProgressDrawTarget::hidden());
}
```

### P5-4 : CI GitHub Actions

Voir `.github/workflows/ci.yml` à la racine du dépôt (push/PR sur `main`).

### P5-5 : Cross-compilation musl

Le workflow **Release** compile `x86_64-unknown-linux-musl` avec `musl-tools` sur Ubuntu. Vérification locale possible avec :

```bash
rustup target add x86_64-unknown-linux-musl
sudo apt-get install -y musl-tools
cargo build --release --target x86_64-unknown-linux-musl
```

### P5-6 : Release packaging

Workflow `.github/workflows/release.yml` sur tags `v*` :
1. Build pour `x86_64-unknown-linux-musl`, `x86_64-apple-darwin`, `aarch64-apple-darwin`, `x86_64-pc-windows-msvc`
2. Profil release : `strip` + LTO (voir `Cargo.toml`)
3. Archives (`tar.gz` Linux/macOS, `zip` Windows)
4. Upload dans GitHub Releases avec `checksums-sha256.txt`

---

## Critères de complétion

- [x] `oxidrive service install/start/stop/uninstall` fonctionne sur Linux (systemd user)
- [x] Les barres de progression s'affichent en mode interactif quand intégrées (`executor.rs`)
- [ ] Le logging fichier JSON fonctionne avec rotation
- [x] CI verte sur Linux, macOS, Windows
- [x] Binaire musl produit par le workflow release
- [x] Workflow release : archives + checksums sur tags `v*`
- [x] `cargo clippy -- -D warnings` passe en CI
- [ ] Documentation opérationnelle : dépannage, FAQ, guide credentials (à enrichir)
- [x] README à jour avec instructions de base

---

## Au-delà de la Phase 5

Fonctionnalités potentielles hors scope initial :

- **Support multi-comptes** : synchroniser plusieurs comptes Drive
- **Sync sélective** : choisir quels dossiers Drive synchroniser
- **Interface TUI** : dashboard temps réel avec `ratatui`
- **Notifications** : desktop notifications sur conflits ou erreurs
- **Encryption at rest** : chiffrer les fichiers locaux
- **Support OneDrive/Dropbox** : abstraction du backend de stockage
- **Streaming upload/download** : éviter de charger les gros fichiers entièrement en mémoire (actuellement `tokio::fs::read` = tout en RAM)
- **Support multi-comptes** : synchroniser plusieurs comptes Drive en parallèle
