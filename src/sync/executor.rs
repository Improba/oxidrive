//! Applies planned [`SyncAction`](crate::types::SyncAction) values with bounded upload/download concurrency.

use std::io::{self, IsTerminal};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use indicatif::{ProgressBar, ProgressStyle};
use tokio::fs;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::drive::download::{download_file, export_file_with_fallback};
use crate::drive::types::{
    export_format_sync, is_google_workspace, remote_content_fingerprint, DriveFile, FOLDER,
};
use crate::drive::upload::{
    update_file_with_resume, upload_file_with_resume, upload_with_conversion_with_resume,
    ResumableUploadState, RESUMABLE_UPLOAD_THRESHOLD_BYTES,
};
use crate::drive::DriveClient;
use crate::error::OxidriveError;
use crate::store::Store;
use crate::types::{
    ConflictResolution, RelativePath, SyncAction, SyncRecord, SyncReport, UploadSession,
    UploadSessionMode, WorkspaceConversion, RESUMABLE_UPLOAD_SESSION_TTL_HOURS,
};
use crate::utils::hash::compute_md5;

type TaskResult = Result<Outcome, (RelativePath, OxidriveError)>;

/// Counts discrete sync steps so the progress bar total matches synchronous work plus join completions.
fn sync_progress_total(actions: &[SyncAction]) -> u64 {
    let mut n = 0u64;
    for action in actions {
        match action {
            SyncAction::Skip { .. }
            | SyncAction::CleanupMetadata { .. }
            | SyncAction::DeleteLocal { .. } => n += 1,
            SyncAction::Upload { .. }
            | SyncAction::Download { .. }
            | SyncAction::DeleteRemote { .. } => n += 1,
            SyncAction::Conflict {
                path,
                remote_id,
                resolution,
                ..
            } => match conflict_resolution_actions(path, remote_id.clone(), resolution.clone()) {
                Ok(follow_ups) => {
                    for fu in &follow_ups {
                        match fu {
                            SyncAction::Upload { .. } | SyncAction::Download { .. } => n += 1,
                            _ => n += 1,
                        }
                    }
                }
                Err(_) => n += 1,
            },
        }
    }
    n
}

/// Runs sync I/O with separate semaphores for uploads and downloads.
pub struct SyncExecutor {
    upload_sem: Arc<Semaphore>,
    download_sem: Arc<Semaphore>,
}

impl SyncExecutor {
    /// Creates an executor with the given concurrency limits (each at least one slot).
    pub fn new(max_uploads: usize, max_downloads: usize) -> Self {
        Self {
            upload_sem: Arc::new(Semaphore::new(max_uploads.max(1))),
            download_sem: Arc::new(Semaphore::new(max_downloads.max(1))),
        }
    }

