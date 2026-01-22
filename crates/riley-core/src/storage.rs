//! S3/R2 storage operations for riley_cms

use crate::config::StorageConfig;
use crate::error::{Error, Result};
use crate::types::Asset;
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

        Ok(Self {
            client,
            config: config.clone(),
        })
    }

    /// List all assets in the bucket
    pub async fn list_assets(&self) -> Result<Vec<Asset>> {
        let mut assets = Vec::new();
        let mut continuation_token: Option<String> = None;

        loop {
            let mut request = self.client.list_objects_v2().bucket(&self.config.bucket);

            if let Some(token) = continuation_token {
                request = request.continuation_token(token);
            }

            let response = request
                .send()
                .await
                .map_err(|e| Error::S3(format!("Failed to list objects: {}", e)))?;

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

            if response.is_truncated == Some(true) {
                continuation_token = response.next_continuation_token;
            } else {
                break;
            }
        }

        Ok(assets)
    }

    /// Upload an asset to the bucket
    pub async fn upload_asset(&self, path: &Path, dest: Option<&str>) -> Result<Asset> {
        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| Error::Storage("Invalid file name".to_string()))?;

        let key = match dest {
            Some(prefix) => format!("{}/{}", prefix.trim_matches('/'), file_name),
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
