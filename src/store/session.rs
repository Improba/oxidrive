//! In-memory sync session: metadata map, remote listing snapshot, and root folder id for one run.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use crate::drive::types::DriveFile;
use crate::drive::types::FOLDER;
use crate::error::OxidriveError;
use crate::store::db::SessionStateBatch;
use crate::store::RedbStore;
use crate::types::{
    RelativePath, SyncRecord, UploadSession, WorkspaceConversion, MAX_UPLOAD_SESSION_BLOB_BYTES,
};
use chrono::Utc;

/// Per-run state used by [`crate::sync::engine`] and [`crate::sync::executor`].
///
/// Durable history lives in [`super::RedbStore`]; this handle is cheap to clone across tasks.
#[derive(Clone)]
pub struct Store {
    sync_dir: PathBuf,
    records: Arc<Mutex<HashMap<RelativePath, SyncRecord>>>,
    conversions: Arc<Mutex<HashMap<RelativePath, WorkspaceConversion>>>,
    upload_sessions: Arc<Mutex<HashMap<RelativePath, UploadSession>>>,
    remote_snapshot: Arc<Mutex<Option<HashMap<RelativePath, DriveFile>>>>,
    root_drive_folder_id: Arc<Mutex<Option<String>>>,
    folder_ids: Arc<Mutex<HashMap<String, String>>>,
}