    /// Executes every action, returning an aggregated [`SyncReport`].
    pub async fn execute(
        &self,
        actions: Vec<SyncAction>,
        client: &DriveClient,
        store: &Store,
    ) -> Result<SyncReport, OxidriveError> {
        let started = Instant::now();
        let root = store.sync_dir().clone();
        let folder_id = store
            .root_drive_folder_id()?
            .ok_or_else(|| OxidriveError::sync("root drive folder id not set on store"))?;
        let remote_snap = store.remote_snapshot()?.unwrap_or_default();
        let store = store.clone();
        let client = client.clone();
        let stale_upload_sessions = store.purge_stale_upload_sessions(chrono::Duration::hours(
            RESUMABLE_UPLOAD_SESSION_TTL_HOURS,
        ))?;
        if stale_upload_sessions > 0 {
            tracing::info!(
                stale_upload_sessions,
                "purged stale resumable upload sessions before execution"
            );
        }

        let mut report = SyncReport {
            uploaded: Vec::new(),
            downloaded: Vec::new(),
            deleted_local: Vec::new(),
            deleted_remote: Vec::new(),
            conflicts: Vec::new(),
            skipped: 0,
            errors: Vec::new(),
            duration: Duration::ZERO,
        };

        let total_steps = sync_progress_total(&actions);
        let pb = if io::stdout().is_terminal() {
            let pb = ProgressBar::new(total_steps);
            pb.set_style(
                ProgressStyle::with_template(
                    "{spinner:.green} [{bar:40.cyan/blue}] {pos}/{len} {msg}",
                )
                .unwrap_or_else(|_| ProgressStyle::default_bar()),
            );
            pb
        } else {
            ProgressBar::hidden()
        };

        let mut pending: JoinSet<TaskResult> = JoinSet::new();

        for action in actions {
            match action {
                SyncAction::Skip { .. } => {
                    report.skipped += 1;
                    pb.inc(1);
                    pb.set_message("skip");
                }
                SyncAction::Conflict {
                    path,
                    remote_id,
                    resolution,
                    ..
                } => {
                    report.conflicts.push(path.clone());
                    if let ConflictResolution::Rename { suffix } = resolution.clone() {
                        let Some(remote_id) = remote_id else {
                            report.errors.push((
                                path.clone(),
                                "conflict rename requires a remote id".to_string(),
                            ));
                            pb.inc(1);
                            pb.set_message("error");
                            continue;
                        };
                        let renamed_path = path_with_suffix(&path, &suffix);
                        let remote_meta = remote_snap.get(&path).cloned();
                        let listing_row = remote_meta.clone();

                        tracing::warn!(
                            path = %path,
                            resolution = ?resolution,
                            action_count = 2,
                            "applying conflict resolution"
                        );

                        let download = async {
                            let _permit = self
                                .download_sem
                                .acquire()
                                .await
                                .map_err(|e| OxidriveError::sync(e.to_string()))?;
                            run_download(
                                &client,
                                &store,
                                &root,
                                renamed_path,
                                remote_id.clone(),
                                remote_meta.as_ref(),
                            )
                            .await
                        }
                        .await;

                        match download {
                            Ok(outcome) => {
                                apply_outcome(&mut report, outcome);
                                pb.inc(1);
                                pb.set_message("download");
                            }
                            Err(e) => {
                                report.errors.push((path.clone(), e.to_string()));
                                pb.inc(1);
                                pb.set_message("error");
                                continue;
                            }
                        }

                        let upload = async {
                            let _permit = self
                                .upload_sem
                                .acquire()
                                .await
                                .map_err(|e| OxidriveError::sync(e.to_string()))?;
                            run_upload(
                                &client,
                                &store,
                                &root,
                                &folder_id,
                                path.clone(),
                                Some(remote_id),
                                listing_row,
                            )
                            .await
                        }
                        .await;

                        match upload {
                            Ok(outcome) => {
                                apply_outcome(&mut report, outcome);
                                pb.inc(1);
                                pb.set_message("upload");
                            }
                            Err(e) => {
                                report.errors.push((path.clone(), e.to_string()));
                                pb.inc(1);
                                pb.set_message("error");
                            }
                        }
                        continue;
                    }

                    let follow_ups = match conflict_resolution_actions(
                        &path,
                        remote_id.clone(),
                        resolution.clone(),
                    ) {
                        Ok(actions) => actions,
                        Err(e) => {
                            report.errors.push((path.clone(), e.to_string()));
                            pb.inc(1);
                            pb.set_message("conflict");
                            continue;
                        }
                    };
                    tracing::warn!(
                        path = %path,
                        resolution = ?resolution,
                        action_count = follow_ups.len(),
                        "applying conflict resolution"
                    );

                    let original_remote_meta = remote_snap.get(&path).cloned();
                    for follow_up in follow_ups {
                        match follow_up {
                            SyncAction::Upload { path, remote_id } => {
                                let store = store.clone();
                                let client = client.clone();
                                let sem = Arc::clone(&self.upload_sem);
                                let root = root.clone();
                                let folder_id = folder_id.clone();
                                let listing_row = remote_snap.get(&path).cloned();
                                let path_err = path.clone();
                                pending.spawn(async move {
                                    let res = async {
                                        let _permit = sem
                                            .acquire()
                                            .await
                                            .map_err(|e| OxidriveError::sync(e.to_string()))?;
                                        run_upload(
                                            &client,
                                            &store,
                                            &root,
                                            &folder_id,
                                            path,
                                            remote_id,
                                            listing_row,
                                        )
                                        .await
                                    }
                                    .await;
                                    res.map_err(|e| (path_err, e))
                                });
                            }
                            SyncAction::Download { path, remote_id } => {
                                let store = store.clone();
                                let client = client.clone();
                                let sem = Arc::clone(&self.download_sem);
                                let root = root.clone();
                                let remote_meta = remote_snap
                                    .get(&path)
                                    .cloned()
                                    .or_else(|| original_remote_meta.clone());
                                let path_err = path.clone();
                                pending.spawn(async move {
                                    let res = async {
                                        let _permit = sem
                                            .acquire()
                                            .await
                                            .map_err(|e| OxidriveError::sync(e.to_string()))?;
                                        run_download(
                                            &client,
                                            &store,
                                            &root,
                                            path,
                                            remote_id,
                                            remote_meta.as_ref(),
                                        )
                                        .await
                                    }
                                    .await;
                                    res.map_err(|e| (path_err, e))
                                });
                            }
                            _ => {
                                report.errors.push((
                                    path.clone(),
                                    "invalid follow-up action generated for conflict".to_string(),
                                ));
                                pb.inc(1);
                                pb.set_message("error");
                            }
                        }
                    }
                }
                SyncAction::CleanupMetadata { path } => {
                    if let Err(e) = store.remove(&path) {
                        report
                            .errors
                            .push((path.clone(), format!("cleanup metadata: {e}")));
                    }
                    let _ = store.remove_upload_session(&path);
                    pb.inc(1);
                    pb.set_message("cleanup");
                }
                SyncAction::Upload { path, remote_id } => {
                    let store = store.clone();
                    let client = client.clone();
                    let sem = Arc::clone(&self.upload_sem);
                    let root = root.clone();
                    let folder_id = folder_id.clone();
                    let listing_row = remote_snap.get(&path).cloned();
                    let path_err = path.clone();
                    pending.spawn(async move {
                        let res = async {
                            let _permit = sem
                                .acquire()
                                .await
                                .map_err(|e| OxidriveError::sync(e.to_string()))?;
                            run_upload(
                                &client,
                                &store,
                                &root,
                                &folder_id,
                                path,
                                remote_id,
                                listing_row,
                            )
                            .await
                        }
                        .await;
                        res.map_err(|e| (path_err, e))
                    });
                }
                SyncAction::Download { path, remote_id } => {
                    let store = store.clone();
                    let client = client.clone();
                    let sem = Arc::clone(&self.download_sem);
                    let root = root.clone();
                    let remote_meta = remote_snap.get(&path).cloned();
                    let path_err = path.clone();
                    pending.spawn(async move {
                        let res = async {
                            let _permit = sem
                                .acquire()
                                .await
                                .map_err(|e| OxidriveError::sync(e.to_string()))?;
                            run_download(
                                &client,
                                &store,
                                &root,
                                path,
                                remote_id,
                                remote_meta.as_ref(),
                            )
                            .await
                        }
                        .await;
                        res.map_err(|e| (path_err, e))
                    });
                }
                SyncAction::DeleteLocal { path } => {
                    let p = path.clone();
                    let full = match resolve_local_path(&root, &path) {
                        Ok(full) => full,
                        Err(e) => {
                            report.errors.push((p, format!("delete local: {e}")));
                            pb.inc(1);
                            pb.set_message("error");
                            continue;
                        }
                    };
                    match fs::remove_file(&full).await {
                        Ok(()) => {
                            report.deleted_local.push(p.clone());
                            let _ = store.remove(&p);
                            let _ = store.remove_conversion(&p);
                            let _ = store.remove_upload_session(&p);
                        }
                        Err(e) => report.errors.push((p, format!("delete local: {e}"))),
                    }
                    pb.inc(1);
                    pb.set_message("delete local");
                }
                SyncAction::DeleteRemote { path, remote_id } => {
                    let client = client.clone();
                    let store = store.clone();
                    let sem = Arc::clone(&self.upload_sem);
                    let path_err = path.clone();
                    pending.spawn(async move {
                        let res = async {
                            let _permit = sem
                                .acquire()
                                .await
                                .map_err(|e| OxidriveError::sync(e.to_string()))?;
                            run_trash_remote(&client, &store, path, remote_id).await
                        }
                        .await;
                        res.map_err(|e| (path_err, e))
                    });
                }
            }
        }

        while let Some(joined) = pending.join_next().await {
            match joined {
                Ok(Ok(outcome)) => {
                    let msg = match &outcome {
                        Outcome::Uploaded(_) => "upload",
                        Outcome::Downloaded(_) => "download",
                        Outcome::DeletedRemote(_) => "delete remote",
                    };
                    apply_outcome(&mut report, outcome);
                    pb.inc(1);
                    pb.set_message(msg);
                }
                Ok(Err((p, e))) => {
                    report.errors.push((p, e.to_string()));
                    pb.inc(1);
                    pb.set_message("error");
                }
                Err(j) => {
                    report
                        .errors
                        .push((RelativePath::from("__join__"), format!("task join: {j}")));
                    pb.inc(1);
                    pb.set_message("error");
                }
            }
        }

        pb.finish_with_message("Sync complete");
        report.duration = started.elapsed();
        Ok(report)
    }
}

