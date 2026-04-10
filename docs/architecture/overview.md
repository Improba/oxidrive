# Architecture overview

This document describes the software organization of **oxidrive**: modules, data flows between them, and main technical choices.

---

## ASCII diagram (modules and interactions)

```
                    ┌─────────────────────────────────────────┐
                    │              CLI (main / cli)            │
                    │  setup │ sync │ status │ service        │
                    └───────────────┬─────────────────────────┘
                                    │
                    ┌───────────────┼───────────────┐
                    ▼               ▼               ▼
             ┌──────────┐   ┌──────────┐   ┌──────────────┐
             │  config  │   │   auth   │   │    error     │
             └──────────┘   └──────────┘   └──────────────┘
                                    │
        ┌───────────────────────────┼───────────────────────────┐
        ▼                           ▼                           ▼
 ┌─────────────┐            ┌─────────────┐              ┌─────────────┐
 │   watch     │───events──▶│    sync     │◀───scan────▶│   store     │
 │  (notify)   │            │ scan/decision│              │   (redb)    │
 └─────────────┘            │  /executor   │              └──────▲──────┘
        │                   └──────┬───────┘                     │
        │                          │ read/write metadata         │
        │                          ▼                             │
        │                   ┌─────────────┐                     │
        └──────────────────▶│   drive     │─────────────────────┘
                            │ client API  │    (persist ids, hashes)
                            └──────┬──────┘
                                   │ HTTPS (reqwest + rustls)
                                   ▼
                            ┌─────────────┐
                            │ Google Drive│
                            └─────────────┘

 ┌─────────────┐
 │   index     │◀── Markdown / exported files (read from sync_dir)
 │  (future)   │──▶ artifacts in index_dir (optional)
 └─────────────┘

 ┌─────────────┐
 │   utils     │── hash, fs, retry (used by sync, drive, store)
 └─────────────┘
```

---

## Module descriptions

### `drive/`

**Google Drive API** layer: request authentication, file listing, download, upload, **changes** handling (incremental). Remote types (`DriveFile`, MIME, parents) are isolated here to limit how much of the rest of the code depends on API details.

### `sync/`

Core of **reconciliation**:

- **scan**: local and remote inventory (relative paths, sizes, MD5 when available).
- **decision**: pure function `(local, remote, persisted metadata) → action` (upload, download, skip, conflict, deletions, cleanup).
- **executor**: orchestration of concrete operations (`drive` calls, `store` updates).
- **conflict**: application of the configured `ConflictPolicy` when the decision matrix yields a conflict.

### `watch/`

**Watching the sync folder** via the `notify` library (with configurable debounce). Events trigger sync cycles or queue work on the async runtime (`tokio`).

### `store/`

**Local persistence** of sync state (seen files, last local/remote MD5/mtime, Drive identifiers). Implemented with **redb** (embedded key-value store, single file). Blocking access is typically isolated in `spawn_blocking` so the async runtime is not blocked.

### `index/`

**Markdown indexing** (and possibly search) over files in `sync_dir` or produced by Workspace export. Writes to `index_dir` when configured.

### `utils/`

Cross-cutting helpers: **hashing** (MD5 for exportable binaries), **filesystem** helpers, **retry** with backoff for fragile network calls.

### Cross-cutting modules

- **`config`**: TOML/JSON deserialization, defaults.
- **`auth`**: Google OAuth2 flow (setup, token refresh).
- **`types`**: shared types (`SyncAction`, `SyncRecord`, relative paths, etc.).
- **`error`**: typed errors (`thiserror`), mapping to CLI exit codes.

---

## Data flow: one sync cycle

1. **Load**: read `Config`, open `store` database, authenticated `drive` client.
2. **Scan**: enumerate local files (excluding `ignore_patterns`) and fetch Drive tree / metadata in scope (`drive_folder_id` when set).
3. **Join**: for each relative path known on at least one side, associate with the latest `SyncRecord` in the database.
4. **Decision**: `determine_action` yields a `SyncAction` (skip, upload, download, conflict, delete local/remote, cleanup metadata).
5. **Conflict resolution**: if action is `Conflict`, apply `ConflictPolicy` (local, remote, rename).
6. **Execution**: transfers and deletions; local file updates; Drive API calls.
7. **Persistence**: write new `SyncRecord` entries / remove stale entries in `store`.
8. **Index** (optional): if enabled, incremental index update after changes settle.

This pipeline can be triggered manually (`sync`), on a timer (`service`), or by the **watcher** after debounce.

---

## Technical choices

| Domain | Choice | Rationale |
|--------|--------|-----------|
| Async / concurrency | **Tokio** | Mature runtime for network I/O and parallel tasks (uploads/downloads). |
| HTTP / TLS | **reqwest** + **rustls** | HTTP client without system OpenSSL; TLS in pure Rust, suited to static binaries. |
| Local storage | **redb** | Embedded, transactional, single-file database; no external server. |
| CLI | **clap** | Subcommands, global flags, consistent help messages. |
| Config | **serde** + **toml** | Human-readable format, evolvable schema. |
| Errors | **thiserror** | Typed errors and stable messages for the CLI. |
| Logging | **tracing** + **tracing-subscriber** | Levels, `RUST_LOG` filters, optional structured output (JSON). |
| FS watch | **notify** + **notify-debouncer-full** | Event aggregation to avoid sync storms. |
| OAuth2 | **oauth2** | Standard flow for the Google API. |

For details on decision logic, see [decision-tree.md](decision-tree.md).
