# Sync decision tree

This document describes the **reconciliation matrix** used by oxidrive: for each relative path, three views are considered — **local file** (L), **remote file** (R), **persisted metadata** (M, last known state in `store`).

The central function is `determine_action` in `src/sync/decision.rs`: it is **purely deterministic** (aside from conflict policy applied afterward in `conflict`).

---

## Matrix of the 12 cases

The following rows correspond to the **meaningful** combinations covered by the logic and tests (`matrix_1` … `matrix_12`). “Unchanged” means: identical to the last state recorded in M (local and remote MD5 / mtime per the code rules).

| # | Local (L) | Remote (R) | Meta (M) | Condition / sub-case | Action |
|---|-----------|------------|----------|----------------------|--------|
| 1 | present | present | present | L and R **unchanged** relative to M | **Skip** |
| 2 | present | present | present | L **modified**, R unchanged | **Upload** (update of the remote file linked to M) |
| 3 | present | present | present | R **modified**, L unchanged | **Download** |
| 4 | present | present | present | L **and** R modified | **Conflict** |
| 5 | present | present | absent | Equal MD5 (known on both sides) → **Skip**; otherwise (different MD5 or remote MD5 missing) → **Conflict** | **Skip** or **Conflict** |
| 6 | present | absent | present | L **unchanged** relative to M (file deleted on Drive) | **DeleteLocal** |
| 7 | present | absent | present | L **modified** (file deleted on Drive but edited locally) | **Upload** (remote recreation, `remote_id: None`) |
| 8 | present | absent | absent | **New** file local-only | **Upload** |
| 9 | absent | present | present | R **unchanged** relative to M (file deleted locally) | **DeleteRemote** |
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

Both copies diverged from M. The matrix yields **Conflict**; what happens next depends on **`conflict_policy`** (`local_wins`, `remote_wins`, `rename` with suffix). Example: same document open on two machines without an intermediate sync.

### 5 — First L+R encounter with no DB history

- If **MD5** (when available on both sides) are **identical**, the content is considered already the same → **Skip** (no transfer).
- If MD5 **differ** → **Conflict**: oxidrive does not guess truth without history.

### 6 — Deleted on Drive, local file intact

The file was removed from Drive but still exists locally **as at last sync**. Policy: reflect remote deletion → **DeleteLocal** (or business equivalent per future options documented in the code).

### 7 — Drive deleted, but you edited locally again

M still referred to a remote file; the remote no longer exists; local changed since M. → **Upload** to **recreate** the file on Drive (new file on the API side if needed).

### 8 — New local file, never seen by oxidrive

Creating a report in `sync_dir`; no M entry or remote file. → **Upload**.

### 9 — You deleted the file locally, Drive unchanged

Alignment: local deletion is authoritative relative to M → **DeleteRemote**.

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

Configurable in `config.toml`:

| Value | Typical behavior |
|--------|------------------|
| `local_wins` | The **local** version replaces the remote on an explicit conflict. |
| `remote_wins` | The **Drive** version wins; overwrite or merge per executor implementation. |
| `rename` | Keep **both** copies by renaming one (configured suffix; the engine may add a timestamp to ensure uniqueness). |

The matrix **does not choose** between local and remote for case 4: it **reports** `Conflict`; the `sync/conflict` module applies resolution per policy.

For code conventions and tests tied to this matrix, see [../conventions/code-style.md](../conventions/code-style.md).
