use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use chrono::Utc;
use md5::{Digest, Md5};
use oxidrive::config::{Config, ConflictPolicy};
use oxidrive::drive::DriveClient;
use oxidrive::store::{RedbStore, Store};
use oxidrive::sync::engine::run_sync;
use oxidrive::types::{RelativePath, SyncReport};
use tempfile::TempDir;
use wiremock::matchers::any;
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const ROOT_FOLDER_ID: &str = "root-folder";

#[derive(Clone)]
struct MockFile {
    id: String,
    path: String,
    mime_type: String,
    bytes: Vec<u8>,
    md5: String,
    version: i64,
    head_revision_id: String,
    app_properties: BTreeMap<String, String>,
    trashed: bool,
}

impl MockFile {
    fn as_drive_json(&self) -> serde_json::Value {
        serde_json::json!({
            "id": self.id,
            "name": self.path,
            "mimeType": self.mime_type,
            "md5Checksum": self.md5,
            "modifiedTime": Utc::now().to_rfc3339(),
            "size": self.bytes.len().to_string(),
            "headRevisionId": self.head_revision_id,
            "version": self.version,
            "appProperties": self.app_properties,
            "parents": [ROOT_FOLDER_ID],
            "trashed": self.trashed,
            "owners": []
        })
    }
}

struct SharedDriveState {
    files: HashMap<String, MockFile>,
    next_file_id: u64,
    next_page_token: u64,
    upload_media_calls: u64,
}

impl SharedDriveState {
    fn new(seeded_files: Vec<(String, Vec<u8>)>) -> Self {
        let mut state = Self {
            files: HashMap::new(),
            next_file_id: 1,
            next_page_token: 1,
            upload_media_calls: 0,
        };
        for (path, bytes) in seeded_files {
            let id = state.alloc_id();
            state.files.insert(
                id.clone(),
                MockFile {
                    id,
                    path,
                    mime_type: "text/plain".to_string(),
                    md5: md5_hex(&bytes),
                    bytes,
                    version: 1,
                    head_revision_id: "rev-1".to_string(),
                    app_properties: BTreeMap::from([
                        ("ox_vv".to_string(), "seed:1".to_string()),
                        ("ox_origin".to_string(), "seed".to_string()),
                    ]),
                    trashed: false,
                },
            );
        }
        state
    }

    fn alloc_id(&mut self) -> String {
        let id = format!("file-{}", self.next_file_id);
        self.next_file_id += 1;
        id
    }

    fn active_files(&self) -> Vec<&MockFile> {
        let mut out: Vec<&MockFile> = self.files.values().filter(|f| !f.trashed).collect();
        out.sort_by(|a, b| a.path.cmp(&b.path).then_with(|| a.id.cmp(&b.id)));
        out
    }

    fn file_by_path(&self, path: &str) -> Option<&MockFile> {
        self.files.values().find(|f| !f.trashed && f.path == path)
    }

    fn file_by_id_mut(&mut self, id: &str) -> Option<&mut MockFile> {
        self.files.get_mut(id)
    }

    fn file_by_id(&self, id: &str) -> Option<&MockFile> {
        self.files.get(id)
    }
}

#[derive(Clone)]
struct SharedDriveResponder {
    state: Arc<Mutex<SharedDriveState>>,
}

impl SharedDriveResponder {
    fn list_files(&self, request: &Request) -> ResponseTemplate {
        let query = request
            .url
            .query_pairs()
            .find_map(|(k, v)| (k == "q").then_some(v.to_string()))
            .unwrap_or_default();
        let state = self.state.lock().expect("state lock");
        let files: Vec<serde_json::Value> =
            if query.contains("mimeType = 'application/vnd.google-apps.folder'") {
                Vec::new()
            } else {
                state
                    .active_files()
                    .into_iter()
                    .map(MockFile::as_drive_json)
                    .collect()
            };
        ResponseTemplate::new(200).set_body_json(serde_json::json!({ "files": files }))
    }

    fn start_page_token(&self) -> ResponseTemplate {
        let mut state = self.state.lock().expect("state lock");
        let token = format!("token-{}", state.next_page_token);
        state.next_page_token += 1;
        ResponseTemplate::new(200).set_body_json(serde_json::json!({ "startPageToken": token }))
    }

