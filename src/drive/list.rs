//! Recursive Drive listing built on `files.list`.

use std::collections::{HashMap, VecDeque};

use serde::Deserialize;

use crate::drive::client::DriveClient;
use crate::drive::types::{DriveFile, FOLDER};
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
    let mut assigned_names_by_folder: HashMap<String, HashMap<String, String>> = HashMap::new();
    queue.push_back((folder_id.to_string(), RelativePath::from("")));

    while let Some((current_id, prefix)) = queue.pop_front() {
        let mut page_token: Option<String> = None;
        loop {
            let mut url = reqwest::Url::parse(&client.drive_api_url("/files"))
                .map_err(|e| OxidriveError::drive(format!("bad Drive URL: {e}")))?;
            {
                let mut qp = url.query_pairs_mut();
                qp.append_pair("q", &format!("'{current_id}' in parents and trashed=false"));
                qp.append_pair(
                    "fields",
                    "nextPageToken, files(id, name, mimeType, md5Checksum, modifiedTime, size, parents, trashed)",
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

            for file in page.files {
                let assigned_names = assigned_names_by_folder
                    .entry(current_id.clone())
                    .or_default();
                let unique_name = dedupe_name_for_folder(&file.name, assigned_names);
                let rel = if prefix.as_str().is_empty() {
                    RelativePath::from(unique_name.as_str())
                } else {
                    RelativePath::from(format!("{}/{}", prefix.as_str(), unique_name))
                };

                if unique_name != file.name {
                    if let Some(existing_file_id) = assigned_names.get(&file.name) {
                        tracing::warn!(
                            path = %build_relative_from_parts(prefix.as_str(), file.name.as_str()),
                            deduplicated_path = %rel,
                            first_file_id = %existing_file_id,
                            duplicate_file_id = %file.id,
                            "duplicate Drive filename in folder; assigned deduplicated local path"
                        );
                    }
                }
                assigned_names.insert(unique_name, file.id.clone());

                if file.mime_type == FOLDER {
                    queue.push_back((file.id.clone(), rel.clone()));
                }

                result.insert(rel, file);
            }

            page_token = page.next_page_token;
            if page_token.is_none() {
                break;
            }
        }
    }

    Ok(result)
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

fn build_relative_from_parts(prefix: &str, name: &str) -> RelativePath {
    if prefix.is_empty() {
        RelativePath::from(name)
    } else {
        RelativePath::from(format!("{prefix}/{name}"))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::dedupe_name_for_folder;

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
}
