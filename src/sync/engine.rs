//! High-level sync orchestration across local scan, remote listing, decisions, and execution.

use std::collections::{HashMap, HashSet};

use tracing::instrument;

use crate::config::Config;
use crate::drive::changes::{fetch_changes, get_start_page_token};
use crate::drive::folders::ensure_folder_hierarchy;
use crate::drive::list::list_all_files;
use crate::drive::types::{DriveChange, DriveFile, FOLDER};
use crate::drive::DriveClient;
use crate::error::OxidriveError;
use crate::index::generator::update_index;
use crate::store::{RedbStore, Store};
use crate::sync::decision::{determine_action, determine_action_converted};
use crate::sync::executor::SyncExecutor;
use crate::sync::scan::scan_local;
use crate::types::{
    PendingOp, PendingOpKind, PendingOpStage, RelativePath, SyncRecord, SyncReport,
};

/// Runs a full sync cycle: scan → list → decide → execute → clear remote snapshot.
///
/// Per-path [`crate::types::SyncRecord`] values are written by [`crate::sync::executor::SyncExecutor`]
/// as uploads/downloads succeed.
#[instrument(skip_all, fields(sync_dir = %config.sync_dir.display()))]
pub async fn run_sync(
    config: &Config,
    client: &DriveClient,
    store: &Store,
    redb: &RedbStore,
) -> Result<SyncReport, OxidriveError> {
    run_sync_incremental(config, client, store, redb).await
}

/// Runs one sync cycle and uses Drive Changes API when a stored page token is available.
#[instrument(skip_all, fields(sync_dir = %config.sync_dir.display()))]
pub async fn run_sync_incremental(
    config: &Config,
    client: &DriveClient,
    store: &Store,
    redb: &RedbStore,
) -> Result<SyncReport, OxidriveError> {
    let root_id = config
        .drive_folder_id
        .clone()
        .ok_or_else(|| OxidriveError::sync("config.drive_folder_id is required for sync"))?;

    store.set_root_drive_folder_id(Some(root_id.clone()))?;
    store.load_from_redb(redb)?;
    recover_pending_operations(store, redb)?;

    tracing::info!("scanning local filesystem");
    let ignore_patterns = config.effective_ignore_patterns();
    let local = scan_local(&config.sync_dir, &ignore_patterns).await?;

    let remote_state =
        fetch_remote_state_incremental(config, client, store, redb, &root_id).await?;
    let remote = remote_state.remote;
    store.set_remote_snapshot(remote.clone())?;
    for (rel, id) in known_remote_folders(&remote) {
        store.set_folder_id(&rel, &id);
    }

    let mut paths: HashSet<_> = local.keys().cloned().collect();
    paths.extend(remote.keys().cloned());
    paths.extend(store.all_record_paths()?);

    tracing::info!(paths = paths.len(), "computing sync actions");
    let mut actions = Vec::new();
    for p in paths {
        let l = local.get(&p);
        let r = remote.get(&p);
        let meta = store.get(&p)?;
        let m = meta.as_ref();
        let conversion = store.get_conversion(&p)?;
        if let Some(conversion) = conversion.as_ref() {
            actions.push(determine_action_converted(
                &p,
                l,
                r,
                m,
                &config.conflict_policy,
                true,
                conversion.last_export_md5.as_deref(),
            ));
        } else {
            actions.push(determine_action(&p, l, r, m, &config.conflict_policy));
        }
    }

    let executor = SyncExecutor::new(
        config.max_concurrent_uploads,
        config.max_concurrent_downloads,
    );

    let upload_paths = upload_targets_from_actions(&actions);
    if !upload_paths.is_empty() {
        let mut existing_folders = known_remote_folders(&remote);
        existing_folders.extend(store.all_folder_ids()?);
        let upload_path_refs: Vec<&str> = upload_paths.iter().map(|p| p.as_str()).collect();
        tracing::info!(
            uploads = upload_paths.len(),
            known_folders = existing_folders.len(),
            "ensuring remote folder hierarchy for upload parents"
        );
        let ensured =
            ensure_folder_hierarchy(client, &upload_path_refs, &root_id, &existing_folders).await?;
        for (rel, id) in ensured {
            store.set_folder_id(&rel, &id);
        }
    }

    tracing::info!(actions = actions.len(), "executing sync actions");
    let report = executor.execute(actions, client, store, redb).await?;

    if let Some(index_dir) = config.index_dir.as_ref() {
        let changed = changed_paths_for_index(&report);
        if !changed.is_empty() {
            let indexed = update_index(&changed, &config.sync_dir, index_dir).await?;
            tracing::info!(
                changed = changed.len(),
                indexed,
                index_dir = %index_dir.display(),
                "updated index for changed files"
            );
        }
    }

    let committed_pending_keys = metadata_committed_pending_keys(redb)?;
    if report.errors.is_empty() {
        store.persist_to_redb_and_page_token_with_pending_cleanup(
            redb,
            &remote_state.next_page_token,
            &committed_pending_keys,
        )?;
    } else {
        store.persist_to_redb_with_pending_cleanup(redb, &committed_pending_keys)?;
        tracing::warn!(
            errors = report.errors.len(),
            "sync completed with transfer errors; keeping previous page token for retry"
        );
    }
    if !committed_pending_keys.is_empty() {
        tracing::info!(
            cleared_committed_pending = committed_pending_keys.len(),
            "cleared committed pending operations after durable session persist"
        );
    }

    let metadata_rows = store.record_count()?;
    tracing::info!(metadata_rows, "session metadata persisted after sync cycle");

    store.clear_remote_snapshot()?;
    tracing::info!(
        uploaded = report.uploaded.len(),
        downloaded = report.downloaded.len(),
        skipped = report.skipped,
        conflicts = report.conflicts.len(),
        "sync cycle complete"
    );
    Ok(report)
}

