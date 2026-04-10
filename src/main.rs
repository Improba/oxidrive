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
use crate::types::{RelativePath, SyncAction, SyncReport};

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
    let local = sync::scan_local(&config.sync_dir, &config.ignore_patterns).await?;
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
    session_store.load_from_redb(&db)?;

    if dry_run {
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
        daemon::persist_sync_summary(&db, &session_store).await?;
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

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn service_installed_status() -> (bool, &'static str) {
    (false, "unsupported platform")
}

async fn handle_status(config: &config::Config) -> Result<()> {
    let db = store::RedbStore::open(&state_db_path(config))?;
    let last_sync_at = db.get_config("last_sync_at").await?;
    let tracked_files = db.list_sync_metadata().await?.len();
    let page_token = db.get_page_token().await?;
    let conversion_count = db.list_conversions().await?.len();

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
    let page_token_text = if page_token.is_some() {
        "present"
    } else {
        "absent"
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
    let unit_file_text = if unit_installed {
        "installed"
    } else {
        "not installed"
    };
    let daemon_text = if unit_installed {
        "enabled"
    } else {
        "disabled"
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
    println!();
    println!("Service ({platform_label}):");
    println!("  {:<18} {}", "Unit file:", unit_file_text);
    println!("  {:<18} {}", "Daemon mode:", daemon_text);

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
