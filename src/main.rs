//! `oxidrive` — bidirectional Google Drive sync CLI.
//!
//! Crate layout includes `cli`, `config`, `types`, `error`, `auth`, `drive`, `sync`, `watch`,
//! `store`, `index`, and `utils`.

mod auth;
mod cli;
mod config;
mod daemon;
mod drive;
mod error;
mod index;
mod logging;
mod service;
mod store;
mod sync;
mod types;
mod utils;
mod watch;

use cli::{Cli, Command, ServiceAction};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use tracing::warn;

use crate::error::Result;
use crate::types::{
    PendingOp, PendingOpKind, PendingOpStage, RelativePath, SyncAction, SyncReport, UploadSession,
    UploadSessionMode, MAX_UPLOAD_SESSION_BLOB_BYTES, RESUMABLE_UPLOAD_SESSION_TTL_HOURS,
};

fn resolve_log_filter(cli: &Cli, config_log_level: &str) -> String {
    if cli.quiet {
        return "warn".to_string();
    }
    match cli.verbose {
        0 => {
            let level = config_log_level.trim();
            if level.is_empty() {
                "info".to_string()
            } else {
                level.to_string()
            }
        }
        1 => "debug".to_string(),
        _ => "trace".to_string(),
    }
}

/// Initialize the global tracing subscriber (console + optional JSON file with rotation).
///
/// Runs after CLI parsing and config loading so fallback filtering can include `config.log_level`
/// (while still honoring `RUST_LOG` and CLI verbosity flags).
fn init_tracing(cli: &Cli, config_log_level: &str, log_file: Option<&Path>) -> Result<()> {
    let fallback = resolve_log_filter(cli, config_log_level);
    logging::init_logging(&fallback, log_file)?;
    Ok(())
}

fn state_db_path(config: &config::Config) -> PathBuf {
    config.sync_dir.join(".oxidrive").join("state.redb")
}

const STATUS_MAX_SESSION_ROWS_SCANNED: usize = 1024;
const STATUS_MAX_PENDING_ROWS_SCANNED: usize = 1024;

struct StatusUploadSessionRow {
    path: RelativePath,
    mode: &'static str,
    next_offset: u64,
    file_size: u64,
    age_secs: i64,
}

struct StatusPendingOpRow {
    path: RelativePath,
    kind: &'static str,
    stage: &'static str,
    age_secs: i64,
}

fn upload_session_mode_label(mode: &UploadSessionMode) -> &'static str {
    match mode {
        UploadSessionMode::Create { .. } => "create",
        UploadSessionMode::Update { .. } => "update",
        UploadSessionMode::Convert { .. } => "convert",
    }
}

fn pending_op_kind_label(kind: &PendingOpKind) -> &'static str {
    match kind {
        PendingOpKind::Upload => "upload",
        PendingOpKind::Download => "download",
        PendingOpKind::DeleteLocal => "delete_local",
        PendingOpKind::DeleteRemote => "delete_remote",
    }
}

fn pending_op_stage_label(stage: &PendingOpStage) -> &'static str {
    match stage {
        PendingOpStage::Planned => "planned",
        PendingOpStage::SideEffectStarted => "side_effect_started",
        PendingOpStage::SideEffectDone => "side_effect_done",
        PendingOpStage::MetadataCommitted => "metadata_committed",
    }
}

fn human_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.2} GiB", b / GB)
    } else if b >= MB {
        format!("{:.2} MiB", b / MB)
    } else if b >= KB {
        format!("{:.2} KiB", b / KB)
    } else {
        format!("{bytes} B")
    }
}

fn human_age(age_secs: i64) -> String {
    if age_secs < 60 {
        format!("{age_secs}s")
    } else if age_secs < 3600 {
        format!("{}m", age_secs / 60)
    } else {
        format!("{}h", age_secs / 3600)
    }
}

