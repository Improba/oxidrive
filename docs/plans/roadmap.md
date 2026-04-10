# oxidrive Roadmap

Complete development vision for **oxidrive** — bidirectional Google Drive sync in Rust.

Durations are **indicative** for one main contributor; they depend on available time and the complexity of real-world Drive edge cases.

---

## Overview

```
Phase 0 ✅   Phase 1 🟡   Phase 2 ✅    Phase 3 ✅    Phase 4 ✅    Phase 5 ✅
Scaffold     Sync base    Watcher      Workspace    Index MD     Polish
  CLI          Engine       inotify      Export       .docx→md     systemd+schtasks
  Config       Decision     select!      Import       .xlsx→md     CI/CD
  Auth         Executor     Shutdown     Conversions  .pdf→md      musl
  Store        Drive API    Status       Table conv   Integration  Releases
  Types        Persistence               Drawings     txt/binaries Progress
```

---

## Progress by phase

| Phase | Description | Tasks | Done | Remaining | Status |
|-------|------------|-------|------|-----------|--------|
| **0** | Scaffold, CLI, Config, Auth | 11 | 11 | 0 | ✅ Complete |
| **1** | Basic bidirectional sync | 13 | 13 | 0 | 🟡 Code complete (wiremock tests in progress) |
| **2** | Local watcher + real-time sync | 6 | 6 | 0 | ✅ Complete |
| **3** | Google Workspace conversion | 8 | 8 | 0 | ✅ Complete |
| **4** | Markdown index | 10 | 10 | 0 | ✅ Complete |
| **5** | Polish, service, cross-compilation | 9 | 9 | 0 | ✅ Complete |
| **Total** | | **57** | **57** | **0** | **100% done** |

---

## Phase 0 — Scaffolding, CLI, configuration, authentication ✅

**Content**: crate structure, clap CLI, TOML/JSON config loading, tracing with config.log_level, Google OAuth2 (loopback PKCE), account info display, shared types, store module (redb + session), utilities (retry, fs, hash).

**Actual duration**: ~1 session (scaffold + review + fixes).

**Details**: [phase0-scaffold.md](phase0-scaffold.md)

---

## Phase 1 — Basic bidirectional sync 🟡

**Content**:
- Full Drive client: recursive listing, Changes API, download, upload, name deduplication
- Local scan with MD5 and ignore patterns
- Exhaustive decision matrix (12 cases) with applied ConflictPolicy
- Parallel executor (JoinSet + Semaphore) with conflict resolution
- Sync engine: orchestration scan → list → decide → execute → persist
- Incremental sync via Changes API + persisted page token
- RedbStore ↔ Session persistence (bincode)
- Functional dry-run
- Folder management: `create_folder`, `trash_folder`, `ensure_folder_hierarchy` wired in the engine

**Details**: [phase1-sync.md](phase1-sync.md)

---

## Phase 2 — Local watcher and real-time sync ✅

**Content**:
- `LocalWatcher` (notify + debounce) wired in the daemon (`daemon.rs`)
- `tokio::select!` loop: shutdown + periodic timer + watcher events
- Mutual exclusion via `Semaphore(1)` (single sync cycle at a time)
- Graceful shutdown with `CancellationToken` (`tokio-util`)
- Enhanced `status` command: reads RedbStore (last sync, tracked files, page token, conversions, systemd unit)

**Note**: inotify limit warning in `LocalWatcher`; explicit polling fallback still to be finalized (see phase2-watcher.md, P2-2).

**Estimated duration**: maintenance / minor polish

**Details**: [phase2-watcher.md](phase2-watcher.md)

---

## Phase 3 — Google Workspace ↔ open format conversion ✅

**Content**:
- Google Docs/Sheets/Slides export via `files.export` with **OOXML** MIME (`export_format_sync` / `export_format`)
- **`export_file_with_fallback`** fallback (`exportLinks`) for oversized exports
- Import with conversion: `upload_with_conversion()` (.docx → Google Doc, etc.)
- **CONVERSIONS** table in redb (CRUD); used in **`executor.rs`** (upload / Google export)
- **`determine_action_converted`** branch in `decision.rs` (unit tests)
- Google Drawings → **SVG** (`image/svg+xml`)
- Executor: **`export_file_with_fallback`** + OOXML; **engine**: fully integrated with **`determine_action_converted`**
- Documentation of limitations (loss of collaborative metadata)

**Details**: [phase3-workspace.md](phase3-workspace.md)

---

## Phase 4 — Markdown index ✅

