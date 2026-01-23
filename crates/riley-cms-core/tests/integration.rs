//! Integration tests for riley-cms-core
//!
//! These tests verify the full RileyCms workflow works end-to-end.

use riley_cms_core::{ListOptions, RileyCms, RileyCmsConfig};
use std::fs;
use tempfile::TempDir;

fn create_test_config(temp_dir: &TempDir) -> RileyCmsConfig {
    // Parse a minimal config
    let toml = format!(
        r#"
[content]
repo_path = "{}"
content_dir = "content"

[storage]
bucket = "test-bucket"
public_url_base = "https://test.example.com"
"#,
        temp_dir.path().display()
    );
    toml::from_str(&toml).unwrap()
}

fn create_post(dir: &std::path::Path, slug: &str, title: &str, goes_live_at: Option<&str>) {
    let post_dir = dir.join(slug);
    fs::create_dir_all(&post_dir).unwrap();

    let date_line = goes_live_at
        .map(|d| format!("goes_live_at = \"{}\"", d))
        .unwrap_or_default();

    fs::write(
        post_dir.join("config.toml"),
        format!(
            r#"title = "{}"
preview_text = "Preview for {}"
{}
"#,
            title, title, date_line
        ),
    )
    .unwrap();

    fs::write(
        post_dir.join("content.mdx"),
        format!("# {}\n\nContent for this post.", title),
    )
    .unwrap();
}

fn create_series(
    dir: &std::path::Path,
    slug: &str,
    title: &str,
    posts: &[(&str, &str, i32)], // (slug, title, order)
) {
    let series_dir = dir.join(slug);
    fs::create_dir_all(&series_dir).unwrap();

    fs::write(
        series_dir.join("series.toml"),
        format!(
            r#"title = "{}"
description = "Description for {}"
goes_live_at = "2025-01-01T00:00:00Z"
"#,
            title, title
        ),
    )
    .unwrap();

    for (post_slug, post_title, order) in posts {
        let post_dir = series_dir.join(post_slug);
        fs::create_dir_all(&post_dir).unwrap();

        fs::write(
            post_dir.join("config.toml"),
            format!(
                r#"title = "{}"
preview_text = "Preview"
goes_live_at = "2025-01-01T00:00:00Z"
order = {}
"#,
                post_title, order
            ),
        )
        .unwrap();

        fs::write(post_dir.join("content.mdx"), format!("# {}", post_title)).unwrap();
    }
}

#[tokio::test]
async fn test_riley_cms_with_empty_content() {
    let temp_dir = TempDir::new().unwrap();
    let config = create_test_config(&temp_dir);

    let riley_cms = RileyCms::from_config(config).await.unwrap();

    let posts = riley_cms.list_posts(&ListOptions::default()).await.unwrap();
    assert_eq!(posts.total, 0);
    assert!(posts.items.is_empty());

    let series = riley_cms
        .list_series(&ListOptions::default())
        .await
        .unwrap();
    assert_eq!(series.total, 0);
}

#[tokio::test]
async fn test_riley_cms_list_posts() {
    let temp_dir = TempDir::new().unwrap();
    let content_dir = temp_dir.path().join("content");

    // Create 3 live posts
    create_post(
        &content_dir,
        "post-a",
        "Post A",
        Some("2020-01-01T00:00:00Z"),
    );
    create_post(
        &content_dir,
        "post-b",
        "Post B",
        Some("2020-02-01T00:00:00Z"),
    );
    create_post(
        &content_dir,
        "post-c",
        "Post C",
        Some("2020-03-01T00:00:00Z"),
    );

    let config = create_test_config(&temp_dir);
    let riley_cms = RileyCms::from_config(config).await.unwrap();

    let posts = riley_cms.list_posts(&ListOptions::default()).await.unwrap();
    assert_eq!(posts.total, 3);
    assert_eq!(posts.items.len(), 3);

    // Should be sorted by date descending (newest first)
    assert_eq!(posts.items[0].slug, "post-c");
    assert_eq!(posts.items[1].slug, "post-b");
    assert_eq!(posts.items[2].slug, "post-a");
}

#[tokio::test]
async fn test_riley_cms_get_post() {
    let temp_dir = TempDir::new().unwrap();
    let content_dir = temp_dir.path().join("content");

    create_post(
        &content_dir,
        "my-post",
        "My Post",
        Some("2020-01-01T00:00:00Z"),
    );

    let config = create_test_config(&temp_dir);
    let riley_cms = RileyCms::from_config(config).await.unwrap();

    let post = riley_cms.get_post("my-post").await.unwrap().unwrap();
    assert_eq!(post.slug, "my-post");
    assert_eq!(post.title, "My Post");
    assert!(post.content.contains("# My Post"));

    // Non-existent post
    let missing = riley_cms.get_post("nonexistent").await.unwrap();
    assert!(missing.is_none());
}