fn recover_pending_operations(store: &Store, redb: &RedbStore) -> Result<(), OxidriveError> {
    let rows = redb.list_pending_ops_sync()?;
    if rows.is_empty() {
        return Ok(());
    }

    let mut recovered = 0usize;
    let mut discarded = 0usize;
    let mut recovered_pending_keys = Vec::new();
    for (path_raw, data) in rows {
        let path = RelativePath::from(path_raw.as_str());
        if !path.is_safe_non_empty() {
            tracing::warn!(path = %path_raw, "discarding unsafe pending operation path");
            redb.delete_pending_op_sync(&path_raw)?;
            discarded += 1;
            continue;
        }
        let pending: PendingOp = match bincode::deserialize(&data) {
            Ok(pending) => pending,
            Err(e) => {
                tracing::warn!(
                    path = %path_raw,
                    error = %e,
                    "discarding invalid pending operation payload"
                );
                redb.delete_pending_op_sync(&path_raw)?;
                discarded += 1;
                continue;
            }
        };

        match (pending.kind, pending.stage) {
            (_, PendingOpStage::Planned) => {
                redb.delete_pending_op_sync(path.as_str())?;
                discarded += 1;
            }
            (PendingOpKind::Upload, PendingOpStage::SideEffectStarted)
            | (PendingOpKind::Upload, PendingOpStage::SideEffectDone)
            | (PendingOpKind::Upload, PendingOpStage::MetadataCommitted)
            | (PendingOpKind::Download, PendingOpStage::SideEffectStarted)
            | (PendingOpKind::Download, PendingOpStage::SideEffectDone)
            | (PendingOpKind::Download, PendingOpStage::MetadataCommitted)
            | (PendingOpKind::DeleteLocal, PendingOpStage::SideEffectStarted)
            | (PendingOpKind::DeleteLocal, PendingOpStage::SideEffectDone)
            | (PendingOpKind::DeleteLocal, PendingOpStage::MetadataCommitted)
            | (PendingOpKind::DeleteRemote, PendingOpStage::SideEffectStarted)
            | (PendingOpKind::DeleteRemote, PendingOpStage::SideEffectDone)
            | (PendingOpKind::DeleteRemote, PendingOpStage::MetadataCommitted) => {
                match clear_path_state_for_recovery(store, &path) {
                    Ok(()) => {
                        recovered_pending_keys.push(path.as_str().to_string());
                        recovered += 1;
                    }
                    Err(e) => {
                        tracing::warn!(
                            path = %path,
                            error = %e,
                            "pending operation recovery failed; keeping journal entry for retry"
                        );
                    }
                }
            }
        }
    }

    if recovered > 0 {
        store.persist_to_redb(redb)?;
        for key in recovered_pending_keys {
            redb.delete_pending_op_sync(&key)?;
        }
    }
    tracing::info!(
        recovered_pending_ops = recovered,
        discarded_pending_ops = discarded,
        "recovery pass over pending operations complete"
    );
    Ok(())
}

