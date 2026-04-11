//! Multipart uploads and media updates for Drive.

use std::path::Path;
use std::sync::Arc;

use reqwest::header::{CONTENT_LENGTH, CONTENT_RANGE, LOCATION};
use reqwest::Method;
use serde::Deserialize;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use crate::drive::client::DriveClient;
use crate::error::OxidriveError;

pub const RESUMABLE_UPLOAD_THRESHOLD_BYTES: u64 = 32 * 1024 * 1024;
const RESUMABLE_CHUNK_BYTES: usize = 8 * 1024 * 1024;

/// Persisted resumable upload cursor used to continue a large transfer across sync runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumableUploadState {
    pub session_url: String,
    pub next_offset: u64,
    pub file_size: u64,
}

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
    let size = local_file_size(local_path).await?;
    if size > RESUMABLE_UPLOAD_THRESHOLD_BYTES {
        return Err(OxidriveError::drive(format!(
            "file {} is too large for in-memory upload fallback ({} bytes > {} bytes)",
            local_path.display(),
            size,
            RESUMABLE_UPLOAD_THRESHOLD_BYTES
        )));
    }
    tokio::fs::read(local_path)
        .await
        .map_err(|e| OxidriveError::drive(format!("read {}: {e}", local_path.display())))
}

async fn local_file_size(local_path: &Path) -> Result<u64, OxidriveError> {
    Ok(tokio::fs::metadata(local_path)
        .await
        .map_err(|e| OxidriveError::drive(format!("stat {}: {e}", local_path.display())))?
        .len())
}

async fn drive_error_from_response(prefix: &str, resp: reqwest::Response) -> OxidriveError {
    let status = resp.status();
    let body = resp
        .text()
        .await
        .unwrap_or_else(|_| String::from("<body unavailable>"));
    OxidriveError::drive(format!("{prefix} HTTP {status}: {body}"))
}

fn parse_next_offset(resp: &reqwest::Response) -> Option<u64> {
    let range = resp.headers().get("Range")?;
    let range = range.to_str().ok()?;
    let single = range.split(',').next()?;
    let value = single.strip_prefix("bytes=")?;
    let (_, end) = value.split_once('-')?;
    let end = end.parse::<u64>().ok()?;
    end.checked_add(1)
}

async fn start_resumable_session(
    client: &DriveClient,
    method: Method,
    url: String,
    metadata: Option<serde_json::Value>,
    file_size: u64,
) -> Result<String, OxidriveError> {
    let content_len = Arc::new(file_size.to_string());
    let metadata = Arc::new(metadata);
    let resp = client
        .request_raw(method, &url, {
            let content_len = Arc::clone(&content_len);
            let metadata = Arc::clone(&metadata);
            move |b| {
                let b = b
                    .header("X-Upload-Content-Type", "application/octet-stream")
                    .header("X-Upload-Content-Length", content_len.as_str());
                if let Some(meta) = metadata.as_ref() {
                    b.header("Content-Type", "application/json; charset=UTF-8")
                        .json(meta)
                } else {
                    b
                }
            }
        })
        .await?;
    if !resp.status().is_success() {
        return Err(drive_error_from_response("init resumable upload:", resp).await);
    }
    let session = resp
        .headers()
        .get(LOCATION)
        .ok_or_else(|| OxidriveError::drive("resumable upload did not return a Location header"))?
        .to_str()
        .map_err(|e| OxidriveError::drive(format!("invalid resumable session URL: {e}")))?
        .to_string();
    Ok(session)
}

