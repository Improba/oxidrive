use chrono::{DateTime, Utc};
use oxidrive::config::{Config, ConflictPolicy};
use oxidrive::drive::DriveClient;
use oxidrive::store::{RedbStore, Store};
use oxidrive::sync::engine::run_sync;
use oxidrive::types::{RelativePath, SyncRecord, WorkspaceConversion};
use oxidrive::utils::hash::compute_md5;
use serde_json::json;
use tempfile::{NamedTempFile, TempDir};
use wiremock::matchers::{method, path, path_regex, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const GOOGLE_DOC_MIME: &str = "application/vnd.google-apps.document";
const DOCX_EXPORT_MIME: &str = "application/vnd.openxmlformats-officedocument.wordprocessingml.document";
const GOOGLE_SHEET_MIME: &str = "application/vnd.google-apps.spreadsheet";
const XLSX_EXPORT_MIME: &str = "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet";

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
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "startPageToken": "start-token-1"
        })))
        .expect(1)
        .mount(server)
        .await;
}

async fn mock_list(server: &MockServer, files: serde_json::Value) {
    Mock::given(method("GET"))
        .and(path("/drive/v3/files"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "files": files
        })))
        .expect(1)
        .mount(server)
        .await;
}

async fn mock_upload_create(server: &MockServer, expected_calls: u64) {
    Mock::given(method("POST"))
        .and(path("/upload/drive/v3/files"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "new-file-id"
        })))
        .expect(expected_calls)
        .mount(server)
        .await;
}

async fn mock_create_folder(server: &MockServer, expected_calls: u64) {
    Mock::given(method("POST"))
        .and(path("/drive/v3/files"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "folder-id"
        })))
        .expect(expected_calls)
        .mount(server)
        .await;
}

