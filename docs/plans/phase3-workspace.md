# Phase 3 — Google Workspace conversion ↔ open formats

## Objective

Native Google files (Docs, Sheets, Slides, Drawings) are **exported** locally to open formats (`.docx`, `.xlsx`, `.pptx`, `.svg`) and **re-imported** with conversion when modified locally. The conversion cycle is transparent for users.

---

## Current status: ✅ COMPLETE

| Component | Status | Detail |
|-----------|--------|--------|
| Google Workspace MIME detection | ✅ | `is_google_workspace()`, `export_format()` / `export_format_sync()` in `drive/types.rs` |
| Export via `files.export` | ✅ | `export_file`, `export_file_with_fallback` in `drive/download.rs` |
| Export via `exportLinks` (>10MB) | ✅ | `export_file_with_fallback` |
| Import with conversion | ✅ | `upload_with_conversion()` in `drive/upload.rs` |
| OOXML migration | ✅ | `export_format_sync`: OOXML MIME for Docs/Sheets/Slides |
| Conversion table (store) | ✅ | CRUD + usage in `executor.rs` (`get_conversion` / `upsert_conversion` / `remove_conversion`) |
| Integration in `decision.rs` | ✅ | `determine_action_converted` + unit tests |
| Google Drawings → SVG | ✅ | `export_format_sync` → `image/svg+xml` |
| End-to-end engine integration | ✅ | Executor: OOXML export + fallback; `engine.rs` uses `determine_action_converted` for all converted files |

---

## Prerequisites

- Phase 1 complete (working baseline sync)
- Understanding of Google Drive API v3 export/conversion limitations

---

## Technical context

### Native Google file constraints

Google Workspace files (Docs, Sheets, Slides) have **no downloadable binary content**. They are cloud-native objects. The Drive API offers two mechanisms:

1. **`files.export`** (≤10MB): exports to a standard format
2. **`exportLinks`** (no hard size limit): direct export URLs from `files.get?fields=exportLinks`

### MD5 limitations

Native Google files **do not expose MD5** in the API (`md5Checksum` is missing). Change detection must rely on `modifiedTime` compared to the last sync. This is already handled in `decision.rs` through `remote_content_fingerprint()`, which uses `mtime:{iso}` as a fallback fingerprint.

### Conversion is not lossless

The `.gdoc` → `.docx` → `.gdoc` cycle can lose: suggestions, linked comments, smart chips, internal Drive links, and version history. To reduce impact:
- Re-convert `.docx` → Google Doc only when local content **actually changed** (local `.docx` MD5 differs from the previously exported version)
- Document losses clearly

---

## Format mapping table

### Current implementation (`drive/types.rs::export_format_sync()`)

| Google format | Target local format | Export MIME | Note |
|---------------|---------------------|-------------|------|
| Google Docs | `.docx` | OOXML Word | Re-importable |
| Google Sheets | `.xlsx` | OOXML Sheet | Re-importable |
| Google Slides | `.pptx` | OOXML Presentation | Re-importable |
| Google Drawings | `.svg` | `image/svg+xml` | Read-only in round-trip scenarios |

The Markdown index (Phase 4) can still use text exports through `export_format` / dedicated flows when needed.

---

## Task matrix

