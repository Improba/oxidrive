use std::collections::HashSet;

use chrono::{DateTime, Utc};
use oxidrive::config::{Config, ConflictPolicy};
use oxidrive::drive::DriveClient;
use oxidrive::store::{RedbStore, Store};
use oxidrive::sync::engine::run_sync;
use oxidrive::types::{RelativePath, SyncRecord};
use tempfile::{NamedTempFile, TempDir};
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn test_config(sync_dir: &TempDir) -> Config {
    Config {
        sync_dir: sync_dir.path().to_path_buf(),
        drive_folder_id: Some("root-folder".to_string()),
        conflict_policy: ConflictPolicy::LocalWins,
        max_concurrent_uploads: 2,
        max_concurrent_downloads: 2,
        ..Config::default()
    }
}

fn setup_store(sync_dir: &TempDir) -> (Store, RedbStore) {
    let store = Store::open(sync_dir.path()).expect("open in-memory store");
    let db_file = NamedTempFile::new().expect("create temp redb file");
    let redb = RedbStore::open(db_file.path()).expect("open redb");
    (store, redb)
}

async fn mock_start_page_token(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/drive/v3/changes/startPageToken"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "startPageToken": "start-token-1"
        })))
        .expect(1)
        .mount(server)
        .await;
}

async fn mock_list(server: &MockServer, files: serde_json::Value) {
    Mock::given(method("GET"))
        .and(path("/drive/v3/files"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "files": files
        })))
        .expect(1)
        .mount(server)
        .await;
}

async fn mock_upload_create(server: &MockServer, expected_calls: u64) {
    Mock::given(method("POST"))
        .and(path("/upload/drive/v3/files"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "new-file-id"
        })))
        .expect(expected_calls)
        .mount(server)
        .await;
}

async fn mock_create_folder(server: &MockServer, expected_calls: u64) {
    Mock::given(method("POST"))
        .and(path("/drive/v3/files"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "folder-id"
        })))
        .expect(expected_calls)
        .mount(server)
        .await;
}

async fn mock_upload_update(server: &MockServer, expected_calls: u64) {
    Mock::given(method("PATCH"))
        .and(path_regex(r"^/upload/drive/v3/files/[^/]+$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "updated-file-id"
        })))
        .expect(expected_calls)
        .mount(server)
        .await;
}

async fn mock_download_media(server: &MockServer, expected_calls: u64, body: &'static str) {
    Mock::given(method("GET"))
        .and(path_regex(r"^/drive/v3/files/[^/]+$"))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .expect(expected_calls)
        .mount(server)
        .await;
}

#[tokio::test]
async fn first_sync_uploads_new_local_files() {
    let server = MockServer::start().await;
    mock_list(&server, serde_json::json!([])).await;
    mock_start_page_token(&server).await;
    mock_upload_create(&server, 2).await;
    mock_create_folder(&server, 0).await;
    mock_upload_update(&server, 0).await;
    mock_download_media(&server, 0, "").await;

    let sync_dir = tempfile::tempdir().expect("create sync tempdir");
    tokio::fs::write(sync_dir.path().join("a.txt"), b"hello")
        .await
        .expect("write a.txt");
    tokio::fs::write(sync_dir.path().join("b.txt"), b"world")
        .await
        .expect("write b.txt");

    let client = DriveClient::with_base_url("test-token".to_string(), server.uri());
    let (store, redb) = setup_store(&sync_dir);
    let config = test_config(&sync_dir);

    let report = run_sync(&config, &client, &store, &redb)
        .await
        .expect("run sync");

    let uploaded: HashSet<_> = report.uploaded.into_iter().collect();
    assert_eq!(
        uploaded,
        HashSet::from([RelativePath::from("a.txt"), RelativePath::from("b.txt")])
    );
    assert!(report.downloaded.is_empty());
}

