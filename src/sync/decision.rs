//! Pure sync decision logic mapping `(local, remote, metadata)` to a [`SyncAction`](crate::types::SyncAction).

use crate::config::ConflictPolicy;
use crate::drive::types::{remote_content_fingerprint, DriveFile};
use crate::types::{ConflictResolution, LocalFile, RelativePath, SyncAction, SyncRecord};

/// Returns `true` when local content or mtime differs from the last reconciled metadata.
fn local_changed(local: &LocalFile, meta: &SyncRecord) -> bool {
    local.md5 != meta.local_md5 || local.mtime != meta.local_mtime
}

/// Returns `true` when converted local content differs from the last exported bytes.
fn local_changed_converted(local: &LocalFile, last_export_md5: Option<&str>) -> bool {
    match last_export_md5 {
        Some(last) => local.md5 != last,
        None => true,
    }
}

/// Returns `true` when remote content or modification time differs from the last reconciled metadata.
fn remote_changed(remote: &DriveFile, meta: &SyncRecord) -> bool {
    let current = remote_content_fingerprint(remote);
    match meta.remote_md5.as_deref() {
        Some(stored) => stored != current.as_str(),
        None => meta
            .remote_modified_at
            .map(|t| t != remote.modified_time)
            .unwrap_or(true),
    }
}

/// Chooses the next action for `path` given the three reconciliation views.
///
pub fn determine_action(
    path: &RelativePath,
    local: Option<&LocalFile>,
    remote: Option<&DriveFile>,
    metadata: Option<&SyncRecord>,
    policy: &ConflictPolicy,
) -> SyncAction {
    match (local, remote, metadata) {
        (Some(l), Some(r), Some(m)) => {
            let lc = local_changed(l, m);
            let rc = remote_changed(r, m);
            match (lc, rc) {
                (false, false) => SyncAction::Skip {
                    path: path.clone(),
                },
                (true, false) => SyncAction::Upload {
                    path: path.clone(),
                    remote_id: m
                        .drive_file_id
                        .clone()
                        .or_else(|| Some(r.id.clone())),
                },
                (false, true) => SyncAction::Download {
                    path: path.clone(),
                    remote_id: r.id.clone(),
                },
                (true, true) => SyncAction::Conflict {
                    path: path.clone(),
                    remote_id: Some(r.id.clone()),
                    local_md5: Some(l.md5.clone()),
                    resolution: conflict_resolution_from_policy(policy),
                },
            }
        }
        (Some(l), Some(r), None) => {
            match (&r.md5_checksum, &l.md5) {
                (Some(rm), lm) if rm == lm => SyncAction::Skip {
                    path: path.clone(),
                },
                (None, _) => SyncAction::Conflict {
                    path: path.clone(),
                    remote_id: Some(r.id.clone()),
                    local_md5: Some(l.md5.clone()),
                    resolution: conflict_resolution_from_policy(policy),
                },
                _ => SyncAction::Conflict {
                    path: path.clone(),
                    remote_id: Some(r.id.clone()),
                    local_md5: Some(l.md5.clone()),
                    resolution: conflict_resolution_from_policy(policy),
                },
            }
        }
        (Some(l), None, Some(m)) => {
            if local_changed(l, m) {
                SyncAction::Upload {
                    path: path.clone(),
                    remote_id: None,
                }
            } else {
                SyncAction::DeleteLocal {
                    path: path.clone(),
                }
            }
        }
        (Some(_l), None, None) => SyncAction::Upload {
            path: path.clone(),
            remote_id: None,
        },
        (None, Some(r), Some(m)) => {
            if remote_changed(r, m) {
                SyncAction::Download {
                    path: path.clone(),
                    remote_id: r.id.clone(),
                }
            } else {
                SyncAction::DeleteRemote {
                    path: path.clone(),
                    remote_id: m
                        .drive_file_id
                        .clone()
                        .unwrap_or_else(|| r.id.clone()),
                }
            }
        }
        (None, Some(r), None) => SyncAction::Download {
            path: path.clone(),
            remote_id: r.id.clone(),
        },
        (None, None, Some(_)) => SyncAction::CleanupMetadata {
            path: path.clone(),
        },
        (None, None, None) => SyncAction::Skip {
            path: path.clone(),
        },
    }
}