| ID | Task | File(s) | Input | Output | Completion criteria | Dependencies | Complexity | Status |
|----|------|---------|-------|--------|---------------------|--------------|------------|--------|
| **P3-1** | GWS MIME detection | `src/drive/types.rs` | — | `is_google_workspace(mime) → Option<ExportFormat>` | Test: each MIME maps to expected format | P1-1 | Low | ✅ |
| **P3-2** | Export via `files.export` | `src/drive/download.rs` | P1-2 | `export_file(drive_id, export_mime)` → bytes | Mock test: export returns content | P1-2 | Low | ✅ |
| **P3-3** | Export via `exportLinks` (>10MB) | `src/drive/download.rs` | P1-2 | Auto fallback when `files.export` fails (size limit / error) | `export_file_with_fallback` | P1-2 | Medium | ✅ |
| **P3-4** | Import with conversion | `src/drive/upload.rs` | P1-6 | Upload `.docx` with `mimeType: vnd.google-apps.document` | Mock test: converted upload keeps same Drive ID | P1-6 | Low | ✅ |
| **P3-5** | Conversion table (store) | `src/store/db.rs`, `src/store/session.rs`, `src/sync/executor.rs` | P0-8 | CRUD + usage in Google upload/export paths | Store + executor round-trip | P0-8 | Low | ✅ |
| **P3-6** | `decision.rs` integration | `src/sync/decision.rs` | P1-8, P3-5 | `determine_action_converted`: skip when exported MD5 is unchanged | Converted unit tests | P1-8, P3-5 | Medium | ✅ |
| **P3-7** | Google Drawings export → SVG | `src/drive/download.rs`, `src/drive/types.rs` | P1-2, P3-1 | MIME `image/svg+xml` | Drawing exports as SVG | P1-2, P3-1 | Low | ✅ |
| **P3-8** | Download/upload + engine integration | `src/sync/executor.rs`, `src/sync/engine.rs` | P3-2..P3-7 | Unified sync flow (converted decisions + exports) | E2E test: Google Doc → `.docx` → re-upload | P3-2..P3-7 | Medium | ✅ |

---

## Dependency graph

```
P3-1 (MIME) ──→ P3-2 (export) ──→ P3-3 (export >10MB) ──┐
                                                           │
P3-4 (conversion import) ────────────────────────────────┤
                                                           │
P3-5 (conversion table) ────→ P3-6 (decision.rs) ───────┤
                                                           │
P3-7 (Drawings SVG) ─────────────────────────────────────┤
                                                           │
                                                           └──→ P3-8 (E2E integration)
```

**Parallelizable**: P3-2, P3-4, P3-5, and P3-7 are independent once P3-1 is complete.

---

## Technical detail

### P3-3: `exportLinks` fallback

```rust
async fn export_file_large(
    client: &DriveClient,
    drive_id: &str,
    export_mime: &str,
    dest: &Path,
) -> Result<(), OxidriveError> {
    // 1. Try files.export first (simple path, 10MB limit)
    match export_file(client, drive_id, export_mime, dest).await {
        Ok(()) => return Ok(()),
        Err(e) if is_size_limit_error(&e) => {
            tracing::warn!("Export exceeded 10MB limit, falling back to exportLinks");
        }
        Err(e) => return Err(e),
    }

    // 2. Fallback: fetch exportLinks via files.get
    let file_meta = client.request(
        Method::GET,
        &format!("https://www.googleapis.com/drive/v3/files/{}?fields=exportLinks", drive_id),
    ).await?;
    let links: HashMap<String, String> = file_meta.json().await?;
    let url = links.get(export_mime)
        .ok_or_else(|| OxidriveError::drive("No exportLink for requested MIME"))?;

    // 3. Download through the direct export URL
    download_url(client, url, dest).await
}
```

### P3-6: `decision.rs` integration

The decision tree must distinguish 3 file categories:

1. **Regular files** (have MD5) — existing logic
2. **Native Google files** (no MD5, have `modifiedTime`) — `mtime:` fingerprint already handled
3. **Converted files** (a local `.docx` maps to a remote Google Doc) — requires the CONVERSIONS table

For converted files:
- `local_changed` = local `.docx` MD5 differs from the last exported `.docx` MD5
- `remote_changed` = Google Doc `modifiedTime` > `last_synced_at`
- If both changed → conflict (same logic, with documented data-loss caveats)
- If only local changed → upload with conversion
- If only remote changed → re-export

---

## Completion criteria

- [x] Google Docs/Sheets/Slides export to OOXML through `export_format_sync` + download
- [x] Local `.docx` modifications trigger converted re-upload while preserving the same Drive ID (executor + conversion store)
- [x] Google Drawings export to SVG through MIME `image/svg+xml`
- [x] Large documents use `exportLinks` fallback through `export_file_with_fallback`
- [x] CONVERSIONS table is maintained in relevant executor paths
- [x] `determine_action_converted` covers converted scenarios (unit tests)
- [x] `determine_action_converted` is systematically used from `engine.rs`
- [x] Integration tests with mocks exist for each conversion type
- [x] Conversion limitations are documented (collaborative metadata loss)

→ Next: [Phase 4 — Markdown Index](phase4-index.md)