fn clear_path_state_for_recovery(store: &Store, path: &RelativePath) -> Result<(), OxidriveError> {
    let mut issues = Vec::new();
    if let Err(e) = store.remove(path) {
        issues.push(format!("sync record: {e}"));
    }
    if let Err(e) = store.remove_conversion(path) {
        issues.push(format!("conversion: {e}"));
    }
    if let Err(e) = store.remove_upload_session(path) {
        issues.push(format!("upload session: {e}"));
    }
    if issues.is_empty() {
        Ok(())
    } else {
        Err(OxidriveError::sync(issues.join("; ")))
    }
}

fn metadata_committed_pending_keys(redb: &RedbStore) -> Result<Vec<String>, OxidriveError> {
    let rows = redb.list_pending_ops_sync()?;
    let mut keys = Vec::new();
    for (path, data) in rows {
        let pending: PendingOp = match bincode::deserialize(&data) {
            Ok(pending) => pending,
            Err(_) => continue,
        };
        if matches!(pending.stage, PendingOpStage::MetadataCommitted) {
            keys.push(path);
        }
    }
    Ok(keys)
}

struct RemoteSyncInput {
    remote: HashMap<RelativePath, DriveFile>,
    next_page_token: String,
}

async fn fetch_remote_state_incremental(
    config: &Config,
    client: &DriveClient,
    store: &Store,
    redb: &RedbStore,
    root_id: &str,
) -> Result<RemoteSyncInput, OxidriveError> {
    match redb.get_page_token().await? {
        Some(page_token) => {
            tracing::info!("page token found, fetching incremental Drive changes");
            let (changes, next_page_token) = fetch_changes(client, &page_token).await?;
            let remote = match build_incremental_remote_view(store, root_id, changes) {
                Ok(remote) => remote,
                Err(err) => {
                    tracing::warn!(
                        error = %err,
                        "failed to resolve incremental changes to full paths; falling back to full scan"
                    );
                    list_all_files(client, root_id).await?
                }
            };
            Ok(RemoteSyncInput {
                remote,
                next_page_token,
            })
        }
        None => {
            tracing::info!("no page token found, running full scan for initial sync");
            tracing::info!(sync_dir = %config.sync_dir.display(), "listing remote Drive tree");
            let remote = list_all_files(client, root_id).await?;
            let next_page_token = get_start_page_token(client).await?;
            Ok(RemoteSyncInput {
                remote,
                next_page_token,
            })
        }
    }
}

fn build_incremental_remote_view(
    store: &Store,
    root_id: &str,
    changes: Vec<DriveChange>,
) -> Result<HashMap<RelativePath, DriveFile>, OxidriveError> {
    let mut remote = remote_from_records(store, root_id)?;
    let mut id_to_path: HashMap<String, RelativePath> = remote
        .iter()
        .map(|(path, file)| (file.id.clone(), path.clone()))
        .collect();

    for change in changes {
        let path = resolve_change_path(store, &change, &id_to_path, root_id)?;
        if change.removed {
            remote.remove(&path);
            id_to_path.remove(&change.file_id);
            continue;
        }

        let file = change.file.ok_or_else(|| {
            OxidriveError::sync(format!(
                "change for file id '{}' is missing file metadata",
                change.file_id
            ))
        })?;
        if file.trashed {
            remote.remove(&path);
            id_to_path.remove(&change.file_id);
            continue;
        }

        id_to_path.insert(file.id.clone(), path.clone());
        remote.insert(path, file);
    }

    Ok(remote)
}

fn remote_from_records(
    store: &Store,
    root_id: &str,
) -> Result<HashMap<RelativePath, DriveFile>, OxidriveError> {
    let mut remote = HashMap::new();
    for (path, record) in store.iter_records()? {
        if let Some(stub) = stub_drive_file_from_record(&path, &record, root_id) {
            remote.insert(path, stub);
        }
    }
    Ok(remote)
}

fn stub_drive_file_from_record(
    path: &RelativePath,
    record: &SyncRecord,
    root_id: &str,
) -> Option<DriveFile> {
    let drive_file_id = record.drive_file_id.clone()?;
    Some(DriveFile {
        id: drive_file_id,
        name: file_name_from_relative(path).to_string(),
        mime_type: record
            .remote_mime_type
            .clone()
            .unwrap_or_else(|| "application/octet-stream".to_string()),
        md5_checksum: record
            .remote_md5
            .clone()
            .filter(|v| !v.starts_with("mtime:")),
        modified_time: record.remote_modified_at.unwrap_or(record.last_synced_at),
        size: Some(record.local_size),
        parents: vec![root_id.to_string()],
        trashed: false,
    })
}

