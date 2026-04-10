//! Binary download and Google Workspace export helpers.

use std::collections::HashMap;
use std::path::Path;

use reqwest::Method;
use serde::Deserialize;

use crate::drive::client::DriveClient;
use crate::error::OxidriveError;
use crate::utils::fs::atomic_write;

/// Downloads a file's media bytes to `dest` using a `.part` temporary and atomic rename.
pub async fn download_file(
    client: &DriveClient,
    drive_id: &str,
    dest: &Path,
) -> Result<(), OxidriveError> {
    let url = client.drive_api_url(&format!(
        "/files/{drive_id}?alt=media&supportsAllDrives=true"
    ));
    let bytes = client
        .request(Method::GET, &url, |b| b)
        .await?
        .bytes()
        .await
        .map_err(|e| OxidriveError::drive(format!("read body: {e}")))?;
    atomic_write(dest, &bytes).await?;
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
    let bytes = client
        .request(Method::GET, &url, |b| b)
        .await?
        .bytes()
        .await
        .map_err(|e| OxidriveError::drive(format!("read export body: {e}")))?;
    atomic_write(dest, &bytes).await?;
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

    let bytes = client
        .request(Method::GET, &export_url, |b| b)
        .await?
        .bytes()
        .await
        .map_err(|e| OxidriveError::drive(format!("read exportLinks body: {e}")))?;
    atomic_write(dest, &bytes).await?;
    Ok(())
}
