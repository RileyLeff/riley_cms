//! HTTP request handlers for riley-cms-api

use crate::AppState;
use crate::middleware::AuthStatus;
use axum::{
    Extension, Json,
    extract::{Path, Query, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use futures_util::StreamExt;
use riley_cms_core::ListOptions;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Query parameters for list endpoints
#[derive(Debug, Clone, Deserialize)]
pub struct ListQuery {
    #[serde(default)]
    pub include_drafts: bool,
    #[serde(default)]
    pub include_scheduled: bool,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
}

impl From<ListQuery> for ListOptions {
    fn from(q: ListQuery) -> Self {
        Self {
            include_drafts: q.include_drafts,
            include_scheduled: q.include_scheduled,
            limit: q.limit,
            offset: q.offset,
        }
    }
}

/// Error response format
#[derive(Serialize)]
struct ErrorResponse {
    error: String,
}

/// Convert internal errors to HTTP responses.
///
/// Logs the actual error server-side but returns a generic message to clients
/// to avoid leaking internal details (file paths, S3 errors, etc.).
fn internal_error(err: impl std::fmt::Display) -> Response {
    tracing::error!("Internal error: {}", err);
    let body = Json(ErrorResponse {
        error: "Internal server error".to_string(),
    });
    (StatusCode::INTERNAL_SERVER_ERROR, body).into_response()
}

/// Add caching headers to response
fn with_cache_headers(
    response: impl IntoResponse,
    state: &AppState,
    etag: &str,
    is_authenticated: bool,
) -> Response {
    let mut response = response.into_response();
    let headers = response.headers_mut();

    if is_authenticated {
        headers.insert(
            header::CACHE_CONTROL,
            "private, no-store".parse().expect("valid static header"),
        );
    } else {
        let server = state.config.server.as_ref();
        let max_age = server.map(|s| s.cache_max_age).unwrap_or(60);
        let swr = server
            .map(|s| s.cache_stale_while_revalidate)
            .unwrap_or(300);

        headers.insert(
            header::CACHE_CONTROL,
            format!(
                "public, max-age={}, stale-while-revalidate={}",
                max_age, swr
            )
            .parse()
            .expect("valid cache-control header"),
        );
        headers.insert(header::ETAG, etag.parse().expect("valid etag header"));
    }

    response
}

/// Check if request requires authentication (has include_drafts or include_scheduled)
fn is_authenticated_request(query: &ListQuery) -> bool {
    query.include_drafts || query.include_scheduled
}

/// Check if content is visible based on goes_live_at and auth status.
/// Live content (goes_live_at in the past) is always visible.
/// Drafts (None) and scheduled (future) require admin auth.
fn is_content_visible(
    goes_live_at: Option<chrono::DateTime<chrono::Utc>>,
    auth_status: AuthStatus,
) -> bool {
    if auth_status == AuthStatus::Admin {
        return true;
    }
    match goes_live_at {
        None => false,                                    // Draft
        Some(date) if date > chrono::Utc::now() => false, // Scheduled
        Some(_) => true,                                  // Live
    }
}

/// Generate a standard not-found response
fn not_found_response(slug: &str, kind: &str) -> Response {
    let body = Json(ErrorResponse {
        error: format!("{} not found: {}", kind, slug),
    });
    (StatusCode::NOT_FOUND, body).into_response()
}

// === Handlers ===

/// GET /posts - List all posts
pub async fn list_posts(
    State(state): State<Arc<AppState>>,
    Extension(auth_status): Extension<AuthStatus>,
    Query(query): Query<ListQuery>,
) -> Response {
    let is_auth_required = is_authenticated_request(&query);

    // Security check: require authentication for drafts/scheduled content
    if is_auth_required && auth_status != AuthStatus::Admin {
        return (
            StatusCode::UNAUTHORIZED,
            Json(ErrorResponse {
                error: "Authentication required for drafts/scheduled content".to_string(),
            }),
        )
            .into_response();
    }

    let opts: ListOptions = query.into();

    match state.riley_cms.list_posts(&opts).await {
        Ok(result) => {
            let etag = state.riley_cms.content_etag().await;

            #[derive(Serialize)]
            struct PostsResponse {
                posts: Vec<riley_cms_core::PostSummary>,
                total: usize,
                limit: usize,
                offset: usize,
            }

            let response = Json(PostsResponse {
                posts: result.items,
                total: result.total,
                limit: result.limit,
                offset: result.offset,
            });

            with_cache_headers(response, &state, &etag, is_auth_required)
        }
        Err(e) => internal_error(e),
    }
}

/// GET /posts/:slug - Get a single post
pub async fn get_post(
    State(state): State<Arc<AppState>>,
    Extension(auth_status): Extension<AuthStatus>,
    Path(slug): Path<String>,
) -> Response {
    match state.riley_cms.get_post(&slug).await {
        Ok(Some(post)) => {
            // Visibility check: drafts/scheduled posts require admin auth
            if !is_content_visible(post.goes_live_at, auth_status) {
                return not_found_response(&slug, "Post");
            }
            let etag = state.riley_cms.content_etag().await;
            with_cache_headers(Json(post), &state, &etag, auth_status == AuthStatus::Admin)
        }
        Ok(None) => not_found_response(&slug, "Post"),
        Err(e) => internal_error(e),
    }
}

/// GET /posts/:slug/raw - Get raw MDX content only
pub async fn get_post_raw(
    State(state): State<Arc<AppState>>,
    Extension(auth_status): Extension<AuthStatus>,
    Path(slug): Path<String>,
) -> Response {
    match state.riley_cms.get_post(&slug).await {
        Ok(Some(post)) => {
            // Visibility check: drafts/scheduled posts require admin auth
            if !is_content_visible(post.goes_live_at, auth_status) {
                return not_found_response(&slug, "Post");
            }
            let is_admin = auth_status == AuthStatus::Admin;
            let etag = state.riley_cms.content_etag().await;
            let mut response = post.content.into_response();
            let headers = response.headers_mut();

            headers.insert(
                header::CONTENT_TYPE,
                "text/plain; charset=utf-8"
                    .parse()
                    .expect("valid static header"),
            );

            if is_admin {
                headers.insert(
                    header::CACHE_CONTROL,
                    "private, no-store".parse().expect("valid static header"),
                );
            } else {
                let server = state.config.server.as_ref();
                let max_age = server.map(|s| s.cache_max_age).unwrap_or(60);
                let swr = server
                    .map(|s| s.cache_stale_while_revalidate)
                    .unwrap_or(300);

                headers.insert(
                    header::CACHE_CONTROL,
                    format!(
                        "public, max-age={}, stale-while-revalidate={}",
                        max_age, swr
                    )
                    .parse()
                    .expect("valid cache-control header"),
                );
                headers.insert(header::ETAG, etag.parse().expect("valid etag header"));
            }

            response
        }
        Ok(None) => not_found_response(&slug, "Post"),
        Err(e) => internal_error(e),
    }
}

/// GET /series - List all series
pub async fn list_series(
    State(state): State<Arc<AppState>>,
    Extension(auth_status): Extension<AuthStatus>,
    Query(query): Query<ListQuery>,
) -> Response {
    let is_auth_required = is_authenticated_request(&query);

    // Security check: require authentication for drafts/scheduled content
    if is_auth_required && auth_status != AuthStatus::Admin {
        return (
            StatusCode::UNAUTHORIZED,
            Json(ErrorResponse {
                error: "Authentication required for drafts/scheduled content".to_string(),
            }),
        )
            .into_response();
    }

    let opts: ListOptions = query.into();

    match state.riley_cms.list_series(&opts).await {
        Ok(result) => {
            let etag = state.riley_cms.content_etag().await;

            #[derive(Serialize)]
            struct SeriesResponse {
                series: Vec<riley_cms_core::SeriesSummary>,
                total: usize,
                limit: usize,
                offset: usize,
            }

            let response = Json(SeriesResponse {
                series: result.items,
                total: result.total,
                limit: result.limit,
                offset: result.offset,
            });

            with_cache_headers(response, &state, &etag, is_auth_required)
        }
        Err(e) => internal_error(e),
    }
}

/// GET /series/:slug - Get a single series with posts
pub async fn get_series(
    State(state): State<Arc<AppState>>,
    Extension(auth_status): Extension<AuthStatus>,
    Path(slug): Path<String>,
) -> Response {
    match state.riley_cms.get_series(&slug).await {
        Ok(Some(series)) => {
            // Visibility check: drafts/scheduled series require admin auth
            if !is_content_visible(series.goes_live_at, auth_status) {
                return not_found_response(&slug, "Series");
            }
            let etag = state.riley_cms.content_etag().await;
            with_cache_headers(
                Json(series),
                &state,
                &etag,
                auth_status == AuthStatus::Admin,
            )
        }
        Ok(None) => not_found_response(&slug, "Series"),
        Err(e) => internal_error(e),
    }
}

/// Query parameters for asset list endpoint
#[derive(Debug, Clone, Deserialize)]
pub struct AssetListQuery {
    pub limit: Option<usize>,
    pub continuation_token: Option<String>,
}

/// GET /assets - List assets in storage with pagination (admin only)
pub async fn list_assets(
    State(state): State<Arc<AppState>>,
    Extension(auth_status): Extension<AuthStatus>,
    Query(query): Query<AssetListQuery>,
) -> Response {
    if auth_status != AuthStatus::Admin {
        return (
            StatusCode::UNAUTHORIZED,
            Json(ErrorResponse {
                error: "Authentication required to list assets".to_string(),
            }),
        )
            .into_response();
    }

    let opts = riley_cms_core::AssetListOptions {
        limit: query.limit,
        continuation_token: query.continuation_token,
    };

    match state.riley_cms.list_assets(&opts).await {
        Ok(result) => {
            let mut response = Json(result).into_response();
            response.headers_mut().insert(
                header::CACHE_CONTROL,
                "private, no-store".parse().expect("valid static header"),
            );
            response
        }
        Err(e) => internal_error(e),
    }
}

/// GET /health - Health check
pub async fn health() -> Response {
    #[derive(Serialize)]
    struct HealthResponse {
        status: &'static str,
    }

    Json(HealthResponse { status: "ok" }).into_response()
}

// === Git Smart HTTP Handlers ===

use axum::http::HeaderMap;
use base64::Engine;
use riley_cms_core::GitBackend;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

/// Check Basic Auth for Git operations
///
/// Git clients typically use Basic Auth (not Bearer tokens).
/// Returns true if authentication is valid.
fn check_git_basic_auth(headers: &HeaderMap, state: &AppState) -> bool {
    // Get the configured git_token
    let expected_token = match &state.config.auth {
        Some(auth) => match &auth.git_token {
            Some(token_config) => match token_config.resolve() {
                Ok(token) => {
                    if token.is_empty() {
                        tracing::warn!("Git token resolves to empty string. Git auth disabled.");
                        return false;
                    }
                    token
                }
                Err(e) => {
                    tracing::warn!("Failed to resolve git token: {}. Git auth disabled.", e);
                    return false;
                }
            },
            None => return false, // No git_token configured, deny access
        },
        None => return false, // No auth configured, deny access
    };

    // Parse Basic Auth header
    let auth_header = match headers.get(header::AUTHORIZATION) {
        Some(h) => h,
        None => return false,
    };

    let auth_str = match auth_header.to_str() {
        Ok(s) => s,
        Err(_) => return false,
    };

    // Format: "Basic base64(username:password)"
    let encoded = match auth_str.strip_prefix("Basic ") {
        Some(e) => e,
        None => return false,
    };

    let decoded = match base64::engine::general_purpose::STANDARD.decode(encoded) {
        Ok(d) => d,
        Err(_) => return false,
    };

    let credentials = match String::from_utf8(decoded) {
        Ok(c) => c,
        Err(_) => return false,
    };

    // Format: "username:password" - we only check password (the token)
    // Username can be anything (commonly "git" or the actual username)
    if let Some((_username, password)) = credentials.split_once(':') {
        // Hash both tokens before comparing to prevent leaking
        // token length via timing side-channel.
        let provided_hash = Sha256::digest(password.as_bytes());
        let expected_hash = Sha256::digest(expected_token.as_bytes());
        provided_hash.ct_eq(&expected_hash).into()
    } else {
        false
    }
}

/// Default maximum allowed body size for git operations (100 MB)
const DEFAULT_GIT_MAX_BODY_SIZE: u64 = 100 * 1024 * 1024;

/// Validate that a git path is safe (no traversal, no injection)
fn is_valid_git_path(path: &str) -> bool {
    // Reject path traversal
    if path.contains("..") {
        return false;
    }
    // Only allow alphanumeric, hyphens, underscores, dots, forward slashes, and query-safe chars
    path.chars()
        .all(|c| c.is_ascii_alphanumeric() || "-_./=?&+".contains(c))
}

/// Git Smart HTTP handler
///
/// Handles all Git HTTP protocol requests by proxying to git-http-backend.
/// Supports both read (fetch/clone) and write (push) operations.
/// Request bodies and CGI responses are streamed to avoid buffering large payloads.
pub async fn git_handler(
    State(state): State<Arc<AppState>>,
    Path(path): Path<String>,
    request: axum::http::Request<axum::body::Body>,
) -> Response {
    // Validate path to prevent traversal and injection
    if !is_valid_git_path(&path) {
        tracing::warn!("Rejected invalid git path: {:?}", path);
        return (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "Invalid path".to_string(),
            }),
        )
            .into_response();
    }

    let method = request.method().clone();
    let headers = request.headers().clone();
    let uri = request.uri().clone();

    // Check Basic Auth before consuming the body
    if !check_git_basic_auth(&headers, &state) {
        return (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, "Basic realm=\"Git\"")],
            "Authentication required",
        )
            .into_response();
    }

    // Get the content repository path and git config
    let repo_path = &state.config.content.repo_path;
    let git_config = state.config.git.as_ref();
    let backend_path = git_config.and_then(|g| g.backend_path.clone());
    let max_body_size = git_config
        .map(|g| g.max_body_size)
        .unwrap_or(DEFAULT_GIT_MAX_BODY_SIZE);
    let cgi_timeout =
        std::time::Duration::from_secs(git_config.map(|g| g.cgi_timeout_secs).unwrap_or(300));
    let backend = GitBackend::with_backend_path(repo_path, backend_path);

    // Check if repo exists
    if !backend.is_valid_repo() {
        return (
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: "Git repository not found".to_string(),
            }),
        )
            .into_response();
    }

    // Build path_info (the path after /git/)
    let path_info = format!("/{}", path);

    // Extract query string from URI
    let query_string = uri.query().map(String::from);

    // Get content type and content length
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .map(String::from);

    let content_length = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok());

    // Determine if this is a write operation (push)
    let is_write_operation = path.contains("git-receive-pack");

    // Convert request body to a stream for incremental processing
    let body_stream = request
        .into_body()
        .into_data_stream()
        .map(|result| result.map_err(std::io::Error::other));

    // Run the Git CGI backend with streaming
    match backend
        .run_cgi(
            method.as_str(),
            &path_info,
            query_string.as_deref(),
            content_type.as_deref(),
            content_length,
            body_stream,
            max_body_size,
        )
        .await
    {
        Ok(cgi_response) => {
            let status =
                StatusCode::from_u16(cgi_response.headers.status).unwrap_or(StatusCode::OK);

            let mut response_builder = Response::builder().status(status);

            // Copy headers from CGI response
            for (key, value) in &cgi_response.headers.headers {
                if let Ok(header_name) = key.parse::<axum::http::header::HeaderName>()
                    && let Ok(header_value) = value.parse::<axum::http::header::HeaderValue>()
                {
                    response_builder = response_builder.header(header_name, header_value);
                }
            }

            // Build streaming response body
            let response = match response_builder
                .body(axum::body::Body::from_stream(cgi_response.body_stream))
            {
                Ok(r) => r,
                Err(e) => {
                    tracing::error!("Failed to build response from CGI output: {}", e);
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(ErrorResponse {
                            error: "Git operation failed".to_string(),
                        }),
                    )
                        .into_response();
                }
            };

            // Always spawn a task to reap the child process (prevents zombies).
            // Only trigger refresh/webhooks for successful write operations.
            let state_clone = state.clone();
            tokio::spawn(async move {
                match cgi_response.completion.wait(cgi_timeout).await {
                    Ok(exit_status) => {
                        if is_write_operation && exit_status.success() {
                            if let Err(e) = state_clone.riley_cms.refresh().await {
                                tracing::error!("Failed to refresh content after git push: {}", e);
                            }
                            state_clone.riley_cms.fire_webhooks().await;
                        }
                    }
                    Err(e) => {
                        tracing::error!("Git CGI completion error: {}", e);
                    }
                }
            });

            response
        }
        Err(e) => {
            tracing::error!("Git CGI error: {}", e);

            // Check if this was a body-too-large error
            let error_msg = e.to_string();
            if error_msg.contains("exceeds maximum") {
                return (
                    StatusCode::PAYLOAD_TOO_LARGE,
                    Json(ErrorResponse { error: error_msg }),
                )
                    .into_response();
            }

            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "Git operation failed".to_string(),
                }),
            )
                .into_response()
        }
    }
}
