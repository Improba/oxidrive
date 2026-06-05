//! Conflict observability helpers.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::OxidriveError;

/// Default JSONL conflict log file name under `.oxidrive/`.
pub const CONFLICT_LOG_FILE: &str = "conflicts.log";

/// One JSONL record describing a resolved sync conflict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConflictLogEntry {
    /// Event timestamp in RFC3339 format.
    pub timestamp: DateTime<Utc>,
    /// Relative path that triggered the conflict.
    pub path: String,
    /// Resolution label (`conflict_copy`, `revision_mismatch`, ...).
    pub resolution: String,
    /// Local device id that resolved the conflict.
    pub local_device: String,
    /// Remote origin (`ox_origin`) when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_origin: Option<String>,
    /// Local conflict-copy path when one was created.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copy_path: Option<String>,
}

/// Appends a conflict record to `{oxidrive_dir}/conflicts.log` as a JSON line.
pub fn append_conflict_log(
    oxidrive_dir: &Path,
    entry: &ConflictLogEntry,
) -> Result<(), OxidriveError> {
    fs::create_dir_all(oxidrive_dir)
        .map_err(|e| OxidriveError::sync(format!("create {}: {e}", oxidrive_dir.display())))?;
    let log_path = oxidrive_dir.join(CONFLICT_LOG_FILE);
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|e| OxidriveError::sync(format!("open {}: {e}", log_path.display())))?;
    serde_json::to_writer(&mut file, entry)
        .map_err(|e| OxidriveError::sync(format!("serialize conflict log entry: {e}")))?;
    file.write_all(b"\n")
        .map_err(|e| OxidriveError::sync(format!("append {}: {e}", log_path.display())))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{append_conflict_log, ConflictLogEntry, CONFLICT_LOG_FILE};
    use chrono::{TimeZone, Utc};
    use std::fs;

    #[test]
    fn append_conflict_log_writes_jsonl_records() {
        let dir = tempfile::tempdir().expect("tempdir");
        let oxidrive_dir = dir.path().join(".oxidrive");
        let first = ConflictLogEntry {
            timestamp: Utc
                .with_ymd_and_hms(2026, 1, 1, 10, 0, 0)
                .single()
                .expect("timestamp"),
            path: "docs/file.txt".to_string(),
            resolution: "conflict_copy".to_string(),
            local_device: "device-a".to_string(),
            remote_origin: Some("device-b".to_string()),
            copy_path: Some("docs/file.conflict.device-a.20260101100000.txt".to_string()),
        };
        let second = ConflictLogEntry {
            timestamp: Utc
                .with_ymd_and_hms(2026, 1, 1, 11, 0, 0)
                .single()
                .expect("timestamp"),
            path: "docs/file.txt".to_string(),
            resolution: "revision_mismatch".to_string(),
            local_device: "device-a".to_string(),
            remote_origin: None,
            copy_path: None,
        };

        append_conflict_log(&oxidrive_dir, &first).expect("append first");
        append_conflict_log(&oxidrive_dir, &second).expect("append second");

        let body = fs::read_to_string(oxidrive_dir.join(CONFLICT_LOG_FILE)).expect("read log");
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 2);

        let parsed_first: ConflictLogEntry =
            serde_json::from_str(lines[0]).expect("parse first entry");
        let parsed_second: ConflictLogEntry =
            serde_json::from_str(lines[1]).expect("parse second entry");
        assert_eq!(parsed_first, first);
        assert_eq!(parsed_second, second);
    }
}
