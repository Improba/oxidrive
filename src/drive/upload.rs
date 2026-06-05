//! Multipart uploads and media updates for Drive.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use reqwest::header::{CONTENT_LENGTH, CONTENT_RANGE, LOCATION};
use reqwest::Method;
use serde::Deserialize;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use crate::drive::client::DriveClient;
use crate::drive::types::{remote_content_fingerprint, DriveFile};
use crate::error::OxidriveError;

pub const RESUMABLE_UPLOAD_THRESHOLD_BYTES: u64 = 32 * 1024 * 1024;
const RESUMABLE_CHUNK_BYTES: usize = 8 * 1024 * 1024;
const FILE_METADATA_FIELDS_PREFLIGHT: &str =
    "id,name,mimeType,md5Checksum,modifiedTime,size,headRevisionId,version,appProperties,parents,trashed";
const FILE_METADATA_FIELDS: &str =
    "id,name,mimeType,md5Checksum,modifiedTime,size,headRevisionId,version,appProperties,parents,trashed,owners";

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

/// Expected remote values used by guarded uploads before mutating an existing file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RevisionGuard {
    /// Expected Drive `headRevisionId` from the last successful sync.
    pub head_revision_id: Option<String>,
    /// Expected Drive `version` from the last successful sync.
    pub version: Option<i64>,
    /// Expected remote fingerprint (`md5Checksum` or `mtime:*` fallback).
    pub remote_fingerprint: Option<String>,
    /// Expected Drive `modifiedTime` from the last successful sync.
    pub modified_time: Option<DateTime<Utc>>,
}

impl RevisionGuard {
    /// Builds a guard from head revision / version expectations only.
    #[must_use]
    pub fn from_expected(
        expected_head_revision_id: Option<&str>,
        expected_version: Option<i64>,
    ) -> Self {
        Self {
            head_revision_id: expected_head_revision_id.map(str::to_string),
            version: expected_version,
            remote_fingerprint: None,
            modified_time: None,
        }
    }
}

/// Outcome of an optimistic, preflight-guarded update attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuardedUpdate {
    /// Update proceeded and returned fresh metadata after mutation.
    Updated { remote: DriveFile },
    /// Preflight metadata diverged from expected values; no upload was issued.
    RevisionMismatch { remote: DriveFile },
}

fn revision_guard_matches(remote: &DriveFile, expected: &RevisionGuard) -> bool {
    if let Some(head_revision_id) = expected.head_revision_id.as_ref() {
        return remote.head_revision_id.as_deref() == Some(head_revision_id.as_str());
    }
    if let Some(version) = expected.version {
        return remote.version == Some(version);
    }
    if let Some(remote_fingerprint) = expected.remote_fingerprint.as_ref() {
        return remote_content_fingerprint(remote) == *remote_fingerprint;
    }
    if let Some(modified_time) = expected.modified_time.as_ref() {
        return &remote.modified_time == modified_time;
    }
    true
}

/// Fetches the latest Drive metadata required by guarded update preflights.
pub async fn get_file_metadata(
    client: &DriveClient,
    drive_id: &str,
) -> Result<DriveFile, OxidriveError> {
    get_file_metadata_with_fields(client, drive_id, FILE_METADATA_FIELDS).await
}

/// Updates Drive app properties and returns refreshed metadata.
pub async fn update_app_properties(
    client: &DriveClient,
    drive_id: &str,
    props: &BTreeMap<String, String>,
) -> Result<DriveFile, OxidriveError> {
    let mut url = reqwest::Url::parse(&client.drive_api_url(&format!("/files/{drive_id}")))
        .map_err(|e| OxidriveError::drive(format!("bad app-properties URL: {e}")))?;
    {
        let mut qp = url.query_pairs_mut();
        qp.append_pair("fields", FILE_METADATA_FIELDS);
        qp.append_pair("supportsAllDrives", "true");
    }
    let payload = Arc::new(json!({ "appProperties": props }));
    client
        .request(Method::PATCH, url.as_str(), move |b| {
            b.json(payload.as_ref())
        })
        .await?
        .json::<DriveFile>()
        .await
        .map_err(|e| OxidriveError::drive(format!("parse app-properties update response: {e}")))
}