enum Outcome {
    Uploaded(RelativePath),
    Downloaded(RelativePath),
    DeletedRemote(RelativePath),
}

fn apply_outcome(report: &mut SyncReport, o: Outcome) {
    match o {
        Outcome::Uploaded(p) => report.uploaded.push(p),
        Outcome::Downloaded(p) => report.downloaded.push(p),
        Outcome::DeletedRemote(p) => report.deleted_remote.push(p),
    }
}

fn session_is_valid(
    session: &UploadSession,
    expected_mode: &UploadSessionMode,
    local_md5: &str,
    local_size: u64,
) -> bool {
    if session.mode != *expected_mode
        || session.local_md5 != local_md5
        || session.file_size != local_size
        || session.next_offset >= local_size
    {
        return false;
    }
    let age = Utc::now() - session.updated_at;
    age >= chrono::Duration::zero()
        && age <= chrono::Duration::hours(RESUMABLE_UPLOAD_SESSION_TTL_HOURS)
}

async fn run_upload(
    client: &DriveClient,
    store: &Store,
    root: &Path,
    folder_id: &str,
    path: RelativePath,
    remote_id: Option<String>,
    listing_row: Option<DriveFile>,
) -> Result<Outcome, OxidriveError> {
    let full = resolve_local_path(root, &path)?;
    let name = path
        .as_str()
        .rsplit_once('/')
        .map(|(_, n)| n)
        .unwrap_or_else(|| path.as_str());
    let parent_id = store.parent_drive_id(&path, folder_id)?;
    let conversion = store.get_conversion(&path)?;
    let local_size = fs::metadata(&full)
        .await
        .map_err(|e| OxidriveError::sync(format!("stat {}: {e}", full.display())))?
        .len();
    let local_md5 = compute_md5(&full).await?;
    match remote_id {
        Some(ref id) if !id.is_empty() => {
            let session_mode = if let Some(c) = conversion.as_ref() {
                UploadSessionMode::Convert {
                    drive_id: id.clone(),
                    google_mime: c.google_mime.clone(),
                }
            } else {
                UploadSessionMode::Update {
                    drive_id: id.clone(),
                }
            };
            let resume_state = if local_size > RESUMABLE_UPLOAD_THRESHOLD_BYTES {
                match store.get_upload_session(&path)? {
                    Some(session)
                        if session_is_valid(&session, &session_mode, &local_md5, local_size) =>
                    {
                        Some(ResumableUploadState {
                            session_url: session.session_url,
                            next_offset: session.next_offset,
                            file_size: session.file_size,
                        })
                    }
                    Some(_) => {
                        let _ = store.remove_upload_session(&path);
                        None
                    }
                    None => None,
                }
            } else {
                let _ = store.remove_upload_session(&path);
                None
            };

            if let Some(c) = conversion.as_ref() {
                let path_for_session = path.clone();
                let mode_for_session = session_mode.clone();
                let local_md5_for_session = local_md5.clone();
                let store_for_session = store.clone();
                upload_with_conversion_with_resume(
                    client,
                    &full,
                    id,
                    &c.google_mime,
                    resume_state,
                    move |state| {
                        store_for_session.upsert_upload_session(
                            path_for_session.clone(),
                            UploadSession {
                                mode: mode_for_session.clone(),
                                session_url: state.session_url,
                                next_offset: state.next_offset,
                                file_size: state.file_size,
                                local_md5: local_md5_for_session.clone(),
                                updated_at: Utc::now(),
                            },
                        )
                    },
                )
                .await?;
            } else {
                let path_for_session = path.clone();
                let mode_for_session = session_mode.clone();
                let local_md5_for_session = local_md5.clone();
                let store_for_session = store.clone();
                update_file_with_resume(client, &full, id, resume_state, move |state| {
                    store_for_session.upsert_upload_session(
                        path_for_session.clone(),
                        UploadSession {
                            mode: mode_for_session.clone(),
                            session_url: state.session_url,
                            next_offset: state.next_offset,
                            file_size: state.file_size,
                            local_md5: local_md5_for_session.clone(),
                            updated_at: Utc::now(),
                        },
                    )
                })
                .await?;
            }
            let mut merged = listing_row;
            if let Some(ref mut r) = merged {
                r.id.clone_from(id);
            } else {
                merged = Some(DriveFile {
                    id: id.clone(),
                    name: name.to_string(),
                    mime_type: "application/octet-stream".into(),
                    md5_checksum: None,
                    modified_time: Utc::now(),
                    size: Some(local_size),
                    parents: vec![],
                    trashed: false,
                });
            }
            upsert_local_record(
                store,
                &path,
                &full,
                merged.as_ref(),
                Some(local_md5.as_str()),
                Some(local_size),
            )
            .await?;
            if let Some(mut c) = conversion {
                c.last_export_md5 = Some(local_md5.clone());
                store.upsert_conversion(path.clone(), c)?;
            }
            let _ = store.remove_upload_session(&path);
        }
        _ => {
            let session_mode = UploadSessionMode::Create {
                parent_id: parent_id.clone(),
                name: name.to_string(),
            };
            let resume_state = if local_size > RESUMABLE_UPLOAD_THRESHOLD_BYTES {
                match store.get_upload_session(&path)? {
                    Some(session)
                        if session_is_valid(&session, &session_mode, &local_md5, local_size) =>
                    {
                        Some(ResumableUploadState {
                            session_url: session.session_url,
                            next_offset: session.next_offset,
                            file_size: session.file_size,
                        })
                    }
                    Some(_) => {
                        let _ = store.remove_upload_session(&path);
                        None
                    }
                    None => None,
                }
            } else {
                let _ = store.remove_upload_session(&path);
                None
            };
            let path_for_session = path.clone();
            let mode_for_session = session_mode.clone();
            let local_md5_for_session = local_md5.clone();
            let store_for_session = store.clone();
            let new_id = upload_file_with_resume(
                client,
                &full,
                &parent_id,
                name,
                resume_state,
                move |state| {
                    store_for_session.upsert_upload_session(
                        path_for_session.clone(),
                        UploadSession {
                            mode: mode_for_session.clone(),
                            session_url: state.session_url,
                            next_offset: state.next_offset,
                            file_size: state.file_size,
                            local_md5: local_md5_for_session.clone(),
                            updated_at: Utc::now(),
                        },
                    )
                },
            )
            .await?;
            let stub = DriveFile {
                id: new_id,
                name: name.to_string(),
                mime_type: "application/octet-stream".into(),
                md5_checksum: None,
                modified_time: Utc::now(),
                size: Some(local_size),
                parents: vec![],
                trashed: false,
            };
            upsert_local_record(
                store,
                &path,
                &full,
                Some(&stub),
                Some(local_md5.as_str()),
                Some(local_size),
            )
            .await?;
            if let Some(mut c) = conversion {
                c.last_export_md5 = Some(local_md5);
                store.upsert_conversion(path.clone(), c)?;
            } else {
                let _ = store.remove_conversion(&path);
            }
            let _ = store.remove_upload_session(&path);
        }
    }
    Ok(Outcome::Uploaded(path))
}

