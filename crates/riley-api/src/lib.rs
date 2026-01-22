//! riley-api: HTTP API server for riley_cms

mod handlers;
mod middleware;

use axum::{Router, routing::get};
use riley_core::{Riley, RileyConfig};
use std::net::SocketAddr;
use std::sync::Arc;
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;

/// Application state shared across handlers
pub struct AppState {
    pub riley: Riley,
    pub config: RileyConfig,
}

/// Build the Axum router with all routes
pub fn build_router(state: Arc<AppState>) -> Router {
    let cors = build_cors_layer(&state.config);

    Router::new()
        // Content routes
        .route("/posts", get(handlers::list_posts))
        .route("/posts/{slug}", get(handlers::get_post))
        .route("/posts/{slug}/raw", get(handlers::get_post_raw))
        .route("/series", get(handlers::list_series))
        .route("/series/{slug}", get(handlers::get_series))
        .route("/assets", get(handlers::list_assets))
        // Health check
        .route("/health", get(handlers::health))
        // State and middleware
        .with_state(state)
        .layer(cors)
        .layer(TraceLayer::new_for_http())
}

/// Build CORS layer from config
fn build_cors_layer(config: &RileyConfig) -> CorsLayer {
    let origins = config
        .server
        .as_ref()
        .map(|s| &s.cors_origins)
        .filter(|o| !o.is_empty());

    match origins {
        Some(origins) if origins.iter().any(|o| o == "*") => CorsLayer::permissive(),
        Some(origins) => {
            let origins: Vec<_> = origins.iter().filter_map(|o| o.parse().ok()).collect();
            CorsLayer::new().allow_origin(origins)
        }
        None => CorsLayer::new().allow_origin(Any),
    }
}

/// Run the API server
pub async fn serve(riley: Riley) -> anyhow::Result<()> {
    let config = riley.config().clone();
    let server_config = config.server.clone().unwrap_or_default();

    let state = Arc::new(AppState { riley, config });
    let app = build_router(state);

    let addr: SocketAddr = format!("{}:{}", server_config.host, server_config.port).parse()?;

    tracing::info!("Starting server on {}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
