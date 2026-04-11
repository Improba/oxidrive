//! Core value types shared across sync, storage, and reporting.
//!
//! Paths are normalized to POSIX-style separators for stable serialization and map keys.

use std::fmt;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Max age for persisted resumable upload cursors before they are considered stale.
pub const RESUMABLE_UPLOAD_SESSION_TTL_HOURS: i64 = 24;
/// Upper bound for serialized upload-session payloads accepted from disk.
pub const MAX_UPLOAD_SESSION_BLOB_BYTES: usize = 16 * 1024;

/// Normalizes a relative path to use `/` as the separator.
fn normalize_relative(s: &str) -> String {
    s.replace('\\', "/")
}

fn is_windows_drive_path(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 2 && b[1] == b':' && b[0].is_ascii_alphabetic()
}

fn is_safe_normalized_relative(s: &str) -> bool {
    if s.is_empty() {
        return true;
    }
    if s.contains('\0') || s.starts_with('/') || s.ends_with('/') || is_windows_drive_path(s) {
        return false;
    }
    for segment in s.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            return false;
        }
    }
    true
}

mod duration_serde {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::time::Duration;

    pub fn serialize<S>(d: &Duration, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let micros = d.as_micros();
        micros.serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Duration, D::Error>
    where
        D: Deserializer<'de>,
    {
        let micros = u128::deserialize(deserializer)?;
        u64::try_from(micros)
            .map(Duration::from_micros)
            .map_err(serde::de::Error::custom)
    }
}

/// Path relative to the sync root, using `/` as the separator.
///
/// Construct via [`From`] implementations or deserialization; backslashes are normalized to `/`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RelativePath(
    /// Normalized path string (forward slashes only).
    pub String,
);

impl Serialize for RelativePath {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for RelativePath {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Ok(Self(normalize_relative(&s)))
    }
}

impl RelativePath {
    /// Returns the normalized path string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    /// Returns true when the path is safe to join under a sync root.
    #[must_use]
    pub fn is_safe(&self) -> bool {
        is_safe_normalized_relative(self.as_str())
    }

    /// Returns true when [`RelativePath::is_safe`] and non-empty.
    #[must_use]
    pub fn is_safe_non_empty(&self) -> bool {
        !self.as_str().is_empty() && self.is_safe()
    }
}

impl fmt::Display for RelativePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for RelativePath {
    fn from(value: String) -> Self {
        Self(normalize_relative(&value))
    }
}

impl From<&str> for RelativePath {
    fn from(value: &str) -> Self {
        Self(normalize_relative(value))
    }
}

impl AsRef<str> for RelativePath {
    fn as_ref(&self) -> &str {
        self.0.as_ref()
    }
}

/// Metadata for a file on disk under the sync root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalFile {
    /// Relative path from sync root.
    pub path: RelativePath,
    /// MD5 checksum of file contents (hex).
    pub md5: String,
    /// Last modification time (UTC).
    pub mtime: DateTime<Utc>,
    /// Size in bytes.
    pub size: u64,
}

/// Per-file sync state persisted between runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncRecord {
    /// Google Drive file id when the remote object is known.
    #[serde(default)]
    pub drive_file_id: Option<String>,
    /// Remote fingerprint: MD5 from Drive when present, or a synthetic `mtime:` value for native Google files.
    pub remote_md5: Option<String>,
    /// Remote MIME type from Drive at last successful sync.
    #[serde(default)]
    pub remote_mime_type: Option<String>,
    /// Remote `modifiedTime` from Drive at last successful sync (when MD5 is unavailable).
    #[serde(default)]
    pub remote_modified_at: Option<DateTime<Utc>>,
    /// Local content MD5 at last successful sync.
    pub local_md5: String,
    /// Local mtime at last successful sync.
    pub local_mtime: DateTime<Utc>,
    /// Local size at last successful sync.
    pub local_size: u64,
    /// When this record was last reconciled with remote.
    pub last_synced_at: DateTime<Utc>,
}

/// Mapping from a converted local path back to its Google Workspace source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceConversion {
    /// Google Drive file id of the source Workspace document.
    pub drive_file_id: String,
    /// Google Workspace MIME type (`application/vnd.google-apps.*`).
    pub google_mime: String,
    /// MD5 of the last exported bytes written to local disk.
    #[serde(default)]
    pub last_export_md5: Option<String>,
}

/// Upload intent metadata for resumable sessions persisted across sync runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum UploadSessionMode {
    /// Creating a new remote file under `parent_id` with `name`.
    Create { parent_id: String, name: String },
    /// Updating existing binary media for Drive file `drive_id`.
    Update { drive_id: String },
    /// Updating a converted Workspace document (`drive_id`, target Google MIME).
    Convert {
        drive_id: String,
        google_mime: String,
    },
}