async fn upload_resumable_bytes(
    client: &DriveClient,
    session_url: &str,
    local_path: &Path,
    total_size: u64,
    start_offset: u64,
    mut on_progress: impl FnMut(ResumableUploadState) -> Result<(), OxidriveError>,
) -> Result<reqwest::Response, OxidriveError> {
    if total_size == 0 {
        let resp = client
            .request_raw(Method::PUT, session_url, |b| {
                b.header(CONTENT_LENGTH, "0")
                    .header(CONTENT_RANGE, "bytes */0")
                    .body(Vec::new())
            })
            .await?;
        if resp.status().is_success() {
            return Ok(resp);
        }
        return Err(drive_error_from_response("resumable upload:", resp).await);
    }

    let mut file = tokio::fs::File::open(local_path)
        .await
        .map_err(|e| OxidriveError::drive(format!("open {}: {e}", local_path.display())))?;
    let mut offset = start_offset.min(total_size);
    if offset > 0 {
        file.seek(std::io::SeekFrom::Start(offset))
            .await
            .map_err(|e| {
                OxidriveError::drive(format!(
                    "seek {} to resumable offset {offset}: {e}",
                    local_path.display()
                ))
            })?;
    }
    let mut buffer = vec![0u8; RESUMABLE_CHUNK_BYTES];

    loop {
        let remaining = total_size.saturating_sub(offset);
        if remaining == 0 {
            return Err(OxidriveError::drive(format!(
                "resumable upload for {} has no remaining bytes at offset {}",
                local_path.display(),
                offset
            )));
        }
        let to_read = remaining.min(buffer.len() as u64) as usize;
        file.read_exact(&mut buffer[..to_read]).await.map_err(|e| {
            OxidriveError::drive(format!(
                "read chunk {} at offset {offset}: {e}",
                local_path.display()
            ))
        })?;

        let chunk = Arc::new(buffer[..to_read].to_vec());
        let start = offset;
        let end = start + (to_read as u64) - 1;
        let content_len = Arc::new(to_read.to_string());
        let content_range = Arc::new(format!("bytes {start}-{end}/{total_size}"));
        let resp = client
            .request_raw(Method::PUT, session_url, {
                let chunk = Arc::clone(&chunk);
                let content_len = Arc::clone(&content_len);
                let content_range = Arc::clone(&content_range);
                move |b| {
                    b.header(CONTENT_LENGTH, content_len.as_str())
                        .header(CONTENT_RANGE, content_range.as_str())
                        .body((*chunk).clone())
                }
            })
            .await?;
        let status = resp.status();
        if status.is_success() {
            return Ok(resp);
        }
        if status.as_u16() == 308 {
            let next = parse_next_offset(&resp).unwrap_or_else(|| end.saturating_add(1));
            if next > total_size {
                return Err(OxidriveError::drive(format!(
                    "invalid resumable range progress: next offset {next} > total size {total_size}"
                )));
            }
            if next <= offset {
                return Err(OxidriveError::drive(format!(
                    "invalid resumable range progress: non-increasing offset {next} (previous {offset})"
                )));
            }
            offset = next;
            on_progress(ResumableUploadState {
                session_url: session_url.to_string(),
                next_offset: offset,
                file_size: total_size,
            })?;
            file.seek(std::io::SeekFrom::Start(offset))
                .await
                .map_err(|e| {
                    OxidriveError::drive(format!(
                        "seek {} to resumable offset {offset}: {e}",
                        local_path.display()
                    ))
                })?;
            continue;
        }
        return Err(drive_error_from_response("resumable upload:", resp).await);
    }
}