async fn run_download(
    client: &DriveClient,
    store: &Store,
    root: &Path,
    path: RelativePath,
    remote_id: String,
    remote_meta: Option<&DriveFile>,
) -> Result<Outcome, OxidriveError> {
    let mut local_path = path.clone();
    let mut full = resolve_local_path(root, &local_path)?;
    if let Some(dir) = full.parent() {
        fs::create_dir_all(dir)
            .await
            .map_err(|e| OxidriveError::sync(format!("mkdir {}: {e}", dir.display())))?;
    }

    if let Some(r) = remote_meta {
        if r.mime_type == FOLDER {
            fs::create_dir_all(&full).await.map_err(|e| {
                OxidriveError::sync(format!("mkdir folder {}: {e}", full.display()))
            })?;
            tracing::debug!(path = %path, "created local folder mirror");
            return Ok(Outcome::Downloaded(path));
        }
        if is_google_workspace(&r.mime_type) {
            let fmt = export_format_sync(&r.mime_type).ok_or_else(|| {
                OxidriveError::sync(format!(
                    "missing sync export format for mime '{}'",
                    r.mime_type
                ))
            })?;
            local_path = converted_relative_path(&path, fmt.extension);
            full = resolve_local_path(root, &local_path)?;
            if let Some(dir) = full.parent() {
                fs::create_dir_all(dir)
                    .await
                    .map_err(|e| OxidriveError::sync(format!("mkdir {}: {e}", dir.display())))?;
            }
            export_file_with_fallback(client, &remote_id, fmt.export_mime, &full).await?;
            let export_md5 =
                upsert_local_record(store, &local_path, &full, Some(r), None, None).await?;
            store.upsert_conversion(
                local_path.clone(),
                WorkspaceConversion {
                    drive_file_id: remote_id.clone(),
                    google_mime: r.mime_type.clone(),
                    last_export_md5: Some(export_md5),
                },
            )?;
            let _ = store.remove_upload_session(&local_path);
            if local_path != path {
                let _ = store.remove(&path);
                let _ = store.remove_conversion(&path);
                let _ = store.remove_upload_session(&path);
            }
            return Ok(Outcome::Downloaded(local_path));
        }
    }

    download_file(client, &remote_id, &full).await?;
    let r = remote_meta.cloned();
    let _ = upsert_local_record(store, &local_path, &full, r.as_ref(), None, None).await?;
    let _ = store.remove_conversion(&local_path);
    let _ = store.remove_upload_session(&local_path);
    Ok(Outcome::Downloaded(path))
}