    fn changes(&self) -> ResponseTemplate {
        let mut state = self.state.lock().expect("state lock");
        let token = format!("token-{}", state.next_page_token);
        state.next_page_token += 1;
        let changes: Vec<serde_json::Value> = state
            .active_files()
            .into_iter()
            .map(|file| {
                serde_json::json!({
                    "fileId": file.id,
                    "removed": false,
                    "time": Utc::now().to_rfc3339(),
                    "file": file.as_drive_json()
                })
            })
            .collect();
        ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "changes": changes,
            "newStartPageToken": token
        }))
    }

    fn get_metadata(&self, path: &str) -> ResponseTemplate {
        let Some(file_id) = path.strip_prefix("/drive/v3/files/") else {
            return ResponseTemplate::new(400);
        };
        let state = self.state.lock().expect("state lock");
        let Some(file) = state.file_by_id(file_id) else {
            return ResponseTemplate::new(404);
        };
        if file.trashed {
            return ResponseTemplate::new(404);
        }
        ResponseTemplate::new(200).set_body_json(file.as_drive_json())
    }

    fn get_media(&self, path: &str) -> ResponseTemplate {
        let Some(file_id) = path.strip_prefix("/drive/v3/files/") else {
            return ResponseTemplate::new(400);
        };
        let state = self.state.lock().expect("state lock");
        let Some(file) = state.file_by_id(file_id) else {
            return ResponseTemplate::new(404);
        };
        if file.trashed {
            return ResponseTemplate::new(404);
        }
        ResponseTemplate::new(200).set_body_bytes(file.bytes.clone())
    }

    fn patch_upload_media(&self, path: &str, body: &[u8]) -> ResponseTemplate {
        let Some(file_id) = path.strip_prefix("/upload/drive/v3/files/") else {
            return ResponseTemplate::new(400);
        };
        let mut state = self.state.lock().expect("state lock");
        let Some(file) = state.file_by_id_mut(file_id) else {
            return ResponseTemplate::new(404);
        };
        let file_id_out = {
            file.bytes = body.to_vec();
            file.md5 = md5_hex(&file.bytes);
            file.version += 1;
            file.head_revision_id = format!("rev-{}", file.version);
            file.id.clone()
        };
        state.upload_media_calls += 1;
        ResponseTemplate::new(200).set_body_json(serde_json::json!({ "id": file_id_out }))
    }

    fn patch_file_metadata(&self, path: &str, body: &[u8]) -> ResponseTemplate {
        let Some(file_id) = path.strip_prefix("/drive/v3/files/") else {
            return ResponseTemplate::new(400);
        };
        let mut state = self.state.lock().expect("state lock");
        let Some(file) = state.file_by_id_mut(file_id) else {
            return ResponseTemplate::new(404);
        };
        let payload: serde_json::Value = match serde_json::from_slice(body) {
            Ok(payload) => payload,
            Err(_) => return ResponseTemplate::new(400),
        };
        if let Some(trashed) = payload.get("trashed").and_then(serde_json::Value::as_bool) {
            file.trashed = trashed;
            file.version += 1;
            return ResponseTemplate::new(200).set_body_json(file.as_drive_json());
        }
        if let Some(app_props) = payload
            .get("appProperties")
            .and_then(serde_json::Value::as_object)
        {
            // Drive merges appProperties by key: a string value upserts the key,
            // an explicit null removes it. Mirror that here for fidelity.
            for (k, v) in app_props {
                if let Some(s) = v.as_str() {
                    file.app_properties.insert(k.clone(), s.to_string());
                } else if v.is_null() {
                    file.app_properties.remove(k);
                }
            }
            file.version += 1;
            return ResponseTemplate::new(200).set_body_json(file.as_drive_json());
        }
        ResponseTemplate::new(200).set_body_json(file.as_drive_json())
    }
}

impl Respond for SharedDriveResponder {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let path = request.url.path();
        let is_media_get = request
            .url
            .query_pairs()
            .any(|(k, v)| k == "alt" && v == "media");
        match (request.method.as_str(), path) {
            ("GET", "/drive/v3/changes/startPageToken") => self.start_page_token(),
            ("GET", "/drive/v3/changes") => self.changes(),
            ("GET", "/drive/v3/files") => self.list_files(request),
            ("GET", _) if path.starts_with("/drive/v3/files/") && is_media_get => {
                self.get_media(path)
            }
            ("GET", _) if path.starts_with("/drive/v3/files/") => self.get_metadata(path),
            ("PATCH", _) if path.starts_with("/upload/drive/v3/files/") => {
                self.patch_upload_media(path, &request.body)
            }
            ("PATCH", _) if path.starts_with("/drive/v3/files/") => {
                self.patch_file_metadata(path, &request.body)
            }
            _ => ResponseTemplate::new(404).set_body_json(serde_json::json!({
                "error": format!("unhandled {} {}", request.method.as_str(), path)
            })),
        }
    }
}

struct ClientHarness {
    device_id: String,
    sync_dir: TempDir,
    store: Store,
    redb: RedbStore,
    config: Config,
    drive: DriveClient,
}

struct MultiClientHarness {
    _server: MockServer,
    state: Arc<Mutex<SharedDriveState>>,
    clients: Vec<ClientHarness>,
}

