//! Google Drive API value types and Workspace export helpers.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize};

/// A file or folder object returned by Drive API `files` resources.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DriveFile {
    /// Drive file id.
    pub id: String,
    /// Display name.
    pub name: String,
    /// MIME type (`application/vnd.google-apps.*` for native Google files).
    pub mime_type: String,
    /// MD5 checksum when Drive exposes it (often absent for native Google files).
    #[serde(default)]
    pub md5_checksum: Option<String>,
    /// Last modification time on Drive.
    pub modified_time: DateTime<Utc>,
    /// Size in bytes when applicable.
    #[serde(default, deserialize_with = "deserialize_opt_size")]
    pub size: Option<u64>,
    /// Parent folder ids.
    #[serde(default)]
    pub parents: Vec<String>,
    /// Whether the file is in the trash.
    #[serde(default)]
    pub trashed: bool,
}

/// Value stored in [`crate::types::SyncRecord::remote_md5`] to compare remote state when resuming sync.
///
/// Uses the Drive MD5 when present; otherwise encodes [`DriveFile::modified_time`] so native Google
/// files without a checksum can still be tracked.
pub fn remote_content_fingerprint(remote: &DriveFile) -> String {
    remote.md5_checksum.clone().unwrap_or_else(|| {
        format!("mtime:{}", remote.modified_time.to_rfc3339())
    })
}

fn deserialize_opt_size<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Aux {
        Str(String),
        Num(u64),
    }
    let v = Option::<Aux>::deserialize(deserializer)?;
    Ok(match v {
        None => None,
        Some(Aux::Num(n)) => Some(n),
        Some(Aux::Str(s)) => s.parse().ok(),
    })
}

/// A single change returned by the Drive `changes` API.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DriveChange {
    /// Affected file id.
    pub file_id: String,
    /// File metadata when available.
    #[serde(default)]
    pub file: Option<DriveFile>,
    /// Whether the file was removed from the user's view.
    #[serde(default)]
    pub removed: bool,
    /// Time of the change.
    pub time: DateTime<Utc>,
}

/// Google Docs MIME type.
pub const GOOGLE_DOC: &str = "application/vnd.google-apps.document";
/// Google Sheets MIME type.
pub const GOOGLE_SHEET: &str = "application/vnd.google-apps.spreadsheet";
/// Google Slides MIME type.
pub const GOOGLE_SLIDES: &str = "application/vnd.google-apps.presentation";
/// Google Drawings MIME type.
pub const GOOGLE_DRAWING: &str = "application/vnd.google-apps.drawing";
/// Drive folder MIME type.
pub const FOLDER: &str = "application/vnd.google-apps.folder";

/// Describes how to export a native Google Workspace file to a portable MIME type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExportFormat {
    /// Source Google MIME type.
    pub google_mime: &'static str,
    /// MIME type to request from the export endpoint.
    pub export_mime: &'static str,
    /// Suggested file extension for the exported bytes.
    pub extension: &'static str,
}

/// Returns the preferred export mapping for sync downloads (OOXML/SVG fidelity).
pub fn export_format_sync(mime: &str) -> Option<ExportFormat> {
    match mime {
        GOOGLE_DOC => Some(ExportFormat {
            google_mime: GOOGLE_DOC,
            export_mime: "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            extension: "docx",
        }),
        GOOGLE_SHEET => Some(ExportFormat {
            google_mime: GOOGLE_SHEET,
            export_mime: "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            extension: "xlsx",
        }),
        GOOGLE_SLIDES => Some(ExportFormat {
            google_mime: GOOGLE_SLIDES,
            export_mime: "application/vnd.openxmlformats-officedocument.presentationml.presentation",
            extension: "pptx",
        }),
        GOOGLE_DRAWING => Some(ExportFormat {
            google_mime: GOOGLE_DRAWING,
            export_mime: "image/svg+xml",
            extension: "svg",
        }),
        _ => None,
    }
}

/// Returns the preferred export mapping for indexing (lightweight text extraction).
#[allow(dead_code)]
pub fn export_format_index(mime: &str) -> Option<ExportFormat> {
    match mime {
        GOOGLE_DOC => Some(ExportFormat {
            google_mime: GOOGLE_DOC,
            export_mime: "text/markdown",
            extension: "md",
        }),
        GOOGLE_SHEET => Some(ExportFormat {
            google_mime: GOOGLE_SHEET,
            export_mime: "text/csv",
            extension: "csv",
        }),
        GOOGLE_SLIDES => Some(ExportFormat {
            google_mime: GOOGLE_SLIDES,
            export_mime: "text/plain",
            extension: "txt",
        }),
        _ => None,
    }
}

/// Backward-compatible alias to the sync export mapping.
#[allow(dead_code)]
pub fn export_format(mime: &str) -> Option<ExportFormat> {
    export_format_sync(mime)
}

/// Returns `true` when `mime` refers to an `application/vnd.google-apps.*` object (except folders).
pub fn is_google_workspace(mime: &str) -> bool {
    mime.starts_with("application/vnd.google-apps.") && mime != FOLDER
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_sync_maps_docs_to_docx() {
        let f = export_format_sync(GOOGLE_DOC).expect("doc");
        assert_eq!(f.extension, "docx");
        assert_eq!(
            f.export_mime,
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
        );
    }

    #[test]
    fn export_index_maps_docs_to_markdown() {
        let f = export_format_index(GOOGLE_DOC).expect("doc");
        assert_eq!(f.extension, "md");
        assert_eq!(f.export_mime, "text/markdown");
    }

    #[test]
    fn export_alias_uses_sync_format() {
        assert_eq!(export_format(GOOGLE_SHEET), export_format_sync(GOOGLE_SHEET));
    }

    #[test]
    fn export_index_does_not_map_drawings() {
        assert!(export_format_index(GOOGLE_DRAWING).is_none());
    }

    #[test]
    fn workspace_detection() {
        assert!(is_google_workspace(GOOGLE_SHEET));
        assert!(!is_google_workspace(FOLDER));
        assert!(!is_google_workspace("text/plain"));
    }

    #[test]
    fn drive_file_deserializes_size_string() {
        let j = r#"{
            "id": "1",
            "name": "a",
            "mimeType": "text/plain",
            "modifiedTime": "2024-01-01T00:00:00.000Z",
            "size": "42"
        }"#;
        let f: DriveFile = serde_json::from_str(j).expect("parse");
        assert_eq!(f.size, Some(42));
    }
}
