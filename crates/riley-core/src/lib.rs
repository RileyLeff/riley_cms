//! # riley-core
//!
//! Core library for riley_cms - a minimal, self-hosted headless CMS.
//!
//! This crate provides the domain logic for riley_cms without any HTTP or CLI concerns.
//! It can be embedded in other Rust applications or used standalone.
//!
//! ## Features
//!
//! - **Content Management**: Parse and query posts and series from a Git-based content directory
//! - **S3/R2 Storage**: Upload and list assets from S3-compatible storage
//! - **In-Memory Caching**: Fast content access with cache refresh on demand
//! - **Visibility Control**: Support for drafts, scheduled posts, and live content
//!
//! ## Quick Start
//!
//! ```ignore
//! use riley_core::{Riley, resolve_config, ListOptions};
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     // Load config from standard locations
//!     let config = resolve_config(None)?;
//!
//!     // Create the Riley instance
//!     let riley = Riley::from_config(config).await?;
//!
//!     // List all live posts
//!     let posts = riley.list_posts(&ListOptions::default()).await?;
//!     for post in posts.items {
//!         println!("{}: {}", post.slug, post.title);
//!     }
//!
//!     // Get a specific post
//!     if let Some(post) = riley.get_post("my-post").await? {
//!         println!("Content: {}", post.content);
//!     }
//!
//!     Ok(())
//! }
//! ```
//!
//! ## Content Structure
//!
//! riley_cms expects content in this structure:
//!
//! ```text
//! content/
//! ├── my-post/
//! │   ├── config.toml
//! │   └── content.mdx
//! └── my-series/
//!     ├── series.toml
//!     ├── part-one/
//!     │   ├── config.toml
//!     │   └── content.mdx
//!     └── part-two/
//!         ├── config.toml
//!         └── content.mdx
//! ```
//!
//! ## Visibility Model
//!
//! Content visibility is controlled by the `goes_live_at` field:
//!
//! - `None` → Draft (only visible with `include_drafts`)
//! - `Some(past_date)` → Live (always visible)
//! - `Some(future_date)` → Scheduled (only visible with `include_scheduled`)

mod config;
mod content;
mod error;
pub mod git;
mod storage;
mod types;

pub use config::{Config, RileyConfig, resolve_config};
pub use content::ContentCache;
pub use error::{Error, Result};
pub use git::{GitBackend, GitCgiResponse};
pub use storage::Storage;
pub use types::*;