#[tokio::test]
async fn first_sync_downloads_new_remote_files() {
    let server = MockServer::start().await;
    mock_list(
        &server,
        serde_json::json!([
            {
                "id": "remote-a",
                "name": "remote-a.txt",
                "mimeType": "text/plain",
                "md5Checksum": "2c1743a391305fbf367df8e4f069f9f9",
                "modifiedTime": "2024-01-01T00:00:00Z",
                "size": "5",
                "parents": ["root-folder"],
                "trashed": false
            },
            {
                "id": "remote-b",
                "name": "remote-b.txt",
                "mimeType": "text/plain",
                "md5Checksum": "987bcab01b929eb2c07877b224215c92",
                "modifiedTime": "2024-01-01T00:00:01Z",
                "size": "4",
                "parents": ["root-folder"],
                "trashed": false
            }
        ]),
    )
    .await;
    mock_start_page_token(&server).await;
    mock_download_media(&server, 2, "downloaded-content").await;
    mock_upload_create(&server, 0).await;
    mock_create_folder(&server, 0).await;
    mock_upload_update(&server, 0).await;

    let sync_dir = tempfile::tempdir().expect("create sync tempdir");
    let client = DriveClient::with_base_url("test-token".to_string(), server.uri());
    let (store, redb) = setup_store(&sync_dir);
    let config = test_config(&sync_dir);

    let report = run_sync(&config, &client, &store, &redb)
        .await
        .expect("run sync");

    let downloaded: HashSet<_> = report.downloaded.into_iter().collect();
    assert_eq!(
        downloaded,
        HashSet::from([
            RelativePath::from("remote-a.txt"),
            RelativePath::from("remote-b.txt")
        ])
    );
    assert!(report.uploaded.is_empty());
    assert!(sync_dir.path().join("remote-a.txt").exists());
    assert!(sync_dir.path().join("remote-b.txt").exists());
}

#[tokio::test]
async fn unchanged_files_are_skipped() {
    let server = MockServer::start().await;
    mock_list(
        &server,
        serde_json::json!([
            {
                "id": "same-id",
                "name": "same.txt",
                "mimeType": "text/plain",
                "md5Checksum": "5d41402abc4b2a76b9719d911017c592",
                "modifiedTime": "2024-02-01T00:00:00Z",
                "size": "5",
                "parents": ["root-folder"],
                "trashed": false
            }
        ]),
    )
    .await;
    mock_start_page_token(&server).await;
    mock_download_media(&server, 0, "").await;
    mock_upload_create(&server, 0).await;
    mock_create_folder(&server, 0).await;
    mock_upload_update(&server, 0).await;

    let sync_dir = tempfile::tempdir().expect("create sync tempdir");
    let local_path = sync_dir.path().join("same.txt");
    tokio::fs::write(&local_path, b"hello")
        .await
        .expect("write local file");

    let local_md5 = oxidrive::utils::hash::compute_md5(&local_path)
        .await
        .expect("compute local md5");
    let local_meta = tokio::fs::metadata(&local_path)
        .await
        .expect("stat local file");
    let local_mtime: DateTime<Utc> = local_meta
        .modified()
        .expect("read local modified time")
        .into();

    let (store, redb) = setup_store(&sync_dir);
    store
        .upsert(
            RelativePath::from("same.txt"),
            SyncRecord {
                drive_file_id: Some("same-id".to_string()),
                remote_md5: Some(local_md5.clone()),
                remote_modified_at: Some(
                    DateTime::parse_from_rfc3339("2024-02-01T00:00:00Z")
                        .expect("parse remote mtime")
                        .with_timezone(&Utc),
                ),
                local_md5,
                local_mtime,
                local_size: local_meta.len(),
                last_synced_at: Utc::now(),
            },
        )
        .expect("seed session metadata");
    store
        .persist_to_redb(&redb)
        .expect("persist seeded metadata");

    let client = DriveClient::with_base_url("test-token".to_string(), server.uri());
    let config = test_config(&sync_dir);
    let report = run_sync(&config, &client, &store, &redb)
        .await
        .expect("run sync");

    assert_eq!(report.skipped, 1);
    assert!(report.uploaded.is_empty());
    assert!(report.downloaded.is_empty());
}

