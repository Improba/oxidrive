//! MD5 helpers with a simple in-memory cache keyed by `(size, mtime)`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use md5::{Digest, Md5};
use tokio::task;

use crate::error::OxidriveError;

/// Computes the MD5 digest of `path` and returns a lowercase hex string (alias for [`compute_md5`]).
#[allow(dead_code)]
pub async fn md5_file(path: &Path) -> Result<String, OxidriveError> {
    compute_md5(path).await
}

/// Computes the MD5 digest of a file and returns it as a lowercase hex string.
pub async fn compute_md5(path: &Path) -> Result<String, OxidriveError> {
    let path = path.to_path_buf();
    task::spawn_blocking(move || compute_md5_blocking(&path))
        .await
        .map_err(|e| {
            OxidriveError::from(std::io::Error::other(
                format!("compute_md5 join: {e}"),
            ))
        })?
}

/// Caches MD5 digests keyed by path, invalidated when file size or modification time changes.
#[allow(dead_code)]
#[derive(Debug, Default)]
pub struct Md5Cache {
    entries: HashMap<PathBuf, (u64, SystemTime, String)>,
}

#[allow(dead_code)]
impl Md5Cache {
    /// Creates an empty cache.
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    /// Returns a cached MD5 when `(size, mtime)` matches; otherwise recomputes and stores the result.
    pub async fn compute_md5_cached(&mut self, path: &Path) -> Result<String, OxidriveError> {
        let meta = tokio::fs::metadata(path).await?;
        let len = meta.len();
        let mtime = meta.modified()?;
        let path_buf = path.to_path_buf();

        if let Some((sz, mt, digest)) = self.entries.get(&path_buf) {
            if *sz == len && *mt == mtime {
                return Ok(digest.clone());
            }
        }

        let digest = compute_md5(path).await?;
        self.entries.insert(path_buf, (len, mtime, digest.clone()));
        Ok(digest)
    }
}

fn compute_md5_blocking(path: &Path) -> Result<String, OxidriveError> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Md5::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = std::io::Read::read(&mut file, &mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;
    use std::time::Duration;
    use tempfile::tempdir;
    use tokio::time::sleep;

    #[tokio::test]
    async fn compute_md5_matches_known_vector() {
        let dir = tempdir().expect("tempdir");
        let p = dir.path().join("f.bin");
        fs::write(&p, b"hello").expect("write");
        let h = compute_md5(&p).await.expect("md5");
        assert_eq!(h, "5d41402abc4b2a76b9719d911017c592");
    }

    #[tokio::test]
    async fn cache_hits_when_size_and_mtime_unchanged() {
        let dir = tempdir().expect("tempdir");
        let p = dir.path().join("f.bin");
        fs::write(&p, b"data").expect("write");
        let mut cache = Md5Cache::new();
        let a = cache.compute_md5_cached(&p).await.expect("first");
        let b = cache.compute_md5_cached(&p).await.expect("second");
        assert_eq!(a, b);
    }

    #[tokio::test]
    async fn cache_misses_after_content_change() {
        let dir = tempdir().expect("tempdir");
        let p = dir.path().join("f.bin");
        fs::write(&p, b"v1").expect("write");
        let mut cache = Md5Cache::new();
        let a = cache.compute_md5_cached(&p).await.expect("first");
        sleep(Duration::from_millis(20)).await;
        let mut f = fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&p)
            .expect("open");
        f.write_all(b"v2-longer").expect("write");
        f.sync_all().expect("sync");
        drop(f);
        let b = cache.compute_md5_cached(&p).await.expect("after change");
        assert_ne!(a, b);
    }
}
