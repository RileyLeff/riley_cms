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

pub use config::{Config, GitConfig, RileyConfig, resolve_config};
pub use content::ContentCache;
pub use error::{Error, Result};
pub use git::{GitBackend, GitCgiResponse};
pub use storage::Storage;
pub use types::*;

use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::net::ToSocketAddrs;
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
            .map_err(|e| Error::Io(std::io::Error::other(e)))??;

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

    /// List assets in the S3/R2 storage bucket with pagination.
    ///
    /// Uses cursor-based pagination via S3 continuation tokens.
    /// Defaults to 100 assets per page, capped at 1000.
    pub async fn list_assets(&self, opts: &AssetListOptions) -> Result<AssetListResult> {
        self.storage.list_assets(opts).await
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
            .map_err(|e| Error::Io(std::io::Error::other(e)))??;

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
    ///
    /// Validates webhook URLs against private/internal IP ranges to prevent SSRF.
    /// If a `secret` is configured in `[webhooks]`, signs each request body with
    /// HMAC-SHA256 and includes the hex signature in the `X-Riley-Signature` header.
    /// Retries up to 3 times with exponential backoff on network errors or 5xx responses.
    pub async fn fire_webhooks(&self) {
        if let Some(ref webhooks) = self.config.webhooks {
            // Resolve webhook secret once (if configured)
            let secret = webhooks.secret.as_ref().and_then(|s| match s.resolve() {
                Ok(v) => Some(v),
                Err(e) => {
                    tracing::warn!("Failed to resolve webhook secret: {}. Sending unsigned.", e);
                    None
                }
            });

            for url in &webhooks.on_content_update {
                if let Err(reason) = validate_webhook_url(url) {
                    tracing::warn!("Skipping webhook {}: {}", url, reason);
                    continue;
                }
                let url = url.clone();
                let secret = secret.clone();
                tokio::spawn(async move {
                    send_webhook(&url, secret.as_deref()).await;
                });
            }
        }
    }

    /// Get a reference to the config.
    pub fn config(&self) -> &RileyConfig {
        &self.config
    }
}

/// Validate a webhook URL to prevent SSRF attacks.
///
/// Rejects URLs that resolve to private, loopback, or link-local IP addresses.
fn validate_webhook_url(url: &str) -> std::result::Result<(), String> {
    // Parse the URL to extract host and port
    let parsed = reqwest::Url::parse(url).map_err(|e| format!("invalid URL: {}", e))?;

    let scheme = parsed.scheme();
    if scheme != "http" && scheme != "https" {
        return Err(format!("unsupported scheme: {}", scheme));
    }

    let host = parsed.host_str().ok_or("missing host")?;
    let port = parsed.port_or_known_default().unwrap_or(443);

    // Resolve the hostname and check all resulting IPs
    let addr_str = format!("{}:{}", host, port);
    let addrs: Vec<_> = addr_str
        .to_socket_addrs()
        .map_err(|e| format!("DNS resolution failed: {}", e))?
        .collect();

    if addrs.is_empty() {
        return Err("hostname did not resolve to any addresses".to_string());
    }

    for addr in &addrs {
        let ip = addr.ip();
        if ip.is_loopback() || ip.is_unspecified() || is_private_ip(&ip) || is_link_local(&ip) {
            return Err(format!("resolves to private/internal IP: {}", ip));
        }
    }

    Ok(())
}

/// Check if an IP address is in a private range (RFC 1918 / RFC 4193).
fn is_private_ip(ip: &std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => {
            let octets = v4.octets();
            // 10.0.0.0/8
            octets[0] == 10
            // 172.16.0.0/12
            || (octets[0] == 172 && (16..=31).contains(&octets[1]))
            // 192.168.0.0/16
            || (octets[0] == 192 && octets[1] == 168)
            // 100.64.0.0/10 (Carrier-grade NAT)
            || (octets[0] == 100 && (64..=127).contains(&octets[1]))
        }
        std::net::IpAddr::V6(v6) => {
            let segments = v6.segments();
            // fc00::/7 (Unique Local Addresses)
            (segments[0] & 0xfe00) == 0xfc00
        }
    }
}

/// Check if an IP address is link-local (169.254.0.0/16 or fe80::/10).
fn is_link_local(ip: &std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => {
            let octets = v4.octets();
            // 169.254.0.0/16 (includes AWS metadata endpoint 169.254.169.254)
            octets[0] == 169 && octets[1] == 254
        }
        std::net::IpAddr::V6(v6) => {
            let segments = v6.segments();
            // fe80::/10
            (segments[0] & 0xffc0) == 0xfe80
        }
    }
}

/// Maximum number of retry attempts for webhook delivery.
const WEBHOOK_MAX_RETRIES: u32 = 3;

/// Send a single webhook with optional HMAC signing and retry with exponential backoff.
///
/// Retries on network errors or 5xx responses. Does not retry on 4xx (client errors)
/// since those indicate a problem with the receiver's configuration, not a transient issue.
async fn send_webhook(url: &str, secret: Option<&str>) {
    let body = "{}";

    // Compute HMAC signature if secret is configured
    let signature = secret.and_then(|s| {
        let mut mac = Hmac::<Sha256>::new_from_slice(s.as_bytes())
            .map_err(|e| tracing::warn!("Invalid webhook secret key: {}. Sending unsigned.", e))
            .ok()?;
        mac.update(body.as_bytes());
        Some(hex::encode(mac.finalize().into_bytes()))
    });

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    for attempt in 0..WEBHOOK_MAX_RETRIES {
        let mut request = client
            .post(url)
            .header("Content-Type", "application/json")
            .body(body);

        if let Some(ref sig) = signature {
            request = request.header("X-Riley-Signature", format!("sha256={}", sig));
        }

        match request.send().await {
            Ok(response) if response.status().is_success() => return,
            Ok(response) if response.status().is_client_error() => {
                // 4xx: don't retry, receiver has a config problem
                tracing::warn!(
                    "Webhook {} returned {} (not retrying)",
                    url,
                    response.status()
                );
                return;
            }
            Ok(response) => {
                // 5xx or other: retry
                tracing::warn!(
                    "Webhook {} returned {} (attempt {}/{})",
                    url,
                    response.status(),
                    attempt + 1,
                    WEBHOOK_MAX_RETRIES
                );
            }
            Err(e) => {
                tracing::warn!(
                    "Webhook {} failed: {} (attempt {}/{})",
                    url,
                    e,
                    attempt + 1,
                    WEBHOOK_MAX_RETRIES
                );
            }
        }

        // Exponential backoff: 1s, 2s, 4s
        if attempt < WEBHOOK_MAX_RETRIES - 1 {
            tokio::time::sleep(std::time::Duration::from_secs(1 << attempt)).await;
        }
    }

    tracing::error!(
        "Webhook {} failed after {} attempts",
        url,
        WEBHOOK_MAX_RETRIES
    );
}