fn active_upload_sessions(db: &store::RedbStore) -> Result<Vec<StatusUploadSessionRow>> {
    let now = chrono::Utc::now();
    let ttl = chrono::Duration::hours(RESUMABLE_UPLOAD_SESSION_TTL_HOURS);
    let mut out = Vec::new();
    for (path_raw, data) in db.scan_upload_sessions_sync(
        STATUS_MAX_SESSION_ROWS_SCANNED,
        MAX_UPLOAD_SESSION_BLOB_BYTES,
    )? {
        let path = RelativePath::from(path_raw.as_str());
        if !path.is_safe_non_empty() {
            continue;
        }
        let session: UploadSession = match bincode::deserialize(&data) {
            Ok(s) => s,
            Err(e) => {
                warn!(
                    path = %path,
                    error = %e,
                    "status: skipping invalid persisted upload session payload"
                );
                continue;
            }
        };
        if session.file_size == 0 || session.next_offset >= session.file_size {
            continue;
        }
        let age = now - session.updated_at;
        if age < chrono::Duration::zero() || age > ttl {
            continue;
        }
        out.push(StatusUploadSessionRow {
            path,
            mode: upload_session_mode_label(&session.mode),
            next_offset: session.next_offset,
            file_size: session.file_size,
            age_secs: age.num_seconds().max(0),
        });
    }
    out.sort_by(|a, b| a.path.as_str().cmp(b.path.as_str()));
    Ok(out)
}

fn active_pending_ops(db: &store::RedbStore) -> Result<Vec<StatusPendingOpRow>> {
    let now = chrono::Utc::now();
    let mut out = Vec::new();
    for (path_raw, data) in db.scan_pending_ops_sync(
        STATUS_MAX_PENDING_ROWS_SCANNED,
        MAX_UPLOAD_SESSION_BLOB_BYTES,
    )? {
        let path = RelativePath::from(path_raw.as_str());
        if !path.is_safe_non_empty() {
            continue;
        }
        let pending: PendingOp = match bincode::deserialize(&data) {
            Ok(op) => op,
            Err(e) => {
                warn!(
                    path = %path,
                    error = %e,
                    "status: skipping invalid persisted pending-op payload"
                );
                continue;
            }
        };
        let age = (now - pending.updated_at).num_seconds().max(0);
        out.push(StatusPendingOpRow {
            path,
            kind: pending_op_kind_label(&pending.kind),
            stage: pending_op_stage_label(&pending.stage),
            age_secs: age,
        });
    }
    out.sort_by(|a, b| a.path.as_str().cmp(b.path.as_str()));
    Ok(out)
}

fn token_path_is_within_sync_dir(config: &config::Config) -> bool {
    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd,
        Err(_) => return false,
    };
    let sync_abs = if config.sync_dir.is_absolute() {
        config.sync_dir.clone()
    } else {
        cwd.join(&config.sync_dir)
    };
    let token_abs = if config.token_path.is_absolute() {
        config.token_path.clone()
    } else {
        cwd.join(&config.token_path)
    };
    token_abs.starts_with(sync_abs)
}

fn service_install_label(platform_label: &str) -> &'static str {
    match platform_label {
        "systemd" => "Unit file:",
        "launchd" => "LaunchAgent:",
        "task scheduler" => "Scheduled task:",
        _ => "Service install:",
    }
}

fn auth_manager_from_config(config: &config::Config) -> Result<auth::AuthManager> {
    if config.client_id.trim().is_empty() {
        return Err(error::OxidriveError::config(
            "config.client_id is required; set it in config.toml",
        ));
    }
    if config.client_secret.trim().is_empty() {
        return Err(error::OxidriveError::config(
            "config.client_secret is required; set it in config.toml",
        ));
    }
    if token_path_is_within_sync_dir(config) {
        return Err(error::OxidriveError::config(format!(
            "config.token_path ({}) must be outside config.sync_dir ({}) to avoid syncing credentials",
            config.token_path.display(),
            config.sync_dir.display()
        )));
    }
    Ok(auth::AuthManager::new(
        config.client_id.clone(),
        config.client_secret.clone(),
        config.token_path.clone(),
    ))
}

