//! Embedded `redb` store with string keys and opaque byte values.

use std::path::Path;
use std::sync::Arc;

use redb::{Database, ReadableTable, TableDefinition};
use thiserror::Error;
use tokio::task::spawn_blocking;

use crate::error::OxidriveError;

/// Remote file index keyed by normalized relative path (UTF-8).
#[allow(dead_code)]
pub const REMOTE_FILES: TableDefinition<&str, &[u8]> = TableDefinition::new("remote_files");

/// Last-known sync metadata keyed by normalized relative path (UTF-8).
pub const SYNC_METADATA: TableDefinition<&str, &[u8]> = TableDefinition::new("sync_metadata");

/// Small key/value configuration (page token, feature flags, etc.).
pub const CONFIG_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("config");

/// Google Workspace conversion bookkeeping keyed by local path (UTF-8).
pub const CONVERSIONS: TableDefinition<&str, &[u8]> = TableDefinition::new("conversions");

/// Resumable upload cursors keyed by local relative path (UTF-8).
pub const UPLOAD_SESSIONS: TableDefinition<&str, &[u8]> = TableDefinition::new("upload_sessions");

/// Cached Drive folder ids keyed by normalized relative folder path (UTF-8).
pub const FOLDER_IDS: TableDefinition<&str, &[u8]> = TableDefinition::new("folder_ids");

/// In-flight operation journal keyed by local relative path (UTF-8).
pub const PENDING_OPS: TableDefinition<&str, &[u8]> = TableDefinition::new("pending_ops");

/// Errors surfaced by the embedded database layer.
#[derive(Debug, Error)]
pub enum StoreError {
    /// Low-level `redb` failure.
    #[error("database operation failed: {0}")]
    Db(String),
    /// `bincode` (or similar) serialization failure.
    #[error("serialization failed: {0}")]
    Serialize(String),
}

impl From<redb::Error> for StoreError {
    fn from(value: redb::Error) -> Self {
        StoreError::Db(value.to_string())
    }
}

impl From<redb::DatabaseError> for StoreError {
    fn from(value: redb::DatabaseError) -> Self {
        StoreError::Db(value.to_string())
    }
}

impl From<redb::TransactionError> for StoreError {
    fn from(value: redb::TransactionError) -> Self {
        StoreError::Db(value.to_string())
    }
}

impl From<redb::StorageError> for StoreError {
    fn from(value: redb::StorageError) -> Self {
        StoreError::Db(value.to_string())
    }
}

impl From<redb::CommitError> for StoreError {
    fn from(value: redb::CommitError) -> Self {
        StoreError::Db(value.to_string())
    }
}

impl From<bincode::Error> for StoreError {
    fn from(value: bincode::Error) -> Self {
        StoreError::Serialize(value.to_string())
    }
}

impl From<StoreError> for OxidriveError {
    fn from(value: StoreError) -> Self {
        OxidriveError::Store(value.to_string())
    }
}

/// Snapshot of all session-backed tables to persist atomically.
pub struct SessionStateBatch {
    pub sync_metadata: Vec<(String, Vec<u8>)>,
    pub stale_sync_metadata: Vec<String>,
    pub conversions: Vec<(String, Vec<u8>)>,
    pub stale_conversions: Vec<String>,
    pub upload_sessions: Vec<(String, Vec<u8>)>,
    pub stale_upload_sessions: Vec<String>,
    pub folder_ids: Vec<(String, Vec<u8>)>,
    pub stale_folder_ids: Vec<String>,
}

fn table_err(e: redb::TableError) -> StoreError {
    StoreError::Db(e.to_string())
}

impl From<redb::TableError> for StoreError {
    fn from(value: redb::TableError) -> Self {
        table_err(value)
    }
}

fn get_optional(
    db: &Database,
    def: TableDefinition<&str, &[u8]>,
    key: &str,
) -> Result<Option<Vec<u8>>, StoreError> {
    let read = db.begin_read()?;
    let table = match read.open_table(def) {
        Ok(t) => t,
        Err(redb::TableError::TableDoesNotExist(_)) => return Ok(None),
        Err(e) => return Err(table_err(e)),
    };
    match table.get(key)? {
        Some(g) => Ok(Some(g.value().to_vec())),
        None => Ok(None),
    }
}

