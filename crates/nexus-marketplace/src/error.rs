//! Error types for marketplace operations

use thiserror::Error;

/// Result type for marketplace operations
pub type Result<T> = std::result::Result<T, MarketplaceError>;

/// Marketplace errors
#[derive(Error, Debug)]
pub enum MarketplaceError {
    /// Unknown marketplace provider
    #[error("Unknown marketplace provider: {0}")]
    UnknownProvider(String),

    /// Mod not found
    #[error("Mod not found: {0}")]
    ModNotFound(String),

    /// Version not found
    #[error("Version '{version}' not found for mod '{mod_id}'")]
    VersionNotFound {
        mod_id: String,
        version: String,
    },

    /// HTTP/network error
    #[error("Network error: {0}")]
    NetworkError(#[from] reqwest::Error),

    /// JSON parsing error
    #[error("Failed to parse response: {0}")]
    ParseError(#[from] serde_json::Error),

    /// IO error
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    /// Rate limited by the marketplace
    #[error("Rate limited by {provider}. Retry after {retry_after_secs} seconds")]
    RateLimited {
        provider: String,
        retry_after_secs: u64,
    },

    /// Authentication required
    #[error("Authentication required for {provider}")]
    AuthRequired {
        provider: String,
    },

    /// Invalid API key
    #[error("Invalid API key for {provider}")]
    InvalidApiKey {
        provider: String,
    },

    /// Download failed
    #[error("Failed to download mod: {reason}")]
    DownloadFailed {
        reason: String,
    },

    /// Checksum mismatch
    #[error("Checksum mismatch for {file}: expected {expected}, got {actual}")]
    ChecksumMismatch {
        file: String,
        expected: String,
        actual: String,
    },

    /// Unsupported game
    #[error("Game '{game}' is not supported by {provider}")]
    UnsupportedGame {
        provider: String,
        game: String,
    },

    /// Generic API error
    #[error("API error from {provider}: {message}")]
    ApiError {
        provider: String,
        message: String,
        status_code: Option<u16>,
    },
}