fn resolve_local_path(root: &Path, path: &RelativePath) -> Result<PathBuf, OxidriveError> {
    if !path.is_safe_non_empty() {
        return Err(OxidriveError::sync(format!(
            "unsafe relative path '{}'",
            path.as_str()
        )));
    }
    Ok(root.join(path.as_str()))
}

async fn run_trash_remote(
    client: &DriveClient,
    store: &Store,
    path: RelativePath,
    remote_id: String,
) -> Result<Outcome, OxidriveError> {
    let body = Arc::new(serde_json::json!({ "trashed": true }));
    let url = client.drive_api_url(&format!("/files/{remote_id}?supportsAllDrives=true"));
    let _ = client
        .request(reqwest::Method::PATCH, &url, move |b| b.json(body.as_ref()))
        .await?;
    store.remove(&path)?;
    let _ = store.remove_conversion(&path);
    let _ = store.remove_upload_session(&path);
    Ok(Outcome::DeletedRemote(path))
}

async fn upsert_local_record(
    store: &Store,
    path: &RelativePath,
    local_path: &std::path::Path,
    remote: Option<&DriveFile>,
    known_md5: Option<&str>,
    known_size: Option<u64>,
) -> Result<String, OxidriveError> {
    let md5 = match known_md5 {
        Some(value) => value.to_string(),
        None => compute_md5(local_path).await?,
    };
    let meta = fs::metadata(local_path)
        .await
        .map_err(|e| OxidriveError::sync(format!("stat {}: {e}", local_path.display())))?;
    let mtime = meta
        .modified()
        .map_err(|e| OxidriveError::sync(format!("mtime: {e}")))?;
    let mtime_utc = DateTime::<Utc>::from(mtime);

    let record = SyncRecord {
        drive_file_id: remote.map(|r| r.id.clone()),
        remote_md5: remote.map(remote_content_fingerprint),
        remote_modified_at: remote.map(|r| r.modified_time),
        local_md5: md5.clone(),
        local_mtime: mtime_utc,
        local_size: known_size.unwrap_or(meta.len()),
        last_synced_at: Utc::now(),
    };
    store.upsert(path.clone(), record)?;
    Ok(md5)
}