fn print_sync_report(report: &SyncReport) {
    println!(
        "Sync complete: uploaded={}, downloaded={}, deleted_local={}, deleted_remote={}, conflicts={}, skipped={}, errors={}, duration_ms={}",
        report.uploaded.len(),
        report.downloaded.len(),
        report.deleted_local.len(),
        report.deleted_remote.len(),
        report.conflicts.len(),
        report.skipped,
        report.errors.len(),
        report.duration.as_millis()
    );
}

fn print_dry_run_summary(actions: &[SyncAction]) {
    let mut upload = 0usize;
    let mut download = 0usize;
    let mut delete_local = 0usize;
    let mut delete_remote = 0usize;
    let mut conflict = 0usize;
    let mut cleanup = 0usize;
    let mut skip = 0usize;
    for action in actions {
        match action {
            SyncAction::Upload { .. } => upload += 1,
            SyncAction::Download { .. } => download += 1,
            SyncAction::DeleteLocal { .. } => delete_local += 1,
            SyncAction::DeleteRemote { .. } => delete_remote += 1,
            SyncAction::Conflict { .. } => conflict += 1,
            SyncAction::CleanupMetadata { .. } => cleanup += 1,
            SyncAction::Skip { .. } => skip += 1,
        }
    }
    println!(
        "Dry-run summary: upload={}, download={}, delete_local={}, delete_remote={}, conflicts={}, cleanup_metadata={}, skipped={}",
        upload, download, delete_local, delete_remote, conflict, cleanup, skip
    );
}

async fn dry_run_actions(
    config: &config::Config,
    client: &drive::DriveClient,
    store: &store::Store,
) -> Result<Vec<SyncAction>> {
    let root_id = config
        .drive_folder_id
        .clone()
        .ok_or_else(|| error::OxidriveError::sync("config.drive_folder_id is required for sync"))?;
    store.set_root_drive_folder_id(Some(root_id.clone()))?;

    tracing::info!("dry-run: scanning local filesystem");
    let ignore_patterns = config.effective_ignore_patterns();
    let local = sync::scan_local(&config.sync_dir, &ignore_patterns).await?;
    tracing::info!(files = local.len(), "dry-run: listing remote Drive tree");
    let remote = drive::list_all_files(client, &root_id).await?;
    store.set_remote_snapshot(remote.clone())?;

    let mut paths: HashSet<RelativePath> = local.keys().cloned().collect();
    paths.extend(remote.keys().cloned());
    paths.extend(store.all_record_paths()?);

    let mut actions = Vec::with_capacity(paths.len());
    for p in paths {
        let l = local.get(&p);
        let r = remote.get(&p);
        let meta = store.get(&p)?;
        let conversion = store.get_conversion(&p)?;
        if let Some(conversion) = conversion.as_ref() {
            actions.push(sync::decision::determine_action_converted(
                &p,
                l,
                r,
                meta.as_ref(),
                &config.conflict_policy,
                true,
                conversion.last_export_md5.as_deref(),
            ));
        } else {
            actions.push(sync::determine_action(
                &p,
                l,
                r,
                meta.as_ref(),
                &config.conflict_policy,
            ));
        }
    }
    Ok(actions)
}

async fn handle_setup(config: &config::Config) -> Result<()> {
    let auth_manager = auth_manager_from_config(config)?;
    tracing::info!("setup: starting OAuth2 setup flow");
    auth_manager.setup().await?;
    println!(
        "OAuth setup complete. Token saved to {}",
        config.token_path.display()
    );
    Ok(())
}

