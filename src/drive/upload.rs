//! Multipart uploads and media updates for Drive.

use std::path::Path;
use std::sync::Arc;

use reqwest::Method;
use serde::Deserialize;
use serde_json::json;

use crate::drive::client::DriveClient;
use crate::error::OxidriveError;

const MAX_IN_MEMORY_UPLOAD_BYTES: u64 = 128 * 1024 * 1024;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreatedFile {
    id: String,
}

fn multipart_related(
    metadata: &serde_json::Value,
    media: &[u8],
    media_type: &str,
) -> (String, Vec<u8>) {
    let boundary = "oxidrive_related_7q2n";
    let mut body = Vec::new();
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(b"Content-Type: application/json; charset=UTF-8\r\n\r\n");
    body.extend_from_slice(metadata.to_string().as_bytes());
    body.extend_from_slice(format!("\r\n--{boundary}\r\n").as_bytes());
    body.extend_from_slice(format!("Content-Type: {media_type}\r\n\r\n").as_bytes());
    body.extend_from_slice(media);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    (boundary.to_string(), body)
}

async fn read_upload_bytes(local_path: &Path) -> Result<Vec<u8>, OxidriveError> {
    let size = tokio::fs::metadata(local_path)
        .await
        .map_err(|e| OxidriveError::drive(format!("stat {}: {e}", local_path.display())))?
        .len();
    if size > MAX_IN_MEMORY_UPLOAD_BYTES {
        return Err(OxidriveError::drive(format!(
            "file {} is too large for current upload mode ({} bytes > {} bytes)",
            local_path.display(),
            size,
            MAX_IN_MEMORY_UPLOAD_BYTES
        )));
    }
    tokio::fs::read(local_path)
        .await
        .map_err(|e| OxidriveError::drive(format!("read {}: {e}", local_path.display())))
}

/// Creates a new binary file under `parent_id` and returns its Drive id.
pub async fn upload_file(
    client: &DriveClient,
    local_path: &Path,
    parent_id: &str,
    name: &str,
) -> Result<String, OxidriveError> {
    let data = read_upload_bytes(local_path).await?;
    let meta = json!({
        "name": name,
        "parents": [parent_id],
    });
    let (boundary, body) = multipart_related(&meta, &data, "application/octet-stream");
    let ctype = Arc::new(format!("multipart/related; boundary={boundary}"));
    let body = Arc::new(body);
    let url = client.upload_api_url("/files?uploadType=multipart&supportsAllDrives=true");
    let resp = client
        .request(Method::POST, &url, {
            let ctype = Arc::clone(&ctype);
            let body = Arc::clone(&body);
            move |b| {
                b.header("Content-Type", ctype.as_str())
                    .body((*body).clone())
            }
        })
        .await?;
    let created: CreatedFile = resp
        .json()
        .await
        .map_err(|e| OxidriveError::drive(format!("parse upload response: {e}")))?;
    Ok(created.id)
}

/// Replaces the media of an existing file id.
pub async fn update_file(
    client: &DriveClient,
    local_path: &Path,
    drive_id: &str,
) -> Result<(), OxidriveError> {
    let data = read_upload_bytes(local_path).await?;
    let data = Arc::new(data);
    let url = client.upload_api_url(&format!(
        "/files/{drive_id}?uploadType=media&supportsAllDrives=true"
    ));
    let _ = client
        .request(Method::PATCH, &url, {
            let data = Arc::clone(&data);
            move |b| {
                b.header("Content-Type", "application/octet-stream")
                    .body((*data).clone())
            }
        })
        .await?;
    Ok(())
}

/// Uploads local bytes and sets the Google Apps `mimeType` metadata (conversion).
pub async fn upload_with_conversion(
    client: &DriveClient,
    local_path: &Path,
    drive_id: &str,
    google_mime: &str,
) -> Result<(), OxidriveError> {
    let data = read_upload_bytes(local_path).await?;
    let meta = json!({
        "mimeType": google_mime,
    });
    let (boundary, body) = multipart_related(&meta, &data, "application/octet-stream");
    let ctype = Arc::new(format!("multipart/related; boundary={boundary}"));
    let body = Arc::new(body);
    let url = client.upload_api_url(&format!(
        "/files/{drive_id}?uploadType=multipart&supportsAllDrives=true"
    ));
    let _ = client
        .request(Method::PATCH, &url, {
            let ctype = Arc::clone(&ctype);
            let body = Arc::clone(&body);
            move |b| {
                b.header("Content-Type", ctype.as_str())
                    .body((*body).clone())
            }
        })
        .await?;
    Ok(())
}