fn insert(
    db: &Database,
    def: TableDefinition<&str, &[u8]>,
    key: &str,
    data: &[u8],
) -> Result<(), StoreError> {
    let write = db.begin_write()?;
    {
        let mut table = write.open_table(def)?;
        table.insert(key, data)?;
    }
    write.commit()?;
    Ok(())
}

fn remove(db: &Database, def: TableDefinition<&str, &[u8]>, key: &str) -> Result<(), StoreError> {
    let write = db.begin_write()?;
    {
        let mut table = write.open_table(def)?;
        table.remove(key)?;
    }
    write.commit()?;
    Ok(())
}

fn list_all(
    db: &Database,
    def: TableDefinition<&str, &[u8]>,
) -> Result<Vec<(String, Vec<u8>)>, StoreError> {
    let read = db.begin_read()?;
    let table = match read.open_table(def) {
        Ok(t) => t,
        Err(redb::TableError::TableDoesNotExist(_)) => return Ok(Vec::new()),
        Err(e) => return Err(table_err(e)),
    };
    let mut out = Vec::new();
    for entry in table.iter()? {
        let (k, v) = entry?;
        out.push((k.value().to_string(), v.value().to_vec()));
    }
    Ok(out)
}

fn list_keys(db: &Database, def: TableDefinition<&str, &[u8]>) -> Result<Vec<String>, StoreError> {
    let read = db.begin_read()?;
    let table = match read.open_table(def) {
        Ok(t) => t,
        Err(redb::TableError::TableDoesNotExist(_)) => return Ok(Vec::new()),
        Err(e) => return Err(table_err(e)),
    };
    let mut out = Vec::new();
    for entry in table.iter()? {
        let (k, _) = entry?;
        out.push(k.value().to_string());
    }
    Ok(out)
}

fn count_all(db: &Database, def: TableDefinition<&str, &[u8]>) -> Result<usize, StoreError> {
    let read = db.begin_read()?;
    let table = match read.open_table(def) {
        Ok(t) => t,
        Err(redb::TableError::TableDoesNotExist(_)) => return Ok(0),
        Err(e) => return Err(table_err(e)),
    };
    let mut count = 0usize;
    for entry in table.iter()? {
        let _ = entry?;
        count = count.saturating_add(1);
    }
    Ok(count)
}

/// Embedded `redb` database handle (thread-safe, async-friendly via blocking tasks).
#[derive(Clone)]
pub struct RedbStore {
    db: Arc<Database>,
}

