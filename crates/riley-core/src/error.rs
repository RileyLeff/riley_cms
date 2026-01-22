//! Error types for riley-core

use std::path::PathBuf;
use thiserror::Error;

/// Result type for riley-core operations
pub type Result<T> = std::result::Result<T, Error>;

/// Error types for riley-core
#[derive(Debug, Error)]
pub enum Error {
    #[error("Config error: {0}")]
    Config(String),

    #[error("Config not found. Searched: {searched:?}")]
    ConfigNotFound { searched: Vec<PathBuf> },

    #[error("Failed to parse config at {path}: {source}")]
    ConfigParse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },

    #[error("Content error at {path}: {message}")]
    Content { path: PathBuf, message: String },

    #[error("Post not found: {0}")]
    PostNotFound(String),

    #[error("Series not found: {0}")]
    SeriesNotFound(String),

    #[error("Storage error: {0}")]
    Storage(String),

    #[error("Git error: {0}")]
    Git(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("S3 error: {0}")]
    S3(String),
}

impl From<git2::Error> for Error {
    fn from(e: git2::Error) -> Self {
        Error::Git(e.message().to_string())
    }
}
