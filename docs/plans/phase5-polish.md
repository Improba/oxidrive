# Phase 5 — Polish, service, cross-compilation

## Objective

Transform oxidrive into a **production-ready daily tool**: system service, progress bars, advanced logging, CI/CD, cross-platform static binaries, operational documentation.

---

## Current status: ✅ COMPLETE

| Component | Status | Detail |
|-----------|--------|--------|
| systemd service | ✅ | `service.rs`: install / uninstall / start / stop (user unit) |
| Windows scheduled task | ✅ | `service.rs`: install / uninstall / start / stop via `schtasks.exe` |
| Advanced logging | ✅ | tracing console + env-filter + `config.log_level` + rolling JSON file via `tracing-appender` |
| Progress reporting | ✅ | `indicatif` in `executor.rs` (interactive mode) |
| CI GitHub Actions | ✅ | `.github/workflows/ci.yml` (fmt, clippy, test, build release) — triggered on `vX.Y.Z` tags |
| musl cross-compilation | ✅ | Validated via `.github/workflows/release.yml` (`x86_64-unknown-linux-musl`) |
| Release packaging | ✅ | `.github/workflows/release.yml` on `v*` tags (archives + SHA256) |
| README documentation | ✅ | README.md |
| docs/ documentation | ✅ | architecture/, conventions/, plans/ |
| Warning cleanup | ✅ | `cargo clippy -- -D warnings` clean |

---

## Prerequisites

- Phases 1-4 substantially complete
- Integration tests covering main scenarios

---

## Task matrix

| ID | Task | File(s) | Input | Output | Completion criteria | Dependencies | Complexity | Status |
|----|------|---------|-------|--------|---------------------|--------------|------------|--------|
| **P5-1** | systemd service | `src/service.rs`, `src/main.rs` | — | `oxidrive service install/uninstall/start/stop` | Functional systemd user service | P2-3 | Low–Medium | ✅ |
| **P5-1b** | Windows scheduled task | `src/service.rs` | `schtasks` API | `oxidrive service install/uninstall` Windows | Logon task functional | P2-3 | Low–Medium | ✅ |
| **P5-2** | Advanced logging | `src/logging.rs` | P0-10 | Rolling JSON file, per-module levels, compact format | File logs parseable, console readable | P0-10 | Low | ✅ |
| **P5-3** | Progress reporting | `src/sync/executor.rs` | crate `indicatif` | Progress bar / sync counters | UX in interactive mode | P2-3 | Low | ✅ |
| **P5-4** | CI GitHub Actions | `.github/workflows/ci.yml` | — | Build + test + clippy + fmt on Linux/macOS/Windows | Green CI on 3 OS (triggered on vX.Y.Z tags) | P0-1 | Low | ✅ |
| **P5-5** | musl cross-compilation | `Cargo.toml`, `.github/workflows/release.yml` | — | Release build `x86_64-unknown-linux-musl` | musl binary in releases | P0-1 | Low | ✅ |
| **P5-6** | Release packaging | `.github/workflows/release.yml` | P5-4, P5-5 | GitHub Releases: Linux/macOS/Windows + checksums | Downloadable | P5-4, P5-5 | Low | ✅ |
| **P5-7** | Operational documentation | `docs/`, `README.md` | — | Troubleshooting guide, FAQ, Google credentials instructions | Readable and useful | All | Low | ✅ |
| **P5-8** | Warning cleanup | `src/**/*.rs` | — | Remove `#[allow(unused)]` and dead code | `cargo clippy` with no warnings | All | Low | ✅ |

---

## Dependency graph

```
P2-3 (main loop) ──→ P5-1 (systemd)
                      P5-1b (Windows)
                      P5-3 (progress)

P0-10 (tracing) ────────→ P5-2 (advanced logging)

P0-1 ──→ P5-4 (CI) ──→ P5-5 (musl) ──→ P5-6 (releases)

All ──→ P5-7 (docs) + P5-8 (cleanup)
```

