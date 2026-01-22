//! Content parsing and caching for riley_cms

use crate::config::ContentConfig;
use crate::error::{Error, Result};
use crate::types::*;
use chrono::Utc;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::path::Path;

/// In-memory cache of parsed content
#[derive(Debug)]
pub struct ContentCache {
    posts: HashMap<String, Post>,
    series: HashMap<String, SeriesData>,
    etag: String,
}

/// Internal series data with owned posts
#[derive(Debug)]
struct SeriesData {
    slug: String,
    config: SeriesConfig,
    post_slugs: Vec<String>,
}

impl ContentCache {
    /// Load content from disk into cache
    pub fn load(config: &ContentConfig) -> Result<Self> {
        let content_path = config.repo_path.join(&config.content_dir);

        if !content_path.exists() {
            return Ok(Self {
                posts: HashMap::new(),
                series: HashMap::new(),
                etag: Self::compute_etag(&HashMap::new(), &HashMap::new()),
            });
        }

        let mut posts = HashMap::new();
        let mut series = HashMap::new();
        let mut errors = 0u32;

        // Iterate through content directory
        for entry in fs::read_dir(&content_path)? {
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    tracing::warn!("Failed to read directory entry: {}", e);
                    errors += 1;
                    continue;
                }
            };
            let path = entry.path();

            if !path.is_dir() {
                continue;
            }

            let slug = match path.file_name().and_then(|n| n.to_str()) {
                Some(s) => s.to_string(),
                None => {
                    tracing::warn!("Skipping directory with invalid name: {:?}", path);
                    errors += 1;
                    continue;
                }
            };

