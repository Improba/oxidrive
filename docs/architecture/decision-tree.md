# Sync decision tree

This document describes the **reconciliation matrix** used by oxidrive: for each relative path, three views are considered — **local file** (L), **remote file** (R), **persisted metadata** (M, last known state in `store`).

The central function is `determine_action` in `src/sync/decision.rs`: it is **purely deterministic** (aside from conflict policy applied afterward in `conflict`).

---

## Matrix of the 12 cases

The following rows correspond to the **meaningful** combinations covered by the logic and tests (`matrix_1` … `matrix_12`).

**How "changed" is decided** (`local_delta` in `decision.rs`):

- Local change detection is **content-based (MD5)**. `mtime` and `size` are only a fast pre-filter: when both are unchanged the file is treated as `Unchanged` without rehashing; otherwise the MD5 is compared.
- A file whose **MD5 is identical** but whose `mtime`/`size` changed is `MetaOnly` → resolved with **`TouchMetadata`** (the local record is refreshed, **no network transfer**). This avoids spurious uploads when an application merely opens/rewrites a file without changing its bytes.
- Remote change detection uses the `md5Checksum` fingerprint when available, falling back to `modifiedTime` for Google-native files.

| # | Local (L) | Remote (R) | Meta (M) | Condition / sub-case | Action |
|---|-----------|------------|----------|----------------------|--------|
| 1 | present | present | present | L and R **unchanged** relative to M | **Skip** |
| 2 | present | present | present | L **content** changed, R unchanged | **Upload** (optimistic, revision-guarded) |
| 2b | present | present | present | L **MetaOnly** (same MD5), R unchanged | **TouchMetadata** (refresh record, no transfer) |
| 3 | present | present | present | R **modified**, L unchanged | **Download** |
| 4 | present | present | present | L **and** R modified | **Upload** or **Conflict** per version-vector causality (see below) |
| 5 | present | present | absent | Equal MD5 (known on both sides) → **Skip**; otherwise (different MD5 or remote MD5 missing) → **Conflict** | **Skip** or **Conflict** |
| 6 | present | absent | present | L **unchanged** relative to M (file deleted on Drive) | **DeleteLocal** (confirmed across cycles, moved to `.trash/`) |
| 7 | present | absent | present | L **modified** (file deleted on Drive but edited locally) | **Upload** (remote recreation, `remote_id: None`) |
| 8 | present | absent | absent | **New** file local-only | **Upload** |
| 9 | absent | present | present | R **unchanged** relative to M (file deleted locally) | **DeleteRemote** (confirmed across cycles) |
| 10 | absent | present | present | R **modified** (file missing locally but remote evolved) | **Download** |
| 11 | absent | present | absent | **New** file on Drive only | **Download** |
| 12 | absent | absent | present | Both copies are gone | **CleanupMetadata** (remove orphan DB entry) |

**Edge case**: (L absent, R absent, M absent) → **Skip** (nothing to do; phantom path).

---

## Explanation and examples by case

### 1 — Nothing changed

You synced yesterday; neither the file on disk nor the Drive version has changed since the last record. **No network operation** is needed.

### 2 — You edited locally only

Example: fixing a `.txt`; local MD5/mtime no longer matches M while Drive still matches the last sync. → **Upload** to push your version.

### 3 — Someone else (or you on the web) changed Drive

The local file stayed as at last sync; Drive has a new revision. → **Download** to realign disk.

### 4 — Edit conflict (edit/edit)

Both copies diverged from M. oxidrive uses **version vectors** (stored in Drive `appProperties` as `ox_vv`) to tell **causally safe** updates from **true concurrency**:

- If the version vectors prove the remote did **not** advance past the last synced state (`Equal` or `DominatedBy`), the local edit is causally newer → **Upload** (still revision-guarded; on a preflight mismatch it degrades to a conflict copy).
- If the vectors are **concurrent** (`Concurrent` / `Dominates`), or no vectors are available on either side, the result is **Conflict**, resolved per **`conflict_policy`** (default **`conflict_copy`**).

