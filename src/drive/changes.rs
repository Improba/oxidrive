//! Drive `changes` API helpers.

use serde::Deserialize;

use crate::drive::client::DriveClient;
use crate::drive::types::DriveChange;
use crate::error::OxidriveError;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StartPageTokenBody {
    start_page_token: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChangesPage {
    #[serde(default)]
    changes: Vec<DriveChange>,
    #[serde(default)]
    next_page_token: Option<String>,
    #[serde(default)]
    new_start_page_token: Option<String>,
}

/// Returns the starting page token for a full remote changelog scan.
pub async fn get_start_page_token(client: &DriveClient) -> Result<String, OxidriveError> {
    let url = client.drive_api_url("/changes/startPageToken?supportsAllDrives=true");
    let body: StartPageTokenBody = client
        .request(reqwest::Method::GET, &url, |b| b)
        .await?
        .json()
        .await
        .map_err(|e| OxidriveError::drive(format!("parse startPageToken: {e}")))?;
    Ok(body.start_page_token)
}

/// Fetches one logical page of changes, following `nextPageToken` until exhausted.
///
/// The returned string is the token to persist for the next incremental sync (`newStartPageToken`
/// from the final page when present, otherwise the last `nextPageToken` seen).
pub async fn fetch_changes(
    client: &DriveClient,
    page_token: &str,
) -> Result<(Vec<DriveChange>, String), OxidriveError> {
    let mut collected = Vec::new();
    let mut token = page_token.to_string();
    let mut latest_new_start: Option<String> = None;

    loop {
        let mut url = reqwest::Url::parse(&client.drive_api_url("/changes"))
            .map_err(|e| OxidriveError::drive(format!("bad changes URL: {e}")))?;
        {
            let mut qp = url.query_pairs_mut();
            qp.append_pair("pageToken", &token);
            qp.append_pair(
                "fields",
                "nextPageToken, newStartPageToken, changes(fileId, removed, time, file(id, name, mimeType, md5Checksum, modifiedTime, size, parents, trashed))",
            );
            qp.append_pair("supportsAllDrives", "true");
            qp.append_pair("includeItemsFromAllDrives", "true");
        }

        let page: ChangesPage = client
            .request(reqwest::Method::GET, url.as_str(), |b| b)
            .await?
            .json()
            .await
            .map_err(|e| OxidriveError::drive(format!("parse changes: {e}")))?;

        collected.extend(page.changes);
        if let Some(n) = page.new_start_page_token.clone() {
            latest_new_start = Some(n);
        }

        match page.next_page_token {
            Some(next) => token = next,
            None => {
                let out_token = latest_new_start
                    .or(page.new_start_page_token)
                    .ok_or_else(|| {
                        OxidriveError::drive("changes response missing newStartPageToken")
                    })?;
                return Ok((collected, out_token));
            }
        }
    }
}
