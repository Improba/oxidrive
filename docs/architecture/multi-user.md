# Multi-user sync architecture

This document describes the final multi-user model in `oxidrive` when several devices share the same Google Drive folder through the same OAuth application.

## Goals

- Prevent silent data loss when two devices edit the same file.
- Keep conflict handling deterministic and observable.
- Make deletions reversible by default.
- Stay compatible with Google Drive API limitations (no atomic write lock).

## Building blocks

### Content-based change detection

`sync::scan` computes local MD5 checksums and compares them with persisted metadata. When only timestamps change and content is identical, the decision matrix emits `SyncAction::TouchMetadata` instead of uploading bytes.

### Optimistic concurrency with preflight checks

Before media updates, the upload layer runs a guarded preflight (`get_file_metadata` + expected `headRevisionId`/`version`) and aborts the upload when remote metadata moved. This catches stale writes and routes them to conflict handling instead of blind overwrite.

### Version vectors in Drive `appProperties`

`sync::coordination::VersionVector` stores causality in `appProperties` (`ox_vv`, `ox_origin`). On dual-edit cases, vectors decide whether a remote state is causally ahead, behind, or concurrent:

- causally safe: upload can continue,
- concurrent or dominant remote: conflict path.

### Conflict copies tagged by device

With `ConflictResolution::ConflictCopy`, the incoming remote bytes are preserved as a sibling file using:

`<name>.conflict.<device_id>.<timestamp><ext>`

The local edit is then uploaded so both versions survive.

### Safe deletes, tombstones, and local trash

Local deletes are reversible:

- remote-delete observations are confirmed through tombstones,
- local file removal uses move-to-trash (`.trash/`) first,
- periodic purge (`purge_trash`) removes expired trash entries based on TTL.

This turns destructive propagation into a staged process.

### Advisory leases

`drive::locks` supports optional advisory leases in `appProperties` (`ox_lease`). A device can avoid uploading when another device advertises an active lease, reducing collision frequency for long edits.

### Conflict observability

Resolved conflicts are appended to `.oxidrive/conflicts.log` as JSONL (`timestamp`, path, resolution, device, origin, copy path). The `status` command surfaces recent conflict count and details.

## Residual limits (explicitly accepted)

- **TOCTOU remains best-effort**: preflight + upload is still check-then-act because Drive media updates do not provide strict `If-Match` style atomic guards.
- **No global atomic lock in Drive**: leases are advisory metadata, not an enforced distributed mutex.
- **Clock skew and API timing**: suffix timestamps and conflict event ordering depend on local clock and request sequencing.
- **Folder-level transactional semantics**: each file is reconciled independently; cross-file atomicity is not provided.

## Operational guide: 3 developers on one shared folder

### Recommended defaults

- Keep `conflict_policy = "conflict_copy"` (default).
- Keep safe delete enabled (`safe_delete = true`).
- Keep default ignore patterns (`~$*`, `.oxidrive/**`, `.trash/**`, temporary/editor files).
- Optionally enable leases (`use_leases = true`) for long manual edits.

### Typical workflow

1. Each developer runs sync on their own local checkout folder.
2. If two edits collide, expect one conflict copy file named `<name>.conflict.<device>.<ts>.<ext>` next to the original. The canonical name keeps the local edit; the diverging remote content is written to the conflict copy.
3. The conflict copy is a normal file in the synced tree, so it propagates to the other devices on the next sync (Dropbox-style): every collaborator ends up seeing both versions.
4. Review both versions and merge manually, then delete the obsolete conflict copy (the deletion also propagates).

### Deletion workflow

Deletions are confirmed symmetrically across sync cycles, so a transient
disappearance (an accidental `rm`, a half-synced checkout, an atomic-save rename
race) is not immediately propagated:

1. When a file disappears locally, the first sync only records the observation;
   the remote file is trashed on a later cycle once the local deletion is
   confirmed. If the file reappears in the meantime, no remote deletion happens.
2. Symmetrically, when the remote file is gone, other devices confirm the
   deletion across cycles before moving their local copy to `.trash/`.
3. Recovery is possible by restoring the file from `.trash/` within the TTL.

### Diagnostics checklist

- Run `oxidrive status`:
  - device id,
  - recent conflicts,
  - pending tombstones,
  - active leases.
- Inspect `.oxidrive/conflicts.log` for audit-friendly JSONL history.