fn resolve_change_path(
    store: &Store,
    change: &DriveChange,
    id_to_path: &HashMap<String, RelativePath>,
    root_id: &str,
) -> Result<RelativePath, OxidriveError> {
    if let Some(existing) = id_to_path.get(&change.file_id) {
        if let Some(file) = change.file.as_ref() {
            let current_name = file_name_from_relative(existing);
            if file.name != current_name {
                return Err(OxidriveError::sync(format!(
                    "remote rename detected for id '{}' ({} -> {})",
                    change.file_id, current_name, file.name
                )));
            }
            let current_parent = parent_relative_str(existing);
            let same_parent = if current_parent.is_empty() {
                file.parents.iter().any(|parent_id| parent_id == root_id)
            } else {
                matches!(
                    store.get_folder_id(current_parent).as_deref(),
                    Some(folder_id) if file.parents.iter().any(|parent_id| parent_id == folder_id)
                )
            };
            if !same_parent {
                return Err(OxidriveError::sync(format!(
                    "remote move detected for id '{}' (path '{}')",
                    change.file_id, existing
                )));
            }
        }
        return Ok(existing.clone());
    }

    if change.removed {
        return Err(OxidriveError::sync(format!(
            "removed change for unknown id '{}'",
            change.file_id
        )));
    }

    let file = change.file.as_ref().ok_or_else(|| {
        OxidriveError::sync(format!(
            "missing file payload for changed id '{}'",
            change.file_id
        ))
    })?;

    if file.parents.iter().any(|p| p == root_id) {
        return Ok(RelativePath::from(file.name.as_str()));
    }

    Err(OxidriveError::sync(format!(
        "cannot resolve path for nested/new file id '{}'",
        change.file_id
    )))
}

fn file_name_from_relative(path: &RelativePath) -> &str {
    path.as_str()
        .rsplit_once('/')
        .map(|(_, name)| name)
        .unwrap_or_else(|| path.as_str())
}

fn parent_relative_str(path: &RelativePath) -> &str {
    path.as_str()
        .rsplit_once('/')
        .map(|(parent, _)| parent)
        .unwrap_or("")
}

fn upload_targets_from_actions(actions: &[crate::types::SyncAction]) -> Vec<RelativePath> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for action in actions {
        match action {
            crate::types::SyncAction::Upload { path, .. } => {
                if seen.insert(path.as_str().to_string()) {
                    out.push(path.clone());
                }
            }
            crate::types::SyncAction::Conflict {
                path, resolution, ..
            } => {
                if matches!(
                    resolution,
                    crate::types::ConflictResolution::LocalWins
                        | crate::types::ConflictResolution::Rename { .. }
                ) && seen.insert(path.as_str().to_string())
                {
                    out.push(path.clone());
                }
            }
            _ => {}
        }
    }
    out
}

fn known_remote_folders(remote: &HashMap<RelativePath, DriveFile>) -> HashMap<String, String> {
    remote
        .iter()
        .filter_map(|(rel, file)| {
            if file.mime_type == FOLDER {
                Some((rel.as_str().to_string(), file.id.clone()))
            } else {
                None
            }
        })
        .collect()
}