fn path_with_suffix(path: &RelativePath, suffix: &str) -> RelativePath {
    let raw = path.as_str();
    let (dir, file_name) = match raw.rsplit_once('/') {
        Some((d, f)) => (Some(d), f),
        None => (None, raw),
    };
    let renamed = apply_suffix_to_file_name(file_name, suffix);
    match dir {
        Some(d) => RelativePath::from(format!("{d}/{renamed}")),
        None => RelativePath::from(renamed),
    }
}

fn conflict_resolution_actions(
    path: &RelativePath,
    remote_id: Option<String>,
    resolution: ConflictResolution,
) -> Result<Vec<SyncAction>, OxidriveError> {
    match resolution {
        ConflictResolution::LocalWins => Ok(vec![SyncAction::Upload {
            path: path.clone(),
            remote_id,
        }]),
        ConflictResolution::RemoteWins => {
            let remote_id = remote_id
                .ok_or_else(|| OxidriveError::sync("conflict remote_wins requires a remote id"))?;
            Ok(vec![SyncAction::Download {
                path: path.clone(),
                remote_id,
            }])
        }
        ConflictResolution::Rename { suffix } => {
            let remote_id = remote_id
                .ok_or_else(|| OxidriveError::sync("conflict rename requires a remote id"))?;
            Ok(vec![
                SyncAction::Download {
                    path: path_with_suffix(path, &suffix),
                    remote_id: remote_id.clone(),
                },
                SyncAction::Upload {
                    path: path.clone(),
                    remote_id: Some(remote_id),
                },
            ])
        }
    }
}

