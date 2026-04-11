//! Binary download and Google Workspace export helpers.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::Path;

use reqwest::Method;
use serde::Deserialize;
use tokio::io::AsyncWriteExt;

use crate::drive::client::DriveClient;
use crate::error::OxidriveError;

async fn stream_response_to_path(
    mut response: reqwest::Response,
    dest: &Path,
) -> Result<(), OxidriveError> {
    let parent = dest.parent().ok_or_else(|| {
        OxidriveError::drive(format!("destination has no parent: {}", dest.display()))
    })?;
    let filename = dest.file_name().ok_or_else(|| {
        OxidriveError::drive(format!("destination has no file name: {}", dest.display()))
    })?;
    let mut part_name: OsString = filename.to_os_string();
    part_name.push(".part");
    let part_path = parent.join(part_name);

    tokio::fs::create_dir_all(parent)
        .await
        .map_err(|e| OxidriveError::drive(format!("mkdir {}: {e}", parent.display())))?;
    let mut file = tokio::fs::File::create(&part_path)
        .await
        .map_err(|e| OxidriveError::drive(format!("create {}: {e}", part_path.display())))?;
    loop {
        let chunk = match response.chunk().await {
            Ok(chunk) => chunk,
            Err(e) => {
                drop(file);
                let _ = tokio::fs::remove_file(&part_path).await;
                return Err(OxidriveError::drive(format!("read response stream: {e}")));
            }
        };
        let Some(chunk) = chunk else {
            break;
        };
        if let Err(e) = file.write_all(&chunk).await {
            drop(file);
            let _ = tokio::fs::remove_file(&part_path).await;
            return Err(OxidriveError::drive(format!(
                "write {}: {e}",
                part_path.display()
            )));
        }
    }
    if let Err(e) = file.sync_all().await {
        drop(file);
        let _ = tokio::fs::remove_file(&part_path).await;
        return Err(OxidriveError::drive(format!(
            "sync {}: {e}",
            part_path.display()
        )));
    }
    drop(file);
    if let Err(e) = tokio::fs::rename(&part_path, dest).await {
        let _ = tokio::fs::remove_file(&part_path).await;
        return Err(OxidriveError::drive(format!(
            "rename {} -> {}: {e}",
            part_path.display(),
            dest.display()
        )));
    }
    Ok(())
}

/// Downloads a file's media bytes to `dest` using a `.part` temporary and atomic rename.
pub async fn download_file(
    client: &DriveClient,
    drive_id: &str,
    dest: &Path,
) -> Result<(), OxidriveError> {
    let url = client.drive_api_url(&format!(
        "/files/{drive_id}?alt=media&supportsAllDrives=true"
    ));
    let response = client.request(Method::GET, &url, |b| b).await?;
    stream_response_to_path(response, dest).await?;
    Ok(())
}

/// Exports a Google Workspace file to `export_mime` and writes to `dest` atomically.
pub async fn export_file(
    client: &DriveClient,
    drive_id: &str,
    export_mime: &str,
    dest: &Path,
) -> Result<(), OxidriveError> {
    let mut url = reqwest::Url::parse(&client.drive_api_url(&format!("/files/{drive_id}/export")))
        .map_err(|e| OxidriveError::drive(format!("export URL: {e}")))?;
    url.query_pairs_mut()
        .append_pair("mimeType", export_mime)
        .append_pair("supportsAllDrives", "true");
    let url = url.to_string();
    let response = client.request(Method::GET, &url, |b| b).await?;
    stream_response_to_path(response, dest).await?;
    Ok(())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExportLinksResponse {
    #[serde(default)]
    export_links: HashMap<String, String>,
}

fn is_export_size_limit_error(err: &OxidriveError) -> bool {
    let msg = err.to_string();
    let lower = msg.to_ascii_lowercase();
    lower.contains("http 413")
        || (lower.contains("http 403")
            && (lower.contains("exportsizelimitexceeded")
                || lower.contains("export size limit exceeded")))
}

/// Exports a Google Workspace file and falls back to `exportLinks` when direct export is too large.
pub async fn export_file_with_fallback(
    client: &DriveClient,
    drive_id: &str,
    export_mime: &str,
    dest: &Path,
) -> Result<(), OxidriveError> {
    match export_file(client, drive_id, export_mime, dest).await {
        Ok(()) => return Ok(()),
        Err(err) if !is_export_size_limit_error(&err) => return Err(err),
        Err(_) => {}
    }

    let metadata_url = client.drive_api_url(&format!(
        "/files/{drive_id}?fields=exportLinks&supportsAllDrives=true"
    ));
    let links: ExportLinksResponse = client
        .request(Method::GET, &metadata_url, |b| b)
        .await?
        .json()
        .await
        .map_err(|e| OxidriveError::drive(format!("parse exportLinks metadata: {e}")))?;
    let export_url = links
        .export_links
        .get(export_mime)
        .ok_or_else(|| {
            OxidriveError::drive(format!(
                "exportLinks missing URL for mime type {export_mime}"
            ))
        })?
        .to_string();

    let response = client.request(Method::GET, &export_url, |b| b).await?;
    stream_response_to_path(response, dest).await?;
    Ok(())
}