async fn mock_upload_update(server: &MockServer, expected_calls: u64) {
    Mock::given(method("PATCH"))
        .and(path_regex(r"^/upload/drive/v3/files/[^/]+$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
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

async fn mock_export(
    server: &MockServer,
    drive_id: &str,
    export_mime: &str,
    body: &'static str,
    expected_calls: u64,
) {
    Mock::given(method("GET"))
        .and(path(format!("/drive/v3/files/{drive_id}/export")))
        .and(query_param("mimeType", export_mime))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .expect(expected_calls)
        .mount(server)
        .await;
}

#[tokio::test]
async fn google_doc_exported_as_docx_on_first_sync() {
    let server = MockServer::start().await;
    mock_list(
        &server,
        json!([{
            "id": "doc-1",
            "name": "Meeting Notes",
            "mimeType": GOOGLE_DOC_MIME,
            "modifiedTime": "2024-06-01T10:00:00Z",
            "size": null,
            "parents": ["root-folder"],
            "trashed": false
        }]),
    )
    .await;
    mock_export(
        &server,
        "doc-1",
        DOCX_EXPORT_MIME,
        "exported docx content",
        1,
    )
    .await;
    mock_start_page_token(&server).await;
    mock_upload_create(&server, 0).await;
    mock_create_folder(&server, 0).await;
    mock_upload_update(&server, 0).await;
    mock_download_media(&server, 0, "").await;

    let sync_dir = tempfile::tempdir().expect("create sync tempdir");
    let client = DriveClient::with_base_url("test-token".to_string(), server.uri());
    let (store, redb) = setup_store(&sync_dir);
    let config = test_config(&sync_dir);

    let report = run_sync(&config, &client, &store, &redb)
        .await
        .expect("run sync");

    assert!(!report.downloaded.is_empty());
    assert!(report
        .downloaded
        .contains(&RelativePath::from("Meeting Notes.docx")));

    let local_path = sync_dir.path().join("Meeting Notes.docx");
    assert!(local_path.exists());
    let bytes = tokio::fs::read(&local_path)
        .await
        .expect("read converted docx file");
    assert_eq!(bytes, b"exported docx content");
}

#[tokio::test]
async fn google_sheet_exported_as_xlsx_on_first_sync() {
    let server = MockServer::start().await;
    mock_list(
        &server,
        json!([{
            "id": "sheet-1",
            "name": "Budget",
            "mimeType": GOOGLE_SHEET_MIME,
            "modifiedTime": "2024-06-01T10:00:00Z",
            "size": null,
            "parents": ["root-folder"],
            "trashed": false
        }]),
    )
    .await;
    mock_export(
        &server,
        "sheet-1",
        XLSX_EXPORT_MIME,
        "exported xlsx content",
        1,
    )
    .await;
    mock_start_page_token(&server).await;
    mock_upload_create(&server, 0).await;
    mock_create_folder(&server, 0).await;
    mock_upload_update(&server, 0).await;
    mock_download_media(&server, 0, "").await;

    let sync_dir = tempfile::tempdir().expect("create sync tempdir");
    let client = DriveClient::with_base_url("test-token".to_string(), server.uri());
    let (store, redb) = setup_store(&sync_dir);
    let config = test_config(&sync_dir);

    let report = run_sync(&config, &client, &store, &redb)
        .await
        .expect("run sync");

    assert!(!report.downloaded.is_empty());
    assert!(report.downloaded.contains(&RelativePath::from("Budget.xlsx")));

    let local_path = sync_dir.path().join("Budget.xlsx");
    assert!(local_path.exists());
    let bytes = tokio::fs::read(&local_path)
        .await
        .expect("read converted xlsx file");
    assert_eq!(bytes, b"exported xlsx content");
}

#[tokio::test]
async fn converted_file_redownloaded_when_remote_changes() {
    let server = MockServer::start().await;
    mock_list(
        &server,
        json!([{
            "id": "doc-1",
            "name": "Meeting Notes",
            "mimeType": GOOGLE_DOC_MIME,
            "modifiedTime": "2024-06-15T10:00:00Z",
            "size": null,
            "parents": ["root-folder"],
            "trashed": false
        }]),
    )
    .await;
    mock_export(&server, "doc-1", DOCX_EXPORT_MIME, "new exported content", 1).await;
    mock_start_page_token(&server).await;
    mock_upload_create(&server, 0).await;
    mock_create_folder(&server, 0).await;
    mock_upload_update(&server, 0).await;
    mock_download_media(&server, 0, "").await;

    let sync_dir = tempfile::tempdir().expect("create sync tempdir");
    let local_path = sync_dir.path().join("Meeting Notes.docx");
    tokio::fs::write(&local_path, b"old exported content")
        .await
        .expect("write old exported content");

    let old_export_md5 = compute_md5(&local_path)
        .await
        .expect("compute old exported content md5");
    let local_meta = tokio::fs::metadata(&local_path)
        .await
        .expect("stat old exported file");
    let local_mtime: DateTime<Utc> = local_meta
        .modified()
        .expect("read old exported file mtime")
        .into();

    let (store, redb) = setup_store(&sync_dir);
    store
        .upsert(
            RelativePath::from("Meeting Notes.docx"),
            SyncRecord {
                drive_file_id: Some("doc-1".to_string()),
                remote_md5: Some("mtime:2024-06-01T10:00:00Z".to_string()),
                remote_modified_at: Some(
                    DateTime::parse_from_rfc3339("2024-06-01T10:00:00Z")
                        .expect("parse original remote mtime")
                        .with_timezone(&Utc),
                ),
                local_md5: old_export_md5.clone(),
                local_mtime,
                local_size: local_meta.len(),
                last_synced_at: Utc::now(),
            },
        )
        .expect("seed sync metadata");
    store
        .upsert_conversion(
            RelativePath::from("Meeting Notes.docx"),
            WorkspaceConversion {
                drive_file_id: "doc-1".to_string(),
                google_mime: GOOGLE_DOC_MIME.to_string(),
                last_export_md5: Some(old_export_md5),
            },
        )
        .expect("seed conversion metadata");
    store.persist_to_redb(&redb).expect("persist seeded metadata");

    let client = DriveClient::with_base_url("test-token".to_string(), server.uri());
    let config = test_config(&sync_dir);
    let report = run_sync(&config, &client, &store, &redb)
        .await
        .expect("run sync");

    assert!(report
        .downloaded
        .contains(&RelativePath::from("Meeting Notes.docx")));
    let bytes = tokio::fs::read(&local_path)
        .await
        .expect("read re-exported file");
    assert_eq!(bytes, b"new exported content");
}