/// Persisted resumable upload cursor so large uploads can resume after restart.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UploadSession {
    /// Mode and target that this resumable session belongs to.
    pub mode: UploadSessionMode,
    /// Drive resumable session URL returned via `Location` header.
    pub session_url: String,
    /// Next byte offset to upload.
    pub next_offset: u64,
    /// Total local file size expected by the session.
    pub file_size: u64,
    /// Local MD5 used to invalidate sessions when content changes.
    pub local_md5: String,
    /// Last update time for TTL-based cleanup.
    pub updated_at: DateTime<Utc>,
}

/// Operation kinds that are journaled while side effects are in progress.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PendingOpKind {
    /// Local content is being uploaded to Drive.
    Upload,
    /// Remote content is being downloaded to disk.
    Download,
    /// Local file deletion is being applied.
    DeleteLocal,
    /// Remote file trashing is being applied.
    DeleteRemote,
}

/// Progress marker for a journaled pending operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PendingOpStage {
    /// Operation intent recorded before side effects.
    Planned,
    /// Side effect is about to run (or may have started) and must be reconciled on restart.
    SideEffectStarted,
    /// External side effect completed; metadata reconciliation pending.
    SideEffectDone,
    /// In-memory metadata update completed; waiting for durable flush to redb.
    MetadataCommitted,
}

/// Persisted record for an in-flight sync operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingOp {
    /// Type of operation being tracked.
    pub kind: PendingOpKind,
    /// Current step reached by the operation.
    pub stage: PendingOpStage,
    /// Last update instant for diagnostics and recovery.
    pub updated_at: DateTime<Utc>,
}

/// Summary of a completed or partial sync run (aggregated paths and timing).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncReport {
    /// Paths uploaded to Drive.
    pub uploaded: Vec<RelativePath>,
    /// Paths downloaded from Drive.
    pub downloaded: Vec<RelativePath>,
    /// Local paths removed.
    pub deleted_local: Vec<RelativePath>,
    /// Remote files removed.
    pub deleted_remote: Vec<RelativePath>,
    /// Paths needing user resolution.
    pub conflicts: Vec<RelativePath>,
    /// Count of intentionally skipped items.
    pub skipped: usize,
    /// Paths that failed with a message.
    pub errors: Vec<(RelativePath, String)>,
    /// Wall-clock duration of the run.
    #[serde(with = "duration_serde")]
    pub duration: Duration,
}

/// Planned action for a single path during sync (path plus remote ids where relevant).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SyncAction {
    /// No change required.
    Skip {
        /// Target path.
        path: RelativePath,
    },
    /// Push local content to Drive.
    Upload {
        /// Target path.
        path: RelativePath,
        /// Remote file id if updating an existing file.
        remote_id: Option<String>,
    },
    /// Fetch remote content to disk.
    Download {
        /// Target path.
        path: RelativePath,
        /// Remote Drive file id.
        remote_id: String,
    },
    /// Remove the local copy.
    DeleteLocal {
        /// Target path.
        path: RelativePath,
    },
    /// Remove or trash the remote object.
    DeleteRemote {
        /// Target path (mirror path under sync root).
        path: RelativePath,
        /// Remote Drive file id.
        remote_id: String,
    },
    /// Local and remote both changed; needs resolution policy or user input.
    Conflict {
        /// Target path.
        path: RelativePath,
        /// Optional remote file id when known.
        remote_id: Option<String>,
        /// Optional local content hash for diagnostics.
        local_md5: Option<String>,
        /// Resolution derived from configured conflict policy.
        resolution: ConflictResolution,
    },
    /// Remove stale sidecar or index entries without touching file bytes.
    CleanupMetadata {
        /// Target path.
        path: RelativePath,
    },
}