**Content**:
- Extractors: .docx, .xlsx, .pptx, .csv, **.pdf** (`pdf-extract`), .txt/.md (read / passthrough), binaries → metadata card (`generator.rs`)
- Markdown export for index: via `export_format_index` / existing Drive flows (no dedicated `export_as_markdown` helper, see P4-1 in phase4-index.md)
- `update_index` generator with extension-based dispatch
- **Post-sync integration**: `update_index` called from `engine.rs` when `index_dir` is configured
- `.index/` excluded from local scan

**Estimated duration**: polish (P4-1 helper, additional reference tests)

**Details**: [phase4-index.md](phase4-index.md)

---

## Phase 5 — Polish, service, cross-compilation ✅

**Content**:
- systemd user service (`service.rs`): install / uninstall / start / stop ✅
- Windows scheduled task (`schtasks`): install / uninstall / start / stop via `schtasks.exe` ✅
- Advanced logging (JSON file, rotation, per-module levels) ✅
- **`indicatif`** progress bars in the executor ✅
- **CI & Release** `.github/workflows/ci.yml` — single unified workflow on `v*` tags: check (fmt, clippy, test) → build (musl, macOS x86_64/aarch64, Windows) → release (archives + SHA256) ✅
- musl cross-compilation validated via the release workflow ✅
- Warning and dead code cleanup ✅
- Operational documentation: README + `docs/` ✅

**Details**: [phase5-polish.md](phase5-polish.md)

---

## Global timeline

```
         Month 1        Month 2        Month 3        Month 4        Month 5
    ┌──────────────┬──────────────┬──────────────┬──────────────┬──────────────┐
    │  Phase 0 ✅  │              │              │              │              │
    │              │  Phase 1 🟡  │              │              │              │
    │              │              │  Phase 2 ✅  │              │              │
    │              │              │  Phase 3 ✅  │  Phase 3     │              │
    │              │              │              │  Phase 4 ✅  │  Phase 4     │
    │              │              │              │              │  Phase 5 ✅  │
    └──────────────┴──────────────┴──────────────┴──────────────┴──────────────┘
```

| Phase | Estimated duration | Cumulative |
|-------|-------------------|------------|
| 0 — Scaffold + auth | ✅ Done | — |
| 1 — Basic sync | 1-2 weeks (remaining) | ~1 month |
| 2 — Watcher | 2-4 weeks | ~2 months |
| 3 — Workspace | 4-6 weeks | ~3.5 months |
| 4 — MD index | 3-5 weeks | ~4.5 months |
| 5 — Polish + releases | 3-5 weeks | ~5.5 months |

**Total estimated**: **4 to 6 months** with continuous development, **3 to 4 months** full-time focused.

---

## Current project metrics

| Metric | Value |
|--------|-------|
| Source files (.rs) | 39 |
| Lines of code (src/) | ~7,200 |
| Unit tests | 80 |
| Direct dependencies | 23 |
| Dev dependencies | 4 |
| Build (`cargo check`) | ✅ |
| Tests (`cargo test`) | ✅ 80/80 |
| Clippy (`cargo clippy`) | ✅ (`-D warnings` in CI) |

---

## Risks and mitigations

| Risk | Impact | Mitigation |
|------|--------|------------|
| Underestimated bisync complexity | High | Formalized matrix (12 cases), exhaustive tests |
| Google rate limiting on large Drives | Medium | Async token bucket, respect Retry-After, pagination |
| inotify limit on large trees | Medium | Detection + polling fallback |
| Synchronous redb in tokio | Medium | Systematic `spawn_blocking` |
| Complex OOXML conversion | Medium | Best-effort, text extraction only |
| Variable Markdown export API | Low | Fallback to text/plain |
| Symlinks in the sync tree | Low | Detection + skip + log warning |
| OAuth token refresh / revocation | Medium | Proactive expiration handling, clear re-auth flow |
| Large files: OOM on upload/download | Medium | Streaming (reqwest stream feature) instead of full in-memory `read` |
| Network interruption mid-sync | Medium | Atomic writes (.part), retry, SyncReport aggregates partial errors |
| Multi-device on the same account | Low | Document the limitation (single client per sync folder) |
| Google Drive API changes | Low | Client abstraction, stable v3 API |
| token.json security on disk | Low | Restrictive file permissions (0600), documentation |

---

## Related documents

- Architecture: [../architecture/overview.md](../architecture/overview.md)
- Decision tree: [../architecture/decision-tree.md](../architecture/decision-tree.md)
- Conventions: [../conventions/code-style.md](../conventions/code-style.md)
- Git workflow: [../conventions/git-workflow.md](../conventions/git-workflow.md)
