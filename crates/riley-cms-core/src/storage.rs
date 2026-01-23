//! S3/R2 storage operations for riley_cms

use crate::config::StorageConfig;
use crate::error::{Error, Result};
use crate::types::{Asset, AssetListOptions, AssetListResult};
use aws_sdk_s3::Client;
use aws_sdk_s3::primitives::ByteStream;
use chrono::{DateTime, Utc};
use std::path::Path;

/// Storage backend for assets
pub struct Storage {
    client: Client,
    config: StorageConfig,
}

impl Storage {
    /// Create a new storage backend
    pub async fn new(config: &StorageConfig) -> Result<Self> {
        let mut aws_config_builder = aws_config::from_env();

        // Set custom endpoint for R2 or other S3-compatible storage
        if let Some(endpoint) = &config.endpoint {
            aws_config_builder = aws_config_builder.endpoint_url(endpoint);
        }

        // Set region
        aws_config_builder =
            aws_config_builder.region(aws_config::Region::new(config.region.clone()));

        let aws_config = aws_config_builder.load().await;
        let client = Client::new(&aws_config);

        let storage = Self {
            client,
            config: config.clone(),
        };

        // Non-fatal connectivity check at startup
        if let Err(e) = storage.check_connectivity().await {
            tracing::warn!(
                "S3 connectivity check failed for bucket '{}': {}. Asset operations may fail.",
                config.bucket,
                e
            );
        }

        Ok(storage)
    }

    /// Check S3 connectivity by issuing a HeadBucket request.
    ///
    /// This is a lightweight check that verifies credentials and bucket access
    /// without listing or reading any objects.
    async fn check_connectivity(&self) -> Result<()> {
        self.client
            .head_bucket()
            .bucket(&self.config.bucket)
            .send()
            .await
            .map_err(|e| Error::S3(format!("HeadBucket failed: {}", e)))?;
        Ok(())
    }

    /// Maximum assets per page
    const MAX_PAGE_SIZE: usize = 1000;

    /// List assets in the bucket with pagination.
    ///
    /// Uses S3's native continuation token for efficient cursor-based pagination.
    /// Defaults to 100 assets per page, capped at 1000.
    pub async fn list_assets(&self, opts: &AssetListOptions) -> Result<AssetListResult> {
        let limit = opts.limit.unwrap_or(100).min(Self::MAX_PAGE_SIZE);

        let mut request = self
            .client
            .list_objects_v2()
            .bucket(&self.config.bucket)
            .max_keys(limit as i32);

        if let Some(ref token) = opts.continuation_token {
            request = request.continuation_token(token);
        }

        let response = request
            .send()
            .await
            .map_err(|e| Error::S3(format!("Failed to list objects: {}", e)))?;

        let mut assets = Vec::new();
        if let Some(contents) = response.contents {
            for obj in contents {
                let key = obj.key.unwrap_or_default();
                let size = obj.size.unwrap_or(0) as u64;
                let last_modified = obj
                    .last_modified
                    .and_then(|t| DateTime::from_timestamp(t.secs(), t.subsec_nanos()))
                    .unwrap_or_else(Utc::now);

                let url = format!(
                    "{}/{}",
                    self.config.public_url_base.trim_end_matches('/'),
                    key
                );

                assets.push(Asset {
                    key,
                    url,
                    size,
                    last_modified,
                });
            }
        }

        let next_continuation_token = if response.is_truncated == Some(true) {
            response.next_continuation_token
        } else {
            None
        };

        Ok(AssetListResult {
            assets,
            next_continuation_token,
        })
    }

    /// Upload an asset to the bucket
    pub async fn upload_asset(&self, path: &Path, dest: Option<&str>) -> Result<Asset> {
        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| Error::Storage("Invalid file name".to_string()))?;

        let key = match dest {
            Some(prefix) => {
                // Reject path traversal attempts in the destination prefix
                let sanitized = prefix.trim_matches('/');
                if sanitized.split('/').any(|seg| seg == "..") {
                    return Err(Error::Storage(
                        "Invalid destination: path traversal not allowed".to_string(),
                    ));
                }
                format!("{}/{}", sanitized, file_name)
            }
            None => file_name.to_string(),
        };

        let body = ByteStream::from_path(path)
            .await
            .map_err(|e| Error::Storage(format!("Failed to read file: {}", e)))?;

        // Detect content type
        let content_type = mime_guess::from_path(path)
            .first_or_octet_stream()
            .to_string();

        self.client
            .put_object()
            .bucket(&self.config.bucket)
            .key(&key)
            .body(body)
            .content_type(content_type)
            .send()
            .await
            .map_err(|e| Error::S3(format!("Failed to upload: {}", e)))?;

        let metadata = std::fs::metadata(path)?;
        let url = format!(
            "{}/{}",
            self.config.public_url_base.trim_end_matches('/'),
            key
        );

        Ok(Asset {
            key,
            url,
            size: metadata.len(),
            last_modified: Utc::now(),
        })
    }
}