#[tokio::test]
async fn test_riley_cms_drafts_and_scheduled() {
    let temp_dir = TempDir::new().unwrap();
    let content_dir = temp_dir.path().join("content");

    // Live post (past date)
    create_post(&content_dir, "live", "Live", Some("2020-01-01T00:00:00Z"));
    // Draft post (no date)
    create_post(&content_dir, "draft", "Draft", None);
    // Scheduled post (future date)
    create_post(
        &content_dir,
        "scheduled",
        "Scheduled",
        Some("2099-01-01T00:00:00Z"),
    );

    let config = create_test_config(&temp_dir);
    let riley_cms = RileyCms::from_config(config).await.unwrap();

    // Default: only live
    let posts = riley_cms.list_posts(&ListOptions::default()).await.unwrap();
    assert_eq!(posts.total, 1);
    assert_eq!(posts.items[0].slug, "live");

    // Include drafts
    let posts = riley_cms
        .list_posts(&ListOptions {
            include_drafts: true,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(posts.total, 2); // live + draft

    // Include scheduled
    let posts = riley_cms
        .list_posts(&ListOptions {
            include_scheduled: true,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(posts.total, 2); // live + scheduled

    // Include both
    let posts = riley_cms
        .list_posts(&ListOptions {
            include_drafts: true,
            include_scheduled: true,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(posts.total, 3); // all
}

#[tokio::test]
async fn test_riley_cms_series() {
    let temp_dir = TempDir::new().unwrap();
    let content_dir = temp_dir.path().join("content");

    create_series(
        &content_dir,
        "rust-series",
        "Learning Rust",
        &[
            ("intro", "Introduction", 1),
            ("advanced", "Advanced Topics", 3),
            ("basics", "Basic Concepts", 2),
        ],
    );

    let config = create_test_config(&temp_dir);
    let riley_cms = RileyCms::from_config(config).await.unwrap();

    // List series
    let series_list = riley_cms
        .list_series(&ListOptions::default())
        .await
        .unwrap();
    assert_eq!(series_list.total, 1);
    assert_eq!(series_list.items[0].slug, "rust-series");
    assert_eq!(series_list.items[0].post_count, 3);

    // Get series with posts
    let series = riley_cms.get_series("rust-series").await.unwrap().unwrap();
    assert_eq!(series.title, "Learning Rust");
    assert_eq!(series.posts.len(), 3);

    // Posts should be ordered by explicit order field
    assert_eq!(series.posts[0].slug, "intro"); // order 1
    assert_eq!(series.posts[1].slug, "basics"); // order 2
    assert_eq!(series.posts[2].slug, "advanced"); // order 3
}

#[tokio::test]
async fn test_riley_cms_pagination() {
    let temp_dir = TempDir::new().unwrap();
    let content_dir = temp_dir.path().join("content");

    // Create 5 posts
    for i in 1..=5 {
        create_post(
            &content_dir,
            &format!("post-{}", i),
            &format!("Post {}", i),
            Some("2020-01-01T00:00:00Z"),
        );
    }

    let config = create_test_config(&temp_dir);
    let riley_cms = RileyCms::from_config(config).await.unwrap();

    // Limit to 2
    let posts = riley_cms
        .list_posts(&ListOptions {
            limit: Some(2),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(posts.items.len(), 2);
    assert_eq!(posts.total, 5);
    assert_eq!(posts.limit, 2);
    assert_eq!(posts.offset, 0);

    // Offset 2, limit 2
    let posts = riley_cms
        .list_posts(&ListOptions {
            limit: Some(2),
            offset: Some(2),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(posts.items.len(), 2);
    assert_eq!(posts.offset, 2);
}

#[tokio::test]
async fn test_riley_cms_content_validation() {
    let temp_dir = TempDir::new().unwrap();
    let content_dir = temp_dir.path().join("content");

    // Create a valid post
    create_post(
        &content_dir,
        "valid",
        "Valid Post",
        Some("2020-01-01T00:00:00Z"),
    );

    // Create an invalid post (empty title)
    let bad_post = content_dir.join("bad-post");
    fs::create_dir_all(&bad_post).unwrap();
    fs::write(
        bad_post.join("config.toml"),
        r#"title = ""
preview_text = "Preview"
"#,
    )
    .unwrap();
    fs::write(bad_post.join("content.mdx"), "# Content").unwrap();

    let config = create_test_config(&temp_dir);
    let riley_cms = RileyCms::from_config(config).await.unwrap();

    let errors = riley_cms.validate_content().await.unwrap();
    assert!(!errors.is_empty());
    assert!(errors.iter().any(|e| e.message.contains("Title")));
}

#[tokio::test]
async fn test_riley_cms_refresh() {
    let temp_dir = TempDir::new().unwrap();
    let content_dir = temp_dir.path().join("content");

    // Start with one post
    create_post(
        &content_dir,
        "post-1",
        "Post 1",
        Some("2020-01-01T00:00:00Z"),
    );

    let config = create_test_config(&temp_dir);
    let riley_cms = RileyCms::from_config(config).await.unwrap();

    let posts = riley_cms.list_posts(&ListOptions::default()).await.unwrap();
    assert_eq!(posts.total, 1);

    // Add another post
    create_post(
        &content_dir,
        "post-2",
        "Post 2",
        Some("2020-01-01T00:00:00Z"),
    );

    // Before refresh, still shows 1
    let posts = riley_cms.list_posts(&ListOptions::default()).await.unwrap();
    assert_eq!(posts.total, 1);

    // After refresh, shows 2
    riley_cms.refresh().await.unwrap();
    let posts = riley_cms.list_posts(&ListOptions::default()).await.unwrap();
    assert_eq!(posts.total, 2);
}

#[tokio::test]
async fn test_riley_cms_etag() {
    let temp_dir = TempDir::new().unwrap();
    let content_dir = temp_dir.path().join("content");

    create_post(
        &content_dir,
        "post-1",
        "Post 1",
        Some("2020-01-01T00:00:00Z"),
    );

    let config = create_test_config(&temp_dir);
    let riley_cms = RileyCms::from_config(config).await.unwrap();

    let etag1 = riley_cms.content_etag().await;
    assert!(!etag1.is_empty());
    assert!(etag1.starts_with('"'));
    assert!(etag1.ends_with('"'));

    // Same content = same etag
    let etag2 = riley_cms.content_etag().await;
    assert_eq!(etag1, etag2);

    // Modify content and refresh
    fs::write(content_dir.join("post-1/content.mdx"), "# Modified Content").unwrap();
    riley_cms.refresh().await.unwrap();

    let etag3 = riley_cms.content_etag().await;
    assert_ne!(etag1, etag3);
}