impl MultiClientHarness {
    async fn new(seeded_files: Vec<(String, Vec<u8>)>, devices: &[&str]) -> Self {
        // Simplification: this mock focuses on deterministic root-level file workflows used by the
        // starred scenarios. It implements the exact endpoints exercised by those sync paths.
        let server = MockServer::start().await;
        let state = Arc::new(Mutex::new(SharedDriveState::new(seeded_files)));
        Mock::given(any())
            .respond_with(SharedDriveResponder {
                state: Arc::clone(&state),
            })
            .expect(1..)
            .mount(&server)
            .await;

        let mut clients = Vec::with_capacity(devices.len());
        for device_id in devices {
            let sync_dir = tempfile::tempdir().expect("create client sync dir");
            let store = Store::open(sync_dir.path()).expect("open store");
            let redb =
                RedbStore::open(&sync_dir.path().join(".oxidrive/state.redb")).expect("open redb");
            let config = Config {
                sync_dir: sync_dir.path().to_path_buf(),
                drive_folder_id: Some(ROOT_FOLDER_ID.to_string()),
                device_id: Some((*device_id).to_string()),
                conflict_policy: ConflictPolicy::ConflictCopy,
                max_concurrent_uploads: 1,
                max_concurrent_downloads: 1,
                stability_ms: 0,
                safe_delete: true,
                sync_interval_secs: 0,
                ..Config::default()
            };
            let drive = DriveClient::with_base_url("test-token".to_string(), server.uri());
            clients.push(ClientHarness {
                device_id: (*device_id).to_string(),
                sync_dir,
                store,
                redb,
                config,
                drive,
            });
        }

        Self {
            _server: server,
            state,
            clients,
        }
    }
}

fn md5_hex(bytes: &[u8]) -> String {
    let mut hasher = Md5::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

async fn run_client_sync(client: &ClientHarness) -> SyncReport {
    client
        .redb
        .delete_config("page_token")
        .await
        .expect("clear page token for deterministic full scan");
    run_sync(&client.config, &client.drive, &client.store, &client.redb)
        .await
        .expect("run client sync")
}

async fn write_text(path: &Path, body: &str) {
    tokio::fs::write(path, body.as_bytes())
        .await
        .expect("write local file");
}

async fn read_text(path: &Path) -> String {
    String::from_utf8(tokio::fs::read(path).await.expect("read local file")).expect("utf8")
}

fn find_conflict_copy(sync_root: &Path, prefix: &str) -> Option<PathBuf> {
    let entries = std::fs::read_dir(sync_root).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        let name = path.file_name()?.to_string_lossy();
        if name.starts_with(prefix) {
            return Some(path);
        }
    }
    None
}

#[tokio::test]
async fn concurrent_edits_keep_both_versions_via_conflict_copy() {
    let harness = MultiClientHarness::new(
        vec![("shared.txt".to_string(), b"base-content".to_vec())],
        &["device-a", "device-b"],
    )
    .await;
    let client_a = &harness.clients[0];
    let client_b = &harness.clients[1];

    let _ = run_client_sync(client_a).await;
    let _ = run_client_sync(client_b).await;

    write_text(&client_a.sync_dir.path().join("shared.txt"), "edited-by-a").await;
    write_text(&client_b.sync_dir.path().join("shared.txt"), "edited-by-b").await;

    let report_a = run_client_sync(client_a).await;
    assert!(report_a
        .uploaded
        .contains(&RelativePath::from("shared.txt")));

    let report_b = run_client_sync(client_b).await;
    assert!(report_b
        .conflicts
        .contains(&RelativePath::from("shared.txt")));
    assert!(report_b
        .uploaded
        .contains(&RelativePath::from("shared.txt")));

    let conflict_prefix = format!("shared.conflict.{}.", client_b.device_id);
    let conflict_copy = find_conflict_copy(client_b.sync_dir.path(), &conflict_prefix)
        .expect("conflict copy should be created");
    let surviving_remote_bytes = read_text(&client_b.sync_dir.path().join("shared.txt")).await;
    let conflict_copy_bytes = read_text(&conflict_copy).await;

    assert_eq!(surviving_remote_bytes, "edited-by-b");
    assert_eq!(conflict_copy_bytes, "edited-by-a");

    let state = harness.state.lock().expect("state lock");
    let remote = state
        .file_by_path("shared.txt")
        .expect("remote file exists");
    assert_eq!(remote.bytes.as_slice(), b"edited-by-b");
}

