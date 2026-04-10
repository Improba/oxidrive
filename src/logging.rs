//! Tracing subscriber setup: human-readable console (ANSI when stderr is a TTY) and optional JSON
//! lines to a daily-rotating log file.

use std::io::{stderr, IsTerminal};
use std::path::Path;
use std::sync::OnceLock;

use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::fmt;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::Layer;

use crate::error::OxidriveError;

static FILE_LOG_GUARD: OnceLock<tracing_appender::non_blocking::WorkerGuard> = OnceLock::new();

/// Initializes tracing using a pre-resolved filter string (for example from CLI + config) and
/// optional JSON file output. Equivalent to [`init_logging_with_cli_flags`] with `verbose` and
/// `quiet` set to `false` (so `RUST_LOG` still overrides when set).
pub fn init_logging(resolved_filter: &str, log_file: Option<&Path>) -> Result<(), OxidriveError> {
    init_logging_with_cli_flags(resolved_filter, false, false, log_file)
}

/// Initializes the tracing subscriber.
///
/// - `level`: default log level when `RUST_LOG` is unset and neither `verbose` nor `quiet` apply
///   (for example `"info"`, `"debug"`, or a pre-resolved filter string such as `"trace"`).
/// - `verbose`: override to `debug`.
/// - `quiet`: override to `warn`.
/// - `log_file`: optional path; the parent directory is created if needed. The file stem is the
///   rolling log prefix under that directory ([`Rotation::DAILY`]).
///
/// For a pre-resolved filter (CLI + config already applied), pass `verbose` and `quiet` as `false`
/// and put that string in `level`.
pub fn init_logging_with_cli_flags(
    level: &str,
    verbose: bool,
    quiet: bool,
    log_file: Option<&Path>,
) -> Result<(), OxidriveError> {
    let filter = if verbose {
        "debug".to_string()
    } else if quiet {
        "warn".to_string()
    } else {
        std::env::var("RUST_LOG").unwrap_or_else(|_| {
            let t = level.trim();
            if t.is_empty() {
                "info".to_string()
            } else {
                t.to_string()
            }
        })
    };

    let env_filter = EnvFilter::try_new(filter.as_str()).unwrap_or_else(|_| EnvFilter::new("info"));

    let is_tty = stderr().is_terminal();
    let console_layer = fmt::layer()
        .with_target(false)
        .with_thread_ids(false)
        .compact()
        .with_ansi(is_tty)
        .with_writer(std::io::stderr)
        .with_filter(env_filter.clone());

    if let Some(path) = log_file {
        let dir = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(dir)?;

        let prefix = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("oxidrive");

        let appender = RollingFileAppender::new(Rotation::DAILY, dir, prefix);
        let (non_blocking, guard) = tracing_appender::non_blocking(appender);
        let _ = FILE_LOG_GUARD.set(guard);

        let file_layer = fmt::layer()
            .json()
            .with_writer(non_blocking)
            .with_filter(EnvFilter::new("debug"));

        tracing_subscriber::registry()
            .with(console_layer)
            .with(file_layer)
            .try_init()
            .map_err(|e| OxidriveError::other(format!("tracing init failed: {e}")))?;
    } else {
        tracing_subscriber::registry()
            .with(console_layer)
            .try_init()
            .map_err(|e| OxidriveError::other(format!("tracing init failed: {e}")))?;
    }

    Ok(())
}