async fn upload_file_multipart(
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

async fn update_file_media(
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

async fn upload_with_conversion_multipart(
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

/// Creates a new binary file under `parent_id` and returns its Drive id.
#[allow(dead_code)]
pub async fn upload_file(
    client: &DriveClient,
    local_path: &Path,
    parent_id: &str,
    name: &str,
) -> Result<String, OxidriveError> {
    upload_file_with_resume(client, local_path, parent_id, name, None, |_| Ok(())).await
}

/// Creates a new binary file and optionally resumes an existing Drive resumable session.
pub async fn upload_file_with_resume(
    client: &DriveClient,
    local_path: &Path,
    parent_id: &str,
    name: &str,
    resume_state: Option<ResumableUploadState>,
    mut on_progress: impl FnMut(ResumableUploadState) -> Result<(), OxidriveError>,
) -> Result<String, OxidriveError> {
    let size = local_file_size(local_path).await?;
    if size > RESUMABLE_UPLOAD_THRESHOLD_BYTES {
        let meta = json!({
            "name": name,
            "parents": [parent_id],
        });
        let valid_resume =
            resume_state.filter(|s| s.file_size == size && s.next_offset < s.file_size);
        let used_existing_session = valid_resume.is_some();
        let mut session = match valid_resume {
            Some(state) => state,
            None => {
                let init_url =
                    client.upload_api_url("/files?uploadType=resumable&supportsAllDrives=true");
                ResumableUploadState {
                    session_url: start_resumable_session(
                        client,
                        Method::POST,
                        init_url,
                        Some(meta),
                        size,
                    )
                    .await?,
                    next_offset: 0,
                    file_size: size,
                }
            }
        };
        on_progress(session.clone())?;
        let resp = match upload_resumable_bytes(
            client,
            &session.session_url,
            local_path,
            size,
            session.next_offset,
            &mut on_progress,
        )
        .await
        {
            Ok(resp) => resp,
            Err(_) if used_existing_session => {
                let retry_meta = json!({
                    "name": name,
                    "parents": [parent_id],
                });
                let retry_init_url =
                    client.upload_api_url("/files?uploadType=resumable&supportsAllDrives=true");
                session = ResumableUploadState {
                    session_url: start_resumable_session(
                        client,
                        Method::POST,
                        retry_init_url,
                        Some(retry_meta),
                        size,
                    )
                    .await?,
                    next_offset: 0,
                    file_size: size,
                };
                on_progress(session.clone())?;
                upload_resumable_bytes(
                    client,
                    &session.session_url,
                    local_path,
                    size,
                    session.next_offset,
                    &mut on_progress,
                )
                .await?
            }
            Err(e) => return Err(e),
        };
        let created: CreatedFile = resp
            .json()
            .await
            .map_err(|e| OxidriveError::drive(format!("parse upload response: {e}")))?;
        return Ok(created.id);
    }
    upload_file_multipart(client, local_path, parent_id, name).await
}

/// Replaces the media of an existing file id.
#[allow(dead_code)]
pub async fn update_file(
    client: &DriveClient,
    local_path: &Path,
    drive_id: &str,
) -> Result<(), OxidriveError> {
    update_file_with_resume(client, local_path, drive_id, None, |_| Ok(())).await
}

/// Replaces existing file media and optionally resumes a previous resumable session.
pub async fn update_file_with_resume(
    client: &DriveClient,
    local_path: &Path,
    drive_id: &str,
    resume_state: Option<ResumableUploadState>,
    mut on_progress: impl FnMut(ResumableUploadState) -> Result<(), OxidriveError>,
) -> Result<(), OxidriveError> {
    let size = local_file_size(local_path).await?;
    if size > RESUMABLE_UPLOAD_THRESHOLD_BYTES {
        let valid_resume =
            resume_state.filter(|s| s.file_size == size && s.next_offset < s.file_size);
        let used_existing_session = valid_resume.is_some();
        let mut session = match valid_resume {
            Some(state) => state,
            None => {
                let init_url = client.upload_api_url(&format!(
                    "/files/{drive_id}?uploadType=resumable&supportsAllDrives=true"
                ));
                ResumableUploadState {
                    session_url: start_resumable_session(
                        client,
                        Method::PATCH,
                        init_url,
                        None,
                        size,
                    )
                    .await?,
                    next_offset: 0,
                    file_size: size,
                }
            }
        };
        on_progress(session.clone())?;
        match upload_resumable_bytes(
            client,
            &session.session_url,
            local_path,
            size,
            session.next_offset,
            &mut on_progress,
        )
        .await
        {
            Ok(_) => {}
            Err(_) if used_existing_session => {
                let retry_init_url = client.upload_api_url(&format!(
                    "/files/{drive_id}?uploadType=resumable&supportsAllDrives=true"
                ));
                session = ResumableUploadState {
                    session_url: start_resumable_session(
                        client,
                        Method::PATCH,
                        retry_init_url,
                        None,
                        size,
                    )
                    .await?,
                    next_offset: 0,
                    file_size: size,
                };
                on_progress(session.clone())?;
                let _ = upload_resumable_bytes(
                    client,
                    &session.session_url,
                    local_path,
                    size,
                    session.next_offset,
                    &mut on_progress,
                )
                .await?;
            }
            Err(e) => return Err(e),
        }
        return Ok(());
    }
    update_file_media(client, local_path, drive_id).await?;
    Ok(())
}

/// Uploads local bytes and sets the Google Apps `mimeType` metadata (conversion).
#[allow(dead_code)]
pub async fn upload_with_conversion(
    client: &DriveClient,
    local_path: &Path,
    drive_id: &str,
    google_mime: &str,
) -> Result<(), OxidriveError> {
    upload_with_conversion_with_resume(client, local_path, drive_id, google_mime, None, |_| Ok(()))
        .await
}

/// Uploads local bytes with Workspace conversion metadata and resumable session support.
pub async fn upload_with_conversion_with_resume(
    client: &DriveClient,
    local_path: &Path,
    drive_id: &str,
    google_mime: &str,
    resume_state: Option<ResumableUploadState>,
    mut on_progress: impl FnMut(ResumableUploadState) -> Result<(), OxidriveError>,
) -> Result<(), OxidriveError> {
    let size = local_file_size(local_path).await?;
    if size > RESUMABLE_UPLOAD_THRESHOLD_BYTES {
        let meta = json!({
            "mimeType": google_mime,
        });
        let valid_resume =
            resume_state.filter(|s| s.file_size == size && s.next_offset < s.file_size);
        let used_existing_session = valid_resume.is_some();
        let mut session = match valid_resume {
            Some(state) => state,
            None => {
                let init_url = client.upload_api_url(&format!(
                    "/files/{drive_id}?uploadType=resumable&supportsAllDrives=true"
                ));
                ResumableUploadState {
                    session_url: start_resumable_session(
                        client,
                        Method::PATCH,
                        init_url,
                        Some(meta),
                        size,
                    )
                    .await?,
                    next_offset: 0,
                    file_size: size,
                }
            }
        };
        on_progress(session.clone())?;
        match upload_resumable_bytes(
            client,
            &session.session_url,
            local_path,
            size,
            session.next_offset,
            &mut on_progress,
        )
        .await
        {
            Ok(_) => {}
            Err(_) if used_existing_session => {
                let retry_meta = json!({
                    "mimeType": google_mime,
                });
                let retry_init_url = client.upload_api_url(&format!(
                    "/files/{drive_id}?uploadType=resumable&supportsAllDrives=true"
                ));
                session = ResumableUploadState {
                    session_url: start_resumable_session(
                        client,
                        Method::PATCH,
                        retry_init_url,
                        Some(retry_meta),
                        size,
                    )
                    .await?,
                    next_offset: 0,
                    file_size: size,
                };
                on_progress(session.clone())?;
                let _ = upload_resumable_bytes(
                    client,
                    &session.session_url,
                    local_path,
                    size,
                    session.next_offset,
                    &mut on_progress,
                )
                .await?;
            }
            Err(e) => return Err(e),
        }
        return Ok(());
    }
    upload_with_conversion_multipart(client, local_path, drive_id, google_mime).await?;
    Ok(())
}