#[tokio::test]
async fn sequential_edit_downloads_without_conflict_copy() {
    let harness = MultiClientHarness::new(
        vec![("shared.txt".to_string(), b"base-content".to_vec())],
        &["device-a", "device-b"],
    )
    .await;
    let client_a = &harness.clients[0];
    let client_b = &harness.clients[1];

    let _ = run_client_sync(client_a).await;
    let _ = run_client_sync(client_b).await;

    write_text(&client_a.sync_dir.path().join("shared.txt"), "sequential-a").await;
    let report_a = run_client_sync(client_a).await;
    assert!(report_a
        .uploaded
        .contains(&RelativePath::from("shared.txt")));

    let report_b = run_client_sync(client_b).await;
    assert!(report_b.conflicts.is_empty());
    assert!(report_b
        .downloaded
        .contains(&RelativePath::from("shared.txt")));
    assert_eq!(
        read_text(&client_b.sync_dir.path().join("shared.txt")).await,
        "sequential-a"
    );
    assert!(
        find_conflict_copy(client_b.sync_dir.path(), "shared.conflict.").is_none(),
        "no conflict copy should be generated on sequential edits"
    );
}

#[tokio::test]
async fn mtime_only_change_triggers_touch_metadata_without_upload() {
    let harness = MultiClientHarness::new(
        vec![("shared.txt".to_string(), b"stable-content".to_vec())],
        &["device-a", "device-b"],
    )
    .await;
    let client_a = &harness.clients[0];
    let client_b = &harness.clients[1];

    let _ = run_client_sync(client_a).await;
    let _ = run_client_sync(client_b).await;
    let uploads_before = harness.state.lock().expect("state lock").upload_media_calls;

    write_text(
        &client_a.sync_dir.path().join("shared.txt"),
        "stable-content",
    )
    .await;
    let report_a = run_client_sync(client_a).await;
    assert!(report_a.uploaded.is_empty());
    assert!(report_a.conflicts.is_empty());

    let report_b = run_client_sync(client_b).await;
    assert!(report_b.downloaded.is_empty());
    assert!(report_b.conflicts.is_empty());

    let uploads_after = harness.state.lock().expect("state lock").upload_media_calls;
    assert_eq!(
        uploads_after, uploads_before,
        "touch_metadata path must not upload media"
    );
}

#[tokio::test]
async fn remote_delete_moves_other_client_file_to_trash_after_confirmation() {
    let harness = MultiClientHarness::new(
        vec![("shared.txt".to_string(), b"to-delete".to_vec())],
        &["device-a", "device-b"],
    )
    .await;
    let client_a = &harness.clients[0];
    let client_b = &harness.clients[1];

    let _ = run_client_sync(client_a).await;
    let _ = run_client_sync(client_b).await;

    tokio::fs::remove_file(client_a.sync_dir.path().join("shared.txt"))
        .await
        .expect("remove file on client A");

    // Symmetric confirmation: the first sync after a local deletion only records
    // the observation and must NOT trash the remote file yet.
    let report_a_first = run_client_sync(client_a).await;
    assert!(
        report_a_first.deleted_remote.is_empty(),
        "first local-deletion observation must defer the remote trash"
    );
    assert!(
        report_a_first.skipped >= 1,
        "the deferred deletion must be reported as skipped, not silently dropped"
    );
    {
        let state = harness.state.lock().expect("state lock");
        assert!(
            state.file_by_path("shared.txt").is_some(),
            "remote file must still exist after a single deletion observation"
        );
    }

    // The second sync confirms the local deletion and propagates the remote trash.
    let report_a = run_client_sync(client_a).await;
    assert!(report_a
        .deleted_remote
        .contains(&RelativePath::from("shared.txt")));

    let report_b_first = run_client_sync(client_b).await;
    assert!(report_b_first.deleted_local.is_empty());
    assert!(
        client_b.sync_dir.path().join("shared.txt").exists(),
        "first observation should only register tombstone"
    );

    let report_b_second = run_client_sync(client_b).await;
    assert!(report_b_second
        .deleted_local
        .contains(&RelativePath::from("shared.txt")));
    assert!(!client_b.sync_dir.path().join("shared.txt").exists());
    assert!(client_b.sync_dir.path().join(".trash/shared.txt").exists());
}

#[tokio::test]
async fn office_lock_files_are_not_shared_between_clients() {
    let harness = MultiClientHarness::new(Vec::new(), &["device-a", "device-b"]).await;
    let client_a = &harness.clients[0];
    let client_b = &harness.clients[1];

    write_text(&client_a.sync_dir.path().join("~$x.docx"), "lock-bytes").await;
    let report_a = run_client_sync(client_a).await;
    let report_b = run_client_sync(client_b).await;

    assert!(report_a.uploaded.is_empty());
    assert!(report_a.conflicts.is_empty());
    assert!(report_b.downloaded.is_empty());
    assert!(!client_b.sync_dir.path().join("~$x.docx").exists());

    let state = harness.state.lock().expect("state lock");
    assert!(
        state.file_by_path("~$x.docx").is_none(),
        "lock file must never be persisted remotely"
    );
}