async fn handle_sync(config: &config::Config, dry_run: bool, once: bool) -> Result<()> {
    let auth_manager = auth_manager_from_config(config)?;
    let access_token = auth_manager.get_access_token().await?;
    let client = drive::DriveClient::new(access_token);
    let session_store = store::Store::open(config.sync_dir.clone())?;
    let db = store::RedbStore::open(&state_db_path(config))?;

    if dry_run {
        session_store.load_from_redb(&db)?;
        tracing::info!("sync: running dry-run planning (no execution)");
        let actions = dry_run_actions(config, &client, &session_store).await?;
        print_dry_run_summary(&actions);
        return Ok(());
    }

    let daemon_enabled = config.sync_interval_secs > 0 && !once;
    if daemon_enabled {
        tracing::info!(
            interval_secs = config.sync_interval_secs,
            "sync: starting continuous daemon mode"
        );
        daemon::run_daemon(config, &client, &session_store, &db).await?;
    } else {
        tracing::info!("sync: running single sync cycle");
        let report = sync::engine::run_sync(config, &client, &session_store, &db).await?;
        print_sync_report(&report);
        daemon::persist_sync_summary(&db, &session_store, report.errors.is_empty()).await?;
    }
    Ok(())
}

async fn handle_service(action: ServiceAction, config_path: Option<&Path>) -> Result<()> {
    match action {
        ServiceAction::Install => service::install_service(config_path)?,
        ServiceAction::Uninstall => service::uninstall_service()?,
        ServiceAction::Start => service::start_service()?,
        ServiceAction::Stop => service::stop_service()?,
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn service_installed_status() -> (bool, &'static str) {
    let installed = std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join(".config/systemd/user/oxidrive.service").is_file())
        .unwrap_or(false);
    (installed, "systemd")
}

#[cfg(target_os = "macos")]
fn service_installed_status() -> (bool, &'static str) {
    let installed = std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| {
            home.join("Library/LaunchAgents/com.oxidrive.sync.plist")
                .is_file()
        })
        .unwrap_or(false);
    (installed, "launchd")
}

