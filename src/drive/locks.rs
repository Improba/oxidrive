//! Advisory lease helpers stored in Drive appProperties (`ox_lease`).
//!
//! Lease operations are best effort. Drive appProperties updates are not atomic across clients,
//! so a check-then-write race (TOCTOU) remains possible.

use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use reqwest::Method;
use serde_json::json;

use crate::drive::client::DriveClient;
use crate::drive::upload::{get_file_metadata, update_app_properties};
use crate::error::OxidriveError;
use crate::types::Lease;

const LEASE_PROPERTY_KEY: &str = "ox_lease";
#[allow(dead_code)]
const FILE_METADATA_FIELDS: &str =
    "id,name,mimeType,md5Checksum,modifiedTime,size,headRevisionId,version,appProperties,parents,trashed,owners";

/// Parses `ox_lease` from appProperties (`"{device};{expires_at_rfc3339}"`).
#[must_use]
pub fn parse_lease(props: &BTreeMap<String, String>) -> Option<Lease> {
    let raw = props.get(LEASE_PROPERTY_KEY)?;
    let (owner_device, expires_at_raw) = raw.split_once(';')?;
    if owner_device.trim().is_empty() {
        return None;
    }
    let expires_at = DateTime::parse_from_rfc3339(expires_at_raw)
        .ok()?
        .with_timezone(&Utc);
    Some(Lease {
        drive_file_id: String::new(),
        owner_device: owner_device.to_string(),
        expires_at,
    })
}

/// Returns true when `lease` has not yet expired at `now`.
#[must_use]
pub fn lease_is_active(lease: &Lease, now: DateTime<Utc>) -> bool {
    lease.expires_at > now
}

/// Writes `ox_lease` for `drive_id` and returns the parsed lease from refreshed metadata.
#[allow(dead_code)]
pub async fn acquire_lease(
    client: &DriveClient,
    drive_id: &str,
    device: &str,
    ttl: Duration,
    now: DateTime<Utc>,
) -> Result<Lease, OxidriveError> {
    if device.trim().is_empty() {
        return Err(OxidriveError::sync(
            "cannot acquire lease with empty device id",
        ));
    }
    let expires_at = now + ttl;
    let mut props = get_file_metadata(client, drive_id).await?.app_properties;
    props.insert(
        LEASE_PROPERTY_KEY.to_string(),
        format!("{device};{}", expires_at.to_rfc3339()),
    );
    let refreshed = update_app_properties(client, drive_id, &props).await?;
    let mut lease = parse_lease(&refreshed.app_properties).ok_or_else(|| {
        OxidriveError::sync(format!(
            "lease write acknowledged but ox_lease missing for drive id '{drive_id}'"
        ))
    })?;
    lease.drive_file_id = refreshed.id;
    Ok(lease)
}

/// Removes `ox_lease` from `drive_id` appProperties.
#[allow(dead_code)]
pub async fn release_lease(client: &DriveClient, drive_id: &str) -> Result<(), OxidriveError> {
    let mut url = reqwest::Url::parse(&client.drive_api_url(&format!("/files/{drive_id}")))
        .map_err(|e| OxidriveError::drive(format!("bad lease release URL: {e}")))?;
    {
        let mut qp = url.query_pairs_mut();
        qp.append_pair("fields", FILE_METADATA_FIELDS);
        qp.append_pair("supportsAllDrives", "true");
    }
    let payload = Arc::new(json!({
        "appProperties": {
            LEASE_PROPERTY_KEY: serde_json::Value::Null
        }
    }));
    let _ = client
        .request(Method::PATCH, url.as_str(), move |b| {
            b.json(payload.as_ref())
        })
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{lease_is_active, parse_lease};
    use chrono::{TimeZone, Utc};
    use std::collections::BTreeMap;

    #[test]
    fn parse_lease_accepts_expected_format() {
        let mut props = BTreeMap::new();
        props.insert(
            "ox_lease".to_string(),
            "device-a;2026-06-05T10:00:00Z".to_string(),
        );
        let lease = parse_lease(&props).expect("lease should parse");
        assert_eq!(lease.owner_device, "device-a");
        assert_eq!(
            lease.expires_at,
            Utc.with_ymd_and_hms(2026, 6, 5, 10, 0, 0)
                .single()
                .expect("valid timestamp")
        );
        assert!(lease.drive_file_id.is_empty());
    }

    #[test]
    fn parse_lease_rejects_invalid_values() {
        let mut props = BTreeMap::new();
        props.insert("ox_lease".to_string(), "missing-separator".to_string());
        assert!(parse_lease(&props).is_none());

        props.insert("ox_lease".to_string(), "device-a;not-a-date".to_string());
        assert!(parse_lease(&props).is_none());
    }

    #[test]
    fn lease_activity_checks_expiration() {
        let lease = crate::types::Lease {
            drive_file_id: "file-1".to_string(),
            owner_device: "device-a".to_string(),
            expires_at: Utc
                .with_ymd_and_hms(2026, 6, 5, 10, 0, 0)
                .single()
                .expect("valid timestamp"),
        };
        assert!(lease_is_active(
            &lease,
            Utc.with_ymd_and_hms(2026, 6, 5, 9, 59, 59)
                .single()
                .expect("valid timestamp")
        ));
        assert!(!lease_is_active(
            &lease,
            Utc.with_ymd_and_hms(2026, 6, 5, 10, 0, 0)
                .single()
                .expect("valid timestamp")
        ));
    }
}