/// Like [`determine_action`] but aware of converted Google Workspace files.
///
/// When `is_converted` is true, local edits are compared against `last_export_md5` instead of
/// [`SyncRecord::local_md5`], which avoids false local-change detection when a re-export produced
/// identical bytes.
pub fn determine_action_converted(
    path: &RelativePath,
    local: Option<&LocalFile>,
    remote: Option<&DriveFile>,
    metadata: Option<&SyncRecord>,
    policy: &ConflictPolicy,
    is_converted: bool,
    last_export_md5: Option<&str>,
) -> SyncAction {
    if !is_converted {
        return determine_action(path, local, remote, metadata, policy);
    }

    match (local, remote, metadata) {
        (Some(l), Some(r), Some(m)) => {
            let lc = local_changed_converted(l, last_export_md5);
            let rc = remote_changed(r, m);
            match (lc, rc) {
                (false, false) => SyncAction::Skip {
                    path: path.clone(),
                },
                (true, false) => SyncAction::Upload {
                    path: path.clone(),
                    remote_id: m
                        .drive_file_id
                        .clone()
                        .or_else(|| Some(r.id.clone())),
                },
                (false, true) => SyncAction::Download {
                    path: path.clone(),
                    remote_id: r.id.clone(),
                },
                (true, true) => SyncAction::Conflict {
                    path: path.clone(),
                    remote_id: Some(r.id.clone()),
                    local_md5: Some(l.md5.clone()),
                    resolution: conflict_resolution_from_policy(policy),
                },
            }
        }
        (Some(l), None, Some(_m)) => {
            if local_changed_converted(l, last_export_md5) {
                SyncAction::Upload {
                    path: path.clone(),
                    remote_id: None,
                }
            } else {
                SyncAction::DeleteLocal {
                    path: path.clone(),
                }
            }
        }
        _ => determine_action(path, local, remote, metadata, policy),
    }
}