use std::path::Path;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Main entry point for riley_cms functionality.
///
/// `Riley` provides access to all CMS operations: listing posts and series,
/// retrieving individual content, managing assets, and cache control.
///
/// # Example
///
/// ```ignore
/// use riley_core::{Riley, RileyConfig, ListOptions};
///
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let config: RileyConfig = toml::from_str(r#"
/// [content]
/// repo_path = "/data/content"
/// [storage]
/// bucket = "my-assets"
/// public_url_base = "https://assets.example.com"
/// "#)?;
///
/// let riley = Riley::from_config(config).await?;
/// let posts = riley.list_posts(&ListOptions::default()).await?;
/// # Ok(())
/// # }
/// ```
pub struct Riley {
    config: RileyConfig,
    cache: Arc<RwLock<ContentCache>>,
    storage: Storage,
}

impl Riley {
    /// Create a new Riley instance from configuration.
    ///
    /// This loads content from disk into an in-memory cache and initializes
    /// the S3 storage client.
    ///
    /// # Errors
    ///
    /// Returns an error if content cannot be loaded or S3 configuration is invalid.
    pub async fn from_config(config: RileyConfig) -> Result<Self> {
        let storage = Storage::new(&config.storage).await?;

        // Clone content config to move into the blocking task closure
        let content_config = config.content.clone();

        // Offload blocking filesystem I/O to a dedicated thread pool
        let cache = tokio::task::spawn_blocking(move || ContentCache::load(&content_config))
            .await
            .map_err(|e| Error::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))??;

        Ok(Self {
            config,
            cache: Arc::new(RwLock::new(cache)),
            storage,
        })
    }

    /// List posts with filtering and pagination.
    ///
    /// By default, only live posts (with `goes_live_at` in the past) are returned.
    /// Use [`ListOptions`] to include drafts or scheduled posts.
    ///
    /// Posts are sorted by `goes_live_at` descending (newest first).
    pub async fn list_posts(&self, opts: &ListOptions) -> Result<ListResult<PostSummary>> {
        let cache = self.cache.read().await;
        cache.list_posts(opts)
    }

    /// Get a single post by its slug.
    ///
    /// Returns `None` if no post with the given slug exists.
    /// Note: This returns the post regardless of visibility status.
    pub async fn get_post(&self, slug: &str) -> Result<Option<Post>> {
        let cache = self.cache.read().await;
        cache.get_post(slug)
    }

    /// List series with filtering and pagination.
    ///
    /// By default, only live series are returned.
    /// Series are sorted by `goes_live_at` descending.
    pub async fn list_series(&self, opts: &ListOptions) -> Result<ListResult<SeriesSummary>> {
        let cache = self.cache.read().await;
        cache.list_series(opts)
    }

    /// Get a single series by its slug, including all posts.
    ///
    /// Posts within the series are sorted by their `order` field,
    /// with alphabetical fallback for ties or missing values.
    pub async fn get_series(&self, slug: &str) -> Result<Option<Series>> {
        let cache = self.cache.read().await;
        cache.get_series(slug)
    }

    /// Validate content structure and return any errors.
    ///
    /// Checks for common issues like empty titles, missing content, etc.
    pub async fn validate_content(&self) -> Result<Vec<ValidationError>> {
        let cache = self.cache.read().await;
        Ok(cache.validate())
    }

    /// List all assets in the S3/R2 storage bucket.
    pub async fn list_assets(&self) -> Result<Vec<Asset>> {
        self.storage.list_assets().await
    }

    /// Upload a file to the storage bucket.
    ///
    /// # Arguments
    ///
    /// * `path` - Local file path to upload
    /// * `dest` - Optional destination path in bucket (defaults to filename)
    pub async fn upload_asset(&self, path: &Path, dest: Option<&str>) -> Result<Asset> {
        self.storage.upload_asset(path, dest).await
    }

    /// Refresh the content cache from disk.
    ///
    /// Call this after content has been updated (e.g., after a git push)
    /// to reload the in-memory cache.
    pub async fn refresh(&self) -> Result<()> {
        // Clone the config to move into the blocking task closure
        let content_config = self.config.content.clone();

        // Offload blocking filesystem I/O to a dedicated thread pool
        let new_cache = tokio::task::spawn_blocking(move || ContentCache::load(&content_config))
            .await
            .map_err(|e| Error::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))??;

        let mut cache = self.cache.write().await;
        *cache = new_cache;
        Ok(())
    }

    /// Get an ETag representing the current content state.
    ///
    /// This is a hash of all content, suitable for HTTP caching headers.
    /// The ETag changes when any content is modified.
    pub async fn content_etag(&self) -> String {
        let cache = self.cache.read().await;
        cache.etag()
    }

    /// Fire webhooks after content update.
    pub async fn fire_webhooks(&self) {
        if let Some(ref webhooks) = self.config.webhooks {
            for url in &webhooks.on_content_update {
                let url = url.clone();
                tokio::spawn(async move {
                    if let Err(e) = reqwest::Client::new()
                        .post(&url)
                        .header("Content-Type", "application/json")
                        .body("{}")
                        .send()
                        .await
                    {
                        tracing::warn!("Webhook failed for {}: {}", url, e);
                    }
                });
            }
        }
    }

    /// Get a reference to the config.
    pub fn config(&self) -> &RileyConfig {
        &self.config
    }
}
