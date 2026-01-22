//! Integration tests for riley-api HTTP endpoints

use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use http_body_util::BodyExt;
use riley_api::{AppState, build_router};
use riley_core::{Riley, RileyConfig};
use serde_json::Value;
use std::fs;
use std::sync::Arc;
use tempfile::TempDir;
use tower::ServiceExt;

/// Create a minimal test config
fn create_test_config(temp_dir: &TempDir) -> RileyConfig {
    let toml_content = format!(
        r#"
[content]
repo_path = "{}"
content_dir = "content"

[storage]
bucket = "test-bucket"
public_url_base = "https://test.example.com"

[auth]
api_token = "test-secret-token"
"#,
        temp_dir.path().display()
    );
    toml::from_str(&toml_content).unwrap()
}

/// Create test post files
fn create_test_post(dir: &std::path::Path, slug: &str, title: &str, goes_live_at: Option<&str>) {
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
        format!("# {}\n\nContent here.", title),
    )
    .unwrap();
}

/// Helper to setup test environment and build router
async fn setup_test_app(temp_dir: &TempDir) -> axum::Router {
    let config = create_test_config(temp_dir);
    let riley = Riley::from_config(config.clone()).await.unwrap();
    let state = Arc::new(AppState { riley, config });
    build_router(state)
}