fn changed_paths_for_index(report: &SyncReport) -> Vec<RelativePath> {
    let mut seen = HashSet::new();
    let mut changed = Vec::new();
    for p in report
        .uploaded
        .iter()
        .chain(report.downloaded.iter())
        .chain(report.deleted_local.iter())
        .chain(report.deleted_remote.iter())
    {
        if seen.insert(p.as_str().to_string()) {
            changed.push(p.clone());
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::RedbStore;
    use crate::types::{
        PendingOpKind, PendingOpStage, UploadSession, UploadSessionMode, WorkspaceConversion,
    };
    use chrono::{TimeZone, Utc};
    use tempfile::{tempdir, NamedTempFile};

    fn ts() -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(2024, 2, 3, 4, 5, 6)
            .single()
            .expect("valid timestamp")
    }

    fn record(id: &str, remote_md5: &str) -> SyncRecord {
        let t = ts();
        SyncRecord {
            drive_file_id: Some(id.to_string()),
            remote_md5: Some(remote_md5.to_string()),
            remote_mime_type: Some("text/plain".to_string()),
            remote_modified_at: Some(t),
            local_md5: "local".to_string(),
            local_mtime: t,
            local_size: 10,
            last_synced_at: t,
        }
    }

    fn file(id: &str, name: &str, md5: Option<&str>, parents: Vec<&str>) -> DriveFile {
        DriveFile {
            id: id.to_string(),
            name: name.to_string(),
            mime_type: "text/plain".to_string(),
            md5_checksum: md5.map(|v| v.to_string()),
            modified_time: ts(),
            size: Some(10),
            parents: parents.into_iter().map(str::to_string).collect(),
            trashed: false,
        }
    }

    #[test]
    fn incremental_changes_update_known_path_by_drive_id() {
        let dir = tempdir().expect("tempdir");
        let store = Store::open(dir.path()).expect("open store");
        let path = RelativePath::from("nested/a.txt");
        store
            .upsert(path.clone(), record("id-1", "old-md5"))
            .expect("upsert");
        store.set_folder_id("nested", "folder-1");

        let change = DriveChange {
            file_id: "id-1".to_string(),
            file: Some(file("id-1", "a.txt", Some("new-md5"), vec!["folder-1"])),
            removed: false,
            time: ts(),
        };
        let remote = build_incremental_remote_view(&store, "root-folder", vec![change])
            .expect("build incremental view");

        assert_eq!(
            remote.get(&path).and_then(|f| f.md5_checksum.as_deref()),
            Some("new-md5")
        );
    }

    #[test]
    fn incremental_changes_remove_known_path_when_removed() {
        let dir = tempdir().expect("tempdir");
        let store = Store::open(dir.path()).expect("open store");
        let path = RelativePath::from("nested/a.txt");
        store
            .upsert(path.clone(), record("id-1", "old-md5"))
            .expect("upsert");

        let change = DriveChange {
            file_id: "id-1".to_string(),
            file: None,
            removed: true,
            time: ts(),
        };
        let remote = build_incremental_remote_view(&store, "root-folder", vec![change])
            .expect("build incremental view");
        assert!(!remote.contains_key(&path));
    }

    #[test]
    fn incremental_changes_fail_for_unknown_nested_file() {
        let dir = tempdir().expect("tempdir");
        let store = Store::open(dir.path()).expect("open store");
        let change = DriveChange {
            file_id: "id-2".to_string(),
            file: Some(file("id-2", "new.txt", Some("md5"), vec!["nested-parent"])),
            removed: false,
            time: ts(),
        };
        let err = build_incremental_remote_view(&store, "root-folder", vec![change])
            .expect_err("should fail");
        assert!(err.to_string().contains("cannot resolve path"));
    }

    #[test]
    fn incremental_changes_fail_for_remote_move_with_same_name() {
        let dir = tempdir().expect("tempdir");
        let store = Store::open(dir.path()).expect("open store");
        let path = RelativePath::from("nested/a.txt");
        store
            .upsert(path.clone(), record("id-1", "old-md5"))
            .expect("upsert");
        store.set_folder_id("nested", "folder-1");

        let change = DriveChange {
            file_id: "id-1".to_string(),
            file: Some(file("id-1", "a.txt", Some("new-md5"), vec!["other-folder"])),
            removed: false,
            time: ts(),
        };
        let err = build_incremental_remote_view(&store, "root-folder", vec![change])
            .expect_err("should fail");
        assert!(err.to_string().contains("remote move detected"));
    }

    #[test]
    fn stubs_preserve_folder_mime_from_persisted_record() {
        let dir = tempdir().expect("tempdir");
        let store = Store::open(dir.path()).expect("open store");
        let path = RelativePath::from("nested");
        let mut folder_record = record("folder-1", "mtime:2024-02-03T04:05:06Z");
        folder_record.remote_md5 = Some("mtime:2024-02-03T04:05:06Z".to_string());
        folder_record.remote_mime_type = Some(FOLDER.to_string());
        store
            .upsert(path.clone(), folder_record)
            .expect("upsert folder");

        let remote = remote_from_records(&store, "root-folder").expect("build remote stubs");
        assert_eq!(
            remote.get(&path).map(|f| f.mime_type.as_str()),
            Some(FOLDER)
        );
    }

    #[test]
    fn recovery_clears_side_effect_done_pending_state() {
        let dir = tempdir().expect("tempdir");
        let store = Store::open(dir.path()).expect("open store");
        let path = RelativePath::from("docs/report.docx");
        let now = ts();
        store
            .upsert(path.clone(), record("id-1", "old-md5"))
            .expect("upsert");
        store
            .upsert_conversion(
                path.clone(),
                WorkspaceConversion {
                    drive_file_id: "id-1".to_string(),
                    google_mime: "application/vnd.google-apps.document".to_string(),
                    last_export_md5: Some("old-md5".to_string()),
                },
            )
            .expect("seed conversion");
        store
            .upsert_upload_session(
                path.clone(),
                UploadSession {
                    mode: UploadSessionMode::Update {
                        drive_id: "id-1".to_string(),
                    },
                    session_url: "https://upload.example/session".to_string(),
                    next_offset: 12,
                    file_size: 42,
                    local_md5: "old-md5".to_string(),
                    updated_at: now,
                },
            )
            .expect("seed upload session");

        let db_file = NamedTempFile::new().expect("tempfile");
        let redb = RedbStore::open(db_file.path()).expect("open redb");
        let payload = bincode::serialize(&crate::types::PendingOp {
            kind: PendingOpKind::Upload,
            stage: PendingOpStage::SideEffectDone,
            updated_at: now,
        })
        .expect("serialize pending");
        redb.set_pending_op_sync(path.as_str(), &payload)
            .expect("seed pending");

        recover_pending_operations(&store, &redb).expect("recover pending");

        assert!(store.get(&path).expect("record").is_none());
        assert!(store.get_conversion(&path).expect("conversion").is_none());
        assert!(store.get_upload_session(&path).expect("upload").is_none());
        assert!(redb
            .list_pending_ops_sync()
            .expect("list pending")
            .is_empty());
    }

    #[test]
    fn recovery_discards_planned_pending_without_mutating_metadata() {
        let dir = tempdir().expect("tempdir");
        let store = Store::open(dir.path()).expect("open store");
        let path = RelativePath::from("note.txt");
        let now = ts();
        let seeded = record("id-1", "md5");
        store.upsert(path.clone(), seeded.clone()).expect("upsert");

        let db_file = NamedTempFile::new().expect("tempfile");
        let redb = RedbStore::open(db_file.path()).expect("open redb");
        let payload = bincode::serialize(&crate::types::PendingOp {
            kind: PendingOpKind::Download,
            stage: PendingOpStage::Planned,
            updated_at: now,
        })
        .expect("serialize pending");
        redb.set_pending_op_sync(path.as_str(), &payload)
            .expect("seed pending");

        recover_pending_operations(&store, &redb).expect("recover pending");

        assert_eq!(store.get(&path).expect("record"), Some(seeded));
        assert!(redb
            .list_pending_ops_sync()
            .expect("list pending")
            .is_empty());
    }

    #[test]
    fn recovery_clears_side_effect_started_pending_state() {
        let dir = tempdir().expect("tempdir");
        let store = Store::open(dir.path()).expect("open store");
        let path = RelativePath::from("note.txt");
        let now = ts();
        store
            .upsert(path.clone(), record("id-1", "md5"))
            .expect("upsert");

        let db_file = NamedTempFile::new().expect("tempfile");
        let redb = RedbStore::open(db_file.path()).expect("open redb");
        let payload = bincode::serialize(&crate::types::PendingOp {
            kind: PendingOpKind::Download,
            stage: PendingOpStage::SideEffectStarted,
            updated_at: now,
        })
        .expect("serialize pending");
        redb.set_pending_op_sync(path.as_str(), &payload)
            .expect("seed pending");

        recover_pending_operations(&store, &redb).expect("recover pending");

        assert!(store.get(&path).expect("record").is_none());
        assert!(redb
            .list_pending_ops_sync()
            .expect("list pending")
            .is_empty());
    }

    #[test]
    fn recovery_clears_metadata_committed_pending_state() {
        let dir = tempdir().expect("tempdir");
        let store = Store::open(dir.path()).expect("open store");
        let path = RelativePath::from("note.txt");
        let now = ts();
        store
            .upsert(path.clone(), record("id-1", "md5"))
            .expect("upsert");

        let db_file = NamedTempFile::new().expect("tempfile");
        let redb = RedbStore::open(db_file.path()).expect("open redb");
        let payload = bincode::serialize(&crate::types::PendingOp {
            kind: PendingOpKind::Upload,
            stage: PendingOpStage::MetadataCommitted,
            updated_at: now,
        })
        .expect("serialize pending");
        redb.set_pending_op_sync(path.as_str(), &payload)
            .expect("seed pending");

        recover_pending_operations(&store, &redb).expect("recover pending");

        assert!(store.get(&path).expect("record").is_none());
        assert!(redb
            .list_pending_ops_sync()
            .expect("list pending")
            .is_empty());
    }
}
