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
    // Directory subtree: "prefix/**" matches the directory and anything under it.
    if let Some(prefix) = pat.strip_suffix("/**") {
        let prefix = prefix.trim_end_matches('/');
        if prefix.is_empty() {
            return true;
        }
        return path == prefix || path.starts_with(&format!("{prefix}/"));
    }
    // Patterns without a slash match against the file's base name (e.g. `~$*`,
    // `.~lock.*#`, `*.tmp`). Patterns containing a slash match the full path.
    if pat.contains('/') {
        glob_match(path, pat)
    } else {
        let base = path.rsplit('/').next().unwrap_or(path);
        glob_match(base, pat)
    }
}

/// Minimal glob matcher supporting `*` (any run of non-`/` chars) and `?`
/// (exactly one non-`/` char). Other characters match literally.
fn glob_match(text: &str, pat: &str) -> bool {
    let t: Vec<char> = text.chars().collect();
    let p: Vec<char> = pat.chars().collect();
    let (mut ti, mut pi) = (0usize, 0usize);
    // Backtracking anchors for the most recent `*`.
    let mut star_pi: Option<usize> = None;
    let mut star_ti = 0usize;
    while ti < t.len() {
        if pi < p.len() && p[pi] != '*' && (p[pi] == t[ti] || (p[pi] == '?' && t[ti] != '/')) {
            ti += 1;
            pi += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star_pi = Some(pi);
            star_ti = ti;
            pi += 1;
        } else if let Some(sp) = star_pi {
            // `*` cannot cross a path separator.
            if t[star_ti] == '/' {
                return false;
            }
            star_ti += 1;
            ti = star_ti;
            pi = sp + 1;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

fn system_time_to_utc(st: std::time::SystemTime) -> DateTime<Utc> {
    let Ok(dur) = st.duration_since(std::time::UNIX_EPOCH) else {
        return Utc::now();
    };
    DateTime::from_timestamp(dur.as_secs() as i64, dur.subsec_nanos()).unwrap_or_else(Utc::now)
}

/// Returns true when `mtime` is older than `stability_ms` relative to `now`.
#[must_use]
pub fn is_stable(mtime: DateTime<Utc>, now: DateTime<Utc>, stability_ms: u64) -> bool {
    let threshold = i64::try_from(stability_ms).unwrap_or(i64::MAX);
    (now - mtime).num_milliseconds() >= threshold
}

/// Returns true when a matching office/LibreOffice lock file is present for `rel_path`.
#[must_use]
pub fn has_open_lock(sync_dir: &Path, rel_path: &RelativePath) -> bool {
    if !rel_path.is_safe_non_empty() {
        return false;
    }
    let raw = rel_path.as_str();
    let (parent, file_name) = match raw.rsplit_once('/') {
        Some((parent, file_name)) => (sync_dir.join(parent), file_name),
        None => (sync_dir.to_path_buf(), raw),
    };
    let office_lock = parent.join(format!("~${file_name}"));
    if office_lock.exists() {
        return true;
    }
    let libre_lock = parent.join(format!(".~lock.{file_name}#"));
    libre_lock.exists()
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
        if name_s == ".index" || name_s == ".oxidrive" || name_s == ".trash" {
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
    use chrono::{Duration as ChronoDuration, TimeZone};
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::symlink;
    use tempfile::tempdir;

    #[tokio::test]
    async fn skips_index_and_respects_ignore() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".index")).unwrap();
        fs::write(dir.path().join(".index/secret"), "x").unwrap();
        fs::create_dir_all(dir.path().join(".oxidrive")).unwrap();
        fs::write(dir.path().join(".oxidrive/state.redb"), "db").unwrap();
        fs::write(dir.path().join("keep.txt"), "hello").unwrap();
        fs::create_dir_all(dir.path().join("sub")).unwrap();
        fs::write(dir.path().join("sub/a.tmp"), "t").unwrap();
        let m = scan_local(dir.path(), &["*.tmp".into()]).await.unwrap();
        assert!(m.contains_key(&RelativePath::from("keep.txt")));
        assert!(!m.contains_key(&RelativePath::from("sub/a.tmp")));
        assert!(!m.contains_key(&RelativePath::from(".index/secret")));
        assert!(!m.contains_key(&RelativePath::from(".oxidrive/state.redb")));
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

    #[test]
    fn ignore_patterns_match_real_lock_and_temp_files() {
        // Office and LibreOffice lock files (the patterns that previously never matched).
        assert!(pattern_matches("docs/~$rapport.docx", "~$*"));
        assert!(pattern_matches("~$budget.xlsx", "~$*"));
        assert!(pattern_matches("docs/.~lock.rapport.docx#", ".~lock.*#"));
        assert!(pattern_matches(".~lock.notes.odt#", ".~lock.*#"));

        // Common editor/OS temp files.
        assert!(pattern_matches("src/main.rs.swp", "*.swp"));
        assert!(pattern_matches("a/b/c~", "*~"));
        assert!(pattern_matches("nested/.DS_Store", ".DS_Store"));
        assert!(pattern_matches("video.mp4.part", "*.part"));

        // Directory subtree and literals.
        assert!(pattern_matches(".oxidrive/state.redb", ".oxidrive/**"));
        assert!(pattern_matches("any/4913", "4913"));

        // Non-matches: a regular document is never ignored by lock patterns.
        assert!(!pattern_matches("docs/rapport.docx", "~$*"));
        assert!(!pattern_matches("docs/rapport.docx", ".~lock.*#"));
        assert!(!pattern_matches("notes.txt", "*.swp"));
        // `*` does not cross path separators.
        assert!(!pattern_matches("a/b.tmp", "*.tmp/extra"));
    }

    #[tokio::test]
    async fn scan_skips_office_lock_files() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("docs")).unwrap();
        fs::write(dir.path().join("docs/rapport.docx"), "content").unwrap();
        fs::write(dir.path().join("docs/~$rapport.docx"), "lock").unwrap();
        fs::write(dir.path().join("docs/.~lock.rapport.docx#"), "lock").unwrap();

        let patterns = crate::config::Config::default().effective_ignore_patterns();
        let m = scan_local(dir.path(), &patterns).await.unwrap();
        assert!(m.contains_key(&RelativePath::from("docs/rapport.docx")));
        assert!(!m.contains_key(&RelativePath::from("docs/~$rapport.docx")));
        assert!(!m.contains_key(&RelativePath::from("docs/.~lock.rapport.docx#")));
    }

    #[test]
    fn stability_requires_minimum_age() {
        let now = Utc.with_ymd_and_hms(2024, 1, 2, 3, 4, 5).unwrap();
        let just_written = now - ChronoDuration::milliseconds(400);
        let stable = now - ChronoDuration::milliseconds(1800);
        assert!(!is_stable(just_written, now, 1500));
        assert!(is_stable(stable, now, 1500));
    }

    #[test]
    fn detects_matching_local_lock_files() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("docs")).unwrap();
        let rel = RelativePath::from("docs/rapport.docx");
        assert!(!has_open_lock(dir.path(), &rel));

        fs::write(dir.path().join("docs/~$rapport.docx"), "").unwrap();
        assert!(has_open_lock(dir.path(), &rel));

        fs::remove_file(dir.path().join("docs/~$rapport.docx")).unwrap();
        fs::write(dir.path().join("docs/.~lock.rapport.docx#"), "").unwrap();
        assert!(has_open_lock(dir.path(), &rel));
    }
}
