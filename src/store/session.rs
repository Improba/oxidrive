//! In-memory sync session: metadata map, remote listing snapshot, and root folder id for one run.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use crate::drive::types::DriveFile;
use crate::drive::types::FOLDER;
use crate::error::OxidriveError;
use crate::store::RedbStore;
use crate::types::{RelativePath, SyncRecord, WorkspaceConversion};

/// Per-run state used by [`crate::sync::engine`] and [`crate::sync::executor`].
///
/// Durable history lives in [`super::RedbStore`]; this handle is cheap to clone across tasks.
#[derive(Clone)]
pub struct Store {
    sync_dir: PathBuf,
    records: Arc<Mutex<HashMap<RelativePath, SyncRecord>>>,
    conversions: Arc<Mutex<HashMap<RelativePath, WorkspaceConversion>>>,
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
            remote_snapshot: Arc::new(Mutex::new(None)),
            root_drive_folder_id: Arc::new(Mutex::new(None)),
            folder_ids: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// Directory mirrored with Google Drive.
    pub fn sync_dir(&self) -> &PathBuf {
        &self.sync_dir
    }

    fn lock_records(&self) -> Result<MutexGuard<'_, HashMap<RelativePath, SyncRecord>>, OxidriveError> {
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

    /// Loads all persisted sync metadata from `redb` into this session.
    pub fn load_from_redb(&self, redb: &RedbStore) -> Result<(), OxidriveError> {
        let rows = redb.list_sync_metadata_sync()?;
        let mut records = HashMap::with_capacity(rows.len());
        for (path, data) in rows {
            let record: SyncRecord = bincode::deserialize(&data)
                .map_err(|e| OxidriveError::store(format!("decode SyncRecord for '{path}': {e}")))?;
            records.insert(RelativePath::from(path), record);
        }
        let mut guard = self.lock_records()?;
        *guard = records;
        let conversion_rows = redb.list_conversions_sync()?;
        let mut conversions = HashMap::with_capacity(conversion_rows.len());
        for (path, data) in conversion_rows {
            let conversion: WorkspaceConversion = bincode::deserialize(&data)
                .map_err(|e| OxidriveError::store(format!("decode conversion for '{path}': {e}")))?;
            conversions.insert(RelativePath::from(path), conversion);
        }
        let mut conversion_guard = self
            .conversions
            .lock()
            .map_err(|e: std::sync::PoisonError<_>| OxidriveError::store(e.to_string()))?;
        *conversion_guard = conversions;
        tracing::info!(rows = guard.len(), "loaded sync metadata from redb");
        Ok(())
    }

    /// Persists all in-memory sync metadata into `redb`, removing stale keys.
    pub fn persist_to_redb(&self, redb: &RedbStore) -> Result<(), OxidriveError> {
        let snapshot: Vec<(RelativePath, SyncRecord)> = self.iter_records()?;
        let existing_rows = redb.list_sync_metadata_sync()?;
        let existing_keys: HashSet<String> =
            existing_rows.into_iter().map(|(k, _)| k).collect();
        let mut desired_keys = HashSet::with_capacity(snapshot.len());

        for (path, record) in snapshot {
            let key = path.as_str().to_string();
            let bytes = bincode::serialize(&record)
                .map_err(|e| OxidriveError::store(format!("encode SyncRecord for '{key}': {e}")))?;
            redb.set_sync_metadata_sync(&key, &bytes)?;
            desired_keys.insert(key);
        }

        let mut removed = 0usize;
        for stale in existing_keys.difference(&desired_keys) {
            redb.delete_sync_metadata_sync(stale)?;
            removed += 1;
        }

        let conversion_snapshot = self
            .conversions
            .lock()
            .map_err(|e: std::sync::PoisonError<_>| OxidriveError::store(e.to_string()))?
            .clone();
        let existing_conversions = redb.list_conversions_sync()?;
        let existing_conversion_keys: HashSet<String> =
            existing_conversions.into_iter().map(|(k, _)| k).collect();
        let mut desired_conversion_keys = HashSet::with_capacity(conversion_snapshot.len());
        for (path, conversion) in conversion_snapshot {
            let key = path.as_str().to_string();
            let bytes = bincode::serialize(&conversion)
                .map_err(|e| OxidriveError::store(format!("encode conversion for '{key}': {e}")))?;
            redb.set_conversion_sync(&key, &bytes)?;
            desired_conversion_keys.insert(key);
        }
        for stale in existing_conversion_keys.difference(&desired_conversion_keys) {
            redb.delete_conversion_sync(stale)?;
        }

        tracing::info!(
            rows_written = desired_keys.len(),
            stale_rows_removed = removed,
            "persisted sync metadata to redb"
        );
        Ok(())
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
    pub fn all_folder_ids(&self) -> HashMap<String, String> {
        match self.folder_ids.lock() {
            Ok(g) => g.clone(),
            Err(e) => {
                tracing::error!(error = %e, "failed to lock folder id map snapshot");
                HashMap::new()
            }
        }
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
    use tempfile::{NamedTempFile, tempdir};

    fn sample_record(id: &str) -> SyncRecord {
        let t = Utc
            .with_ymd_and_hms(2024, 1, 2, 3, 4, 5)
            .single()
            .expect("valid timestamp");
        SyncRecord {
            drive_file_id: Some(id.to_string()),
            remote_md5: Some("abcd".to_string()),
            remote_modified_at: Some(t),
            local_md5: "efgh".to_string(),
            local_mtime: t,
            local_size: 42,
            last_synced_at: t,
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
}