#[cfg(target_os = "windows")]
fn service_installed_status() -> (bool, &'static str) {
    let installed = std::process::Command::new("schtasks")
        .args(["/Query", "/TN", "oxidrive"])
        .status()
        .map(|status| status.success())
        .unwrap_or(false);
    (installed, "task scheduler")
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn service_installed_status() -> (bool, &'static str) {
    (false, "unsupported platform")
}

async fn handle_status(config: &config::Config) -> Result<()> {
    let db = store::RedbStore::open(&state_db_path(config))?;
    let last_sync_at = db.get_config("last_sync_at").await?;
    let tracked_files = db.count_sync_metadata_sync()?;
    let page_token_raw = db.get_config("page_token").await?;
    let conversion_count = db.count_conversions_sync()?;
    let upload_sessions = active_upload_sessions(&db)?;
    let pending_ops = active_pending_ops(&db)?;

    let last_sync_text = match last_sync_at {
        Some(bytes) => match String::from_utf8(bytes) {
            Ok(raw) => match chrono::DateTime::parse_from_rfc3339(&raw) {
                Ok(parsed) => parsed
                    .with_timezone(&chrono::Utc)
                    .format("%Y-%m-%d %H:%M:%S UTC")
                    .to_string(),
                Err(_) => raw,
            },
            Err(_) => String::from("<invalid UTF-8 value in state db>"),
        },
        None => String::from("<never>"),
    };
    let page_token_text = match page_token_raw {
        Some(bytes) => {
            if String::from_utf8(bytes).is_ok() {
                "present"
            } else {
                "invalid"
            }
        }
        None => "absent",
    };
    let index_dir = config
        .index_dir
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| String::from("<disabled>"));
    let drive_folder_id = config.drive_folder_id.as_deref().unwrap_or("<not set>");
    let conflict_policy = match &config.conflict_policy {
        config::ConflictPolicy::LocalWins => "local-wins".to_string(),
        config::ConflictPolicy::RemoteWins => "remote-wins".to_string(),
        config::ConflictPolicy::Rename { suffix } => format!("rename ({suffix})"),
    };
    let (unit_installed, platform_label) = service_installed_status();
    let sync_loop_configured = config.sync_interval_secs > 0;
    let unit_file_text = if unit_installed {
        "installed"
    } else {
        "not installed"
    };
    let sync_loop_text = if sync_loop_configured {
        "configured"
    } else {
        "disabled (sync_interval_secs=0)"
    };

    println!("oxidrive v{}", env!("CARGO_PKG_VERSION"));
    println!();
    println!("Configuration:");
    println!("  {:<18} {}", "Sync directory:", config.sync_dir.display());
    println!("  {:<18} {}", "Drive folder:", drive_folder_id);
    println!("  {:<18} {}s", "Sync interval:", config.sync_interval_secs);
    println!("  {:<18} {}", "Conflict policy:", conflict_policy);
    println!("  {:<18} {}", "Index directory:", index_dir);
    println!();
    println!("Sync State:");
    println!("  {:<18} {}", "Last sync:", last_sync_text);
    println!("  {:<18} {}", "Tracked files:", tracked_files);
    println!("  {:<18} {}", "Page token:", page_token_text);
    println!("  {:<18} {}", "Conversions:", conversion_count);
    println!("  {:<18} {}", "Upload sessions:", upload_sessions.len());
    println!("  {:<18} {}", "Pending ops:", pending_ops.len());
    println!(
        "  {:<18} {}",
        "Session list cap:", STATUS_MAX_SESSION_ROWS_SCANNED
    );
    println!(
        "  {:<18} {}",
        "Pending list cap:", STATUS_MAX_PENDING_ROWS_SCANNED
    );
    if upload_sessions.is_empty() {
        println!("  {:<18} <none>", "Session details:");
    } else {
        println!("  {:<18}", "Session details:");
        for session in upload_sessions.iter().take(10) {
            let progress = if session.file_size == 0 {
                0.0
            } else {
                (session.next_offset as f64 / session.file_size as f64) * 100.0
            };
            println!(
                "    - {} [{}] {}/{} ({:.1}%) age {}",
                session.path,
                session.mode,
                human_size(session.next_offset),
                human_size(session.file_size),
                progress,
                human_age(session.age_secs)
            );
        }
        if upload_sessions.len() > 10 {
            println!("    ... and {} more", upload_sessions.len() - 10);
        }
    }
    if pending_ops.is_empty() {
        println!("  {:<18} <none>", "Pending details:");
    } else {
        println!("  {:<18}", "Pending details:");
        for pending in pending_ops.iter().take(10) {
            println!(
                "    - {} [{} / {}] age {}",
                pending.path,
                pending.kind,
                pending.stage,
                human_age(pending.age_secs)
            );
        }
        if pending_ops.len() > 10 {
            println!("    ... and {} more", pending_ops.len() - 10);
        }
    }
    println!();
    println!("Service ({platform_label}):");
    println!(
        "  {:<18} {}",
        service_install_label(platform_label),
        unit_file_text
    );
    println!("  {:<18} {}", "Sync loop:", sync_loop_text);
    if !pending_ops.is_empty() {
        println!(
            "  {:<18} rerun `oxidrive sync --once` to flush/recover pending operations",
            "Recovery hint:"
        );
    }

    if config.drive_folder_id.is_none() {
        warn!("config.drive_folder_id is not set; sync cannot run until it is configured");
    }
    Ok(())
}

async fn run_async(cli: Cli, config: config::Config) -> Result<()> {
    match cli.command {
        Command::Setup => handle_setup(&config).await,
        Command::Sync { dry_run, once } => handle_sync(&config, dry_run, once).await,
        Command::Service { action } => handle_service(action, cli.config.as_deref()).await,
        Command::Status => handle_status(&config).await,
    }
}

fn run() -> Result<()> {
    let cli = cli::parse_args();
    let config = config::Config::load(cli.config.as_deref())?;
    init_tracing(&cli, &config.log_level, config.log_file.as_deref())?;
    tracing::debug!(log_level = %config.log_level, "loaded configuration");

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| error::OxidriveError::other(format!("tokio runtime: {e}")))?;

    runtime.block_on(run_async(cli, config))
}

fn main() -> Result<()> {
    run()
}