fn conflict_resolution_from_policy(policy: &ConflictPolicy) -> ConflictResolution {
    match policy {
        ConflictPolicy::LocalWins => ConflictResolution::LocalWins,
        ConflictPolicy::RemoteWins => ConflictResolution::RemoteWins,
        ConflictPolicy::Rename { suffix } => {
            let ts = chrono::Utc::now().format("%Y%m%d%H%M%S");
            let actual_suffix = if suffix.is_empty() {
                format!(".conflict.{ts}")
            } else {
                format!("{suffix}.{ts}")
            };
            ConflictResolution::Rename {
                suffix: actual_suffix,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    fn t(y: i32, m: u32, d: u32) -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, 12, 0, 0).unwrap()
    }

    fn path(name: &str) -> RelativePath {
        RelativePath::from(name)
    }

    fn local(md5: &str, mtime: chrono::DateTime<Utc>) -> LocalFile {
        LocalFile {
            path: path("ignored"),
            md5: md5.to_string(),
            mtime,
            size: 1,
        }
    }

    fn remote(
        id: &str,
        md5: Option<&str>,
        mtime: chrono::DateTime<Utc>,
    ) -> DriveFile {
        DriveFile {
            id: id.into(),
            name: "n".into(),
            mime_type: "text/plain".into(),
            md5_checksum: md5.map(String::from),
            modified_time: mtime,
            size: Some(1),
            parents: vec![],
            trashed: false,
        }
    }

    fn meta(
        local_md5: &str,
        local_mtime: chrono::DateTime<Utc>,
        remote_md5_stored: Option<&str>,
        drive_id: Option<&str>,
    ) -> SyncRecord {
        SyncRecord {
            drive_file_id: drive_id.map(String::from),
            remote_md5: remote_md5_stored.map(String::from),
            remote_modified_at: None,
            local_md5: local_md5.into(),
            local_mtime,
            local_size: 1,
            last_synced_at: local_mtime,
        }
    }

    #[test]
    fn matrix_1_skip_both_unchanged() {
        let m = meta("a", t(2020, 1, 1), Some("b"), Some("id"));
        let a = determine_action(
            &path("f"),
            Some(&local("a", t(2020, 1, 1))),
            Some(&remote("id", Some("b"), t(2020, 1, 2))),
            Some(&m),
            &ConflictPolicy::LocalWins,
        );
        assert!(matches!(a, SyncAction::Skip { .. }));
    }

    #[test]
    fn matrix_2_upload_local_changed() {
        let m = meta("old", t(2020, 1, 1), Some("b"), Some("id"));
        let a = determine_action(
            &path("f"),
            Some(&local("new", t(2020, 1, 1))),
            Some(&remote("id", Some("b"), t(2020, 1, 2))),
            Some(&m),
            &ConflictPolicy::LocalWins,
        );
        assert!(
            matches!(a, SyncAction::Upload { remote_id: Some(ref id), .. } if id == "id")
        );
    }

    #[test]
    fn matrix_3_download_remote_changed() {
        let m = meta("a", t(2020, 1, 1), Some("old"), Some("id"));
        let a = determine_action(
            &path("f"),
            Some(&local("a", t(2020, 1, 1))),
            Some(&remote("id", Some("new"), t(2020, 1, 3))),
            Some(&m),
            &ConflictPolicy::LocalWins,
        );
        assert!(
            matches!(a, SyncAction::Download { remote_id, .. } if remote_id == "id")
        );
    }

    #[test]
    fn matrix_4_conflict_both_changed() {
        let m = meta("a", t(2020, 1, 1), Some("b"), Some("id"));
        let a = determine_action(
            &path("f"),
            Some(&local("a2", t(2020, 1, 1))),
            Some(&remote("id", Some("b2"), t(2020, 1, 2))),
            Some(&m),
            &ConflictPolicy::LocalWins,
        );
        assert!(matches!(
            a,
            SyncAction::Conflict {
                resolution: ConflictResolution::LocalWins,
                ..
            }
        ));
    }

    #[test]
    fn matrix_4_conflict_policy_remote_wins() {
        let m = meta("a", t(2020, 1, 1), Some("b"), Some("id"));
        let a = determine_action(
            &path("f"),
            Some(&local("a2", t(2020, 1, 1))),
            Some(&remote("id", Some("b2"), t(2020, 1, 2))),
            Some(&m),
            &ConflictPolicy::RemoteWins,
        );
        assert!(matches!(
            a,
            SyncAction::Conflict {
                resolution: ConflictResolution::RemoteWins,
                ..
            }
        ));
    }

    #[test]
    fn matrix_4_conflict_policy_rename_uses_configured_suffix_with_timestamp() {
        let m = meta("a", t(2020, 1, 1), Some("b"), Some("id"));
        let a = determine_action(
            &path("f"),
            Some(&local("a2", t(2020, 1, 1))),
            Some(&remote("id", Some("b2"), t(2020, 1, 2))),
            Some(&m),
            &ConflictPolicy::Rename {
                suffix: "_ignored".into(),
            },
        );
        match a {
            SyncAction::Conflict {
                resolution: ConflictResolution::Rename { suffix },
                ..
            } => assert!(suffix.starts_with("_ignored.")),
            _ => panic!("expected conflict rename resolution"),
        }
    }

    #[test]
    fn matrix_5_no_meta_md5_equal_skips() {
        let a = determine_action(
            &path("f"),
            Some(&local("same", t(2020, 1, 1))),
            Some(&remote("id", Some("same"), t(2020, 1, 2))),
            None,
            &ConflictPolicy::LocalWins,
        );
        assert!(matches!(a, SyncAction::Skip { .. }));
    }

    #[test]
    fn matrix_5_no_meta_md5_diff_conflict() {
        let a = determine_action(
            &path("f"),
            Some(&local("x", t(2020, 1, 1))),
            Some(&remote("id", Some("y"), t(2020, 1, 2))),
            None,
            &ConflictPolicy::LocalWins,
        );
        assert!(matches!(a, SyncAction::Conflict { .. }));
    }

    #[test]
    fn matrix_6_delete_local_remote_gone_unchanged() {
        let m = meta("a", t(2020, 1, 1), Some("b"), Some("id"));
        let a = determine_action(
            &path("f"),
            Some(&local("a", t(2020, 1, 1))),
            None,
            Some(&m),
            &ConflictPolicy::LocalWins,
        );
        assert!(matches!(a, SyncAction::DeleteLocal { .. }));
    }

    #[test]
    fn matrix_7_upload_after_remote_delete_local_changed() {
        let m = meta("old", t(2020, 1, 1), Some("b"), Some("id"));
        let a = determine_action(
            &path("f"),
            Some(&local("new", t(2020, 1, 1))),
            None,
            Some(&m),
            &ConflictPolicy::LocalWins,
        );
        assert!(
            matches!(a, SyncAction::Upload { remote_id: None, .. })
        );
    }

    #[test]
    fn matrix_8_upload_new_local_only() {
        let a = determine_action(
            &path("f"),
            Some(&local("a", t(2020, 1, 1))),
            None,
            None,
            &ConflictPolicy::LocalWins,
        );
        assert!(
            matches!(a, SyncAction::Upload { remote_id: None, .. })
        );
    }

    #[test]
    fn matrix_9_delete_remote_local_gone_unchanged_remote() {
        let m = meta("a", t(2020, 1, 1), Some("b"), Some("id"));
        let a = determine_action(
            &path("f"),
            None,
            Some(&remote("id", Some("b"), t(2020, 1, 2))),
            Some(&m),
            &ConflictPolicy::LocalWins,
        );
        assert!(
            matches!(a, SyncAction::DeleteRemote { remote_id, .. } if remote_id == "id")
        );
    }

    #[test]
    fn matrix_10_download_local_deleted_remote_changed() {
        let m = meta("a", t(2020, 1, 1), Some("old"), Some("id"));
        let a = determine_action(
            &path("f"),
            None,
            Some(&remote("id", Some("new"), t(2020, 1, 3))),
            Some(&m),
            &ConflictPolicy::LocalWins,
        );
        assert!(
            matches!(a, SyncAction::Download { remote_id, .. } if remote_id == "id")
        );
    }

    #[test]
    fn matrix_11_download_new_remote_only() {
        let a = determine_action(
            &path("f"),
            None,
            Some(&remote("id", Some("b"), t(2020, 1, 2))),
            None,
            &ConflictPolicy::LocalWins,
        );
        assert!(
            matches!(a, SyncAction::Download { remote_id, .. } if remote_id == "id")
        );
    }

    #[test]
    fn matrix_12_cleanup_both_absent_with_meta() {
        let m = meta("a", t(2020, 1, 1), Some("b"), Some("id"));
        let a = determine_action(
            &path("f"),
            None,
            None,
            Some(&m),
            &ConflictPolicy::LocalWins,
        );
        assert!(matches!(a, SyncAction::CleanupMetadata { .. }));
    }

    #[test]
    fn edge_google_native_no_md5_first_sync_conflict() {
        let a = determine_action(
            &path("doc"),
            Some(&local("localhash", t(2020, 1, 1))),
            Some(&remote("id", None, t(2020, 1, 2))),
            None,
            &ConflictPolicy::LocalWins,
        );
        assert!(matches!(a, SyncAction::Conflict { .. }));
    }

    #[test]
    fn edge_google_native_tracks_remote_mtime_in_meta() {
        let r = remote("id", None, t(2020, 1, 2));
        let m = SyncRecord {
            drive_file_id: Some("id".into()),
            remote_md5: Some(remote_content_fingerprint(&r)),
            remote_modified_at: Some(r.modified_time),
            local_md5: "a".into(),
            local_mtime: t(2020, 1, 1),
            local_size: 1,
            last_synced_at: t(2020, 1, 1),
        };
        let a = determine_action(
            &path("doc"),
            Some(&local("a", t(2020, 1, 1))),
            Some(&r),
            Some(&m),
            &ConflictPolicy::LocalWins,
        );
        assert!(matches!(a, SyncAction::Skip { .. }));
    }

    #[test]
    fn edge_none_none_none_skips() {
        let a = determine_action(
            &path("ghost"),
            None,
            None,
            None,
            &ConflictPolicy::LocalWins,
        );
        assert!(matches!(a, SyncAction::Skip { .. }));
    }

    #[test]
    fn converted_remote_changed_downloads() {
        let m = meta("a", t(2020, 1, 1), Some("old"), Some("id"));
        let a = determine_action_converted(
            &path("doc.docx"),
            Some(&local("last-export", t(2020, 1, 1))),
            Some(&remote("id", None, t(2020, 1, 3))),
            Some(&m),
            &ConflictPolicy::LocalWins,
            true,
            Some("last-export"),
        );
        assert!(
            matches!(a, SyncAction::Download { remote_id, .. } if remote_id == "id")
        );
    }

    #[test]
    fn converted_local_changed_uploads() {
        let m = meta("ignored", t(2020, 1, 1), Some("same"), Some("id"));
        let a = determine_action_converted(
            &path("sheet.xlsx"),
            Some(&local("local-edited", t(2020, 1, 1))),
            Some(&remote("id", Some("same"), t(2020, 1, 2))),
            Some(&m),
            &ConflictPolicy::LocalWins,
            true,
            Some("last-export"),
        );
        assert!(
            matches!(a, SyncAction::Upload { remote_id: Some(ref id), .. } if id == "id")
        );
    }

    #[test]
    fn converted_unchanged_skips() {
        let r = remote("id", None, t(2020, 1, 2));
        let m = SyncRecord {
            drive_file_id: Some("id".into()),
            remote_md5: Some(remote_content_fingerprint(&r)),
            remote_modified_at: Some(r.modified_time),
            local_md5: "ignored".into(),
            local_mtime: t(2020, 1, 1),
            local_size: 1,
            last_synced_at: t(2020, 1, 1),
        };
        let a = determine_action_converted(
            &path("slides.pptx"),
            Some(&local("last-export", t(2020, 1, 1))),
            Some(&r),
            Some(&m),
            &ConflictPolicy::LocalWins,
            true,
            Some("last-export"),
        );
        assert!(matches!(a, SyncAction::Skip { .. }));
    }

    #[test]
    fn converted_both_changed_conflicts() {
        let m = meta("ignored", t(2020, 1, 1), Some("old"), Some("id"));
        let a = determine_action_converted(
            &path("doc.docx"),
            Some(&local("local-edited", t(2020, 1, 1))),
            Some(&remote("id", Some("new"), t(2020, 1, 2))),
            Some(&m),
            &ConflictPolicy::LocalWins,
            true,
            Some("last-export"),
        );
        assert!(matches!(
            a,
            SyncAction::Conflict {
                resolution: ConflictResolution::LocalWins,
                ..
            }
        ));
    }
}
