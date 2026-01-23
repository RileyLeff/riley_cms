//! Domain types for riley_cms

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// === Config types (deserialized from TOML) ===

/// Post configuration from config.toml
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PostConfig {
    pub title: String,
    pub subtitle: Option<String>,
    pub preview_text: String,
    pub preview_image: Option<String>,
    pub tags: Option<Vec<String>>,
    /// None = draft, Some(past) = live, Some(future) = scheduled
    pub goes_live_at: Option<DateTime<Utc>>,
    /// For series posts; alphabetical fallback for ties/missing
    pub order: Option<i32>,
}

/// Series configuration from series.toml
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SeriesConfig {
    pub title: String,
    pub description: Option<String>,
    pub preview_image: Option<String>,
    /// None = draft, Some(past) = live, Some(future) = scheduled
    pub goes_live_at: Option<DateTime<Utc>>,
}

// === Domain types ===

/// A blog post with full content
#[derive(Debug, Clone, Serialize)]
pub struct Post {
    pub slug: String,
    pub title: String,
    pub subtitle: Option<String>,
    pub preview_text: String,
    pub preview_image: Option<String>,
    pub tags: Option<Vec<String>>,
    pub goes_live_at: Option<DateTime<Utc>>,
    pub series_slug: Option<String>,
    pub content: String,
    /// Order within a series (not serialized in API responses for standalone posts)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub order: Option<i32>,
}

/// Post summary without content (for list endpoints)
#[derive(Debug, Clone, Serialize)]
pub struct PostSummary {
    pub slug: String,
    pub title: String,
    pub subtitle: Option<String>,
    pub preview_text: String,
    pub preview_image: Option<String>,
    pub tags: Option<Vec<String>>,
    pub goes_live_at: Option<DateTime<Utc>>,
    pub series_slug: Option<String>,
}

impl From<&Post> for PostSummary {
    fn from(post: &Post) -> Self {
        Self {
            slug: post.slug.clone(),
            title: post.title.clone(),
            subtitle: post.subtitle.clone(),
            preview_text: post.preview_text.clone(),
            preview_image: post.preview_image.clone(),
            tags: post.tags.clone(),
            goes_live_at: post.goes_live_at,
            series_slug: post.series_slug.clone(),
        }
    }
}

/// A series with its posts
#[derive(Debug, Clone, Serialize)]
pub struct Series {
    pub slug: String,
    pub title: String,
    pub description: Option<String>,
    pub preview_image: Option<String>,
    pub goes_live_at: Option<DateTime<Utc>>,
    pub posts: Vec<SeriesPostSummary>,
}

/// Series summary without posts (for list endpoints)
#[derive(Debug, Clone, Serialize)]
pub struct SeriesSummary {
    pub slug: String,
    pub title: String,
    pub description: Option<String>,
    pub preview_image: Option<String>,
    pub goes_live_at: Option<DateTime<Utc>>,
    pub post_count: usize,
}

/// Post summary within a series (includes order)
#[derive(Debug, Clone, Serialize)]
pub struct SeriesPostSummary {
    pub slug: String,
    pub title: String,
    pub subtitle: Option<String>,
    pub preview_text: String,
    pub preview_image: Option<String>,
    pub tags: Option<Vec<String>>,
    pub goes_live_at: Option<DateTime<Utc>>,
    pub order: Option<i32>,
}

/// An asset in the storage bucket
#[derive(Debug, Clone, Serialize)]
pub struct Asset {
    pub key: String,
    pub url: String,
    pub size: u64,
    pub last_modified: DateTime<Utc>,
}

// === API types ===

/// Options for listing content
#[derive(Debug, Clone, Default)]
pub struct ListOptions {
    pub include_drafts: bool,
    pub include_scheduled: bool,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
}

/// Paginated list result
#[derive(Debug, Clone, Serialize)]
pub struct ListResult<T> {
    pub items: Vec<T>,
    pub total: usize,
    pub limit: usize,
    pub offset: usize,
}

/// Options for listing assets with pagination
#[derive(Debug, Clone, Default)]
pub struct AssetListOptions {
    /// Maximum number of assets to return (default 100, max 1000)
    pub limit: Option<usize>,
    /// Continuation token from a previous response for fetching the next page
    pub continuation_token: Option<String>,
}

