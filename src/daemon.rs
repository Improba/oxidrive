//! Continuous sync daemon with local watch + periodic triggers.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use crate::config::Config;
use crate::drive::client::DriveClient;
use crate::error::OxidriveError;
use crate::store::{RedbStore, Store};
use crate::sync::engine::run_sync_incremental;
use crate::types::SyncReport;
use crate::watch::local::LocalWatcher;

/// Runs the continuous sync daemon.
///
/// - Performs initial full sync on startup
/// - Watches local filesystem for changes
/// - Runs periodic sync every `interval` seconds
/// - Shuts down gracefully on cancellation
pub async fn run_daemon(
    config: &Config,
    client: &DriveClient,
    store: &Store,
    redb: &RedbStore,
) -> Result<(), OxidriveError> {
    let shutdown = CancellationToken::new();
    spawn_shutdown_handler(shutdown.clone());

    tracing::info!("daemon: running initial sync cycle");
    let initial = run_sync_cycle(config, client, store, redb).await;
    log_report(&initial);

    let mut watcher = LocalWatcher::new(config.sync_dir.clone(), config.debounce_ms)?;
    let mut watcher_rx = watcher.watch().await?;
    tracing::info!(
        sync_dir = %config.sync_dir.display(),
        debounce_ms = config.debounce_ms,
        "daemon: local filesystem watcher started"
    );

    let interval_secs = config.sync_interval_secs.max(1);
    let mut interval = tokio::time::interval(Duration::from_secs(interval_secs));
    interval.tick().await;
    tracing::info!(
        interval_secs,
        "daemon: periodic timer started"
    );

    let sync_semaphore = Arc::new(Semaphore::new(1));

    loop {
        tokio::select! {
            _ = shutdown.cancelled() => {
                tracing::info!("daemon: shutting down on signal");
                break;
            }
            _ = interval.tick() => {
                if let Ok(permit) = sync_semaphore.clone().try_acquire_owned() {
                    let report = run_sync_cycle(config, client, store, redb).await;
                    log_report(&report);
                    drop(permit);
                } else {
                    tracing::debug!("daemon: sync in progress, skipping periodic trigger");
                }
            }
            maybe_event = watcher_rx.recv() => {
                match maybe_event {
                    Some(event) => {
                        tracing::debug!(?event, "daemon: file change detected");
                        tokio::time::sleep(Duration::from_millis(500)).await;
                        while watcher_rx.try_recv().is_ok() {}

                        if let Ok(permit) = sync_semaphore.clone().try_acquire_owned() {
                            let report = run_sync_cycle(config, client, store, redb).await;
                            log_report(&report);
                            drop(permit);
                        } else {
                            tracing::debug!("daemon: sync in progress, skipping watch trigger");
                        }
                    }
                    None => {
                        tracing::warn!("daemon: watcher channel closed; continuing with periodic sync only");
                    }
                }
            }
        }
    }

    store.persist_to_redb(redb)?;
    tracing::info!("daemon: final metadata persisted; shutdown complete");
    Ok(())
}

async fn run_sync_cycle(
    config: &Config,
    client: &DriveClient,
    store: &Store,
    redb: &RedbStore,
) -> Result<SyncReport, OxidriveError> {
    let report = run_sync_incremental(config, client, store, redb).await?;
    store.persist_to_redb(redb)?;
    persist_sync_summary(redb, store).await?;
    Ok(report)
}

pub async fn persist_sync_summary(db: &RedbStore, session_store: &Store) -> Result<(), OxidriveError> {
    let tracked_files_count = session_store.iter_records()?.len();
    db.set_config(
        "tracked_files_count",
        tracked_files_count.to_string().as_bytes(),
    )
    .await?;
    let now = chrono::Utc::now().to_rfc3339();
    db.set_config("last_sync_at", now.as_bytes()).await?;
    Ok(())
}

fn log_report(report: &Result<SyncReport, OxidriveError>) {
    match report {
        Ok(report) => {
            tracing::info!(
                uploaded = report.uploaded.len(),
                downloaded = report.downloaded.len(),
                deleted_local = report.deleted_local.len(),
                deleted_remote = report.deleted_remote.len(),
                conflicts = report.conflicts.len(),
                skipped = report.skipped,
                errors = report.errors.len(),
                duration_ms = report.duration.as_millis(),
                "daemon: sync cycle complete"
            );
        }
        Err(error) => {
            tracing::error!(error = %error, "daemon: sync cycle failed");
        }
    }
}

fn spawn_shutdown_handler(shutdown: CancellationToken) {
    tokio::spawn(async move {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};

            let mut sigterm = match signal(SignalKind::terminate()) {
                Ok(s) => s,
                Err(error) => {
                    tracing::warn!(error = %error, "daemon: failed to install SIGTERM handler; waiting for Ctrl+C only");
                    if let Err(ctrl_c_error) = tokio::signal::ctrl_c().await {
                        tracing::warn!(error = %ctrl_c_error, "daemon: ctrl_c signal handler failed");
                    }
                    shutdown.cancel();
                    return;
                }
            };

            tokio::select! {
                ctrl = tokio::signal::ctrl_c() => {
                    if let Err(error) = ctrl {
                        tracing::warn!(error = %error, "daemon: ctrl_c signal handler failed");
                    } else {
                        tracing::info!("daemon: received SIGINT");
                    }
                }
                _ = sigterm.recv() => {
                    tracing::info!("daemon: received SIGTERM");
                }
            }
            shutdown.cancel();
        }

        #[cfg(not(unix))]
        {
            if let Err(error) = tokio::signal::ctrl_c().await {
                tracing::warn!(error = %error, "daemon: ctrl_c signal handler failed");
            } else {
                tracing::info!("daemon: received Ctrl+C");
            }
            shutdown.cancel();
        }
    });
}
