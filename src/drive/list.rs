//! Recursive Drive listing built on `files.list`.

use std::collections::{HashMap, VecDeque};

use serde::Deserialize;

use crate::drive::client::DriveClient;
use crate::drive::folders::escape_drive_query_value;
use crate::drive::types::{export_format_sync, local_sync_name, DriveFile, FOLDER};
use crate::error::OxidriveError;
use crate::types::RelativePath;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FileListResponse {
    #[serde(default)]
    files: Vec<DriveFile>,
    #[serde(default)]
    next_page_token: Option<String>,
}

/// Lists every non-trashed file and folder under `folder_id`, keyed by path relative to that folder.
pub async fn list_all_files(
    client: &DriveClient,
    folder_id: &str,
) -> Result<HashMap<RelativePath, DriveFile>, OxidriveError> {
    let mut result = HashMap::new();
    let mut queue: VecDeque<(String, RelativePath)> = VecDeque::new();
    queue.push_back((folder_id.to_string(), RelativePath::from("")));

    while let Some((current_id, prefix)) = queue.pop_front() {
        let mut page_token: Option<String> = None;
        let mut folder_files: Vec<DriveFile> = Vec::new();
        loop {
            let mut url = reqwest::Url::parse(&client.drive_api_url("/files"))
                .map_err(|e| OxidriveError::drive(format!("bad Drive URL: {e}")))?;
            {
                let mut qp = url.query_pairs_mut();
                qp.append_pair("q", &format!("'{current_id}' in parents and trashed=false"));
                qp.append_pair(
                    "fields",
                    "nextPageToken, files(id, name, mimeType, md5Checksum, modifiedTime, size, headRevisionId, version, appProperties, parents, trashed)",
                );
                qp.append_pair("pageSize", "1000");
                qp.append_pair("supportsAllDrives", "true");
                qp.append_pair("includeItemsFromAllDrives", "true");
                if let Some(ref tok) = page_token {
                    qp.append_pair("pageToken", tok);
                }
            }

            let page: FileListResponse = client
                .request(reqwest::Method::GET, url.as_str(), |b| b)
                .await?
                .json()
                .await
                .map_err(|e| OxidriveError::drive(format!("parse file list: {e}")))?;
            folder_files.extend(page.files);

            page_token = page.next_page_token;
            if page_token.is_none() {
                break;
            }
        }

        folder_files.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
        for (unique_name, file) in local_names_in_assignment_order(folder_files) {
            let rel = if prefix.as_str().is_empty() {
                RelativePath::from(unique_name.as_str())
            } else {
                RelativePath::from(format!("{}/{}", prefix.as_str(), unique_name))
            };
            if !rel.is_safe_non_empty() {
                tracing::warn!(
                    path = %rel,
                    file_id = %file.id,
                    "skipping remote file with an unsafe path"
                );
                continue;
            }

            if file.mime_type == FOLDER {
                queue.push_back((file.id.clone(), rel.clone()));
            }

            result.insert(rel, file);
        }
    }

    Ok(result)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FileContentMatch {
    id: String,
    #[serde(default)]
    md5_checksum: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FileContentMatchResponse {
    #[serde(default)]
    files: Vec<FileContentMatch>,
}

/// Finds a non-trashed file named `name` directly under `parent_id` whose content hash equals
/// `local_md5`, returning its Drive id.
///
/// Used before creating a new file to avoid uploading a duplicate of an identical object that
/// already exists on Drive but is not yet tracked locally (e.g. during an incremental cycle where
/// the remote view is built from session stubs rather than a full listing). Matching only on an
/// identical md5 guarantees we never attach to, or overwrite, a genuinely different remote file
/// that happens to share the same name (Drive allows duplicate file names).
pub async fn find_remote_file_id_by_content(
    client: &DriveClient,
    name: &str,
    parent_id: &str,
    local_md5: &str,
) -> Result<Option<String>, OxidriveError> {
    let escaped_name = escape_drive_query_value(name);
    let escaped_parent = escape_drive_query_value(parent_id);
    let query = format!(
        "'{escaped_parent}' in parents and name = '{escaped_name}' and mimeType != '{FOLDER}' and trashed = false"
    );
    let mut url = reqwest::Url::parse(&client.drive_api_url("/files"))
        .map_err(|e| OxidriveError::drive(format!("bad Drive URL: {e}")))?;
    {
        let mut qp = url.query_pairs_mut();
        qp.append_pair("q", &query);
        qp.append_pair("fields", "files(id, md5Checksum)");
        qp.append_pair("pageSize", "100");
        qp.append_pair("supportsAllDrives", "true");
        qp.append_pair("includeItemsFromAllDrives", "true");
    }
    let resp: FileContentMatchResponse = client
        .request(reqwest::Method::GET, url.as_str(), |b| b)
        .await?
        .json()
        .await
        .map_err(|e| OxidriveError::drive(format!("parse file content lookup: {e}")))?;
    Ok(pick_content_match(resp.files, local_md5))
}

/// Deterministically selects the smallest-id file whose md5 matches `local_md5`.
fn pick_content_match(files: Vec<FileContentMatch>, local_md5: &str) -> Option<String> {
    files
        .into_iter()
        .filter(|f| f.md5_checksum.as_deref() == Some(local_md5))
        .min_by(|a, b| a.id.cmp(&b.id))
        .map(|f| f.id)
}

/// Assigns a local sync name to each file in one folder.
///
/// Binary files and folders keep their Drive names and are claimed first, so a
/// Google Doc is never given the path of an existing `.docx` (or other export)
/// that already lives in the same folder. Workspace files then take the export
/// name, with a numeric suffix when that name is already taken.
fn local_names_in_assignment_order(files: Vec<DriveFile>) -> Vec<(String, DriveFile)> {
    let mut plain = Vec::new();
    let mut exported = Vec::new();
    for file in files {
        if export_format_sync(&file.mime_type).is_some() {
            exported.push(file);
        } else {
            plain.push(file);
        }
    }

    let mut assigned_names: HashMap<String, String> = HashMap::new();
    let mut named = Vec::with_capacity(plain.len() + exported.len());
    for file in plain.into_iter().chain(exported) {
        let sync_name = local_sync_name(&file.name, &file.mime_type);
        let unique_name = dedupe_name_for_folder(&sync_name, &assigned_names);
        if unique_name != sync_name {
            if let Some(existing_file_id) = assigned_names.get(&sync_name) {
                tracing::warn!(
                    path = %unique_name,
                    first_file_id = %existing_file_id,
                    duplicate_file_id = %file.id,
                    "two remote files map to '{}'; using '{}'",
                    sync_name,
                    unique_name
                );
            }
        }
        assigned_names.insert(unique_name.clone(), file.id.clone());
        named.push((unique_name, file));
    }
    named
}

fn dedupe_name_for_folder(name: &str, assigned_names: &HashMap<String, String>) -> String {
    if !assigned_names.contains_key(name) {
        return name.to_string();
    }

    let (stem, ext) = split_file_name(name);
    let mut index = 2usize;
    loop {
        let candidate = format!("{stem} ({index}){ext}");
        if !assigned_names.contains_key(&candidate) {
            return candidate;
        }
        index += 1;
    }
}

fn split_file_name(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(dot) if dot > 0 => (&name[..dot], &name[dot..]),
        _ => (name, ""),
    }
}

#[cfg(test)]
mod tests {
    use crate::drive::types::DriveFile;
    use chrono::{TimeZone, Utc};
    use std::collections::HashMap;

    use super::{
        dedupe_name_for_folder, local_names_in_assignment_order, pick_content_match,
        FileContentMatch,
    };
    use crate::drive::types::GOOGLE_DOC;

    fn content_match(id: &str, md5: Option<&str>) -> FileContentMatch {
        FileContentMatch {
            id: id.to_string(),
            md5_checksum: md5.map(|m| m.to_string()),
        }
    }

    #[test]
    fn content_match_requires_exact_md5_and_prefers_smallest_id() {
        let files = vec![
            content_match("id-c", Some("want")),
            content_match("id-a", Some("want")),
            content_match("id-b", Some("other")),
            content_match("id-d", None),
        ];
        assert_eq!(pick_content_match(files, "want"), Some("id-a".to_string()));
    }

    #[test]
    fn content_match_returns_none_when_no_hash_matches() {
        let files = vec![
            content_match("id-a", Some("other")),
            content_match("id-b", None),
        ];
        assert_eq!(pick_content_match(files, "want"), None);
    }

    #[test]
    fn dedupe_duplicate_names_with_incrementing_suffix() {
        let mut assigned = HashMap::new();

        let first = dedupe_name_for_folder("report.txt", &assigned);
        assigned.insert(first, "id-1".to_string());

        let second = dedupe_name_for_folder("report.txt", &assigned);
        assigned.insert(second.clone(), "id-2".to_string());

        let third = dedupe_name_for_folder("report.txt", &assigned);

        assert_eq!(second, "report (2).txt");
        assert_eq!(third, "report (3).txt");
    }

    #[test]
    fn dedupe_skips_used_suffixes_and_preserves_extensions() {
        let mut assigned = HashMap::new();
        assigned.insert("archive.tar.gz".to_string(), "id-1".to_string());
        assigned.insert("archive.tar (2).gz".to_string(), "id-2".to_string());

        let deduped = dedupe_name_for_folder("archive.tar.gz", &assigned);

        assert_eq!(deduped, "archive.tar (3).gz");
    }

    fn drive_file(id: &str, name: &str) -> DriveFile {
        DriveFile {
            id: id.to_string(),
            name: name.to_string(),
            mime_type: "text/plain".to_string(),
            md5_checksum: None,
            modified_time: Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap(),
            size: Some(1),
            head_revision_id: None,
            version: None,
            app_properties: std::collections::BTreeMap::new(),
            parents: vec!["root".to_string()],
            trashed: false,
        }
    }

    #[test]
    fn workspace_export_does_not_take_an_existing_binary_name() {
        let mut binary = drive_file("bin-1", "Report.docx");
        binary.mime_type =
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document".to_string();
        let mut doc = drive_file("doc-1", "Report");
        doc.mime_type = GOOGLE_DOC.to_string();

        let named = local_names_in_assignment_order(vec![doc, binary]);
        let by_id: HashMap<_, _> = named
            .iter()
            .map(|(name, file)| (file.id.as_str(), name.as_str()))
            .collect();
        assert_eq!(by_id.get("bin-1").copied(), Some("Report.docx"));
        assert_eq!(by_id.get("doc-1").copied(), Some("Report (2).docx"));
    }

    #[test]
    fn workspace_file_keeps_export_name_when_nothing_else_claims_it() {
        let mut doc = drive_file("doc-1", "Meeting Notes");
        doc.mime_type = GOOGLE_DOC.to_string();
        let named = local_names_in_assignment_order(vec![doc]);
        assert_eq!(named[0].0, "Meeting Notes.docx");
    }

    #[test]
    fn duplicate_name_mapping_is_stable_when_sorted_by_id() {
        let mut files = [
            drive_file("id-b", "report.txt"),
            drive_file("id-a", "report.txt"),
        ];
        files.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
        let mut assigned = HashMap::new();
        let first = dedupe_name_for_folder(&files[0].name, &assigned);
        assigned.insert(first.clone(), files[0].id.clone());
        let second = dedupe_name_for_folder(&files[1].name, &assigned);

        assert_eq!(files[0].id, "id-a");
        assert_eq!(first, "report.txt");
        assert_eq!(second, "report (2).txt");
    }
}
