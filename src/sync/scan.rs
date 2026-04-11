//! Local filesystem scanning with ignore rules and content hashing.

use std::collections::HashMap;
use std::path::Path;

use chrono::{DateTime, Utc};
use tokio::fs;

use crate::error::OxidriveError;
use crate::types::{LocalFile, RelativePath};
use crate::utils::hash::compute_md5;

/// Returns `true` when `rel_path` (forward slashes, relative to sync root) matches an ignore rule.
fn is_ignored(rel_path: &str, patterns: &[String]) -> bool {
    patterns.iter().any(|p| pattern_matches(rel_path, p))
}

fn pattern_matches(path: &str, pat: &str) -> bool {
    let pat = pat.trim();
    if pat.is_empty() {
        return false;
    }
    if let Some(prefix) = pat.strip_suffix("/**") {
        let prefix = prefix.trim_end_matches('/');
        if prefix.is_empty() {
            return true;
        }
        return path == prefix || path.starts_with(&format!("{prefix}/"));
    }
    if !pat.contains('*') && !pat.contains('?') && !pat.contains('[') {
        return path == pat || path.ends_with(&format!("/{pat}"));
    }
    if pat.starts_with("*.") && pat.find('/').is_none() && !pat[2..].contains('*') {
        let suffix = &pat[1..];
        return path.ends_with(suffix);
    }
    if pat.starts_with('*') && pat.find('/').is_none() {
        let suffix = &pat[1..];
        return !suffix.contains('*') && path.ends_with(suffix);
    }
    false
}

fn system_time_to_utc(st: std::time::SystemTime) -> DateTime<Utc> {
    let Ok(dur) = st.duration_since(std::time::UNIX_EPOCH) else {
        return Utc::now();
    };
    DateTime::from_timestamp(dur.as_secs() as i64, dur.subsec_nanos()).unwrap_or_else(Utc::now)
}

/// Recursively scans `root`, skipping `.index/`, applying `ignore_patterns`, and hashing files.
pub async fn scan_local(
    root: &Path,
    ignore_patterns: &[String],
) -> Result<HashMap<RelativePath, LocalFile>, OxidriveError> {
    let mut out = HashMap::new();
    walk_tree(root, &RelativePath::from(""), ignore_patterns, &mut out).await?;
    Ok(out)
}

async fn walk_tree(
    dir: &Path,
    rel_dir: &RelativePath,
    patterns: &[String],
    out: &mut HashMap<RelativePath, LocalFile>,
) -> Result<(), OxidriveError> {
    let mut rd = fs::read_dir(dir)
        .await
        .map_err(|e| OxidriveError::sync(format!("read_dir {}: {e}", dir.display())))?;
    while let Some(ent) = rd
        .next_entry()
        .await
        .map_err(|e| OxidriveError::sync(format!("next_entry: {e}")))?
    {
        let name = ent.file_name();
        let name_s = name.to_string_lossy();
        if name_s == ".index" {
            continue;
        }

        let rel: RelativePath = if rel_dir.as_str().is_empty() {
            RelativePath::from(name_s.as_ref())
        } else {
            RelativePath::from(format!("{}/{}", rel_dir.as_str(), name_s.as_ref()))
        };

        if is_ignored(rel.as_str(), patterns) {
            continue;
        }

        let path = ent.path();
        let meta = fs::symlink_metadata(&path)
            .await
            .map_err(|e| OxidriveError::sync(format!("metadata {}: {e}", path.display())))?;
        if meta.file_type().is_symlink() {
            continue;
        }

        if meta.is_dir() {
            Box::pin(walk_tree(&path, &rel, patterns, out)).await?;
        } else if meta.is_file() {
            let md5 = compute_md5(&path).await?;
            let mtime = meta
                .modified()
                .map_err(|e| OxidriveError::sync(format!("mtime {}: {e}", path.display())))?;
            let file = LocalFile {
                path: rel.clone(),
                md5,
                mtime: system_time_to_utc(mtime),
                size: meta.len(),
            };
            out.insert(rel, file);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::symlink;
    use tempfile::tempdir;

    #[tokio::test]
    async fn skips_index_and_respects_ignore() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".index")).unwrap();
        fs::write(dir.path().join(".index/secret"), "x").unwrap();
        fs::write(dir.path().join("keep.txt"), "hello").unwrap();
        fs::create_dir_all(dir.path().join("sub")).unwrap();
        fs::write(dir.path().join("sub/a.tmp"), "t").unwrap();
        let m = scan_local(dir.path(), &["*.tmp".into()]).await.unwrap();
        assert!(m.contains_key(&RelativePath::from("keep.txt")));
        assert!(!m.contains_key(&RelativePath::from("sub/a.tmp")));
        assert!(!m.contains_key(&RelativePath::from(".index/secret")));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn skips_symlinked_file_and_directory() {
        let dir = tempdir().unwrap();
        let outside = tempdir().unwrap();

        fs::write(outside.path().join("secret.txt"), "top-secret").unwrap();
        fs::create_dir_all(outside.path().join("nested")).unwrap();
        fs::write(outside.path().join("nested/n.txt"), "hello").unwrap();

        symlink(
            outside.path().join("secret.txt"),
            dir.path().join("linked-secret.txt"),
        )
        .unwrap();
        symlink(outside.path().join("nested"), dir.path().join("linked-dir")).unwrap();
        fs::write(dir.path().join("regular.txt"), "ok").unwrap();

        let m = scan_local(dir.path(), &[]).await.unwrap();
        assert!(m.contains_key(&RelativePath::from("regular.txt")));
        assert!(!m.contains_key(&RelativePath::from("linked-secret.txt")));
        assert!(!m.contains_key(&RelativePath::from("linked-dir/n.txt")));
    }
}