impl RedbStore {
    /// Opens (or creates) a database at `path`.
    pub fn open(path: &Path) -> Result<Self, OxidriveError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let db = Database::create(path).map_err(|e| StoreError::Db(e.to_string()))?;
        Ok(Self { db: Arc::new(db) })
    }

    /// Reads a remote file record, if present.
    #[allow(dead_code)]
    pub async fn get_remote_file(&self, path: &str) -> Result<Option<Vec<u8>>, OxidriveError> {
        let db = Arc::clone(&self.db);
        let path = path.to_string();
        spawn_blocking(move || get_optional(&db, REMOTE_FILES, &path))
            .await
            .map_err(|e| StoreError::Db(format!("blocking task join: {e}")))?
            .map_err(OxidriveError::from)
    }

    /// Upserts a remote file record.
    #[allow(dead_code)]
    pub async fn set_remote_file(&self, path: &str, data: &[u8]) -> Result<(), OxidriveError> {
        let db = Arc::clone(&self.db);
        let path = path.to_string();
        let data = data.to_vec();
        spawn_blocking(move || insert(&db, REMOTE_FILES, &path, &data))
            .await
            .map_err(|e| StoreError::Db(format!("blocking task join: {e}")))?
            .map_err(OxidriveError::from)
    }

    /// Deletes a remote file record.
    #[allow(dead_code)]
    pub async fn delete_remote_file(&self, path: &str) -> Result<(), OxidriveError> {
        let db = Arc::clone(&self.db);
        let path = path.to_string();
        spawn_blocking(move || remove(&db, REMOTE_FILES, &path))
            .await
            .map_err(|e| StoreError::Db(format!("blocking task join: {e}")))?
            .map_err(OxidriveError::from)
    }

    /// Lists all remote file records.
    #[allow(dead_code)]
    pub async fn list_remote_files(&self) -> Result<Vec<(String, Vec<u8>)>, OxidriveError> {
        let db = Arc::clone(&self.db);
        spawn_blocking(move || list_all(&db, REMOTE_FILES))
            .await
            .map_err(|e| StoreError::Db(format!("blocking task join: {e}")))?
            .map_err(OxidriveError::from)
    }

    /// Reads sync metadata for `path`, if present.
    #[allow(dead_code)]
    pub async fn get_sync_metadata(&self, path: &str) -> Result<Option<Vec<u8>>, OxidriveError> {
        let db = Arc::clone(&self.db);
        let path = path.to_string();
        spawn_blocking(move || get_optional(&db, SYNC_METADATA, &path))
            .await
            .map_err(|e| StoreError::Db(format!("blocking task join: {e}")))?
            .map_err(OxidriveError::from)
    }

    /// Upserts sync metadata for `path`.
    #[allow(dead_code)]
    pub async fn set_sync_metadata(&self, path: &str, data: &[u8]) -> Result<(), OxidriveError> {
        let db = Arc::clone(&self.db);
        let path = path.to_string();
        let data = data.to_vec();
        spawn_blocking(move || insert(&db, SYNC_METADATA, &path, &data))
            .await
            .map_err(|e| StoreError::Db(format!("blocking task join: {e}")))?
            .map_err(OxidriveError::from)
    }

    /// Deletes sync metadata for `path`.
    #[allow(dead_code)]
    pub async fn delete_sync_metadata(&self, path: &str) -> Result<(), OxidriveError> {
        let db = Arc::clone(&self.db);
        let path = path.to_string();
        spawn_blocking(move || remove(&db, SYNC_METADATA, &path))
            .await
            .map_err(|e| StoreError::Db(format!("blocking task join: {e}")))?
            .map_err(OxidriveError::from)
    }

    /// Lists all sync metadata rows.
    #[allow(dead_code)]
    pub async fn list_sync_metadata(&self) -> Result<Vec<(String, Vec<u8>)>, OxidriveError> {
        let db = Arc::clone(&self.db);
        spawn_blocking(move || list_all(&db, SYNC_METADATA))
            .await
            .map_err(|e| StoreError::Db(format!("blocking task join: {e}")))?
            .map_err(OxidriveError::from)
    }

    /// Synchronously upserts sync metadata for `path`.
    #[allow(dead_code)]
    pub fn set_sync_metadata_sync(&self, path: &str, data: &[u8]) -> Result<(), OxidriveError> {
        insert(&self.db, SYNC_METADATA, path, data).map_err(OxidriveError::from)
    }

    /// Synchronously deletes sync metadata for `path`.
    #[allow(dead_code)]
    pub fn delete_sync_metadata_sync(&self, path: &str) -> Result<(), OxidriveError> {
        remove(&self.db, SYNC_METADATA, path).map_err(OxidriveError::from)
    }

    /// Synchronously lists all sync metadata rows.
    pub fn list_sync_metadata_sync(&self) -> Result<Vec<(String, Vec<u8>)>, OxidriveError> {
        list_all(&self.db, SYNC_METADATA).map_err(OxidriveError::from)
    }

    /// Synchronously lists sync metadata keys only.
    pub fn list_sync_metadata_keys_sync(&self) -> Result<Vec<String>, OxidriveError> {
        list_keys(&self.db, SYNC_METADATA).map_err(OxidriveError::from)
    }

    /// Synchronously counts sync metadata rows.
    pub fn count_sync_metadata_sync(&self) -> Result<usize, OxidriveError> {
        count_all(&self.db, SYNC_METADATA).map_err(OxidriveError::from)
    }

    /// Reads a config value by key, if present.
    pub async fn get_config(&self, key: &str) -> Result<Option<Vec<u8>>, OxidriveError> {
        let db = Arc::clone(&self.db);
        let key = key.to_string();
        spawn_blocking(move || get_optional(&db, CONFIG_TABLE, &key))
            .await
            .map_err(|e| StoreError::Db(format!("blocking task join: {e}")))?
            .map_err(OxidriveError::from)
    }

    /// Upserts a config value for `key`.
    pub async fn set_config(&self, key: &str, data: &[u8]) -> Result<(), OxidriveError> {
        let db = Arc::clone(&self.db);
        let key = key.to_string();
        let data = data.to_vec();
        spawn_blocking(move || insert(&db, CONFIG_TABLE, &key, &data))
            .await
            .map_err(|e| StoreError::Db(format!("blocking task join: {e}")))?
            .map_err(OxidriveError::from)
    }

    /// Deletes a config key.
    #[allow(dead_code)]
    pub async fn delete_config(&self, key: &str) -> Result<(), OxidriveError> {
        let db = Arc::clone(&self.db);
        let key = key.to_string();
        spawn_blocking(move || remove(&db, CONFIG_TABLE, &key))
            .await
            .map_err(|e| StoreError::Db(format!("blocking task join: {e}")))?
            .map_err(OxidriveError::from)
    }

    /// Lists all config rows.
    #[allow(dead_code)]
    pub async fn list_config(&self) -> Result<Vec<(String, Vec<u8>)>, OxidriveError> {
        let db = Arc::clone(&self.db);
        spawn_blocking(move || list_all(&db, CONFIG_TABLE))
            .await
            .map_err(|e| StoreError::Db(format!("blocking task join: {e}")))?
            .map_err(OxidriveError::from)
    }

    /// Reads a conversion record for `path`, if present.
    #[allow(dead_code)]
    pub async fn get_conversion(&self, path: &str) -> Result<Option<Vec<u8>>, OxidriveError> {
        let db = Arc::clone(&self.db);
        let path = path.to_string();
        spawn_blocking(move || get_optional(&db, CONVERSIONS, &path))
            .await
            .map_err(|e| StoreError::Db(format!("blocking task join: {e}")))?
            .map_err(OxidriveError::from)
    }

    /// Upserts a conversion record for `path`.
    #[allow(dead_code)]
    pub async fn set_conversion(&self, path: &str, data: &[u8]) -> Result<(), OxidriveError> {
        let db = Arc::clone(&self.db);
        let path = path.to_string();
        let data = data.to_vec();
        spawn_blocking(move || insert(&db, CONVERSIONS, &path, &data))
            .await
            .map_err(|e| StoreError::Db(format!("blocking task join: {e}")))?
            .map_err(OxidriveError::from)
    }

    /// Deletes a conversion record for `path`.
    #[allow(dead_code)]
    pub async fn delete_conversion(&self, path: &str) -> Result<(), OxidriveError> {
        let db = Arc::clone(&self.db);
        let path = path.to_string();
        spawn_blocking(move || remove(&db, CONVERSIONS, &path))
            .await
            .map_err(|e| StoreError::Db(format!("blocking task join: {e}")))?
            .map_err(OxidriveError::from)
    }

    /// Lists all conversion records.
    #[allow(dead_code)]
    pub async fn list_conversions(&self) -> Result<Vec<(String, Vec<u8>)>, OxidriveError> {
        let db = Arc::clone(&self.db);
        spawn_blocking(move || list_all(&db, CONVERSIONS))
            .await
            .map_err(|e| StoreError::Db(format!("blocking task join: {e}")))?
            .map_err(OxidriveError::from)
    }

    /// Synchronously reads a conversion record for `path`, if present.
    #[allow(dead_code)]
    pub fn get_conversion_sync(&self, path: &str) -> Result<Option<Vec<u8>>, OxidriveError> {
        get_optional(&self.db, CONVERSIONS, path).map_err(OxidriveError::from)
    }

    /// Synchronously upserts a conversion record for `path`.
    #[allow(dead_code)]
    pub fn set_conversion_sync(&self, path: &str, data: &[u8]) -> Result<(), OxidriveError> {
        insert(&self.db, CONVERSIONS, path, data).map_err(OxidriveError::from)
    }

    /// Synchronously deletes a conversion record for `path`.
    #[allow(dead_code)]
    pub fn delete_conversion_sync(&self, path: &str) -> Result<(), OxidriveError> {
        remove(&self.db, CONVERSIONS, path).map_err(OxidriveError::from)
    }

    /// Synchronously lists all conversion records.
    pub fn list_conversions_sync(&self) -> Result<Vec<(String, Vec<u8>)>, OxidriveError> {
        list_all(&self.db, CONVERSIONS).map_err(OxidriveError::from)
    }

    /// Synchronously lists conversion keys only.
    pub fn list_conversions_keys_sync(&self) -> Result<Vec<String>, OxidriveError> {
        list_keys(&self.db, CONVERSIONS).map_err(OxidriveError::from)
    }

    /// Synchronously counts conversion rows.
    pub fn count_conversions_sync(&self) -> Result<usize, OxidriveError> {
        count_all(&self.db, CONVERSIONS).map_err(OxidriveError::from)
    }

    /// Synchronously upserts a cached folder id mapping for `path`.
    #[allow(dead_code)]
    pub fn set_folder_id_sync(&self, path: &str, data: &[u8]) -> Result<(), OxidriveError> {
        insert(&self.db, FOLDER_IDS, path, data).map_err(OxidriveError::from)
    }

    /// Synchronously deletes a cached folder id mapping for `path`.
    #[allow(dead_code)]
    pub fn delete_folder_id_sync(&self, path: &str) -> Result<(), OxidriveError> {
        remove(&self.db, FOLDER_IDS, path).map_err(OxidriveError::from)
    }

    /// Synchronously lists all cached folder id mappings.
    pub fn list_folder_ids_sync(&self) -> Result<Vec<(String, Vec<u8>)>, OxidriveError> {
        list_all(&self.db, FOLDER_IDS).map_err(OxidriveError::from)
    }

    /// Synchronously lists cached folder id keys only.
    pub fn list_folder_ids_keys_sync(&self) -> Result<Vec<String>, OxidriveError> {
        list_keys(&self.db, FOLDER_IDS).map_err(OxidriveError::from)
    }

    /// Synchronously upserts an in-flight pending operation for `path`.
    pub fn set_pending_op_sync(&self, path: &str, data: &[u8]) -> Result<(), OxidriveError> {
        insert(&self.db, PENDING_OPS, path, data).map_err(OxidriveError::from)
    }

    /// Synchronously deletes an in-flight pending operation for `path`.
    pub fn delete_pending_op_sync(&self, path: &str) -> Result<(), OxidriveError> {
        remove(&self.db, PENDING_OPS, path).map_err(OxidriveError::from)
    }

    /// Synchronously lists all in-flight pending operations.
    pub fn list_pending_ops_sync(&self) -> Result<Vec<(String, Vec<u8>)>, OxidriveError> {
        list_all(&self.db, PENDING_OPS).map_err(OxidriveError::from)
    }

    /// Synchronously upserts a resumable upload session for `path`.
    #[allow(dead_code)]
    pub fn set_upload_session_sync(&self, path: &str, data: &[u8]) -> Result<(), OxidriveError> {
        insert(&self.db, UPLOAD_SESSIONS, path, data).map_err(OxidriveError::from)
    }

    /// Synchronously deletes a resumable upload session for `path`.
    #[allow(dead_code)]
    pub fn delete_upload_session_sync(&self, path: &str) -> Result<(), OxidriveError> {
        remove(&self.db, UPLOAD_SESSIONS, path).map_err(OxidriveError::from)
    }

    /// Synchronously lists all resumable upload sessions.
    pub fn list_upload_sessions_sync(&self) -> Result<Vec<(String, Vec<u8>)>, OxidriveError> {
        list_all(&self.db, UPLOAD_SESSIONS).map_err(OxidriveError::from)
    }

    /// Synchronously lists resumable upload session keys only.
    pub fn list_upload_sessions_keys_sync(&self) -> Result<Vec<String>, OxidriveError> {
        list_keys(&self.db, UPLOAD_SESSIONS).map_err(OxidriveError::from)
    }

    /// Synchronously scans upload sessions with bounded row and payload limits.
    pub fn scan_upload_sessions_sync(
        &self,
        max_rows: usize,
        max_value_bytes: usize,
    ) -> Result<Vec<(String, Vec<u8>)>, OxidriveError> {
        let read = self
            .db
            .begin_read()
            .map_err(|e| OxidriveError::Store(e.to_string()))?;
        let table = match read.open_table(UPLOAD_SESSIONS) {
            Ok(t) => t,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(Vec::new()),
            Err(e) => return Err(OxidriveError::Store(e.to_string())),
        };
        let mut out = Vec::new();
        let max_rows = max_rows.max(1);
        let max_scanned_rows = max_rows.saturating_mul(10);
        let mut scanned = 0usize;
        for entry in table
            .iter()
            .map_err(|e| OxidriveError::Store(e.to_string()))?
        {
            scanned = scanned.saturating_add(1);
            if out.len() >= max_rows || scanned > max_scanned_rows {
                break;
            }
            let (k, v) = entry.map_err(|e| OxidriveError::Store(e.to_string()))?;
            let value = v.value();
            if value.len() > max_value_bytes {
                continue;
            }
            out.push((k.value().to_string(), value.to_vec()));
        }
        Ok(out)
    }

    /// Synchronously scans pending operations with bounded row and payload limits.
    pub fn scan_pending_ops_sync(
        &self,
        max_rows: usize,
        max_value_bytes: usize,
    ) -> Result<Vec<(String, Vec<u8>)>, OxidriveError> {
        let read = self
            .db
            .begin_read()
            .map_err(|e| OxidriveError::Store(e.to_string()))?;
        let table = match read.open_table(PENDING_OPS) {
            Ok(t) => t,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(Vec::new()),
            Err(e) => return Err(OxidriveError::Store(e.to_string())),
        };
        let mut out = Vec::new();
        let max_rows = max_rows.max(1);
        let max_scanned_rows = max_rows.saturating_mul(10);
        let mut scanned = 0usize;
        for entry in table
            .iter()
            .map_err(|e| OxidriveError::Store(e.to_string()))?
        {
            scanned = scanned.saturating_add(1);
            if out.len() >= max_rows || scanned > max_scanned_rows {
                break;
            }
            let (k, v) = entry.map_err(|e| OxidriveError::Store(e.to_string()))?;
            let value = v.value();
            if value.len() > max_value_bytes {
                continue;
            }
            out.push((k.value().to_string(), value.to_vec()));
        }
        Ok(out)
    }

    /// Reads the stored Drive changes `pageToken`, if any (`config["page_token"]`).
    pub async fn get_page_token(&self) -> Result<Option<String>, OxidriveError> {
        match self.get_config("page_token").await? {
            Some(bytes) => {
                let s = String::from_utf8(bytes).map_err(|e| {
                    StoreError::Serialize(format!("page_token is not valid UTF-8: {e}"))
                })?;
                Ok(Some(s))
            }
            None => Ok(None),
        }
    }

    /// Stores the Drive changes `pageToken` (`config["page_token"]`).
    #[allow(dead_code)]
    pub async fn set_page_token(&self, token: &str) -> Result<(), OxidriveError> {
        self.set_config("page_token", token.as_bytes()).await
    }

    /// Atomically replaces all session-backed tables in one `redb` write transaction.
    #[allow(dead_code)]
    pub fn replace_session_state_sync(
        &self,
        batch: SessionStateBatch,
    ) -> Result<(), OxidriveError> {
        self.replace_session_state_and_page_token_and_pending_cleanup_sync(batch, None, &[])
    }

    /// Atomically replaces all session-backed tables and removes selected pending-operation rows.
    pub fn replace_session_state_and_pending_cleanup_sync(
        &self,
        batch: SessionStateBatch,
        pending_cleanup_keys: &[String],
    ) -> Result<(), OxidriveError> {
        self.replace_session_state_and_page_token_and_pending_cleanup_sync(
            batch,
            None,
            pending_cleanup_keys,
        )
    }

    /// Atomically replaces all session-backed tables and optionally updates the page token.
    #[allow(dead_code)]
    pub fn replace_session_state_and_page_token_sync(
        &self,
        batch: SessionStateBatch,
        page_token: Option<&str>,
    ) -> Result<(), OxidriveError> {
        self.replace_session_state_and_page_token_and_pending_cleanup_sync(batch, page_token, &[])
    }

    /// Atomically replaces session-backed tables, optional page token, and pending-op cleanup.
    pub fn replace_session_state_and_page_token_and_pending_cleanup_sync(
        &self,
        batch: SessionStateBatch,
        page_token: Option<&str>,
        pending_cleanup_keys: &[String],
    ) -> Result<(), OxidriveError> {
        let write = self
            .db
            .begin_write()
            .map_err(StoreError::from)
            .map_err(OxidriveError::from)?;
        {
            let mut sync_table = write
                .open_table(SYNC_METADATA)
                .map_err(StoreError::from)
                .map_err(OxidriveError::from)?;
            for (key, value) in &batch.sync_metadata {
                sync_table
                    .insert(key.as_str(), value.as_slice())
                    .map_err(StoreError::from)
                    .map_err(OxidriveError::from)?;
            }
            for key in &batch.stale_sync_metadata {
                sync_table
                    .remove(key.as_str())
                    .map_err(StoreError::from)
                    .map_err(OxidriveError::from)?;
            }
        }
        {
            let mut conversion_table = write
                .open_table(CONVERSIONS)
                .map_err(StoreError::from)
                .map_err(OxidriveError::from)?;
            for (key, value) in &batch.conversions {
                conversion_table
                    .insert(key.as_str(), value.as_slice())
                    .map_err(StoreError::from)
                    .map_err(OxidriveError::from)?;
            }
            for key in &batch.stale_conversions {
                conversion_table
                    .remove(key.as_str())
                    .map_err(StoreError::from)
                    .map_err(OxidriveError::from)?;
            }
        }
        {
            let mut upload_table = write
                .open_table(UPLOAD_SESSIONS)
                .map_err(StoreError::from)
                .map_err(OxidriveError::from)?;
            for (key, value) in &batch.upload_sessions {
                upload_table
                    .insert(key.as_str(), value.as_slice())
                    .map_err(StoreError::from)
                    .map_err(OxidriveError::from)?;
            }
            for key in &batch.stale_upload_sessions {
                upload_table
                    .remove(key.as_str())
                    .map_err(StoreError::from)
                    .map_err(OxidriveError::from)?;
            }
        }
        {
            let mut folder_table = write
                .open_table(FOLDER_IDS)
                .map_err(StoreError::from)
                .map_err(OxidriveError::from)?;
            for (key, value) in &batch.folder_ids {
                folder_table
                    .insert(key.as_str(), value.as_slice())
                    .map_err(StoreError::from)
                    .map_err(OxidriveError::from)?;
            }
            for key in &batch.stale_folder_ids {
                folder_table
                    .remove(key.as_str())
                    .map_err(StoreError::from)
                    .map_err(OxidriveError::from)?;
            }
        }
        if let Some(page_token) = page_token {
            let mut config_table = write
                .open_table(CONFIG_TABLE)
                .map_err(StoreError::from)
                .map_err(OxidriveError::from)?;
            config_table
                .insert("page_token", page_token.as_bytes())
                .map_err(StoreError::from)
                .map_err(OxidriveError::from)?;
        }
        if !pending_cleanup_keys.is_empty() {
            let mut pending_table = write
                .open_table(PENDING_OPS)
                .map_err(StoreError::from)
                .map_err(OxidriveError::from)?;
            for key in pending_cleanup_keys {
                pending_table
                    .remove(key.as_str())
                    .map_err(StoreError::from)
                    .map_err(OxidriveError::from)?;
            }
        }
        write
            .commit()
            .map_err(StoreError::from)
            .map_err(OxidriveError::from)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    #[tokio::test]
    async fn crud_remote_files_round_trip() {
        let file = NamedTempFile::new().expect("tempfile");
        let store = RedbStore::open(file.path()).expect("open");
        assert!(store.get_remote_file("a").await.expect("get").is_none());
        store.set_remote_file("a", b"one").await.expect("set");
        assert_eq!(
            store.get_remote_file("a").await.expect("get"),
            Some(b"one".to_vec())
        );
        let list = store.list_remote_files().await.expect("list");
        assert_eq!(list.len(), 1);
        store.delete_remote_file("a").await.expect("del");
        assert!(store.get_remote_file("a").await.expect("get").is_none());
    }

    #[tokio::test]
    async fn page_token_config_helpers() {
        let file = NamedTempFile::new().expect("tempfile");
        let store = RedbStore::open(file.path()).expect("open");
        assert!(store.get_page_token().await.expect("token").is_none());
        store.set_page_token("tok").await.expect("set token");
        assert_eq!(
            store.get_page_token().await.expect("token").as_deref(),
            Some("tok")
        );
    }

    #[test]
    fn pending_ops_crud_round_trip() {
        let file = NamedTempFile::new().expect("tempfile");
        let store = RedbStore::open(file.path()).expect("open");
        store
            .set_pending_op_sync("a.txt", b"payload")
            .expect("set pending");
        let rows = store.list_pending_ops_sync().expect("list pending");
        assert_eq!(rows, vec![("a.txt".to_string(), b"payload".to_vec())]);
        store
            .delete_pending_op_sync("a.txt")
            .expect("delete pending");
        let rows = store
            .list_pending_ops_sync()
            .expect("list pending after delete");
        assert!(rows.is_empty());
    }

    #[test]
    fn replace_session_state_can_cleanup_pending_ops_atomically() {
        let file = NamedTempFile::new().expect("tempfile");
        let store = RedbStore::open(file.path()).expect("open");
        store.set_pending_op_sync("a.txt", b"a").expect("seed a");
        store.set_pending_op_sync("b.txt", b"b").expect("seed b");
        store
            .replace_session_state_and_pending_cleanup_sync(
                SessionStateBatch {
                    sync_metadata: Vec::new(),
                    stale_sync_metadata: Vec::new(),
                    conversions: Vec::new(),
                    stale_conversions: Vec::new(),
                    upload_sessions: Vec::new(),
                    stale_upload_sessions: Vec::new(),
                    folder_ids: Vec::new(),
                    stale_folder_ids: Vec::new(),
                },
                &[String::from("a.txt")],
            )
            .expect("replace");
        let rows = store.list_pending_ops_sync().expect("list pending");
        assert_eq!(rows, vec![("b.txt".to_string(), b"b".to_vec())]);
    }

    #[test]
    fn scan_pending_ops_sync_applies_row_limit() {
        let file = NamedTempFile::new().expect("tempfile");
        let store = RedbStore::open(file.path()).expect("open");
        store.set_pending_op_sync("a.txt", b"a").expect("seed a");
        store.set_pending_op_sync("b.txt", b"b").expect("seed b");
        let rows = store.scan_pending_ops_sync(1, 1024).expect("scan");
        assert_eq!(rows.len(), 1);
    }

    #[test]
    fn bincode_error_maps_to_store_error() {
        let v = u8::MAX;
        let _ = bincode::serialize(&v).expect("serialize u8 works");
        let err = bincode::deserialize::<u16>(&[0u8]).expect_err("should fail");
        let se: StoreError = err.into();
        assert!(matches!(se, StoreError::Serialize(_)));
    }
}
