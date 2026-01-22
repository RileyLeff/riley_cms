//! HTTP request handlers for riley-api

use crate::middleware::AuthStatus;
use crate::AppState;
use axum::{
    Extension, Json,
    extract::{Path, Query, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use riley_core::ListOptions;
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

/// Convert internal errors to HTTP responses
fn internal_error(msg: impl std::fmt::Display) -> Response {
    let body = Json(ErrorResponse {
        error: msg.to_string(),
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
        headers.insert(header::CACHE_CONTROL, "private, no-store".parse().unwrap());
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
            .unwrap(),
        );
        headers.insert(header::ETAG, etag.parse().unwrap());
    }

    response
}

/// Check if request requires authentication (has include_drafts or include_scheduled)
fn is_authenticated_request(query: &ListQuery) -> bool {
    query.include_drafts || query.include_scheduled
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

    match state.riley.list_posts(&opts).await {
        Ok(result) => {
            let etag = state.riley.content_etag().await;

            #[derive(Serialize)]
            struct PostsResponse {
                posts: Vec<riley_core::PostSummary>,
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
pub async fn get_post(State(state): State<Arc<AppState>>, Path(slug): Path<String>) -> Response {
    match state.riley.get_post(&slug).await {
        Ok(Some(post)) => {
            let etag = state.riley.content_etag().await;
            with_cache_headers(Json(post), &state, &etag, false)
        }
        Ok(None) => {
            let body = Json(ErrorResponse {
                error: format!("Post not found: {}", slug),
            });
            (StatusCode::NOT_FOUND, body).into_response()
        }
        Err(e) => internal_error(e),
    }
}

/// GET /posts/:slug/raw - Get raw MDX content only
pub async fn get_post_raw(
    State(state): State<Arc<AppState>>,
    Path(slug): Path<String>,
) -> Response {
    match state.riley.get_post(&slug).await {
        Ok(Some(post)) => {
            let etag = state.riley.content_etag().await;
            let mut response = post.content.into_response();
            let headers = response.headers_mut();

            let server = state.config.server.as_ref();
            let max_age = server.map(|s| s.cache_max_age).unwrap_or(60);
            let swr = server
                .map(|s| s.cache_stale_while_revalidate)
                .unwrap_or(300);

            headers.insert(
                header::CONTENT_TYPE,
                "text/plain; charset=utf-8".parse().unwrap(),
            );
            headers.insert(
                header::CACHE_CONTROL,
                format!(
                    "public, max-age={}, stale-while-revalidate={}",
                    max_age, swr
                )
                .parse()
                .unwrap(),
            );
            headers.insert(header::ETAG, etag.parse().unwrap());

            response
        }
        Ok(None) => {
            let body = Json(ErrorResponse {
                error: format!("Post not found: {}", slug),
            });
            (StatusCode::NOT_FOUND, body).into_response()
        }
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

    match state.riley.list_series(&opts).await {
        Ok(result) => {
            let etag = state.riley.content_etag().await;

            #[derive(Serialize)]
            struct SeriesResponse {
                series: Vec<riley_core::SeriesSummary>,
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
pub async fn get_series(State(state): State<Arc<AppState>>, Path(slug): Path<String>) -> Response {
    match state.riley.get_series(&slug).await {
        Ok(Some(series)) => {
            let etag = state.riley.content_etag().await;
            with_cache_headers(Json(series), &state, &etag, false)
        }
        Ok(None) => {
            let body = Json(ErrorResponse {
                error: format!("Series not found: {}", slug),
            });
            (StatusCode::NOT_FOUND, body).into_response()
        }
        Err(e) => internal_error(e),
    }
}

/// GET /assets - List assets in storage
pub async fn list_assets(State(state): State<Arc<AppState>>) -> Response {
    match state.riley.list_assets().await {
        Ok(assets) => {
            #[derive(Serialize)]
            struct AssetsResponse {
                assets: Vec<riley_core::Asset>,
            }

            Json(AssetsResponse { assets }).into_response()
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
use http_body_util::BodyExt;
use riley_core::GitBackend;

/// Check Basic Auth for Git operations
///
/// Git clients typically use Basic Auth (not Bearer tokens).
/// Returns true if authentication is valid.
fn check_git_basic_auth(headers: &HeaderMap, state: &AppState) -> bool {
    // Get the configured git_token
    let expected_token = match &state.config.auth {
        Some(auth) => match &auth.git_token {
            Some(token_config) => match token_config.resolve() {
                Ok(token) => token,
                Err(_) => return false,
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
        password == expected_token
    } else {
        false
    }
}

/// Git Smart HTTP handler
///
/// Handles all Git HTTP protocol requests by proxying to git-http-backend.
/// Supports both read (fetch/clone) and write (push) operations.
pub async fn git_handler(
    State(state): State<Arc<AppState>>,
    Path(path): Path<String>,
    request: axum::http::Request<axum::body::Body>,
) -> Response {
    let method = request.method().clone();
    let headers = request.headers().clone();
    let uri = request.uri().clone();

    // Extract body
    let body = match request.into_body().collect().await {
        Ok(collected) => collected.to_bytes(),
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse {
                    error: format!("Failed to read request body: {}", e),
                }),
            )
                .into_response();
        }
    };
    // Check Basic Auth
    if !check_git_basic_auth(&headers, &state) {
        return (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, "Basic realm=\"Git\"")],
            "Authentication required",
        )
            .into_response();
    }

    // Get the content repository path
    let repo_path = &state.config.content.repo_path;
    let backend = GitBackend::new(repo_path);

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

    // Get content type
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .map(String::from);

    // Determine if this is a write operation (push)
    let is_write_operation = path.contains("git-receive-pack");

    // Run the Git CGI backend
    match backend
        .run_cgi(
            method.as_str(),
            &path_info,
            query_string.as_deref(),
            content_type.as_deref(),
            &body,
        )
        .await
    {
        Ok(cgi_response) => {
            let status = StatusCode::from_u16(cgi_response.status).unwrap_or(StatusCode::OK);

            let mut response_builder = Response::builder().status(status);

            // Copy headers from CGI response
            for (key, value) in &cgi_response.headers {
                if let Ok(header_name) = key.parse::<axum::http::header::HeaderName>() {
                    if let Ok(header_value) = value.parse::<axum::http::header::HeaderValue>() {
                        response_builder = response_builder.header(header_name, header_value);
                    }
                }
            }

            let response = response_builder
                .body(axum::body::Body::from(cgi_response.body))
                .unwrap();

            // If this was a successful push, refresh the cache and fire webhooks
            if is_write_operation && status.is_success() {
                let state_clone = state.clone();
                tokio::spawn(async move {
                    if let Err(e) = state_clone.riley.refresh().await {
                        tracing::error!("Failed to refresh content after git push: {}", e);
                    }
                    state_clone.riley.fire_webhooks().await;
                });
            }

            response
        }
        Err(e) => {
            tracing::error!("Git CGI error: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: format!("Git operation failed: {}", e),
                }),
            )
                .into_response()
        }
    }
}
