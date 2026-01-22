//! HTTP request handlers for riley-api

use crate::AppState;
use axum::{
    Json,
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
    Query(query): Query<ListQuery>,
) -> Response {
    let opts: ListOptions = query.clone().into();
    let is_auth = is_authenticated_request(&query);

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

            with_cache_headers(response, &state, &etag, is_auth)
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
    Query(query): Query<ListQuery>,
) -> Response {
    let opts: ListOptions = query.clone().into();
    let is_auth = is_authenticated_request(&query);

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

            with_cache_headers(response, &state, &etag, is_auth)
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
