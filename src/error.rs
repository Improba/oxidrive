//! Error types and the crate-wide [`Result`] alias.

use thiserror::Error;

/// Top-level error type aggregating failures from configuration, I/O, HTTP, and domain modules.
///
/// Variant messages are intended to be safe to log and to show to the user where appropriate.
/// [`std::fmt::Display`] is implemented via [`thiserror::Error`] for consistent formatting.
#[derive(Debug, Error)]
pub enum OxidriveError {
    /// Configuration file parsing, validation, or resolution failed.
    #[error("config: {0}")]
    Config(String),
    /// OAuth2 flow or token storage failed.
    #[error("auth: {0}")]
    Auth(String),
    /// Google Drive API usage or response handling failed.
    #[error("drive: {0}")]
    Drive(String),
    /// Sync planning or execution failed.
    #[error("sync: {0}")]
    Sync(String),
    /// Local metadata store (for example `redb`) failed.
    #[error("store: {0}")]
    Store(String),
    /// Underlying filesystem or pipe I/O error.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// HTTP client error that is not already represented as [`OxidriveError::Io`].
    #[error("http: {0}")]
    Http(String),
    /// Any other failure not mapped to a specific variant.
    #[error("{0}")]
    Other(String),
}

impl OxidriveError {
    /// Wrap a message as a configuration error.
    pub fn config(msg: impl Into<String>) -> Self {
        Self::Config(msg.into())
    }

    /// Wrap a message as an authentication error.
    #[allow(dead_code)]
    pub fn auth(msg: impl Into<String>) -> Self {
        Self::Auth(msg.into())
    }

    /// Wrap a message as a Drive API error.
    pub fn drive(msg: impl Into<String>) -> Self {
        Self::Drive(msg.into())
    }

    /// Wrap a message as a sync error.
    pub fn sync(msg: impl Into<String>) -> Self {
        Self::Sync(msg.into())
    }

    /// Wrap a message as a store error.
    pub fn store(msg: impl Into<String>) -> Self {
        Self::Store(msg.into())
    }

    /// Wrap a message as an HTTP-layer error.
    pub fn http(msg: impl Into<String>) -> Self {
        Self::Http(msg.into())
    }

    /// Wrap a message as a generic error.
    pub fn other(msg: impl Into<String>) -> Self {
        Self::Other(msg.into())
    }
}

/// Convenient result alias used across the crate.
pub type Result<T> = std::result::Result<T, OxidriveError>;