#[tokio::test]
async fn local_modification_triggers_upload() {
    let server = MockServer::start().await;
    let sync_dir = tempfile::tempdir().expect("create sync tempdir");
    let local_path = sync_dir.path().join("modified.txt");

    tokio::fs::write(&local_path, b"original")
        .await
        .expect("write original file");
    let original_md5 = oxidrive::utils::hash::compute_md5(&local_path)
        .await
        .expect("compute original md5");
    let original_meta = tokio::fs::metadata(&local_path)
        .await
        .expect("stat original file");
    let original_mtime: DateTime<Utc> = original_meta
        .modified()
        .expect("read original mtime")
        .into();

    let (store, redb) = setup_store(&sync_dir);
    store
        .upsert(
            RelativePath::from("modified.txt"),
            SyncRecord {
                drive_file_id: Some("modified-drive-id".to_string()),
                remote_md5: Some(original_md5.clone()),
                remote_modified_at: Some(
                    DateTime::parse_from_rfc3339("2024-03-01T00:00:00Z")
                        .expect("parse remote mtime")
                        .with_timezone(&Utc),
                ),
                local_md5: original_md5.clone(),
                local_mtime: original_mtime,
                local_size: original_meta.len(),
                last_synced_at: Utc::now(),
            },
        )
        .expect("seed metadata");
    store.persist_to_redb(&redb).expect("persist metadata");

    mock_list(
        &server,
        serde_json::json!([{
            "id": "modified-drive-id",
            "name": "modified.txt",
            "mimeType": "text/plain",
            "md5Checksum": original_md5,
            "modifiedTime": "2024-03-01T00:00:00Z",
            "size": "8",
            "parents": ["root-folder"],
            "trashed": false
        }]),
    )
    .await;
    mock_start_page_token(&server).await;
    mock_upload_create(&server, 0).await;
    mock_create_folder(&server, 0).await;
    mock_upload_update(&server, 1).await;
    mock_download_media(&server, 0, "").await;

    tokio::fs::write(&local_path, b"updated content")
        .await
        .expect("overwrite local file");

    let client = DriveClient::with_base_url("test-token".to_string(), server.uri());
    let config = test_config(&sync_dir);
    let report = run_sync(&config, &client, &store, &redb)
        .await
        .expect("run sync");

    assert_eq!(report.uploaded, vec![RelativePath::from("modified.txt")]);
    assert!(report.downloaded.is_empty());
}

#[tokio::test]
async fn remote_modification_triggers_download() {
    let server = MockServer::start().await;
    let sync_dir = tempfile::tempdir().expect("create sync tempdir");
    let local_path = sync_dir.path().join("cloud.txt");

    tokio::fs::write(&local_path, b"still-local")
        .await
        .expect("write local file");
    let local_md5 = oxidrive::utils::hash::compute_md5(&local_path)
        .await
        .expect("compute local md5");
    let local_meta = tokio::fs::metadata(&local_path)
        .await
        .expect("stat local file");
    let local_mtime: DateTime<Utc> = local_meta.modified().expect("read local mtime").into();

    let (store, redb) = setup_store(&sync_dir);
    store
        .upsert(
            RelativePath::from("cloud.txt"),
            SyncRecord {
                drive_file_id: Some("cloud-drive-id".to_string()),
                remote_md5: Some("old-remote-md5".to_string()),
                remote_modified_at: Some(
                    DateTime::parse_from_rfc3339("2024-04-01T00:00:00Z")
                        .expect("parse remote mtime")
                        .with_timezone(&Utc),
                ),
                local_md5,
                local_mtime,
                local_size: local_meta.len(),
                last_synced_at: Utc::now(),
            },
        )
        .expect("seed metadata");
    store.persist_to_redb(&redb).expect("persist metadata");

    mock_list(
        &server,
        serde_json::json!([{
            "id": "cloud-drive-id",
            "name": "cloud.txt",
            "mimeType": "text/plain",
            "md5Checksum": "new-remote-md5",
            "modifiedTime": "2024-04-01T00:10:00Z",
            "size": "10",
            "parents": ["root-folder"],
            "trashed": false
        }]),
    )
    .await;
    mock_start_page_token(&server).await;
    mock_upload_create(&server, 0).await;
    mock_create_folder(&server, 0).await;
    mock_upload_update(&server, 0).await;
    mock_download_media(&server, 1, "fresh cloud content").await;

    let client = DriveClient::with_base_url("test-token".to_string(), server.uri());
    let config = test_config(&sync_dir);
    let report = run_sync(&config, &client, &store, &redb)
        .await
        .expect("run sync");

    assert_eq!(report.downloaded, vec![RelativePath::from("cloud.txt")]);
    assert!(report.uploaded.is_empty());
}

