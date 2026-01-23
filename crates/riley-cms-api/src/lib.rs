//! riley-cms-api: HTTP API server for riley_cms

mod handlers;
pub mod middleware;

use axum::{
    Router,
    middleware::from_fn_with_state,
    routing::{any, get},
};
use middleware::auth_middleware;
use riley_cms_core::{RileyCms, RileyCmsConfig};
use std::net::SocketAddr;
use std::sync::Arc;
use tower_governor::GovernorLayer;
use tower_governor::governor::GovernorConfigBuilder;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;

/// Application state shared across handlers
pub struct AppState {
    pub riley_cms: RileyCms,
    pub config: RileyCmsConfig,
}

/// Build the versioned API routes
fn api_v1_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/posts", get(handlers::list_posts))
        .route("/posts/{slug}", get(handlers::get_post))
        .route("/posts/{slug}/raw", get(handlers::get_post_raw))
        .route("/series", get(handlers::list_series))
        .route("/series/{slug}", get(handlers::get_series))
        .route("/assets", get(handlers::list_assets))
}

/// Build the Axum router with all routes.
///
/// Note: Rate limiting is applied separately in `serve()` because it requires
/// real TCP connection info (peer IP) which isn't available in `oneshot` tests.
pub fn build_router(state: Arc<AppState>) -> Router {
    let cors = build_cors_layer(&state.config);

    Router::new()
        // Versioned API routes
        .nest("/api/v1", api_v1_routes())
        // Health check (unversioned)
        .route("/health", get(handlers::health))
        // Git Smart HTTP routes (uses Basic Auth, not Bearer token)
        .route("/git/{*path}", any(handlers::git_handler))
        // Auth middleware - runs on all routes, sets AuthStatus in extensions
        .layer(from_fn_with_state(state.clone(), auth_middleware))
        // State and other middleware
        .with_state(state)
        .layer(cors)
        .layer(
            TraceLayer::new_for_http().make_span_with(
                tower_http::trace::DefaultMakeSpan::new()
                    .level(tracing::Level::INFO)
                    .include_headers(false),
            ),
        )
}

/// Build CORS layer from config.
///
/// Defaults to denying all cross-origin requests if `cors_origins` is not configured.
/// Set `cors_origins = ["*"]` to allow all origins, or specify explicit origins.
fn build_cors_layer(config: &RileyCmsConfig) -> CorsLayer {
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
        // Default: deny all cross-origin requests (secure by default)
        None => CorsLayer::new(),
    }
}

/// Run the API server with graceful shutdown support.
///
/// The server will drain in-flight connections when receiving SIGINT (Ctrl+C)
/// or SIGTERM (Docker stop / Kubernetes terminate).
pub async fn serve(riley_cms: RileyCms) -> anyhow::Result<()> {
    let config = riley_cms.config().clone();
    let server_config = config.server.clone().unwrap_or_default();

    let state = Arc::new(AppState { riley_cms, config });

    // Rate limiting: 50 burst capacity, replenish 10/second per IP.
    // Allows normal browsing but prevents brute-force on auth endpoints.
    // Applied here (not in build_router) because it requires real TCP peer IP.
    let governor_conf = GovernorConfigBuilder::default()
        .per_second(10)
        .burst_size(50)
        .finish()
        .unwrap();
    let governor_layer = GovernorLayer::new(governor_conf);

    let app = build_router(state).layer(governor_layer);

    let addr: SocketAddr = format!("{}:{}", server_config.host, server_config.port).parse()?;

    tracing::info!("Starting server on {}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    Ok(())
}

/// Wait for a shutdown signal (SIGINT or SIGTERM).
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }

    tracing::info!("Shutdown signal received, draining connections...");
}
