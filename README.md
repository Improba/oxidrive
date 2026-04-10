# oxidrive

**Votre Google Drive, en local. Synchronisé. Automatiquement.**

oxidrive est un CLI Rust qui transforme un dossier de votre machine en miroir bidirectionnel de Google Drive. Modifiez un fichier localement, il remonte sur Drive. Modifiez-le dans l'interface web, il redescend. Les Google Docs, Sheets et Slides sont automatiquement convertis en formats bureautiques standard (`.docx`, `.xlsx`, `.pptx`) pour que vous puissiez les ouvrir, les éditer et les versionner avec vos outils habituels — sans jamais ouvrir un navigateur.

Un seul binaire, aucune dépendance externe, zéro configuration cloud à maintenir.

[![CI](https://img.shields.io/badge/CI-GitHub_Actions-blue)](.github/workflows/ci.yml)
[![Licence](https://img.shields.io/badge/licence-MIT-blue.svg)](LICENSE)

---

## Pourquoi oxidrive ?

- **Travail hors-ligne réel** — éditez vos fichiers Drive sans connexion, la sync rattrape au retour du réseau.
- **Google Workspace → formats ouverts** — Docs devient `.docx`, Sheets `.xlsx`, Slides `.pptx`, Drawings `.svg`. Plus de dépendance à l'éditeur web.
- **Détection de changements intelligente** — matrice de décision 12 cas comparant checksums MD5, timestamps et métadonnées pour ne transférer que le strict nécessaire.
- **Conflits gérés, pas ignorés** — trois politiques au choix : local gagne, remote gagne, ou renommage automatique avec suffixe horodaté.
- **Surveillance temps réel** — un watcher inotify/kqueue détecte les modifications locales et déclenche la sync après debounce. Fallback polling automatique si les limites système sont atteintes.
- **Index Markdown** — extraction automatique du texte des PDF, DOCX, XLSX, PPTX et CSV vers un dossier d'index consultable avec `grep` ou tout outil de recherche.
- **Service système intégré** — `oxidrive service install` et c'est parti : systemd (Linux) ou launchd (macOS), avec redémarrage automatique en cas d'erreur.
- **Binaire unique, zéro runtime** — compilation statique via `rustls`, déployable par simple copie sur Linux, macOS et Windows.

---

## Démarrage rapide

```bash
# 1. Compiler
git clone https://github.com/Improba/oxidrive.git
cd oxidrive
cargo build --release

# 2. Configurer
cp config.example.toml config.toml
# → Renseigner client_id, client_secret et sync_dir

# 3. S'authentifier
./target/release/oxidrive setup

# 4. Synchroniser
./target/release/oxidrive sync --once
```

---

## Installation

### Depuis les sources (Cargo)

Prérequis : [Rust](https://www.rust-lang.org/tools/install) (édition 2021 ou supérieure).

```bash
git clone https://github.com/Improba/oxidrive.git
cd oxidrive
cargo build --release
```

Le binaire se trouve dans `target/release/oxidrive`.

### Depuis les releases binaires

En poussant un **tag** de version `v*` (ex. `v0.1.0`), le workflow [`.github/workflows/release.yml`](.github/workflows/release.yml) construit des binaires pour Linux (musl), macOS (x86_64 et Apple Silicon) et Windows, publie les archives sur la page **Releases** du dépôt et joint un fichier `checksums-sha256.txt`. Téléchargez l'archive correspondant à votre plateforme, vérifiez les sommes si besoin, extrayez `oxidrive` (ou `oxidrive.exe` sous Windows) et placez-le dans un répertoire présent dans votre `PATH`.

---

## Configuration

La configuration est chargée depuis un fichier **TOML** (recommandé) ou **JSON**. Par défaut, le programme cherche `config.toml` dans le répertoire courant ; vous pouvez forcer un chemin avec `--config`.

Copiez le fichier d'exemple et adaptez-le :

```bash
cp config.example.toml config.toml
```

### Exemple (`config.toml`)

```toml
# Dossier local à synchroniser avec Google Drive (obligatoire).
sync_dir = "/home/vous/DriveSync"

# Optionnel : ID du dossier Drive (extrait de l'URL du navigateur).
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

Initialise l'**authentification OAuth2** avec Google (flux navigateur / jetons stockés localement). À exécuter une fois par machine ou compte.

### `oxidrive sync`

Exécute **un cycle de synchronisation** complet. Avec `--dry-run`, les actions sont planifiées et journalisées sans modifier les fichiers locaux ni distants.

Avec `--once`, un seul cycle est exécuté puis le programme se termine (utile pour les tâches planifiées externes ou le débogage).

Sans `--once` et avec `sync_interval_secs > 0`, oxidrive passe en **mode daemon** : il synchronise en boucle, surveille le dossier en temps réel et s'arrête proprement sur `SIGINT`/`SIGTERM`.

### `oxidrive status`

Affiche l'**état** de la synchronisation : configuration active, dernière sync, nombre de fichiers suivis, conversions Workspace, état du service.

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
| **`store/`** | Persistance de l'état (métadonnées par fichier, identifiants Drive) via **redb**. |
| **`index/`** | Construction et mise à jour de l'index Markdown / recherche. |
| **`utils/`** | Hachage, FS, retry, helpers partagés. |

Pour une vue détaillée : [docs/architecture/overview.md](docs/architecture/overview.md).

---

## Développement

```bash
# Compilation debug
cargo build

# Tests unitaires et d'intégration (146 tests)
cargo test

# Analyse statique (recommandé avant commit)
cargo clippy --all-targets -- -D warnings
```

Les conventions du projet sont décrites dans [docs/conventions/code-style.md](docs/conventions/code-style.md) et [docs/conventions/git-workflow.md](docs/conventions/git-workflow.md).

### Publication d'une release

1. Mettre à jour la version dans `Cargo.toml` si nécessaire.
2. Créer et pousser un tag annoté ou léger : `git tag v0.1.0 && git push origin v0.1.0`.
3. Le workflow **Release** attache les binaires et `checksums-sha256.txt` à une release GitHub (notes générées automatiquement).

---

## Licence

Ce projet est distribué sous licence **MIT**. Voir le fichier `LICENSE` à la racine du dépôt.
