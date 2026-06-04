//! Folder lifecycle helpers for Google Drive.

use std::collections::{HashMap, HashSet};

use reqwest::Method;
use serde::Deserialize;
use serde_json::json;

use crate::drive::client::DriveClient;
use crate::drive::types::FOLDER;
use crate::error::OxidriveError;

#[derive(Debug, Deserialize)]
struct CreatedFolder {
    id: String,
}

#[derive(Debug, Deserialize)]
struct FolderLookup {
    id: String,
}

#[derive(Debug, Deserialize)]
struct FolderLookupResponse {
    #[serde(default)]
    files: Vec<FolderLookup>,
}

/// Escapes a value for use inside a Drive `files.list` `q` string literal.
pub(crate) fn escape_drive_query_value(value: &str) -> String {
    value.replace('\\', "\\\\").replace('\'', "\\'")
}

/// Looks up a non-trashed folder named `name` directly under `parent_id`.
///
/// Returns the smallest matching Drive id (deterministic, matching the listing dedup order which
/// sorts by name then id) so the canonical folder is reused instead of creating a duplicate.
pub async fn find_folder(
    client: &DriveClient,
    name: &str,
    parent_id: &str,
) -> Result<Option<String>, OxidriveError> {
    let escaped_name = escape_drive_query_value(name);
    let escaped_parent = escape_drive_query_value(parent_id);
    let query = format!(
        "'{escaped_parent}' in parents and name = '{escaped_name}' and mimeType = '{FOLDER}' and trashed = false"
    );
    let mut url = reqwest::Url::parse(&client.drive_api_url("/files"))
        .map_err(|e| OxidriveError::drive(format!("bad Drive URL: {e}")))?;
    {
        let mut qp = url.query_pairs_mut();
        qp.append_pair("q", &query);
        qp.append_pair("fields", "files(id, name)");
        qp.append_pair("pageSize", "100");
        qp.append_pair("supportsAllDrives", "true");
        qp.append_pair("includeItemsFromAllDrives", "true");
    }
    let resp: FolderLookupResponse = client
        .request(Method::GET, url.as_str(), |b| b)
        .await?
        .json()
        .await
        .map_err(|e| OxidriveError::drive(format!("parse folder lookup: {e}")))?;
    let chosen = resp.files.into_iter().min_by(|a, b| a.id.cmp(&b.id));
    Ok(chosen.map(|f| f.id))
}

/// Reuses an existing folder named `name` under `parent_id`, or creates it if none exists.
pub async fn find_or_create_folder(
    client: &DriveClient,
    name: &str,
    parent_id: &str,
) -> Result<String, OxidriveError> {
    match find_folder(client, name, parent_id).await? {
        Some(existing) => {
            tracing::debug!(name, parent_id, folder_id = %existing, "reusing existing drive folder");
            Ok(existing)
        }
        None => create_folder(client, name, parent_id).await,
    }
}

/// Creates a folder on Google Drive and returns its Drive id.
pub async fn create_folder(
    client: &DriveClient,
    name: &str,
    parent_id: &str,
) -> Result<String, OxidriveError> {
    let body = json!({
        "name": name,
        "mimeType": "application/vnd.google-apps.folder",
        "parents": [parent_id],
    });
    let url = client.drive_api_url("/files?supportsAllDrives=true");
    let resp = client
        .request(Method::POST, &url, move |b| b.json(&body))
        .await?;
    let created: CreatedFolder = resp
        .json()
        .await
        .map_err(|e| OxidriveError::drive(format!("parse folder create response: {e}")))?;
    tracing::debug!(name, parent_id, folder_id = %created.id, "created drive folder");
    Ok(created.id)
}

/// Moves a Drive folder to trash by id.
#[allow(dead_code)]
pub async fn trash_folder(client: &DriveClient, folder_id: &str) -> Result<(), OxidriveError> {
    let body = json!({ "trashed": true });
    let url = client.drive_api_url(&format!("/files/{folder_id}?supportsAllDrives=true"));
    let _ = client
        .request(Method::PATCH, &url, move |b| b.json(&body))
        .await?;
    tracing::debug!(folder_id, "trashed drive folder");
    Ok(())
}

/// Ensures all parent folders for `paths` exist on Drive and returns `rel_path -> drive_id`.
///
/// Existing entries from `existing_folders` are preserved. Missing parents are created in
/// topological order (shortest paths first).
pub async fn ensure_folder_hierarchy(
    client: &DriveClient,
    paths: &[&str],
    root_folder_id: &str,
    existing_folders: &HashMap<String, String>,
) -> Result<HashMap<String, String>, OxidriveError> {
    let mut all_folders = existing_folders.clone();
    let needed = collect_parent_folders(paths);
    for rel in needed {
        if all_folders.contains_key(&rel) {
            continue;
        }
        let (parent_rel, name) = match rel.rsplit_once('/') {
            Some((parent, child)) => (parent, child),
            None => ("", rel.as_str()),
        };
        let parent_id = if parent_rel.is_empty() {
            root_folder_id.to_string()
        } else {
            all_folders.get(parent_rel).cloned().ok_or_else(|| {
                OxidriveError::sync(format!(
                    "missing parent folder id for '{}' while creating '{}'",
                    parent_rel, rel
                ))
            })?
        };
        let folder_id = find_or_create_folder(client, name, &parent_id).await?;
        all_folders.insert(rel.clone(), folder_id);
    }
    Ok(all_folders)
}

fn collect_parent_folders(paths: &[&str]) -> Vec<String> {
    let mut unique = HashSet::new();
    for raw in paths {
        let normalized = normalize_relative_path(raw);
        let parent = normalized
            .rsplit_once('/')
            .map_or("", |(prefix, _)| prefix)
            .trim_matches('/');
        if parent.is_empty() {
            continue;
        }
        let mut current = String::new();
        for segment in parent.split('/') {
            if segment.is_empty() {
                continue;
            }
            if current.is_empty() {
                current.push_str(segment);
            } else {
                current.push('/');
                current.push_str(segment);
            }
            unique.insert(current.clone());
        }
    }
    let mut ordered: Vec<String> = unique.into_iter().collect();
    ordered.sort_by(|a, b| {
        let depth_a = a.split('/').count();
        let depth_b = b.split('/').count();
        depth_a.cmp(&depth_b).then_with(|| a.cmp(b))
    });
    ordered
}

fn normalize_relative_path(raw: &str) -> String {
    raw.replace('\\', "/").trim_matches('/').to_string()
}

#[cfg(test)]
mod tests {
    use super::{collect_parent_folders, escape_drive_query_value};

    #[test]
    fn escapes_single_quotes_and_backslashes_in_query_values() {
        assert_eq!(escape_drive_query_value("o'brien"), "o\\'brien");
        assert_eq!(escape_drive_query_value("a\\b"), "a\\\\b");
        assert_eq!(escape_drive_query_value("plain"), "plain");
    }

    #[test]
    fn parent_folders_are_topologically_sorted() {
        let paths = vec!["docs/reports/q1.csv", "docs/reports/2026/q2.csv"];
        let folders = collect_parent_folders(&paths);
        assert_eq!(
            folders,
            vec![
                "docs".to_string(),
                "docs/reports".to_string(),
                "docs/reports/2026".to_string()
            ]
        );
    }
}
