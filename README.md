# oxidrive

**oxidrive** est un outil en ligne de commande écrit en Rust pour la **synchronisation bidirectionnelle** avec Google Drive. Il prend en charge la conversion des formats Google Workspace, un index Markdown optionnel et une exécution en service système pour une synchronisation continue.

[![CI](https://img.shields.io/badge/CI-GitHub_Actions-blue)](.github/workflows/ci.yml)
[![Licence](https://img.shields.io/badge/licence-MIT-blue.svg)](LICENSE)

---

## Fonctionnalités

- **Synchronisation bidirectionnelle** entre un dossier local et un dossier Google Drive (ou la racine du compte).
- **Conversion Google Workspace** : export et import des documents natifs (Docs, Sheets, etc.) vers des formats utilisables localement.
- **Index Markdown** : génération et mise à jour d’un index pour faciliter la recherche sur les fichiers texte / Markdown exportés.
- **Watcher temps réel** : surveillance du système de fichiers avec déclenchement de synchronisation après stabilisation des événements (debounce).
- **Résolution de conflits** configurable (`local_wins`, `remote_wins`, renommage).
- **Binaire unique** : compilation statique possible (TLS via `rustls`) pour un déploiement simple sur Linux et autres plateformes cibles.

---

## Installation

### Depuis les sources (Cargo)

Prérequis : [Rust](https://www.rust-lang.org/tools/install) (édition 2021 ou supérieure).

```bash
git clone https://github.com/your-org/oxidrive.git
cd oxidrive
cargo build --release
```

Le binaire se trouve dans `target/release/oxidrive`.

### Depuis les releases binaires

En poussant un **tag** de version `v*` (ex. `v0.1.0`), le workflow [`.github/workflows/release.yml`](.github/workflows/release.yml) construit des binaires pour Linux (musl), macOS (x86_64 et Apple Silicon) et Windows, publie les archives sur la page **Releases** du dépôt et joint un fichier `checksums-sha256.txt`. Téléchargez l’archive correspondant à votre plateforme, vérifiez les sommes si besoin, extrayez `oxidrive` (ou `oxidrive.exe` sous Windows) et placez-le dans un répertoire présent dans votre `PATH`.

---

## Configuration

La configuration est chargée depuis un fichier **TOML** (recommandé) ou **JSON**. Par défaut, le programme cherche `config.toml` dans le répertoire courant ; vous pouvez forcer un chemin avec `--config`.

Copiez le fichier d’exemple et adaptez-le :

```bash
cp config.example.toml config.toml
```

### Exemple (`config.toml`)

```toml
# Dossier local à synchroniser avec Google Drive (obligatoire).
sync_dir = "/home/vous/DriveSync"

# Optionnel : ID du dossier Drive (extrait de l’URL du navigateur).
# drive_folder_id = "1BxiMVs0XRA5nFMdKvBdBZjgmUUqptlbs74OgvE2upms"

# Intervalle entre deux synchronisations en mode service (secondes).
sync_interval_secs = 300

# Politique de conflit : "local_wins", "remote_wins", ou renommage.
conflict_policy = "local_wins"
# conflict_policy = { rename = { suffix = "_remote" } }

max_concurrent_uploads = 4
max_concurrent_downloads = 4

ignore_patterns = [
  ".DS_Store",
  "*.tmp",
  ".oxidrive/**",
]

# index_dir = "/home/vous/.cache/oxidrive/index"

log_level = "info"
debounce_ms = 2000
```

Les options détaillées sont documentées dans `config.example.toml` à la racine du projet.

---

## Usage

Options globales utiles :

- `--config PATH` : fichier de configuration.
- `-v` / `-vv` : augmenter la verbosité des logs (`tracing`).
- `--quiet` : réduire le bruit (prioritaire sur `-v`).

### `oxidrive setup`

Initialise l’**authentification OAuth2** avec Google (flux navigateur / jetons stockés localement). À exécuter une fois par machine ou compte.

### `oxidrive sync`

Exécute **un cycle de synchronisation** complet. Avec `--dry-run`, les actions sont planifiées et journalisées sans modifier les fichiers locaux ni distants.

Avec `--once`, un seul cycle est exécuté puis le programme se termine (utile pour les tâches planifiées externes ou le débogage).

### `oxidrive status`

Affiche l’**état** de la synchronisation (dossier configuré, derniers enregistrements en base locale, etc.).

### `oxidrive service`

Gestion du **service d'arrière-plan** pour une synchronisation périodique selon `sync_interval_secs`.

| Plateforme | Backend | Commandes |
|------------|---------|-----------|
| Linux | systemd (user unit) | `oxidrive service install/start/stop/uninstall` |
| macOS | launchd (LaunchAgent) | `oxidrive service install/start/stop/uninstall` |
| Windows | *(non supporté)* | Utilisez le Planificateur de tâches manuellement |

```bash
oxidrive service install
oxidrive service start
```

---

## Architecture

Le code est organisé en modules Rust principaux :

| Module | Rôle |
|--------|------|
| **`drive/`** | Client HTTP Google Drive (liste, téléchargement, upload, suivi des changements). |
| **`sync/`** | Décision de réconciliation, scan local/distant, exécution des actions, gestion des conflits. |
| **`watch/`** | Surveillance du dossier local (`notify`) et déclenchement contrôlé des syncs. |
| **`store/`** | Persistance de l’état (métadonnées par fichier, identifiants Drive) via **redb**. |
| **`index/`** | Construction et mise à jour de l’index Markdown / recherche. |
| **`utils/`** | Hachage, FS, retry, helpers partagés. |

Pour une vue détaillée : [docs/architecture/overview.md](docs/architecture/overview.md).

---

## Développement

```bash
# Compilation debug
cargo build

# Tests unitaires et d’intégration
cargo test

# Analyse statique (recommandé avant commit)
cargo clippy --all-targets -- -D warnings
```

Les conventions du projet sont décrites dans [docs/conventions/code-style.md](docs/conventions/code-style.md) et [docs/conventions/git-workflow.md](docs/conventions/git-workflow.md).

### Publication d’une release

1. Mettre à jour la version dans `Cargo.toml` si nécessaire.
2. Créer et pousser un tag annoté ou léger : `git tag v0.1.0 && git push origin v0.1.0`.
3. Le workflow **Release** attache les binaires et `checksums-sha256.txt` à une release GitHub (notes générées automatiquement).

---

## Licence

Ce projet est distribué sous licence **MIT**. Voir le fichier `LICENSE` à la racine du dépôt.