impl Store {
    /// Creates an empty session rooted at `sync_dir` (the mirrored directory).
    pub fn open(sync_dir: impl Into<PathBuf>) -> Result<Self, OxidriveError> {
        Ok(Self {
            sync_dir: sync_dir.into(),
            records: Arc::new(Mutex::new(HashMap::new())),
            conversions: Arc::new(Mutex::new(HashMap::new())),
            upload_sessions: Arc::new(Mutex::new(HashMap::new())),
            remote_snapshot: Arc::new(Mutex::new(None)),
            root_drive_folder_id: Arc::new(Mutex::new(None)),
            folder_ids: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// Directory mirrored with Google Drive.
    pub fn sync_dir(&self) -> &PathBuf {
        &self.sync_dir
    }

    fn lock_records(
        &self,
    ) -> Result<MutexGuard<'_, HashMap<RelativePath, SyncRecord>>, OxidriveError> {
        self.records
            .lock()
            .map_err(|e: std::sync::PoisonError<_>| OxidriveError::store(e.to_string()))
    }

    /// Returns the persisted record for `path`, if any.
    pub fn get(&self, path: &RelativePath) -> Result<Option<SyncRecord>, OxidriveError> {
        let g = self.lock_records()?;
        Ok(g.get(path).cloned())
    }

    /// Inserts or replaces metadata for `path`.
    pub fn upsert(&self, path: RelativePath, record: SyncRecord) -> Result<(), OxidriveError> {
        let mut g = self.lock_records()?;
        g.insert(path, record);
        Ok(())
    }

    /// Drops metadata for `path`.
    pub fn remove(&self, path: &RelativePath) -> Result<(), OxidriveError> {
        let mut g = self.lock_records()?;
        g.remove(path);
        Ok(())
    }

    /// All stored `(path, record)` pairs.
    pub fn iter_records(&self) -> Result<Vec<(RelativePath, SyncRecord)>, OxidriveError> {
        let g = self.lock_records()?;
        Ok(g.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
    }

    /// Number of tracked sync metadata rows currently held in memory.
    pub fn record_count(&self) -> Result<usize, OxidriveError> {
        let g = self.lock_records()?;
        Ok(g.len())
    }

    /// Loads all persisted sync metadata from `redb` into this session.
    pub fn load_from_redb(&self, redb: &RedbStore) -> Result<(), OxidriveError> {
        let rows = redb.list_sync_metadata_sync()?;
        let mut records = HashMap::with_capacity(rows.len());
        for (path, data) in rows {
            let record: SyncRecord = match bincode::deserialize(&data) {
                Ok(record) => record,
                Err(e) => {
                    tracing::warn!(
                        path = %path,
                        error = %e,
                        "skipping invalid persisted sync metadata payload"
                    );
                    continue;
                }
            };
            let rel = RelativePath::from(path.clone());
            if !rel.is_safe_non_empty() {
                tracing::warn!(path = %path, "skipping unsafe persisted sync metadata path");
                continue;
            }
            records.insert(rel, record);
        }
        let conversion_rows = redb.list_conversions_sync()?;
        let mut conversions = HashMap::with_capacity(conversion_rows.len());
        for (path, data) in conversion_rows {
            let conversion: WorkspaceConversion = match bincode::deserialize(&data) {
                Ok(conversion) => conversion,
                Err(e) => {
                    tracing::warn!(
                        path = %path,
                        error = %e,
                        "skipping invalid persisted conversion payload"
                    );
                    continue;
                }
            };
            let rel = RelativePath::from(path.clone());
            if !rel.is_safe_non_empty() {
                tracing::warn!(path = %path, "skipping unsafe persisted conversion path");
                continue;
            }
            conversions.insert(rel, conversion);
        }
        let conversion_count = conversions.len();
        let upload_rows = redb.list_upload_sessions_sync()?;
        let mut upload_sessions = HashMap::with_capacity(upload_rows.len());
        for (path, data) in upload_rows {
            if data.len() > MAX_UPLOAD_SESSION_BLOB_BYTES {
                tracing::warn!(
                    path = %path,
                    len = data.len(),
                    max = MAX_UPLOAD_SESSION_BLOB_BYTES,
                    "skipping oversized persisted upload session payload"
                );
                continue;
            }
            let session: UploadSession = match bincode::deserialize(&data) {
                Ok(session) => session,
                Err(e) => {
                    tracing::warn!(
                        path = %path,
                        error = %e,
                        "skipping invalid persisted upload session payload"
                    );
                    continue;
                }
            };
            let rel = RelativePath::from(path.clone());
            if !rel.is_safe_non_empty() {
                tracing::warn!(path = %path, "skipping unsafe persisted upload session path");
                continue;
            }
            upload_sessions.insert(rel, session);
        }
        let upload_session_count = upload_sessions.len();
        let folder_rows = redb.list_folder_ids_sync()?;
        let mut folder_ids = HashMap::with_capacity(folder_rows.len());
        for (path, data) in folder_rows {
            let rel = RelativePath::from(path.clone());
            if !rel.is_safe_non_empty() {
                tracing::warn!(path = %path, "skipping unsafe persisted folder id path");
                continue;
            }
            let drive_id = match String::from_utf8(data) {
                Ok(id) if !id.trim().is_empty() => id,
                Ok(_) => {
                    tracing::warn!(path = %path, "skipping empty persisted folder id");
                    continue;
                }
                Err(e) => {
                    tracing::warn!(
                        path = %path,
                        error = %e,
                        "skipping invalid persisted folder id payload"
                    );
                    continue;
                }
            };
            folder_ids.insert(rel.as_str().to_string(), drive_id);
        }
        let folder_count = folder_ids.len();
        {
            let mut record_guard = self.lock_records()?;
            *record_guard = records;
        }
        {
            let mut conversion_guard = self
                .conversions
                .lock()
                .map_err(|e: std::sync::PoisonError<_>| OxidriveError::store(e.to_string()))?;
            *conversion_guard = conversions;
        }
        {
            let mut upload_guard = self
                .upload_sessions
                .lock()
                .map_err(|e: std::sync::PoisonError<_>| OxidriveError::store(e.to_string()))?;
            *upload_guard = upload_sessions;
        }
        {
            let mut folder_guard = self
                .folder_ids
                .lock()
                .map_err(|e: std::sync::PoisonError<_>| OxidriveError::store(e.to_string()))?;
            *folder_guard = folder_ids;
        }
        let record_count = self.lock_records()?.len();
        tracing::info!(
            records = record_count,
            conversions = conversion_count,
            upload_sessions = upload_session_count,
            folder_ids = folder_count,
            "loaded persisted session state from redb"
        );
        Ok(())
    }

    /// Persists all in-memory sync metadata into `redb`, removing stale keys.
    pub fn persist_to_redb(&self, redb: &RedbStore) -> Result<(), OxidriveError> {
        let (batch, rows_written, stale_rows_removed) = self.prepare_session_state_batch(redb)?;
        redb.replace_session_state_sync(batch)?;
        tracing::info!(
            rows_written,
            stale_rows_removed,
            "persisted sync metadata to redb"
        );
        Ok(())
    }

    /// Persists all in-memory sync metadata and page token atomically into `redb`.
    pub fn persist_to_redb_and_page_token(
        &self,
        redb: &RedbStore,
        page_token: &str,
    ) -> Result<(), OxidriveError> {
        let (batch, rows_written, stale_rows_removed) = self.prepare_session_state_batch(redb)?;
        redb.replace_session_state_and_page_token_sync(batch, Some(page_token))?;
        tracing::info!(
            rows_written,
            stale_rows_removed,
            "persisted sync metadata and page token to redb"
        );
        Ok(())
    }

    fn prepare_session_state_batch(
        &self,
        redb: &RedbStore,
    ) -> Result<(SessionStateBatch, usize, usize), OxidriveError> {
        let snapshot: Vec<(RelativePath, SyncRecord)> = self.iter_records()?;
        let existing_keys: HashSet<String> =
            redb.list_sync_metadata_keys_sync()?.into_iter().collect();
        let mut desired_keys = HashSet::with_capacity(snapshot.len());
        let mut sync_metadata = Vec::with_capacity(snapshot.len());

        for (path, record) in snapshot {
            if !path.is_safe_non_empty() {
                tracing::warn!(path = %path, "skipping unsafe in-memory sync metadata path");
                continue;
            }
            let key = path.as_str().to_string();
            let bytes = bincode::serialize(&record)
                .map_err(|e| OxidriveError::store(format!("encode SyncRecord for '{key}': {e}")))?;
            sync_metadata.push((key.clone(), bytes));
            desired_keys.insert(key);
        }

        let stale_sync_metadata: Vec<String> =
            existing_keys.difference(&desired_keys).cloned().collect();

        let conversion_snapshot = self
            .conversions
            .lock()
            .map_err(|e: std::sync::PoisonError<_>| OxidriveError::store(e.to_string()))?
            .clone();
        let existing_conversion_keys: HashSet<String> =
            redb.list_conversions_keys_sync()?.into_iter().collect();
        let mut desired_conversion_keys = HashSet::with_capacity(conversion_snapshot.len());
        let mut conversions = Vec::with_capacity(conversion_snapshot.len());
        for (path, conversion) in conversion_snapshot {
            if !path.is_safe_non_empty() {
                tracing::warn!(path = %path, "skipping unsafe in-memory conversion path");
                continue;
            }
            let key = path.as_str().to_string();
            let bytes = bincode::serialize(&conversion)
                .map_err(|e| OxidriveError::store(format!("encode conversion for '{key}': {e}")))?;
            conversions.push((key.clone(), bytes));
            desired_conversion_keys.insert(key);
        }
        let stale_conversions: Vec<String> = existing_conversion_keys
            .difference(&desired_conversion_keys)
            .cloned()
            .collect();

        let upload_snapshot = self
            .upload_sessions
            .lock()
            .map_err(|e: std::sync::PoisonError<_>| OxidriveError::store(e.to_string()))?
            .clone();
        let existing_upload_session_keys: HashSet<String> =
            redb.list_upload_sessions_keys_sync()?.into_iter().collect();
        let mut desired_upload_session_keys = HashSet::with_capacity(upload_snapshot.len());
        let mut upload_sessions = Vec::with_capacity(upload_snapshot.len());
        for (path, session) in upload_snapshot {
            if !path.is_safe_non_empty() {
                tracing::warn!(path = %path, "skipping unsafe in-memory upload session path");
                continue;
            }
            let key = path.as_str().to_string();
            let bytes = bincode::serialize(&session).map_err(|e| {
                OxidriveError::store(format!("encode upload session for '{key}': {e}"))
            })?;
            if bytes.len() > MAX_UPLOAD_SESSION_BLOB_BYTES {
                tracing::warn!(
                    path = %path,
                    len = bytes.len(),
                    max = MAX_UPLOAD_SESSION_BLOB_BYTES,
                    "skipping oversized in-memory upload session payload"
                );
                continue;
            }
            upload_sessions.push((key.clone(), bytes));
            desired_upload_session_keys.insert(key);
        }
        let stale_upload_sessions: Vec<String> = existing_upload_session_keys
            .difference(&desired_upload_session_keys)
            .cloned()
            .collect();
        let folder_snapshot = self.all_folder_ids()?;
        let existing_folder_keys: HashSet<String> =
            redb.list_folder_ids_keys_sync()?.into_iter().collect();
        let mut desired_folder_keys = HashSet::with_capacity(folder_snapshot.len());
        let mut folder_ids = Vec::with_capacity(folder_snapshot.len());
        for (path, drive_id) in folder_snapshot {
            let rel = RelativePath::from(path.as_str());
            if !rel.is_safe_non_empty() {
                tracing::warn!(path = %path, "skipping unsafe in-memory folder id path");
                continue;
            }
            if drive_id.trim().is_empty() {
                tracing::warn!(path = %path, "skipping empty in-memory folder id");
                continue;
            }
            let key = rel.as_str().to_string();
            folder_ids.push((key.clone(), drive_id.into_bytes()));
            desired_folder_keys.insert(key);
        }
        let stale_folder_ids: Vec<String> = existing_folder_keys
            .difference(&desired_folder_keys)
            .cloned()
            .collect();
        let stale_sync_metadata_count = stale_sync_metadata.len();

        Ok((
            SessionStateBatch {
            sync_metadata,
            stale_sync_metadata,
            conversions,
            stale_conversions,
            upload_sessions,
            stale_upload_sessions,
            folder_ids,
            stale_folder_ids,
            },
            desired_keys.len(),
            stale_sync_metadata_count,
        ))
    }

    /// Union of paths that appear in local metadata.
    pub fn all_record_paths(&self) -> Result<HashSet<RelativePath>, OxidriveError> {
        let g = self.lock_records()?;
        Ok(g.keys().cloned().collect())
    }

    /// Returns conversion metadata for `path` if this local file mirrors a Workspace doc export.
    pub fn get_conversion(
        &self,
        path: &RelativePath,
    ) -> Result<Option<WorkspaceConversion>, OxidriveError> {
        let g = self
            .conversions
            .lock()
            .map_err(|e: std::sync::PoisonError<_>| OxidriveError::store(e.to_string()))?;
        Ok(g.get(path).cloned())
    }

    /// Upserts conversion metadata for `path`.
    pub fn upsert_conversion(
        &self,
        path: RelativePath,
        conversion: WorkspaceConversion,
    ) -> Result<(), OxidriveError> {
        let mut g = self
            .conversions
            .lock()
            .map_err(|e: std::sync::PoisonError<_>| OxidriveError::store(e.to_string()))?;
        g.insert(path, conversion);
        Ok(())
    }

    /// Removes conversion metadata for `path`.
    pub fn remove_conversion(&self, path: &RelativePath) -> Result<(), OxidriveError> {
        let mut g = self
            .conversions
            .lock()
            .map_err(|e: std::sync::PoisonError<_>| OxidriveError::store(e.to_string()))?;
        g.remove(path);
        Ok(())
    }

    /// Returns resumable upload session state for `path`, if present.
    pub fn get_upload_session(
        &self,
        path: &RelativePath,
    ) -> Result<Option<UploadSession>, OxidriveError> {
        let g = self
            .upload_sessions
            .lock()
            .map_err(|e: std::sync::PoisonError<_>| OxidriveError::store(e.to_string()))?;
        Ok(g.get(path).cloned())
    }

    /// Upserts resumable upload session state for `path`.
    pub fn upsert_upload_session(
        &self,
        path: RelativePath,
        session: UploadSession,
    ) -> Result<(), OxidriveError> {
        let mut g = self
            .upload_sessions
            .lock()
            .map_err(|e: std::sync::PoisonError<_>| OxidriveError::store(e.to_string()))?;
        g.insert(path, session);
        Ok(())
    }

    /// Removes resumable upload session state for `path`.
    pub fn remove_upload_session(&self, path: &RelativePath) -> Result<(), OxidriveError> {
        let mut g = self
            .upload_sessions
            .lock()
            .map_err(|e: std::sync::PoisonError<_>| OxidriveError::store(e.to_string()))?;
        g.remove(path);
        Ok(())
    }

    /// Removes stale or invalid resumable upload sessions and returns how many were deleted.
    pub fn purge_stale_upload_sessions(
        &self,
        max_age: chrono::Duration,
    ) -> Result<usize, OxidriveError> {
        let now = Utc::now();
        let mut g = self
            .upload_sessions
            .lock()
            .map_err(|e: std::sync::PoisonError<_>| OxidriveError::store(e.to_string()))?;
        let before = g.len();
        g.retain(|_, session| {
            let age = now - session.updated_at;
            let not_expired = age <= max_age;
            let offset_valid = session.next_offset < session.file_size;
            let size_valid = session.file_size > 0;
            not_expired && offset_valid && size_valid
        });
        Ok(before.saturating_sub(g.len()))
    }

    /// Returns a clone of the remote listing snapshot, if installed.
    pub fn remote_snapshot(
        &self,
    ) -> Result<Option<HashMap<RelativePath, DriveFile>>, OxidriveError> {
        let g = self
            .remote_snapshot
            .lock()
            .map_err(|e: std::sync::PoisonError<_>| OxidriveError::store(e.to_string()))?;
        Ok(g.clone())
    }

    /// Installs the remote listing for parent-folder resolution during execution.
    pub fn set_remote_snapshot(
        &self,
        snap: HashMap<RelativePath, DriveFile>,
    ) -> Result<(), OxidriveError> {
        let mut g = self
            .remote_snapshot
            .lock()
            .map_err(|e: std::sync::PoisonError<_>| OxidriveError::store(e.to_string()))?;
        *g = Some(snap);
        Ok(())
    }

    /// Stores the configured Drive folder id that mirrors [`Store::sync_dir`].
    pub fn set_root_drive_folder_id(&self, id: Option<String>) -> Result<(), OxidriveError> {
        let mut g = self
            .root_drive_folder_id
            .lock()
            .map_err(|e| OxidriveError::store(e.to_string()))?;
        *g = id;
        Ok(())
    }

    /// Returns the root folder id set via [`Store::set_root_drive_folder_id`].
    pub fn root_drive_folder_id(&self) -> Result<Option<String>, OxidriveError> {
        let g = self
            .root_drive_folder_id
            .lock()
            .map_err(|e| OxidriveError::store(e.to_string()))?;
        Ok(g.clone())
    }

    /// Associates a relative folder path with a Drive folder id for this sync session.
    pub fn set_folder_id(&self, rel_path: &str, drive_id: &str) {
        let normalized = rel_path.replace('\\', "/").trim_matches('/').to_string();
        match self.folder_ids.lock() {
            Ok(mut g) => {
                g.insert(normalized, drive_id.to_string());
            }
            Err(e) => {
                tracing::error!(error = %e, "failed to lock folder id map for write");
            }
        }
    }

    /// Returns the known Drive folder id for `rel_path`, if available in this session.
    pub fn get_folder_id(&self, rel_path: &str) -> Option<String> {
        let normalized = rel_path.replace('\\', "/").trim_matches('/').to_string();
        match self.folder_ids.lock() {
            Ok(g) => g.get(&normalized).cloned(),
            Err(e) => {
                tracing::error!(error = %e, "failed to lock folder id map for read");
                None
            }
        }
    }

    /// Returns all known `relative folder path -> drive folder id` mappings.
    pub fn all_folder_ids(&self) -> Result<HashMap<String, String>, OxidriveError> {
        self.folder_ids
            .lock()
            .map(|g| g.clone())
            .map_err(|e: std::sync::PoisonError<_>| OxidriveError::store(e.to_string()))
    }

    /// Clears the listing installed via [`Store::set_remote_snapshot`].
    pub fn clear_remote_snapshot(&self) -> Result<(), OxidriveError> {
        let mut g = self
            .remote_snapshot
            .lock()
            .map_err(|e: std::sync::PoisonError<_>| OxidriveError::store(e.to_string()))?;
        *g = None;
        Ok(())
    }

    /// Resolves the Drive parent id for `child` using the last remote snapshot and `root_folder_id`.
    pub fn parent_drive_id(
        &self,
        child: &RelativePath,
        root_folder_id: &str,
    ) -> Result<String, OxidriveError> {
        let parent_rel = parent_relative_path(child);
        if parent_rel.as_str().is_empty() {
            return Ok(root_folder_id.to_string());
        }
        if let Some(mapped) = self.get_folder_id(parent_rel.as_str()) {
            return Ok(mapped);
        }
        let g = self
            .remote_snapshot
            .lock()
            .map_err(|e: std::sync::PoisonError<_>| OxidriveError::store(e.to_string()))?;
        let map = g
            .as_ref()
            .ok_or_else(|| OxidriveError::sync("remote snapshot missing for parent resolution"))?;
        let folder = map.get(&RelativePath::from(parent_rel.as_str()));
        match folder {
            Some(f) if f.mime_type == FOLDER => Ok(f.id.clone()),
            Some(f) => Err(OxidriveError::sync(format!(
                "parent path '{}' is not a folder (mime={})",
                parent_rel.as_str(),
                f.mime_type
            ))),
            None => Err(OxidriveError::sync(format!(
                "no remote folder metadata for parent '{}'",
                parent_rel.as_str()
            ))),
        }
    }
}

fn parent_relative_path(child: &RelativePath) -> RelativePath {
    let s = child.as_str();
    if let Some((p, _)) = s.rsplit_once('/') {
        RelativePath::from(p)
    } else {
        RelativePath::from("")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use tempfile::{tempdir, NamedTempFile};

    fn sample_record(id: &str) -> SyncRecord {
        let t = Utc
            .with_ymd_and_hms(2024, 1, 2, 3, 4, 5)
            .single()
            .expect("valid timestamp");
        SyncRecord {
            drive_file_id: Some(id.to_string()),
            remote_md5: Some("abcd".to_string()),
            remote_mime_type: Some("text/plain".to_string()),
            remote_modified_at: Some(t),
            local_md5: "efgh".to_string(),
            local_mtime: t,
            local_size: 42,
            last_synced_at: t,
        }
    }

    fn sample_upload_session() -> UploadSession {
        UploadSession {
            mode: crate::types::UploadSessionMode::Update {
                drive_id: "drive-file".to_string(),
            },
            session_url: "https://upload.example/session".to_string(),
            next_offset: 1024,
            file_size: 2048,
            local_md5: "a".repeat(32),
            updated_at: Utc
                .with_ymd_and_hms(2024, 2, 3, 4, 5, 6)
                .single()
                .expect("valid timestamp"),
        }
    }

    #[test]
    fn load_and_persist_sync_metadata_round_trip() {
        let db_file = NamedTempFile::new().expect("temp db");
        let redb = RedbStore::open(db_file.path()).expect("open redb");
        let sync_root = tempdir().expect("sync dir");
        let store = Store::open(sync_root.path()).expect("open store");

        let p = RelativePath::from("docs/a.md");
        let rec = sample_record("drive-1");
        store.upsert(p.clone(), rec.clone()).expect("upsert");
        store.persist_to_redb(&redb).expect("persist");

        let loaded = Store::open(sync_root.path()).expect("open second store");
        loaded.load_from_redb(&redb).expect("load");
        assert_eq!(loaded.get(&p).expect("get"), Some(rec));
    }

    #[test]
    fn persist_removes_stale_metadata_keys() {
        let db_file = NamedTempFile::new().expect("temp db");
        let redb = RedbStore::open(db_file.path()).expect("open redb");
        let sync_root = tempdir().expect("sync dir");
        let store = Store::open(sync_root.path()).expect("open store");

        let stale = RelativePath::from("stale.txt");
        store
            .upsert(stale.clone(), sample_record("stale-id"))
            .expect("upsert stale");
        store.persist_to_redb(&redb).expect("persist stale");
        store.remove(&stale).expect("remove stale in memory");

        let fresh = RelativePath::from("fresh.txt");
        let fresh_record = sample_record("fresh-id");
        store
            .upsert(fresh.clone(), fresh_record.clone())
            .expect("upsert fresh");
        store.persist_to_redb(&redb).expect("persist fresh");

        let loaded = Store::open(sync_root.path()).expect("open loaded store");
        loaded.load_from_redb(&redb).expect("load");
        assert_eq!(loaded.get(&stale).expect("get stale"), None);
        assert_eq!(loaded.get(&fresh).expect("get fresh"), Some(fresh_record));
    }

    #[test]
    fn load_from_redb_skips_unsafe_paths() {
        let db_file = NamedTempFile::new().expect("temp db");
        let redb = RedbStore::open(db_file.path()).expect("open redb");
        let sync_root = tempdir().expect("sync dir");
        let store = Store::open(sync_root.path()).expect("open store");

        let valid = sample_record("valid-id");
        let invalid = sample_record("invalid-id");
        let valid_bytes = bincode::serialize(&valid).expect("serialize valid");
        let invalid_bytes = bincode::serialize(&invalid).expect("serialize invalid");
        redb.set_sync_metadata_sync("ok/file.txt", &valid_bytes)
            .expect("set valid");
        redb.set_sync_metadata_sync("../escape.txt", &invalid_bytes)
            .expect("set invalid");

        store.load_from_redb(&redb).expect("load from redb");
        assert_eq!(
            store
                .get(&RelativePath::from("ok/file.txt"))
                .expect("get valid"),
            Some(valid)
        );
        assert_eq!(
            store
                .get(&RelativePath::from("../escape.txt"))
                .expect("get invalid"),
            None
        );
    }

    #[test]
    fn upload_session_round_trip() {
        let db_file = NamedTempFile::new().expect("temp db");
        let redb = RedbStore::open(db_file.path()).expect("open redb");
        let sync_root = tempdir().expect("sync dir");
        let store = Store::open(sync_root.path()).expect("open store");
        let path = RelativePath::from("video.bin");
        let session = sample_upload_session();
        store
            .upsert_upload_session(path.clone(), session.clone())
            .expect("upsert upload session");
        store.persist_to_redb(&redb).expect("persist");

        let loaded = Store::open(sync_root.path()).expect("open second store");
        loaded.load_from_redb(&redb).expect("load");
        assert_eq!(
            loaded
                .get_upload_session(&path)
                .expect("get upload session"),
            Some(session)
        );
    }

    #[test]
    fn folder_ids_round_trip() {
        let db_file = NamedTempFile::new().expect("temp db");
        let redb = RedbStore::open(db_file.path()).expect("open redb");
        let sync_root = tempdir().expect("sync dir");
        let store = Store::open(sync_root.path()).expect("open store");

        store.set_folder_id("docs", "folder-1");
        store.set_folder_id(r"docs\reports", "folder-2");
        store.persist_to_redb(&redb).expect("persist");

        let loaded = Store::open(sync_root.path()).expect("open second store");
        loaded.load_from_redb(&redb).expect("load");

        assert_eq!(loaded.get_folder_id("docs"), Some("folder-1".to_string()));
        assert_eq!(
            loaded.get_folder_id("docs/reports"),
            Some("folder-2".to_string())
        );
    }

    #[test]
    fn parent_drive_id_uses_reloaded_folder_ids_without_remote_snapshot() {
        let db_file = NamedTempFile::new().expect("temp db");
        let redb = RedbStore::open(db_file.path()).expect("open redb");
        let sync_root = tempdir().expect("sync dir");
        let store = Store::open(sync_root.path()).expect("open store");

        store.set_folder_id("docs", "folder-1");
        store.set_folder_id("docs/reports", "folder-2");
        store.persist_to_redb(&redb).expect("persist");

        let loaded = Store::open(sync_root.path()).expect("open second store");
        loaded.load_from_redb(&redb).expect("load");

        let parent = loaded
            .parent_drive_id(&RelativePath::from("docs/reports/q1.txt"), "root-folder")
            .expect("resolve parent");
        assert_eq!(parent, "folder-2");
    }

    #[test]
    fn purge_stale_upload_sessions_removes_expired_entries() {
        let sync_root = tempdir().expect("sync dir");
        let store = Store::open(sync_root.path()).expect("open store");
        let stale_path = RelativePath::from("stale.bin");
        let fresh_path = RelativePath::from("fresh.bin");
        let mut stale = sample_upload_session();
        stale.updated_at = Utc::now() - chrono::Duration::hours(48);
        let mut fresh = sample_upload_session();
        fresh.updated_at = Utc::now();

        store
            .upsert_upload_session(stale_path.clone(), stale)
            .expect("insert stale");
        store
            .upsert_upload_session(fresh_path.clone(), fresh.clone())
            .expect("insert fresh");

        let removed = store
            .purge_stale_upload_sessions(chrono::Duration::hours(24))
            .expect("purge");
        assert_eq!(removed, 1);
        assert_eq!(
            store
                .get_upload_session(&stale_path)
                .expect("get stale session"),
            None
        );
        assert_eq!(
            store
                .get_upload_session(&fresh_path)
                .expect("get fresh session"),
            Some(fresh)
        );
    }

    #[test]
    fn purge_stale_upload_sessions_keeps_future_timestamp_sessions() {
        let sync_root = tempdir().expect("sync dir");
        let store = Store::open(sync_root.path()).expect("open store");
        let path = RelativePath::from("future.bin");
        let mut session = sample_upload_session();
        session.updated_at = Utc::now() + chrono::Duration::hours(2);

        store
            .upsert_upload_session(path.clone(), session.clone())
            .expect("insert future");

        let removed = store
            .purge_stale_upload_sessions(chrono::Duration::hours(24))
            .expect("purge");
        assert_eq!(removed, 0);
        assert_eq!(
            store
                .get_upload_session(&path)
                .expect("get future session"),
            Some(session)
        );
    }

    #[test]
    fn oversized_upload_session_is_not_left_stale_in_redb() {
        let db_file = NamedTempFile::new().expect("temp db");
        let redb = RedbStore::open(db_file.path()).expect("open redb");
        let sync_root = tempdir().expect("sync dir");
        let store = Store::open(sync_root.path()).expect("open store");
        let path = RelativePath::from("video.bin");

        store
            .upsert_upload_session(path.clone(), sample_upload_session())
            .expect("seed upload session");
        store.persist_to_redb(&redb).expect("persist initial");

        let mut oversized = sample_upload_session();
        oversized.session_url = "x".repeat(MAX_UPLOAD_SESSION_BLOB_BYTES + 1024);
        store
            .upsert_upload_session(path.clone(), oversized)
            .expect("replace with oversized session");
        store.persist_to_redb(&redb).expect("persist oversized");

        let loaded = Store::open(sync_root.path()).expect("open second store");
        loaded.load_from_redb(&redb).expect("load");
        assert_eq!(
            loaded
                .get_upload_session(&path)
                .expect("get upload session after oversized persist"),
            None
        );
    }
}