            // Check if this is a series (has series.toml)
            let series_toml = path.join("series.toml");
            if series_toml.exists() {
                match Self::load_series(&path, &slug) {
                    Ok((series_data, series_posts)) => {
                        series.insert(slug.clone(), series_data);
                        for post in series_posts {
                            posts.insert(post.slug.clone(), post);
                        }
                    }
                    Err(e) => {
                        tracing::error!("Failed to load series '{}': {}", slug, e);
                        errors += 1;
                    }
                }
            } else {
                // Check if this is a post (has config.toml + content.mdx)
                let config_toml = path.join("config.toml");
                let content_mdx = path.join("content.mdx");

                if config_toml.exists() && content_mdx.exists() {
                    match Self::load_post(&path, &slug, None) {
                        Ok(post) => {
                            posts.insert(slug, post);
                        }
                        Err(e) => {
                            tracing::error!("Failed to load post '{}': {}", slug, e);
                            errors += 1;
                        }
                    }
                }
            }
        }

        if errors > 0 {
            tracing::warn!(
                "Content loaded with {} error(s): {} posts, {} series",
                errors,
                posts.len(),
                series.len()
            );
        } else {
            tracing::info!(
                "Content loaded: {} posts, {} series",
                posts.len(),
                series.len()
            );
        }

        let etag = Self::compute_etag(&posts, &series);

        Ok(Self {
            posts,
            series,
            etag,
        })
    }

    /// Load a single post from a directory
    fn load_post(path: &Path, slug: &str, series_slug: Option<&str>) -> Result<Post> {
        let config_path = path.join("config.toml");
        let content_path = path.join("content.mdx");

        let config_str = fs::read_to_string(&config_path)?;
        let config: PostConfig = toml::from_str(&config_str).map_err(|e| Error::Content {
            path: config_path.clone(),
            message: e.to_string(),
        })?;

        let content = fs::read_to_string(&content_path)?;

        Ok(Post {
            slug: slug.to_string(),
            title: config.title,
            subtitle: config.subtitle,
            preview_text: config.preview_text,
            preview_image: config.preview_image,
            tags: config.tags,
            goes_live_at: config.goes_live_at,
            series_slug: series_slug.map(String::from),
            content,
            order: config.order,
        })
    }

    /// Load a series and its posts
    fn load_series(path: &Path, slug: &str) -> Result<(SeriesData, Vec<Post>)> {
        let series_toml = path.join("series.toml");
        let series_str = fs::read_to_string(&series_toml)?;
        let config: SeriesConfig = toml::from_str(&series_str).map_err(|e| Error::Content {
            path: series_toml.clone(),
            message: e.to_string(),
        })?;

        let mut posts = Vec::new();
        let mut post_slugs = Vec::new();

        // Load posts within the series
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let post_path = entry.path();

            if !post_path.is_dir() {
                continue;
            }

            let post_slug = post_path
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| Error::Content {
                    path: post_path.clone(),
                    message: "Invalid directory name".to_string(),
                })?
                .to_string();

            let config_toml = post_path.join("config.toml");
            let content_mdx = post_path.join("content.mdx");

            if config_toml.exists() && content_mdx.exists() {
                let post = Self::load_post(&post_path, &post_slug, Some(slug))?;
                post_slugs.push(post_slug);
                posts.push(post);
            }
        }

        // Sort posts by order field, then alphabetically by slug
        posts.sort_by(|a, b| {
            let order_a = Self::get_post_order(a);
            let order_b = Self::get_post_order(b);
            match (order_a, order_b) {
                (Some(oa), Some(ob)) => oa.cmp(&ob),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => a.slug.cmp(&b.slug),
            }
        });

        post_slugs = posts.iter().map(|p| p.slug.clone()).collect();

        let series_data = SeriesData {
            slug: slug.to_string(),
            config,
            post_slugs,
        };

        Ok((series_data, posts))
    }

    /// Helper to get post order
    fn get_post_order(post: &Post) -> Option<i32> {
        post.order
    }

    /// Compute ETag from content
    fn compute_etag(posts: &HashMap<String, Post>, series: &HashMap<String, SeriesData>) -> String {
        let mut hasher = Sha256::new();

        // Sort keys for deterministic output
        let mut post_keys: Vec<_> = posts.keys().collect();
        post_keys.sort();
        for key in post_keys {
            if let Some(post) = posts.get(key) {
                hasher.update(key.as_bytes());
                hasher.update(post.content.as_bytes());
            }
        }

        let mut series_keys: Vec<_> = series.keys().collect();
        series_keys.sort();
        for key in series_keys {
            hasher.update(key.as_bytes());
        }

        let result = hasher.finalize();
        format!("\"{}\"", hex::encode(result))
    }

    /// Get ETag for HTTP caching
    pub fn etag(&self) -> String {
        self.etag.clone()
    }

    /// Maximum number of items returned in a single list request.
    const MAX_PAGE_SIZE: usize = 500;

    /// List posts with filtering and pagination
    pub fn list_posts(&self, opts: &ListOptions) -> Result<ListResult<PostSummary>> {
        let now = Utc::now();
        let limit = opts.limit.unwrap_or(50).min(Self::MAX_PAGE_SIZE);
        let offset = opts.offset.unwrap_or(0);

        let mut filtered: Vec<_> = self
            .posts
            .values()
            .filter(|post| Self::is_visible(post.goes_live_at, opts, &now))
            .collect();

        // Sort by goes_live_at descending (newest first), drafts at end
        filtered.sort_by(|a, b| match (&b.goes_live_at, &a.goes_live_at) {
            (Some(b_date), Some(a_date)) => b_date.cmp(a_date),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a.slug.cmp(&b.slug),
        });

        let total = filtered.len();
        let items: Vec<PostSummary> = filtered
            .into_iter()
            .skip(offset)
            .take(limit)
            .map(|p| p.into())
            .collect();

        Ok(ListResult {
            items,
            total,
            limit,
            offset,
        })
    }

    /// Get a single post by slug
    pub fn get_post(&self, slug: &str) -> Result<Option<Post>> {
        Ok(self.posts.get(slug).cloned())
    }

    /// List series with filtering and pagination
    pub fn list_series(&self, opts: &ListOptions) -> Result<ListResult<SeriesSummary>> {
        let now = Utc::now();
        let limit = opts.limit.unwrap_or(50).min(Self::MAX_PAGE_SIZE);
        let offset = opts.offset.unwrap_or(0);

        let mut filtered: Vec<_> = self
            .series
            .values()
            .filter(|s| Self::is_visible(s.config.goes_live_at, opts, &now))
            .collect();

        // Sort by goes_live_at descending
        filtered.sort_by(
            |a, b| match (&b.config.goes_live_at, &a.config.goes_live_at) {
                (Some(b_date), Some(a_date)) => b_date.cmp(a_date),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => a.slug.cmp(&b.slug),
            },
        );

        let total = filtered.len();
        let items: Vec<SeriesSummary> = filtered
            .into_iter()
            .skip(offset)
            .take(limit)
            .map(|s| SeriesSummary {
                slug: s.slug.clone(),
                title: s.config.title.clone(),
                description: s.config.description.clone(),
                preview_image: s.config.preview_image.clone(),
                goes_live_at: s.config.goes_live_at,
                post_count: s.post_slugs.len(),
            })
            .collect();

        Ok(ListResult {
            items,
            total,
            limit,
            offset,
        })
    }

    /// Get a single series by slug
    pub fn get_series(&self, slug: &str) -> Result<Option<Series>> {
        let series_data = match self.series.get(slug) {
            Some(s) => s,
            None => return Ok(None),
        };

        let posts: Vec<SeriesPostSummary> = series_data
            .post_slugs
            .iter()
            .filter_map(|post_slug| {
                self.posts.get(post_slug).map(|post| SeriesPostSummary {
                    slug: post.slug.clone(),
                    title: post.title.clone(),
                    subtitle: post.subtitle.clone(),
                    preview_text: post.preview_text.clone(),
                    preview_image: post.preview_image.clone(),
                    tags: post.tags.clone(),
                    goes_live_at: post.goes_live_at,
                    order: post.order,
                })
            })
            .collect();

        Ok(Some(Series {
            slug: series_data.slug.clone(),
            title: series_data.config.title.clone(),
            description: series_data.config.description.clone(),
            preview_image: series_data.config.preview_image.clone(),
            goes_live_at: series_data.config.goes_live_at,
            posts,
        }))
    }

    /// Check if content is visible based on goes_live_at and options
    fn is_visible(
        goes_live_at: Option<chrono::DateTime<Utc>>,
        opts: &ListOptions,
        now: &chrono::DateTime<Utc>,
    ) -> bool {
        match goes_live_at {
            None => opts.include_drafts,
            Some(date) if date > *now => opts.include_scheduled,
            Some(_) => true, // Live
        }
    }

    /// Validate content structure
    pub fn validate(&self) -> Vec<ValidationError> {
        let mut errors = Vec::new();

        for (slug, post) in &self.posts {
            if post.title.is_empty() {
                errors.push(ValidationError {
                    path: format!("{}/config.toml", slug),
                    message: "Title cannot be empty".to_string(),
                });
            }
            if post.preview_text.is_empty() {
                errors.push(ValidationError {
                    path: format!("{}/config.toml", slug),
                    message: "preview_text cannot be empty".to_string(),
                });
            }
            if post.content.is_empty() {
                errors.push(ValidationError {
                    path: format!("{}/content.mdx", slug),
                    message: "Content cannot be empty".to_string(),
                });
            }
        }

        for (slug, series) in &self.series {
            if series.config.title.is_empty() {
                errors.push(ValidationError {
                    path: format!("{}/series.toml", slug),
                    message: "Title cannot be empty".to_string(),
                });
            }
        }

        errors
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn create_content_config(temp_dir: &TempDir) -> ContentConfig {
        ContentConfig {
            repo_path: temp_dir.path().to_path_buf(),
            content_dir: "content".to_string(),
        }
    }

    fn create_post_files(dir: &Path, title: &str, preview: &str, content: &str) {
        fs::create_dir_all(dir).unwrap();
        fs::write(
            dir.join("config.toml"),
            format!(
                r#"title = "{}"
preview_text = "{}"
"#,
                title, preview
            ),
        )
        .unwrap();
        fs::write(dir.join("content.mdx"), content).unwrap();
    }

    fn create_post_with_date(dir: &Path, title: &str, goes_live_at: Option<&str>) {
        fs::create_dir_all(dir).unwrap();
        let date_line = goes_live_at
            .map(|d| format!("goes_live_at = \"{}\"", d))
            .unwrap_or_default();
        fs::write(
            dir.join("config.toml"),
            format!(
                r#"title = "{}"
preview_text = "Preview"
{}
"#,
                title, date_line
            ),
        )
        .unwrap();
        fs::write(dir.join("content.mdx"), "# Content").unwrap();
    }

    #[test]
    fn test_load_empty_content() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_content_config(&temp_dir);

        let cache = ContentCache::load(&config).unwrap();

        assert!(cache.posts.is_empty());
        assert!(cache.series.is_empty());
    }

    #[test]
    fn test_load_single_post() {
        let temp_dir = TempDir::new().unwrap();
        let content_dir = temp_dir.path().join("content");
        let post_dir = content_dir.join("my-post");

        create_post_files(&post_dir, "My Title", "Preview text", "# Hello World");

        let config = create_content_config(&temp_dir);
        let cache = ContentCache::load(&config).unwrap();

        assert_eq!(cache.posts.len(), 1);
        let post = cache.posts.get("my-post").unwrap();
        assert_eq!(post.slug, "my-post");
        assert_eq!(post.title, "My Title");
        assert_eq!(post.preview_text, "Preview text");
        assert_eq!(post.content, "# Hello World");
        assert!(post.series_slug.is_none());
    }

    #[test]
    fn test_load_series_with_posts() {
        let temp_dir = TempDir::new().unwrap();
        let content_dir = temp_dir.path().join("content");
        let series_dir = content_dir.join("my-series");

        fs::create_dir_all(&series_dir).unwrap();
        fs::write(
            series_dir.join("series.toml"),
            r#"title = "My Series"
description = "A test series"
"#,
        )
        .unwrap();

        let post1_dir = series_dir.join("part-one");
        fs::create_dir_all(&post1_dir).unwrap();
        fs::write(
            post1_dir.join("config.toml"),
            r#"title = "Part One"
preview_text = "First part"
order = 1
"#,
        )
        .unwrap();
        fs::write(post1_dir.join("content.mdx"), "# Part 1").unwrap();

        let post2_dir = series_dir.join("part-two");
        fs::create_dir_all(&post2_dir).unwrap();
        fs::write(
            post2_dir.join("config.toml"),
            r#"title = "Part Two"
preview_text = "Second part"
order = 2
"#,
        )
        .unwrap();
        fs::write(post2_dir.join("content.mdx"), "# Part 2").unwrap();

        let config = create_content_config(&temp_dir);
        let cache = ContentCache::load(&config).unwrap();

        assert_eq!(cache.series.len(), 1);
        assert_eq!(cache.posts.len(), 2);

        let series = cache.get_series("my-series").unwrap().unwrap();
        assert_eq!(series.title, "My Series");
        assert_eq!(series.posts.len(), 2);
        assert_eq!(series.posts[0].slug, "part-one");
        assert_eq!(series.posts[1].slug, "part-two");

        // Check posts have series_slug set
        let post = cache.posts.get("part-one").unwrap();
        assert_eq!(post.series_slug, Some("my-series".to_string()));
    }

    #[test]
    fn test_series_ordering() {
        let temp_dir = TempDir::new().unwrap();
        let content_dir = temp_dir.path().join("content");
        let series_dir = content_dir.join("ordered-series");

        fs::create_dir_all(&series_dir).unwrap();
        fs::write(series_dir.join("series.toml"), "title = \"Ordered\"").unwrap();

        // Create posts with explicit ordering (not alphabetical)
        for (name, order) in [("zebra", 1), ("apple", 3), ("middle", 2)] {
            let post_dir = series_dir.join(name);
            fs::create_dir_all(&post_dir).unwrap();
            fs::write(
                post_dir.join("config.toml"),
                format!(
                    r#"title = "{}"
preview_text = "test"
order = {}
"#,
                    name, order
                ),
            )
            .unwrap();
            fs::write(post_dir.join("content.mdx"), "content").unwrap();
        }

        let config = create_content_config(&temp_dir);
        let cache = ContentCache::load(&config).unwrap();
        let series = cache.get_series("ordered-series").unwrap().unwrap();

        assert_eq!(series.posts[0].slug, "zebra"); // order 1
        assert_eq!(series.posts[1].slug, "middle"); // order 2
        assert_eq!(series.posts[2].slug, "apple"); // order 3
    }

    #[test]
    fn test_visibility_filtering_live() {
        let temp_dir = TempDir::new().unwrap();
        let content_dir = temp_dir.path().join("content");

        // Live post (past date)
        create_post_with_date(
            &content_dir.join("live-post"),
            "Live",
            Some("2020-01-01T00:00:00Z"),
        );

        let config = create_content_config(&temp_dir);
        let cache = ContentCache::load(&config).unwrap();

        // Default options - should see live posts
        let opts = ListOptions::default();
        let result = cache.list_posts(&opts).unwrap();
        assert_eq!(result.items.len(), 1);
        assert_eq!(result.items[0].title, "Live");
    }

    #[test]
    fn test_visibility_filtering_drafts() {
        let temp_dir = TempDir::new().unwrap();
        let content_dir = temp_dir.path().join("content");

        // Draft post (no date)
        create_post_with_date(&content_dir.join("draft-post"), "Draft", None);

        let config = create_content_config(&temp_dir);
        let cache = ContentCache::load(&config).unwrap();

        // Default - no drafts
        let opts = ListOptions::default();
        let result = cache.list_posts(&opts).unwrap();
        assert_eq!(result.items.len(), 0);

        // With include_drafts
        let opts = ListOptions {
            include_drafts: true,
            ..Default::default()
        };
        let result = cache.list_posts(&opts).unwrap();
        assert_eq!(result.items.len(), 1);
    }

    #[test]
    fn test_visibility_filtering_scheduled() {
        let temp_dir = TempDir::new().unwrap();
        let content_dir = temp_dir.path().join("content");

        // Scheduled post (future date)
        create_post_with_date(
            &content_dir.join("scheduled-post"),
            "Scheduled",
            Some("2099-01-01T00:00:00Z"),
        );

        let config = create_content_config(&temp_dir);
        let cache = ContentCache::load(&config).unwrap();

        // Default - no scheduled
        let opts = ListOptions::default();
        let result = cache.list_posts(&opts).unwrap();
        assert_eq!(result.items.len(), 0);

        // With include_scheduled
        let opts = ListOptions {
            include_scheduled: true,
            ..Default::default()
        };
        let result = cache.list_posts(&opts).unwrap();
        assert_eq!(result.items.len(), 1);
    }

    #[test]
    fn test_pagination() {
        let temp_dir = TempDir::new().unwrap();
        let content_dir = temp_dir.path().join("content");

        // Create 5 live posts
        for i in 1..=5 {
            create_post_with_date(
                &content_dir.join(format!("post-{}", i)),
                &format!("Post {}", i),
                Some("2020-01-01T00:00:00Z"),
            );
        }

        let config = create_content_config(&temp_dir);
        let cache = ContentCache::load(&config).unwrap();

        // Limit to 2
        let opts = ListOptions {
            limit: Some(2),
            ..Default::default()
        };
        let result = cache.list_posts(&opts).unwrap();
        assert_eq!(result.items.len(), 2);
        assert_eq!(result.total, 5);
        assert_eq!(result.limit, 2);
        assert_eq!(result.offset, 0);

        // Offset 2, limit 2
        let opts = ListOptions {
            limit: Some(2),
            offset: Some(2),
            ..Default::default()
        };
        let result = cache.list_posts(&opts).unwrap();
        assert_eq!(result.items.len(), 2);
        assert_eq!(result.offset, 2);
    }

    #[test]
    fn test_etag_changes_with_content() {
        let temp_dir = TempDir::new().unwrap();
        let content_dir = temp_dir.path().join("content");
        let post_dir = content_dir.join("my-post");

        create_post_files(&post_dir, "Title", "Preview", "Content v1");

        let config = create_content_config(&temp_dir);
        let cache1 = ContentCache::load(&config).unwrap();
        let etag1 = cache1.etag();

        // Modify content
        fs::write(post_dir.join("content.mdx"), "Content v2").unwrap();
        let cache2 = ContentCache::load(&config).unwrap();
        let etag2 = cache2.etag();

        assert_ne!(etag1, etag2);
    }

    #[test]
    fn test_validation_empty_title() {
        let temp_dir = TempDir::new().unwrap();
        let content_dir = temp_dir.path().join("content");
        let post_dir = content_dir.join("bad-post");

        fs::create_dir_all(&post_dir).unwrap();
        fs::write(
            post_dir.join("config.toml"),
            r#"title = ""
preview_text = "Preview"
"#,
        )
        .unwrap();
        fs::write(post_dir.join("content.mdx"), "# Content").unwrap();

        let config = create_content_config(&temp_dir);
        let cache = ContentCache::load(&config).unwrap();
        let errors = cache.validate();

        assert!(!errors.is_empty());
        assert!(errors.iter().any(|e| e.message.contains("Title")));
    }

    #[test]
    fn test_validation_empty_content() {
        let temp_dir = TempDir::new().unwrap();
        let content_dir = temp_dir.path().join("content");
        let post_dir = content_dir.join("empty-content");

        fs::create_dir_all(&post_dir).unwrap();
        fs::write(
            post_dir.join("config.toml"),
            r#"title = "Title"
preview_text = "Preview"
"#,
        )
        .unwrap();
        fs::write(post_dir.join("content.mdx"), "").unwrap();

        let config = create_content_config(&temp_dir);
        let cache = ContentCache::load(&config).unwrap();
        let errors = cache.validate();

        assert!(!errors.is_empty());
        assert!(errors.iter().any(|e| e.message.contains("Content")));
    }

    #[test]
    fn test_get_nonexistent_post() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_content_config(&temp_dir);
        let cache = ContentCache::load(&config).unwrap();

        let result = cache.get_post("nonexistent").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_get_nonexistent_series() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_content_config(&temp_dir);
        let cache = ContentCache::load(&config).unwrap();

        let result = cache.get_series("nonexistent").unwrap();
        assert!(result.is_none());
    }
}
