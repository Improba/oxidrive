//! Dispatches changed files to the correct text extractor.

use std::path::{Path, PathBuf};

use crate::error::OxidriveError;
use crate::index::csv_extract;
use crate::index::docx;
use crate::index::pdf;
use crate::index::pptx;
use crate::index::xlsx;
use crate::types::RelativePath;
use crate::utils::fs::atomic_write;

fn index_markdown_path(index_dir: &Path, rel: &RelativePath) -> PathBuf {
    let mut dest = index_dir.join(rel.as_str());
    dest.set_extension("md");
    dest
}

/// Rebuilds Markdown sidecars under `index_dir` for every supported file in `changed_files`.
///
/// Output paths mirror the relative layout under `sync_dir`, using a `.md` extension.
pub async fn update_index(
    changed_files: &[RelativePath],
    sync_dir: &Path,
    index_dir: &Path,
) -> Result<usize, OxidriveError> {
    tokio::fs::create_dir_all(index_dir)
        .await
        .map_err(|e| OxidriveError::other(format!("create index dir: {e}")))?;

    let mut count = 0usize;
    for rel in changed_files {
        if !rel.is_safe_non_empty() {
            tracing::warn!(path = %rel, "skipping unsafe path for index update");
            continue;
        }
        let src = sync_dir.join(rel.as_str());
        let dest = index_markdown_path(index_dir, rel);

        if !src.exists() {
            if tokio::fs::metadata(&dest).await.is_ok() {
                tokio::fs::remove_file(&dest).await.map_err(|e| {
                    OxidriveError::other(format!("remove index {}: {e}", dest.display()))
                })?;
                count += 1;
            }
            continue;
        }

        let ext = src
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();

        let markdown = match ext.as_str() {
            "docx" => docx::docx_to_markdown(&src)?,
            "xlsx" => xlsx::xlsx_to_markdown(&src)?,
            "pptx" => pptx::pptx_to_markdown(&src)?,
            "pdf" => pdf::pdf_to_markdown(&src)?,
            "csv" => csv_extract::csv_to_markdown(&src)?,
            "txt" | "md" | "markdown" | "rst" | "text" => {
                std::fs::read_to_string(&src).map_err(|e| {
                    OxidriveError::other(format!("Failed to read {}: {e}", src.display()))
                })?
            }
            _ => {
                let meta = std::fs::metadata(&src).map_err(|e| {
                    OxidriveError::other(format!("Failed to stat {}: {e}", src.display()))
                })?;
                let name = src
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                format!(
                    "# {name}\n\n- **Extension** : .{ext}\n- **Taille** : {} octets\n- **Modifié** : {:?}\n",
                    meta.len(),
                    meta.modified().ok(),
                )
            }
        };

        if let Some(parent) = dest.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| OxidriveError::other(format!("mkdir {}: {e}", parent.display())))?;
        }
        atomic_write(&dest, markdown.as_bytes()).await?;
        count += 1;
    }

    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn text_passthrough_writes_content() {
        let sync = tempdir().expect("tempdir");
        let index = tempdir().expect("tempdir");
        std::fs::write(sync.path().join("note.txt"), "hello index").expect("write");
        let changed = [RelativePath::from("note.txt")];
        let n = update_index(&changed, sync.path(), index.path())
            .await
            .expect("update");
        assert_eq!(n, 1);
        let md = std::fs::read_to_string(index.path().join("note.md")).expect("read md");
        assert_eq!(md, "hello index");
    }

    #[tokio::test]
    async fn binary_unknown_writes_metadata() {
        let sync = tempdir().expect("tempdir");
        let index = tempdir().expect("tempdir");
        std::fs::write(sync.path().join("pic.png"), [0u8, 1, 2]).expect("write");
        let changed = [RelativePath::from("pic.png")];
        let n = update_index(&changed, sync.path(), index.path())
            .await
            .expect("update");
        assert_eq!(n, 1);
        let md = std::fs::read_to_string(index.path().join("pic.md")).expect("read md");
        assert!(md.contains("# pic.png"));
        assert!(md.contains("**Extension**"));
        assert!(md.contains(".png"));
        assert!(md.contains("3 octets"));
    }

    #[tokio::test]
    async fn missing_source_removes_index_sidecar() {
        let sync = tempdir().expect("tempdir");
        let index = tempdir().expect("tempdir");
        let sidecar = index.path().join("gone.md");
        std::fs::write(&sidecar, "stale").expect("write sidecar");
        let changed = [RelativePath::from("gone.txt")];
        let n = update_index(&changed, sync.path(), index.path())
            .await
            .expect("update");
        assert_eq!(n, 1);
        assert!(!sidecar.exists());
    }
}