fn apply_suffix_to_file_name(name: &str, suffix: &str) -> String {
    match name.rfind('.') {
        Some(dot) if dot > 0 => format!("{}{}{}", &name[..dot], suffix, &name[dot..]),
        _ => format!("{name}{suffix}"),
    }
}

fn converted_relative_path(path: &RelativePath, extension: &str) -> RelativePath {
    let raw = path.as_str();
    let (dir, file_name) = match raw.rsplit_once('/') {
        Some((d, f)) => (Some(d), f),
        None => (None, raw),
    };
    let stem = match file_name.rsplit_once('.') {
        Some((s, ext)) if !s.is_empty() && !ext.is_empty() => s,
        _ => file_name,
    };
    let renamed = format!("{stem}.{extension}");
    match dir {
        Some(d) => RelativePath::from(format!("{d}/{renamed}")),
        None => RelativePath::from(renamed),
    }
}

#[cfg(test)]
mod tests {
    use crate::types::{ConflictResolution, RelativePath, SyncAction};

    use super::{conflict_resolution_actions, path_with_suffix};

    #[test]
    fn rename_suffix_inserted_before_extension() {
        let renamed = path_with_suffix(
            &RelativePath::from("docs/report.txt"),
            " (conflict 2026-04-10)",
        );
        assert_eq!(renamed.as_str(), "docs/report (conflict 2026-04-10).txt");
    }

    #[test]
    fn rename_suffix_appended_when_no_extension() {
        let renamed = path_with_suffix(&RelativePath::from("README"), ".conflict");
        assert_eq!(renamed.as_str(), "README.conflict");
    }

    #[test]
    fn rename_conflict_resolution_variant_is_supported() {
        let path = RelativePath::from("notes/todo.md");
        let actions = conflict_resolution_actions(
            &path,
            Some("remote-42".to_string()),
            ConflictResolution::Rename {
                suffix: " (conflict)".to_string(),
            },
        )
        .unwrap();

        assert_eq!(actions.len(), 2);
        assert!(matches!(
            &actions[0],
            SyncAction::Download { path, remote_id }
                if path.as_str() == "notes/todo (conflict).md" && remote_id == "remote-42"
        ));
        assert!(matches!(
            &actions[1],
            SyncAction::Upload { path, remote_id: Some(remote_id) }
                if path.as_str() == "notes/todo.md" && remote_id == "remote-42"
        ));
    }

    #[test]
    fn remote_wins_requires_remote_id() {
        let path = RelativePath::from("notes/todo.md");
        let err = conflict_resolution_actions(&path, None, ConflictResolution::RemoteWins)
            .unwrap_err()
            .to_string();
        assert!(err.contains("remote_wins requires a remote id"));
    }
}