#[tokio::test]
async fn conflict_detected_when_both_change() {
    let server = MockServer::start().await;
    let sync_dir = tempfile::tempdir().expect("create sync tempdir");
    let local_path = sync_dir.path().join("clash.txt");
    let old_seed_path = sync_dir.path().join(".seed-old-clash.txt");

    tokio::fs::write(&old_seed_path, b"seed-local-old")
        .await
        .expect("write old seed file");
    let stored_local_md5 = oxidrive::utils::hash::compute_md5(&old_seed_path)
        .await
        .expect("compute stored local md5");
    tokio::fs::remove_file(&old_seed_path)
        .await
        .expect("cleanup old seed file");

    tokio::fs::write(&local_path, b"seed-local-new")
        .await
        .expect("write changed local file");
    let local_meta = tokio::fs::metadata(&local_path)
        .await
        .expect("stat local file");
    let local_mtime: DateTime<Utc> = local_meta.modified().expect("read local mtime").into();

    let (store, redb) = setup_store(&sync_dir);
    store
        .upsert(
            RelativePath::from("clash.txt"),
            SyncRecord {
                drive_file_id: Some("clash-drive-id".to_string()),
                remote_md5: Some("seed-remote-old-md5".to_string()),
                remote_modified_at: Some(
                    DateTime::parse_from_rfc3339("2024-05-01T00:00:00Z")
                        .expect("parse remote mtime")
                        .with_timezone(&Utc),
                ),
                local_md5: stored_local_md5,
                local_mtime,
                local_size: local_meta.len(),
                last_synced_at: Utc::now(),
            },
        )
        .expect("seed metadata");
    store.persist_to_redb(&redb).expect("persist metadata");

    mock_list(
        &server,
        serde_json::json!([{
            "id": "clash-drive-id",
            "name": "clash.txt",
            "mimeType": "text/plain",
            "md5Checksum": "seed-remote-new-md5",
            "modifiedTime": "2024-05-01T00:10:00Z",
            "size": "14",
            "parents": ["root-folder"],
            "trashed": false
        }]),
    )
    .await;
    mock_start_page_token(&server).await;
    mock_upload_create(&server, 0).await;
    mock_create_folder(&server, 0).await;
    mock_upload_update(&server, 1).await;
    mock_download_media(&server, 0, "").await;

    let client = DriveClient::with_base_url("test-token".to_string(), server.uri());
    let mut config = test_config(&sync_dir);
    config.conflict_policy = ConflictPolicy::LocalWins;
    let report = run_sync(&config, &client, &store, &redb)
        .await
        .expect("run sync");

    assert!(report.conflicts.contains(&RelativePath::from("clash.txt")));
    assert!(report.uploaded.contains(&RelativePath::from("clash.txt")));
}

#[tokio::test]
async fn deletion_propagated_when_remote_gone() {
    let server = MockServer::start().await;
    let sync_dir = tempfile::tempdir().expect("create sync tempdir");
    let local_path = sync_dir.path().join("vanished.txt");

    tokio::fs::write(&local_path, b"still here")
        .await
        .expect("write local file");
    let local_md5 = oxidrive::utils::hash::compute_md5(&local_path)
        .await
        .expect("compute local md5");
    let local_meta = tokio::fs::metadata(&local_path)
        .await
        .expect("stat local file");
    let local_mtime: DateTime<Utc> = local_meta.modified().expect("read local mtime").into();

    let (store, redb) = setup_store(&sync_dir);
    store
        .upsert(
            RelativePath::from("vanished.txt"),
            SyncRecord {
                drive_file_id: Some("vanished-drive-id".to_string()),
                remote_md5: Some("known-remote-md5".to_string()),
                remote_modified_at: Some(
                    DateTime::parse_from_rfc3339("2024-06-01T00:00:00Z")
                        .expect("parse remote mtime")
                        .with_timezone(&Utc),
                ),
                local_md5,
                local_mtime,
                local_size: local_meta.len(),
                last_synced_at: Utc::now(),
            },
        )
        .expect("seed metadata");
    store.persist_to_redb(&redb).expect("persist metadata");

    mock_list(&server, serde_json::json!([])).await;
    mock_start_page_token(&server).await;
    mock_upload_create(&server, 0).await;
    mock_create_folder(&server, 0).await;
    mock_upload_update(&server, 0).await;
    mock_download_media(&server, 0, "").await;

    let client = DriveClient::with_base_url("test-token".to_string(), server.uri());
    let config = test_config(&sync_dir);
    let report = run_sync(&config, &client, &store, &redb)
        .await
        .expect("run sync");

    assert!(report
        .deleted_local
        .contains(&RelativePath::from("vanished.txt")));
    assert!(!local_path.exists());
}