/// Paginated asset list result
#[derive(Debug, Clone, Serialize)]
pub struct AssetListResult {
    pub assets: Vec<Asset>,
    /// Token to pass as `continuation_token` to fetch the next page.
    /// `None` means there are no more results.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_continuation_token: Option<String>,
}

/// Content validation error
#[derive(Debug, Clone, Serialize)]
pub struct ValidationError {
    pub path: String,
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn test_post_to_summary_conversion() {
        let post = Post {
            slug: "test-post".to_string(),
            title: "Test Title".to_string(),
            subtitle: Some("Subtitle".to_string()),
            preview_text: "Preview".to_string(),
            preview_image: Some("https://example.com/img.jpg".to_string()),
            tags: Some(vec!["rust".to_string(), "test".to_string()]),
            goes_live_at: Some(Utc.with_ymd_and_hms(2025, 1, 15, 0, 0, 0).unwrap()),
            series_slug: Some("my-series".to_string()),
            content: "# Hello World".to_string(),
            order: Some(1),
        };

        let summary: PostSummary = (&post).into();

        assert_eq!(summary.slug, "test-post");
        assert_eq!(summary.title, "Test Title");
        assert_eq!(summary.subtitle, Some("Subtitle".to_string()));
        assert_eq!(summary.preview_text, "Preview");
        assert_eq!(
            summary.preview_image,
            Some("https://example.com/img.jpg".to_string())
        );
        assert_eq!(
            summary.tags,
            Some(vec!["rust".to_string(), "test".to_string()])
        );
        assert_eq!(summary.series_slug, Some("my-series".to_string()));
        // Content is not included in summary
    }

    #[test]
    fn test_post_serialization_omits_order_when_none() {
        let post = Post {
            slug: "test".to_string(),
            title: "Test".to_string(),
            subtitle: None,
            preview_text: "Preview".to_string(),
            preview_image: None,
            tags: None,
            goes_live_at: None,
            series_slug: None,
            content: "content".to_string(),
            order: None,
        };

        let json = serde_json::to_string(&post).unwrap();
        assert!(!json.contains("order"));
    }

    #[test]
    fn test_post_serialization_includes_order_when_some() {
        let post = Post {
            slug: "test".to_string(),
            title: "Test".to_string(),
            subtitle: None,
            preview_text: "Preview".to_string(),
            preview_image: None,
            tags: None,
            goes_live_at: None,
            series_slug: None,
            content: "content".to_string(),
            order: Some(5),
        };

        let json = serde_json::to_string(&post).unwrap();
        assert!(json.contains("\"order\":5"));
    }

    #[test]
    fn test_list_options_default() {
        let opts = ListOptions::default();
        assert!(!opts.include_drafts);
        assert!(!opts.include_scheduled);
        assert!(opts.limit.is_none());
        assert!(opts.offset.is_none());
    }

    #[test]
    fn test_post_config_deserialization() {
        let toml = r#"
title = "My Post"
preview_text = "A preview"
tags = ["rust", "test"]
goes_live_at = "2025-01-15T00:00:00Z"
order = 3
"#;
        let config: PostConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.title, "My Post");
        assert_eq!(config.preview_text, "A preview");
        assert_eq!(
            config.tags,
            Some(vec!["rust".to_string(), "test".to_string()])
        );
        assert!(config.goes_live_at.is_some());
        assert_eq!(config.order, Some(3));
        assert!(config.subtitle.is_none());
        assert!(config.preview_image.is_none());
    }

    #[test]
    fn test_series_config_deserialization() {
        let toml = r#"
title = "My Series"
description = "A series description"
"#;
        let config: SeriesConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.title, "My Series");
        assert_eq!(config.description, Some("A series description".to_string()));
        assert!(config.preview_image.is_none());
        assert!(config.goes_live_at.is_none());
    }

    #[test]
    fn test_list_result_serialization() {
        let result = ListResult {
            items: vec!["a".to_string(), "b".to_string()],
            total: 10,
            limit: 2,
            offset: 0,
        };

        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("\"total\":10"));
        assert!(json.contains("\"limit\":2"));
        assert!(json.contains("\"offset\":0"));
    }
}