/// User- or policy-selected outcome when the same file diverged locally and on Drive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ConflictResolution {
    /// Keep local; overwrite or ignore remote.
    LocalWins,
    /// Keep remote; overwrite local.
    RemoteWins,
    /// Write the losing side to a new name using `suffix`.
    Rename {
        /// Suffix inserted before the extension (e.g. `"_conflict"`).
        suffix: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_time() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2024-01-02T15:04:05Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn relative_path_round_trip() {
        let p = RelativePath::from(r"a\b\c");
        let json = serde_json::to_string(&p).unwrap();
        let back: RelativePath = serde_json::from_str(&json).unwrap();
        assert_eq!(back.as_str(), "a/b/c");
    }

    #[test]
    fn local_file_round_trip() {
        let v = LocalFile {
            path: RelativePath::from("doc/readme.md"),
            md5: "d41d8cd98f00b204e9800998ecf8427e".to_string(),
            mtime: sample_time(),
            size: 42,
        };
        let json = serde_json::to_string(&v).unwrap();
        let back: LocalFile = serde_json::from_str(&json).unwrap();
        assert_eq!(v, back);
    }

    #[test]
    fn sync_record_round_trip() {
        let v = SyncRecord {
            drive_file_id: Some("drive1".into()),
            remote_md5: Some("ab".repeat(16)),
            remote_mime_type: Some("text/plain".into()),
            remote_modified_at: Some(sample_time()),
            local_md5: "cd".repeat(16),
            local_mtime: sample_time(),
            local_size: 9,
            last_synced_at: sample_time(),
        };
        let json = serde_json::to_string(&v).unwrap();
        let back: SyncRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(v, back);
    }

    #[test]
    fn sync_report_round_trip() {
        let v = SyncReport {
            uploaded: vec![RelativePath::from("up.bin")],
            downloaded: vec![RelativePath::from("down.bin")],
            deleted_local: vec![],
            deleted_remote: vec![],
            conflicts: vec![RelativePath::from("both.md")],
            skipped: 2,
            errors: vec![(RelativePath::from("bad"), "oops".into())],
            duration: Duration::from_millis(1500),
        };
        let json = serde_json::to_string(&v).unwrap();
        let back: SyncReport = serde_json::from_str(&json).unwrap();
        assert_eq!(v, back);
    }

    #[test]
    fn sync_action_round_trip() {
        let cases = vec![
            SyncAction::Skip {
                path: RelativePath::from("a"),
            },
            SyncAction::Upload {
                path: RelativePath::from("b"),
                remote_id: Some("file1".into()),
            },
            SyncAction::Download {
                path: RelativePath::from("c"),
                remote_id: "file2".into(),
            },
            SyncAction::DeleteLocal {
                path: RelativePath::from("d"),
            },
            SyncAction::DeleteRemote {
                path: RelativePath::from("e"),
                remote_id: "file3".into(),
            },
            SyncAction::Conflict {
                path: RelativePath::from("f"),
                remote_id: None,
                local_md5: Some("00".repeat(16)),
                resolution: ConflictResolution::LocalWins,
            },
            SyncAction::CleanupMetadata {
                path: RelativePath::from("g"),
            },
        ];
        for action in cases {
            let json = serde_json::to_string(&action).unwrap();
            let back: SyncAction = serde_json::from_str(&json).unwrap();
            assert_eq!(action, back);
        }
    }

    #[test]
    fn conflict_resolution_round_trip() {
        let cases = vec![
            ConflictResolution::LocalWins,
            ConflictResolution::RemoteWins,
            ConflictResolution::Rename {
                suffix: "_mine".into(),
            },
        ];
        for c in cases {
            let json = serde_json::to_string(&c).unwrap();
            let back: ConflictResolution = serde_json::from_str(&json).unwrap();
            assert_eq!(c, back);
        }
    }

    #[test]
    fn upload_session_round_trip() {
        let v = UploadSession {
            mode: UploadSessionMode::Convert {
                drive_id: "drive-1".to_string(),
                google_mime: "application/vnd.google-apps.document".to_string(),
            },
            session_url: "https://upload.example/session/1".to_string(),
            next_offset: 1024,
            file_size: 2048,
            local_md5: "a".repeat(32),
            updated_at: sample_time(),
        };
        let json = serde_json::to_string(&v).unwrap();
        let back: UploadSession = serde_json::from_str(&json).unwrap();
        assert_eq!(v, back);
    }

    #[test]
    fn pending_op_round_trip() {
        let v = PendingOp {
            kind: PendingOpKind::Upload,
            stage: PendingOpStage::SideEffectDone,
            updated_at: sample_time(),
        };
        let json = serde_json::to_string(&v).unwrap();
        let back: PendingOp = serde_json::from_str(&json).unwrap();
        assert_eq!(v, back);
    }

    #[test]
    fn relative_path_safety_guards_traversal_and_absolute_forms() {
        assert!(RelativePath::from("docs/readme.md").is_safe_non_empty());
        assert!(!RelativePath::from("../etc/passwd").is_safe());
        assert!(!RelativePath::from("/tmp/file").is_safe());
        assert!(!RelativePath::from("a/./b").is_safe());
        assert!(!RelativePath::from("a//b").is_safe());
        assert!(!RelativePath::from(r"C:\windows\system32").is_safe());
    }
}