async fn get_file_metadata_preflight(
    client: &DriveClient,
    drive_id: &str,
) -> Result<DriveFile, OxidriveError> {
    get_file_metadata_with_fields(client, drive_id, FILE_METADATA_FIELDS_PREFLIGHT).await
}

async fn get_file_metadata_with_fields(
    client: &DriveClient,
    drive_id: &str,
    fields: &str,
) -> Result<DriveFile, OxidriveError> {
    let mut url = reqwest::Url::parse(&client.drive_api_url(&format!("/files/{drive_id}")))
        .map_err(|e| OxidriveError::drive(format!("bad file metadata URL: {e}")))?;
    {
        let mut qp = url.query_pairs_mut();
        qp.append_pair("fields", fields);
        qp.append_pair("supportsAllDrives", "true");
    }
    client
        .request(Method::GET, url.as_str(), |b| b)
        .await?
        .json::<DriveFile>()
        .await
        .map_err(|e| OxidriveError::drive(format!("parse file metadata: {e}")))
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

fn create_file_metadata(
    parent_id: &str,
    name: &str,
    app_properties: Option<&BTreeMap<String, String>>,
) -> serde_json::Value {
    let mut metadata = serde_json::Map::new();
    metadata.insert(
        "name".to_string(),
        serde_json::Value::String(name.to_string()),
    );
    metadata.insert(
        "parents".to_string(),
        serde_json::Value::Array(vec![serde_json::Value::String(parent_id.to_string())]),
    );
    if let Some(props) = app_properties {
        metadata.insert("appProperties".to_string(), json!(props));
    }
    serde_json::Value::Object(metadata)
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
    app_properties: Option<&BTreeMap<String, String>>,
) -> Result<String, OxidriveError> {
    let data = read_upload_bytes(local_path).await?;
    let meta = create_file_metadata(parent_id, name, app_properties);
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

/// Replaces media bytes only when preflight metadata matches `expected`.
///
/// This check-then-act flow is best effort: Drive media uploads do not expose an `If-Match`
/// precondition, so a TOCTOU window remains between the preflight `GET` and the upload `PATCH`.
pub async fn update_file_media_guarded(
    client: &DriveClient,
    local_path: &Path,
    drive_id: &str,
    expected_head_revision_id: Option<&str>,
    expected_version: Option<i64>,
) -> Result<GuardedUpdate, OxidriveError> {
    let expected = RevisionGuard::from_expected(expected_head_revision_id, expected_version);
    let current_remote = get_file_metadata_preflight(client, drive_id).await?;
    if !revision_guard_matches(&current_remote, &expected) {
        return Ok(GuardedUpdate::RevisionMismatch {
            remote: current_remote,
        });
    }
    update_file_media(client, local_path, drive_id).await?;
    let updated_remote = get_file_metadata(client, drive_id).await?;
    Ok(GuardedUpdate::Updated {
        remote: updated_remote,
    })
}

/// Preflight-only revision check, used to guard uploads that cannot reuse
/// [`update_file_media_guarded`] (e.g. Google Workspace conversion uploads).
///
/// Returns `Ok(Some(remote))` when the current Drive metadata diverged from
/// `expected` (the caller should fall back to a conflict copy), or `Ok(None)`
/// when it is safe to proceed. Like all media guards this is best effort: a
/// TOCTOU window remains between this `GET` and the subsequent upload.
pub async fn preflight_revision_mismatch(
    client: &DriveClient,
    drive_id: &str,
    expected: &RevisionGuard,
) -> Result<Option<DriveFile>, OxidriveError> {
    let current_remote = get_file_metadata_preflight(client, drive_id).await?;
    if revision_guard_matches(&current_remote, expected) {
        Ok(None)
    } else {
        Ok(Some(current_remote))
    }
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
    upload_file_with_resume(client, local_path, parent_id, name, None, None, |_| Ok(())).await
}

/// Creates a new binary file and optionally resumes an existing Drive resumable session.
#[allow(clippy::too_many_arguments)]
pub async fn upload_file_with_resume(
    client: &DriveClient,
    local_path: &Path,
    parent_id: &str,
    name: &str,
    app_properties: Option<&BTreeMap<String, String>>,
    resume_state: Option<ResumableUploadState>,
    mut on_progress: impl FnMut(ResumableUploadState) -> Result<(), OxidriveError>,
) -> Result<String, OxidriveError> {
    let size = local_file_size(local_path).await?;
    if size > RESUMABLE_UPLOAD_THRESHOLD_BYTES {
        let meta = create_file_metadata(parent_id, name, app_properties);
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
                let retry_meta = create_file_metadata(parent_id, name, app_properties);
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
    upload_file_multipart(client, local_path, parent_id, name, app_properties).await
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

/// Resumable variant of [`update_file_media_guarded`] for existing binary files.
///
/// This check-then-act flow is best effort: Drive media uploads do not expose an `If-Match`
/// precondition, so a TOCTOU window remains between the preflight `GET` and the upload `PATCH`/PUT.
pub async fn update_file_with_resume_guarded(
    client: &DriveClient,
    local_path: &Path,
    drive_id: &str,
    expected: &RevisionGuard,
    resume_state: Option<ResumableUploadState>,
    on_progress: impl FnMut(ResumableUploadState) -> Result<(), OxidriveError>,
) -> Result<GuardedUpdate, OxidriveError> {
    let size = local_file_size(local_path).await?;
    if size <= RESUMABLE_UPLOAD_THRESHOLD_BYTES
        && expected.remote_fingerprint.is_none()
        && expected.modified_time.is_none()
    {
        return update_file_media_guarded(
            client,
            local_path,
            drive_id,
            expected.head_revision_id.as_deref(),
            expected.version,
        )
        .await;
    }
    let current_remote = get_file_metadata_preflight(client, drive_id).await?;
    if !revision_guard_matches(&current_remote, expected) {
        return Ok(GuardedUpdate::RevisionMismatch {
            remote: current_remote,
        });
    }
    update_file_with_resume(client, local_path, drive_id, resume_state, on_progress).await?;
    let updated_remote = get_file_metadata(client, drive_id).await?;
    Ok(GuardedUpdate::Updated {
        remote: updated_remote,
    })
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

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};

    use super::{revision_guard_matches, RevisionGuard};
    use crate::drive::types::DriveFile;

    fn sample_remote() -> DriveFile {
        DriveFile {
            id: "id-1".to_string(),
            name: "demo.txt".to_string(),
            mime_type: "text/plain".to_string(),
            md5_checksum: Some("abcd".to_string()),
            modified_time: Utc
                .with_ymd_and_hms(2024, 1, 2, 3, 4, 5)
                .single()
                .expect("valid timestamp"),
            size: Some(4),
            head_revision_id: Some("rev-1".to_string()),
            version: Some(8),
            app_properties: std::collections::BTreeMap::new(),
            parents: vec!["root".to_string()],
            trashed: false,
        }
    }

    #[test]
    fn revision_guard_prefers_head_revision() {
        let remote = sample_remote();
        let expected = RevisionGuard {
            head_revision_id: Some("rev-1".to_string()),
            version: Some(7),
            remote_fingerprint: Some("wrong".to_string()),
            modified_time: None,
        };
        assert!(revision_guard_matches(&remote, &expected));
    }

    #[test]
    fn revision_guard_falls_back_to_version_then_fingerprint() {
        let remote = sample_remote();
        let by_version = RevisionGuard {
            head_revision_id: None,
            version: Some(8),
            remote_fingerprint: Some("wrong".to_string()),
            modified_time: None,
        };
        assert!(revision_guard_matches(&remote, &by_version));

        let by_fingerprint = RevisionGuard {
            head_revision_id: None,
            version: None,
            remote_fingerprint: Some("abcd".to_string()),
            modified_time: None,
        };
        assert!(revision_guard_matches(&remote, &by_fingerprint));
    }

    #[test]
    fn revision_guard_uses_modified_time_last() {
        let remote = sample_remote();
        let expected = RevisionGuard {
            head_revision_id: None,
            version: None,
            remote_fingerprint: None,
            modified_time: Some(
                Utc.with_ymd_and_hms(2024, 1, 2, 3, 4, 5)
                    .single()
                    .expect("valid timestamp"),
            ),
        };
        assert!(revision_guard_matches(&remote, &expected));
    }

    #[test]
    fn revision_guard_detects_mismatch() {
        let remote = sample_remote();
        let expected = RevisionGuard {
            head_revision_id: Some("rev-other".to_string()),
            version: None,
            remote_fingerprint: None,
            modified_time: None,
        };
        assert!(!revision_guard_matches(&remote, &expected));
    }
}