**Parallelizable**: P5-1/P5-1b, P5-2, P5-3, P5-4 are independent.

---

## Technical detail

### P5-1: systemd service

Template unit to generate in `~/.config/systemd/user/oxidrive.service`:

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

> **Note**: `ExecStart=%h/.cargo/bin/oxidrive` assumes installation via `cargo install`. For binaries from GitHub Releases, the path will differ. The `service install` command should detect the current binary path via `std::env::current_exe()`.

Commands:
- `oxidrive service install` → write the file + `systemctl --user daemon-reload` + `systemctl --user enable oxidrive`
- `oxidrive service start` → `systemctl --user start oxidrive`
- `oxidrive service stop` → `systemctl --user stop oxidrive`
- `oxidrive service uninstall` → `systemctl --user disable oxidrive` + remove the file + daemon-reload

### P5-3: Progress reporting with indicatif

```rust
use indicatif::{ProgressBar, ProgressStyle, MultiProgress};

// During local scan
let pb = ProgressBar::new_spinner();
pb.set_style(ProgressStyle::default_spinner().template("{spinner:.green} Scanning {msg}")?);
pb.set_message(format!("{} files scanned", count));

// During action execution
let pb = ProgressBar::new(actions.len() as u64);
pb.set_style(ProgressStyle::default_bar()
    .template("{spinner:.green} [{bar:40.cyan/blue}] {pos}/{len} {msg}")?);

// In non-interactive mode (pipe, file), disable automatically
if !std::io::stdout().is_terminal() {
    pb.set_draw_target(ProgressDrawTarget::hidden());
}
```

### P5-4: CI GitHub Actions

See `.github/workflows/ci.yml` at the repository root. Triggered on `vX.Y.Z` version tags (not on push/PR to `main`).

### P5-5: musl cross-compilation

The **Release** workflow compiles `x86_64-unknown-linux-musl` with `musl-tools` on Ubuntu. Local verification is possible with:

```bash
rustup target add x86_64-unknown-linux-musl
sudo apt-get install -y musl-tools
cargo build --release --target x86_64-unknown-linux-musl
```

### P5-6: Release packaging

Workflow `.github/workflows/release.yml` on `v*` tags:
1. Build for `x86_64-unknown-linux-musl`, `x86_64-apple-darwin`, `aarch64-apple-darwin`, `x86_64-pc-windows-msvc`
2. Release profile: `strip` + LTO (see `Cargo.toml`)
3. Archives (`tar.gz` Linux/macOS, `zip` Windows)
4. Upload to GitHub Releases with `checksums-sha256.txt`

---

## Completion criteria

- [x] `oxidrive service install/start/stop/uninstall` works on Linux (systemd user)
- [x] `oxidrive service install/start/stop/uninstall` works on Windows (schtasks)
- [x] Progress bars display in interactive mode when integrated (`executor.rs`)
- [x] Rolling JSON file logging works with rotation (`logging.rs` + `tracing-appender`)
- [x] Green CI on Linux, macOS, Windows
- [x] musl binary produced by the release workflow
- [x] Release workflow: archives + checksums on `v*` tags
- [x] `cargo clippy -- -D warnings` passes in CI
- [x] Operational documentation: troubleshooting, FAQ, credentials guide
- [x] README up to date with basic instructions

---

## Beyond Phase 5

Potential features beyond the initial scope:

- **Multi-account support**: sync multiple Drive accounts
- **Selective sync**: choose which Drive folders to sync
- **TUI**: real-time dashboard with `ratatui`
- **Notifications**: desktop notifications on conflicts or errors
- **Encryption at rest**: encrypt local files
- **OneDrive/Dropbox support**: storage backend abstraction
- **Streaming upload/download**: avoid loading large files entirely in memory (currently `tokio::fs::read` = all in RAM)