/// Helper to read response body as JSON
async fn body_json(body: Body) -> Value {
    let bytes = body.collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

// === Health Check Tests ===

#[tokio::test]
async fn test_health_endpoint() {
    let temp_dir = TempDir::new().unwrap();
    let app = setup_test_app(&temp_dir).await;

    let response = app
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = body_json(response.into_body()).await;
    assert_eq!(body["status"], "ok");
}

// === Public Posts Tests ===

#[tokio::test]
async fn test_list_posts_empty() {
    let temp_dir = TempDir::new().unwrap();
    let app = setup_test_app(&temp_dir).await;

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/posts")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = body_json(response.into_body()).await;
    assert_eq!(body["posts"].as_array().unwrap().len(), 0);
    assert_eq!(body["total"], 0);
}

#[tokio::test]
async fn test_list_posts_with_live_content() {
    let temp_dir = TempDir::new().unwrap();
    let content_dir = temp_dir.path().join("content");
    fs::create_dir_all(&content_dir).unwrap();

    // Create a live post (past date)
    create_test_post(
        &content_dir,
        "live-post",
        "Live Post",
        Some("2020-01-01T00:00:00Z"),
    );

    let app = setup_test_app(&temp_dir).await;

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/posts")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = body_json(response.into_body()).await;
    assert_eq!(body["posts"].as_array().unwrap().len(), 1);
    assert_eq!(body["posts"][0]["title"], "Live Post");
}

#[tokio::test]
async fn test_list_posts_excludes_drafts_by_default() {
    let temp_dir = TempDir::new().unwrap();
    let content_dir = temp_dir.path().join("content");
    fs::create_dir_all(&content_dir).unwrap();

    // Create a draft post (no date)
    create_test_post(&content_dir, "draft-post", "Draft Post", None);
    // Create a live post
    create_test_post(
        &content_dir,
        "live-post",
        "Live Post",
        Some("2020-01-01T00:00:00Z"),
    );

    let app = setup_test_app(&temp_dir).await;

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/posts")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = body_json(response.into_body()).await;
    // Should only see the live post
    assert_eq!(body["posts"].as_array().unwrap().len(), 1);
    assert_eq!(body["posts"][0]["title"], "Live Post");
}

// === Authentication Tests ===

#[tokio::test]
async fn test_drafts_require_auth_returns_401() {
    let temp_dir = TempDir::new().unwrap();
    let content_dir = temp_dir.path().join("content");
    fs::create_dir_all(&content_dir).unwrap();

    create_test_post(&content_dir, "draft-post", "Draft Post", None);

    let app = setup_test_app(&temp_dir).await;

    // Request drafts without authentication
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/posts?include_drafts=true")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    let body = body_json(response.into_body()).await;
    assert!(body["error"].as_str().unwrap().contains("Authentication"));
}

#[tokio::test]
async fn test_scheduled_require_auth_returns_401() {
    let temp_dir = TempDir::new().unwrap();
    let content_dir = temp_dir.path().join("content");
    fs::create_dir_all(&content_dir).unwrap();

    create_test_post(
        &content_dir,
        "scheduled-post",
        "Scheduled Post",
        Some("2099-01-01T00:00:00Z"),
    );

    let app = setup_test_app(&temp_dir).await;

    // Request scheduled without authentication
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/posts?include_scheduled=true")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_drafts_with_valid_auth_returns_200() {
    let temp_dir = TempDir::new().unwrap();
    let content_dir = temp_dir.path().join("content");
    fs::create_dir_all(&content_dir).unwrap();

    create_test_post(&content_dir, "draft-post", "Draft Post", None);
    create_test_post(
        &content_dir,
        "live-post",
        "Live Post",
        Some("2020-01-01T00:00:00Z"),
    );

    let app = setup_test_app(&temp_dir).await;

    // Request drafts WITH valid authentication
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/posts?include_drafts=true")
                .header(header::AUTHORIZATION, "Bearer test-secret-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = body_json(response.into_body()).await;
    // Should see both posts (live + draft)
    assert_eq!(body["posts"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn test_invalid_token_returns_401() {
    let temp_dir = TempDir::new().unwrap();
    let content_dir = temp_dir.path().join("content");
    fs::create_dir_all(&content_dir).unwrap();

    create_test_post(&content_dir, "draft-post", "Draft Post", None);

    let app = setup_test_app(&temp_dir).await;

    // Request drafts with INVALID token
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/posts?include_drafts=true")
                .header(header::AUTHORIZATION, "Bearer wrong-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

// === Series Authentication Tests ===

#[tokio::test]
async fn test_series_drafts_require_auth() {
    let temp_dir = TempDir::new().unwrap();
    let app = setup_test_app(&temp_dir).await;

    // Request series drafts without authentication
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/series?include_drafts=true")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_series_drafts_with_valid_auth() {
    let temp_dir = TempDir::new().unwrap();
    let app = setup_test_app(&temp_dir).await;

    // Request series drafts WITH authentication
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/series?include_drafts=true")
                .header(header::AUTHORIZATION, "Bearer test-secret-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

// === Cache Header Tests ===

#[tokio::test]
async fn test_public_response_has_cache_headers() {
    let temp_dir = TempDir::new().unwrap();
    let content_dir = temp_dir.path().join("content");
    fs::create_dir_all(&content_dir).unwrap();

    create_test_post(&content_dir, "post", "Post", Some("2020-01-01T00:00:00Z"));

    let app = setup_test_app(&temp_dir).await;

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/posts")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    // Should have Cache-Control header with public directive
    let cache_control = response.headers().get(header::CACHE_CONTROL).unwrap();
    assert!(cache_control.to_str().unwrap().contains("public"));

    // Should have ETag header
    assert!(response.headers().contains_key(header::ETAG));
}

#[tokio::test]
async fn test_authenticated_response_no_public_cache() {
    let temp_dir = TempDir::new().unwrap();
    let content_dir = temp_dir.path().join("content");
    fs::create_dir_all(&content_dir).unwrap();

    create_test_post(&content_dir, "draft", "Draft", None);

    let app = setup_test_app(&temp_dir).await;

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/posts?include_drafts=true")
                .header(header::AUTHORIZATION, "Bearer test-secret-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    // Should have Cache-Control with private/no-store
    let cache_control = response.headers().get(header::CACHE_CONTROL).unwrap();
    let cc_str = cache_control.to_str().unwrap();
    assert!(cc_str.contains("private") || cc_str.contains("no-store"));
}

// === Single Post Tests ===

#[tokio::test]
async fn test_get_single_post() {
    let temp_dir = TempDir::new().unwrap();
    let content_dir = temp_dir.path().join("content");
    fs::create_dir_all(&content_dir).unwrap();

    create_test_post(
        &content_dir,
        "my-post",
        "My Post",
        Some("2020-01-01T00:00:00Z"),
    );

    let app = setup_test_app(&temp_dir).await;

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/posts/my-post")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = body_json(response.into_body()).await;
    assert_eq!(body["slug"], "my-post");
    assert_eq!(body["title"], "My Post");
    assert!(body["content"].as_str().unwrap().contains("# My Post"));
}

#[tokio::test]
async fn test_get_nonexistent_post_returns_404() {
    let temp_dir = TempDir::new().unwrap();
    let app = setup_test_app(&temp_dir).await;

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/posts/nonexistent")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

// === Visibility Bypass Tests (get_post) ===

#[tokio::test]
async fn test_draft_post_returns_404_without_auth() {
    let temp_dir = TempDir::new().unwrap();
    let content_dir = temp_dir.path().join("content");
    fs::create_dir_all(&content_dir).unwrap();

    // Create a draft post (no goes_live_at)
    create_test_post(&content_dir, "secret-draft", "Secret Draft", None);

    let app = setup_test_app(&temp_dir).await;

    // Accessing draft directly by slug without auth should return 404
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/posts/secret-draft")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_scheduled_post_returns_404_without_auth() {
    let temp_dir = TempDir::new().unwrap();
    let content_dir = temp_dir.path().join("content");
    fs::create_dir_all(&content_dir).unwrap();

    // Create a scheduled post (future date)
    create_test_post(
        &content_dir,
        "future-post",
        "Future Post",
        Some("2099-01-01T00:00:00Z"),
    );

    let app = setup_test_app(&temp_dir).await;

    // Accessing scheduled post directly by slug without auth should return 404
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/posts/future-post")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_draft_post_visible_with_admin_auth() {
    let temp_dir = TempDir::new().unwrap();
    let content_dir = temp_dir.path().join("content");
    fs::create_dir_all(&content_dir).unwrap();

    create_test_post(&content_dir, "secret-draft", "Secret Draft", None);

    let app = setup_test_app(&temp_dir).await;

    // Accessing draft with valid admin token should succeed
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/posts/secret-draft")
                .header(header::AUTHORIZATION, "Bearer test-secret-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = body_json(response.into_body()).await;
    assert_eq!(body["title"], "Secret Draft");
}

#[tokio::test]
async fn test_draft_post_raw_returns_404_without_auth() {
    let temp_dir = TempDir::new().unwrap();
    let content_dir = temp_dir.path().join("content");
    fs::create_dir_all(&content_dir).unwrap();

    create_test_post(&content_dir, "secret-draft", "Secret Draft", None);

    let app = setup_test_app(&temp_dir).await;

    // Accessing draft raw content without auth should return 404
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/posts/secret-draft/raw")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

// === Series Visibility Tests ===

#[tokio::test]
async fn test_draft_series_returns_404_without_auth() {
    let temp_dir = TempDir::new().unwrap();
    let content_dir = temp_dir.path().join("content");
    let series_dir = content_dir.join("draft-series");

    fs::create_dir_all(&series_dir).unwrap();
    fs::write(
        series_dir.join("series.toml"),
        r#"title = "Draft Series"
description = "A draft series"
"#,
    )
    .unwrap();

    let app = setup_test_app(&temp_dir).await;

    // Accessing draft series directly by slug without auth should return 404
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/series/draft-series")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_draft_series_visible_with_admin_auth() {
    let temp_dir = TempDir::new().unwrap();
    let content_dir = temp_dir.path().join("content");
    let series_dir = content_dir.join("draft-series");

    fs::create_dir_all(&series_dir).unwrap();
    fs::write(
        series_dir.join("series.toml"),
        r#"title = "Draft Series"
description = "A draft series"
"#,
    )
    .unwrap();

    let app = setup_test_app(&temp_dir).await;

    // Accessing draft series with admin token should succeed
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/series/draft-series")
                .header(header::AUTHORIZATION, "Bearer test-secret-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = body_json(response.into_body()).await;
    assert_eq!(body["title"], "Draft Series");
}

// === Git Path Validation Tests ===

#[tokio::test]
async fn test_git_path_traversal_rejected() {
    let temp_dir = TempDir::new().unwrap();
    let app = setup_test_app(&temp_dir).await;

    let response = app
        .oneshot(
            Request::builder()
                .uri("/git/../../etc/passwd")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    // Should be rejected (either 400 Bad Request or auth failure)
    assert!(
        response.status() == StatusCode::BAD_REQUEST
            || response.status() == StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn test_git_path_special_chars_rejected() {
    let temp_dir = TempDir::new().unwrap();
    let app = setup_test_app(&temp_dir).await;

    let response = app
        .oneshot(
            Request::builder()
                .uri("/git/;rm%20-rf%20/")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    // Should be rejected
    assert!(
        response.status() == StatusCode::BAD_REQUEST
            || response.status() == StatusCode::UNAUTHORIZED
    );
}

// === ETag Tests ===

#[tokio::test]
async fn test_etag_is_full_sha256() {
    let temp_dir = TempDir::new().unwrap();
    let content_dir = temp_dir.path().join("content");
    fs::create_dir_all(&content_dir).unwrap();

    create_test_post(&content_dir, "post", "Post", Some("2020-01-01T00:00:00Z"));

    let app = setup_test_app(&temp_dir).await;

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/posts")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let etag = response
        .headers()
        .get(header::ETAG)
        .unwrap()
        .to_str()
        .unwrap();
    // Full SHA256 = 64 hex chars + 2 quotes = 66 chars
    assert_eq!(
        etag.len(),
        66,
        "ETag should be full SHA256 (64 hex chars + quotes), got: {}",
        etag
    );
    assert!(etag.starts_with('"') && etag.ends_with('"'));
}
