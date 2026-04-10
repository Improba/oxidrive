//! Atomic filesystem helpers and a simple per-directory trash folder.

use std::ffi::OsString;
use std::io::{Error as IoError, ErrorKind};
use std::path::{Path, PathBuf};

use tokio::fs;
use tokio::io::AsyncWriteExt;

use crate::error::OxidriveError;

/// Writes `data` to `target` atomically by staging to `target` + `.part` in the same directory, then renaming into place.
pub async fn atomic_write(target: &Path, data: &[u8]) -> Result<(), OxidriveError> {
    let parent = target.parent().ok_or_else(|| {
        OxidriveError::from(IoError::new(
            ErrorKind::InvalidInput,
            "atomic_write: path has no parent directory",
        ))
    })?;
    let name = target.file_name().ok_or_else(|| {
        OxidriveError::from(IoError::new(
            ErrorKind::InvalidInput,
            "atomic_write: path has no file name",
        ))
    })?;
    let mut part_name: OsString = name.to_os_string();
    part_name.push(".part");
    let part_path: PathBuf = parent.join(part_name);

    fs::create_dir_all(parent).await?;
    let mut f = fs::File::create(&part_path).await?;
    f.write_all(data).await?;
    f.sync_all().await?;
    drop(f);
    fs::rename(&part_path, target).await?;
    Ok(())
}

/// Moves `path` into `sync_root`/.trash/ (simple local trash, not the OS recycle bin).
///
/// If a file with the same name already exists in trash, a numeric suffix is appended before the extension.
#[allow(dead_code)]
pub async fn move_to_trash(sync_root: &Path, path: &Path) -> Result<(), OxidriveError> {
    let name = path.file_name().ok_or_else(|| {
        OxidriveError::from(IoError::new(
            ErrorKind::InvalidInput,
            "move_to_trash: path has no file name",
        ))
    })?;

    let trash_dir = sync_root.join(".trash");
    fs::create_dir_all(&trash_dir).await?;

    let mut dest = trash_dir.join(name);
    if fs::metadata(&dest).await.is_ok() {
        let stem = Path::new(name)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("file");
        let ext = Path::new(name).extension();
        for i in 1u32.. {
            let mut candidate = trash_dir.join(format!("{stem}.{i}"));
            if let Some(e) = ext {
                candidate.set_extension(e);
            }
            if fs::metadata(&candidate).await.is_err() {
                dest = candidate;
                break;
            }
        }
    }

    fs::rename(path, &dest).await?;
    Ok(())
}

/// Deletes `*.part` files directly under `dir` (non-recursive). Returns how many files were removed.
#[allow(dead_code)]
pub fn cleanup_part_files(dir: &Path) -> Result<usize, OxidriveError> {
    let read_dir = std::fs::read_dir(dir)?;
    let mut removed = 0usize;
    for entry in read_dir {
        let entry = entry?;
        let p = entry.path();
        if !p.is_file() {
            continue;
        }
        let Some(name) = p.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.ends_with(".part") {
            continue;
        }
        std::fs::remove_file(&p)?;
        removed += 1;
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[tokio::test]
    async fn atomic_write_leaves_no_part_file() {
        let dir = tempdir().expect("tempdir");
        let target = dir.path().join("out.txt");
        atomic_write(&target, b"hello")
            .await
            .expect("atomic write");
        assert_eq!(fs::read_to_string(&target).expect("read"), "hello");
        assert!(!dir.path().join("out.txt.part").exists());
    }

    #[tokio::test]
    async fn move_to_trash_places_file_under_sync_root_dot_trash() {
        let dir = tempdir().expect("tempdir");
        let f = dir.path().join("a.txt");
        fs::write(&f, b"x").expect("write");
        move_to_trash(dir.path(), &f).await.expect("trash");
        assert!(!f.exists());
        let trashed = dir.path().join(".trash").join("a.txt");
        assert!(trashed.is_file());
    }

    #[test]
    fn cleanup_part_files_removes_stale_part_files() {
        let dir = tempdir().expect("tempdir");
        fs::write(dir.path().join("ok.txt"), b"a").expect("write");
        fs::write(dir.path().join("stale.part"), b"b").expect("write");
        let n = cleanup_part_files(dir.path()).expect("cleanup");
        assert_eq!(n, 1);
        assert!(!dir.path().join("stale.part").exists());
        assert!(dir.path().join("ok.txt").exists());
    }
}
