# oxidrive

**Your Google Drive, local. Synced. Automatically.**

oxidrive is a Rust CLI that turns a folder on your machine into a bidirectional mirror of Google Drive. Change a file locally and it uploads to Drive. Change it in the web UI and it downloads. Google Docs, Sheets, and Slides are automatically converted to standard office formats (`.docx`, `.xlsx`, `.pptx`) so you can open, edit, and version them with your usual tools—without ever opening a browser.

A single binary, no external dependencies, zero cloud configuration to maintain.

[CI](.github/workflows/ci.yml)
[License](LICENSE)

---

## Why oxidrive?

- **Real offline work**—edit your Drive files without a connection; sync catches up when the network returns.
- **Google Workspace → open formats**—Docs become `.docx`, Sheets `.xlsx`, Slides `.pptx`, Drawings `.svg`. No more tie-in to the web editor.
- **Smart change detection**—a 12-case decision matrix comparing MD5 checksums, timestamps, and metadata so only what’s needed is transferred.
- **Conflicts handled, not ignored**—three policies: local wins, remote wins, or automatic rename with a timestamped suffix.
- **Real-time monitoring**—an inotify/kqueue watcher detects local changes and triggers sync after debounce. Automatic polling fallback if system limits are hit.
- **Markdown index**—automatic extraction of text from PDF, DOCX, XLSX, PPTX, and CSV into a browsable index folder for `grep` or any search tool.
- **Built-in system service**—`oxidrive service install` and you're set: systemd (Linux), launchd (macOS), or Task Scheduler (Windows), with automatic restart on failure.
- **Single binary, zero runtime**—static build via `rustls`, deployable by simple copy on Linux, macOS, and Windows.

---

## Quick start

```bash
# 1. Build
git clone https://github.com/Improba/oxidrive.git
cd oxidrive
cargo build --release

# 2. Configure
cp config.example.toml config.toml
# → Fill in client_id, client_secret, and sync_dir

# 3. Authenticate
./target/release/oxidrive setup

# 4. Sync
./target/release/oxidrive sync --once
```

---

## Installation

### From source (Cargo)

Prerequisites: [Rust](https://www.rust-lang.org/tools/install) (2021 edition or newer).

```bash
git clone https://github.com/Improba/oxidrive.git
cd oxidrive
cargo build --release
```

The binary is at `target/release/oxidrive`.

### From binary releases

Pushing a `**v***` version tag (e.g. `v0.1.0`) runs the `[.github/workflows/release.yml](.github/workflows/release.yml)` workflow, which builds binaries for Linux (musl), macOS (x86_64 and Apple Silicon), and Windows, publishes archives on the repo **Releases** page, and attaches a `checksums-sha256.txt` file. Download the archive for your platform, verify checksums if you like, extract `oxidrive` (or `oxidrive.exe` on Windows), and put it in a directory on your `PATH`.

---

## Configuration

Configuration is loaded from a **TOML** (recommended) or **JSON** file. By default the program looks for `config.toml` in the current directory; you can force a path with `--config`.

Copy the example file and adjust it:

```bash
cp config.example.toml config.toml
```

### Example (`config.toml`)

```toml
# Local folder to sync with Google Drive (required).
sync_dir = "/home/user/DriveSync"

# Optional: Drive folder ID (from the browser URL).
# drive_folder_id = "1BxiMVs0XRA5nFMdKvBdBZjgmUUqptlbs74OgvE2upms"

# Interval between sync cycles in service mode (seconds).
sync_interval_secs = 300

# Conflict policy: "local_wins", "remote_wins", or rename.
conflict_policy = "local_wins"
# conflict_policy = { rename = { suffix = "_remote" } }

max_concurrent_uploads = 4
max_concurrent_downloads = 4

ignore_patterns = [
  ".DS_Store",
  "*.tmp",
  ".oxidrive/**",
]

# index_dir = "/home/user/.cache/oxidrive/index"

log_level = "info"
debounce_ms = 2000
```

Full options are documented in `config.example.toml` at the project root.

---

## Usage

Useful global options:

- `--config PATH`: configuration file.
- `-v` / `-vv`: increase log verbosity (`tracing`).
- `--quiet`: less noise (overrides `-v`).

### `oxidrive setup`

Sets up **OAuth2** authentication with Google (browser flow / tokens stored locally). Run once per machine or account.

### `oxidrive sync`

Runs one full **sync cycle**. With `--dry-run`, actions are planned and logged without changing local or remote files.

With `--once`, a single cycle runs and the program exits (handy for external schedulers or debugging).

Without `--once` and with `sync_interval_secs > 0`, oxidrive runs as a **daemon**: it syncs in a loop, watches the folder in real time, and shuts down cleanly on `SIGINT`/`SIGTERM`.

### `oxidrive status`

Shows **sync status**: active configuration, last sync, tracked file count, Workspace conversions, service state.

### `oxidrive service`

Manages the **background service** for periodic sync according to `sync_interval_secs`.


| Platform | Backend               | Commands                                        |
| -------- | --------------------- | ----------------------------------------------- |
| Linux    | systemd (user unit)   | `oxidrive service install/start/stop/uninstall` |
| macOS    | launchd (LaunchAgent) | `oxidrive service install/start/stop/uninstall` |
| Windows  | Task Scheduler (schtasks) | `oxidrive service install/start/stop/uninstall` |


```bash
oxidrive service install
oxidrive service start
```

---

## Architecture

The code is organized into main Rust modules:


| Module       | Role                                                                              |
| ------------ | --------------------------------------------------------------------------------- |
| `**drive/**` | Google Drive HTTP client (list, download, upload, change tracking).               |
| `**sync/**`  | Reconciliation decisions, local/remote scan, action execution, conflict handling. |
| `**watch/**` | Local folder monitoring (`notify`) and controlled sync triggers.                  |
| `**store/**` | State persistence (per-file metadata, Drive IDs) via **redb**.                    |
| `**index/`** | Building and updating the Markdown / search index.                                |
| `**utils/**` | Hashing, FS, retry, shared helpers.                                               |


For more detail: [docs/architecture/overview.md](docs/architecture/overview.md).

---

## Development

```bash
# Debug build
cargo build

# Unit and integration tests (80 tests)
cargo test

# Static analysis (recommended before commit)
cargo clippy --all-targets -- -D warnings
```

Project conventions are described in [docs/conventions/code-style.md](docs/conventions/code-style.md) and [docs/conventions/git-workflow.md](docs/conventions/git-workflow.md).

### Publishing a release

Use the provided script to bump the version, commit, and create a tag:

```bash
./create-tag.sh          # patch bump: 0.1.0 → 0.1.1
./create-tag.sh minor    # minor bump: 0.1.0 → 0.2.0
./create-tag.sh major    # major bump: 0.1.0 → 1.0.0
```

Then push:

```bash
git push && git push origin v<new-version>
```

The **CI** workflow runs on version tags (`vX.Y.Z`), and the **Release** workflow attaches binaries and `checksums-sha256.txt` to a GitHub release.

---

## License

This project is licensed under the **MIT License**. See the [LICENSE](LICENSE) file for details.