Uploads are **optimistically guarded**: a preflight checks `headRevisionId`/`version` before mutating, and a mismatch is turned into a non-destructive conflict copy instead of overwriting a concurrent revision. Example: the same document edited on two machines without an intermediate sync.

### 5 — First L+R encounter with no DB history

- If **MD5** (when available on both sides) are **identical**, the content is considered already the same → **Skip** (no transfer).
- If MD5 **differ** → **Conflict**: oxidrive does not guess truth without history.

### 6 — Deleted on Drive, local file intact

The file was removed from Drive but still exists locally **as at last sync** → reflect the remote deletion with **DeleteLocal**. The deletion is **confirmed across sync cycles** (a tombstone records the first observation) before it is applied, and—when `safe_delete` is enabled (default)—the local file is moved to `.trash/` (purged after `trash_ttl_days`) rather than removed outright. This protects against a transient remote disappearance.

### 7 — Drive deleted, but you edited locally again

M still referred to a remote file; the remote no longer exists; local changed since M. → **Upload** to **recreate** the file on Drive (new file on the API side if needed).

### 8 — New local file, never seen by oxidrive

Creating a report in `sync_dir`; no M entry or remote file. → **Upload**.

### 9 — You deleted the file locally, Drive unchanged

Local deletion is authoritative relative to M → **DeleteRemote** (the remote file is trashed). Symmetrically to case 6, this is **confirmed across sync cycles** before propagating, so an accidental or transient local `rm` (or a half-synced checkout) is not immediately pushed to every other device.

### 10 — File deleted locally, but Drive was modified

Example: accidental local deletion while a new version exists on Drive. R changed since M → **Download** to **restore** from Drive.

### 11 — New file only on Drive

A colleague dropped a PDF in the shared folder. → **Download**.

### 12 — Metadata cleanup

Neither side has the file anymore, but M still has an entry (residual inconsistent state, or double deletion detected afterward). → **CleanupMetadata** to avoid a polluted database.

---

## Native Google files (no MD5)

The Google Drive API does **not always** provide `md5Checksum` for **Google-native** files (Docs, Sheets, etc.) or certain binary types handled differently.

Consequences in current logic:

- **Without M (first sync)**: if the remote has **no** MD5, the “L+R, no meta” case becomes **Conflict** — byte-for-byte equality with the exported local cannot be proven.
- **With M**: “remote modified” detection then relies on **`modifiedTime`** (and missing remote MD5) to track changes, enabling consistent **Skip** / **Download** once a metadata row exists after a successful first cycle.

Operational recommendation: after a first alignment or Workspace export, let at least one full cycle update **M** so files without MD5 are tracked by **timestamp**.

---

## Conflict policy (`ConflictPolicy`)

Configurable in `config.toml` (default: **`conflict_copy`**):

| Value | Typical behavior |
|--------|------------------|
| `conflict_copy` *(default)* | **Non-destructive**, Dropbox-style: keep **both** sides. The local edit keeps the canonical name; the diverging remote content is written to `<name>.conflict.<device>.<ts>.<ext>`. The conflict copy is a normal file that propagates to every device, so all collaborators see both versions. |
| `local_wins` | The **local** version replaces the remote on an explicit conflict. |
| `remote_wins` | The **Drive** version wins; overwrite or merge per executor implementation. |
| `rename` | Keep **both** copies by renaming one (configured suffix; the engine adds a timestamp to ensure uniqueness). |

The reconciliation matrix **does not choose** between local and remote for case 4: it **reports** `Conflict` (when version vectors indicate true concurrency) and the executor applies the resolution per policy. Every conflict resolution is also appended to `.oxidrive/conflicts.log` (JSONL) for auditing.

For code conventions and tests tied to this matrix, see [../conventions/code-style.md](../conventions/code-style.md).