#[tokio::test]
async fn incremental_sync_uses_changes_api() {
    let server = MockServer::start().await;
    let sync_dir = tempfile::tempdir().expect("create sync tempdir");
    let local_path = sync_dir.path().join("known.txt");

    tokio::fs::write(&local_path, b"known content")
        .await
        .expect("write known.txt");
    let known_md5 = oxidrive::utils::hash::compute_md5(&local_path)
        .await
        .expect("compute known md5");
    let known_meta = tokio::fs::metadata(&local_path)
        .await
        .expect("stat known.txt");
    let known_mtime: DateTime<Utc> = known_meta.modified().expect("read known mtime").into();

    let (store, redb) = setup_store(&sync_dir);
    store
        .upsert(
            RelativePath::from("known.txt"),
            SyncRecord {
                drive_file_id: Some("known-drive-id".to_string()),
                remote_md5: Some(known_md5.clone()),
                remote_modified_at: Some(
                    DateTime::parse_from_rfc3339("2024-06-01T00:00:00Z")
                        .expect("parse remote mtime")
                        .with_timezone(&Utc),
                ),
                local_md5: known_md5,
                local_mtime: known_mtime,
                local_size: known_meta.len(),
                last_synced_at: Utc::now(),
            },
        )
        .expect("seed known.txt metadata");
    store.persist_to_redb(&redb).expect("persist metadata");
    redb.set_page_token("saved-page-token")
        .await
        .expect("set page token");

    Mock::given(method("GET"))
        .and(path("/drive/v3/changes"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "changes": [{
                "fileId": "change-file-1",
                "removed": false,
                "time": "2024-07-01T00:00:00Z",
                "file": {
                    "id": "change-file-1",
                    "name": "added-via-changes.txt",
                    "mimeType": "text/plain",
                    "md5Checksum": "abc123",
                    "modifiedTime": "2024-07-01T00:00:00Z",
                    "size": "11",
                    "parents": ["root-folder"],
                    "trashed": false
                }
            }],
            "newStartPageToken": "next-page-token"
        })))
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/drive/v3/files"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "files": []
        })))
        .expect(0)
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/drive/v3/changes/startPageToken"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "startPageToken": "unused"
        })))
        .expect(0)
        .mount(&server)
        .await;

    mock_download_media(&server, 1, "hello world").await;
    mock_upload_create(&server, 0).await;
    mock_create_folder(&server, 0).await;
    mock_upload_update(&server, 0).await;

    let client = DriveClient::with_base_url("test-token".to_string(), server.uri());
    let config = test_config(&sync_dir);

    let report = run_sync(&config, &client, &store, &redb)
        .await
        .expect("run sync");

    assert!(report
        .downloaded
        .contains(&RelativePath::from("added-via-changes.txt")));
    assert!(sync_dir.path().join("added-via-changes.txt").exists());
    assert!(report.uploaded.is_empty());
}

#[tokio::test]
async fn nested_folder_structure_uploaded_correctly() {
    let server = MockServer::start().await;
    mock_list(&server, serde_json::json!([])).await;
    mock_start_page_token(&server).await;
    mock_create_folder(&server, 1).await;
    mock_upload_create(&server, 1).await;
    mock_upload_update(&server, 0).await;
    mock_download_media(&server, 0, "").await;

    let sync_dir = tempfile::tempdir().expect("create sync tempdir");
    tokio::fs::create_dir_all(sync_dir.path().join("subdir"))
        .await
        .expect("create subdir");
    tokio::fs::write(sync_dir.path().join("subdir/nested.txt"), b"nested content")
        .await
        .expect("write nested file");

    let client = DriveClient::with_base_url("test-token".to_string(), server.uri());
    let (store, redb) = setup_store(&sync_dir);
    let config = test_config(&sync_dir);

    let report = run_sync(&config, &client, &store, &redb)
        .await
        .expect("run sync");

    assert!(report
        .uploaded
        .contains(&RelativePath::from("subdir/nested.txt")));
}
