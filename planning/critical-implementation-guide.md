# Critical Implementation Guide

Detailed implementation guidance for the two Critical priority tasks from the remediation plan.

---

## 1. Fix Async Blocking I/O (Task 1.1)

**Location:** `crates/riley-cms-core/src/lib.rs`

### Overview
Offload the synchronous `ContentCache::load` operation to a dedicated blocking thread pool provided by Tokio. This prevents the heavy filesystem operations from stalling the async executor.

### Changes Required

1.  **Modify `from_config`**: Clone the `ContentConfig` before moving it into the blocking task closure.
2.  **Modify `refresh`**: Similarly, clone the config from `self` before moving it.
3.  **Error Handling**: Handle the `JoinError` (from `spawn_blocking`) and the inner `Result` (from `load`).

### Implementation Reference

```rust
// crates/riley-cms-core/src/lib.rs

impl Riley {
    /// Create a new Riley instance from configuration.
    pub async fn from_config(config: RileyConfig) -> Result<Self> {
        let storage = Storage::new(&config.storage).await?;

        // Clone content config to move into the 'static blocking closure
        let content_config = config.content.clone();

        // Offload blocking I/O to a dedicated thread
        let cache = tokio::task::spawn_blocking(move || {
            ContentCache::load(&content_config)
        })
        .await
        .map_err(|e| Error::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))??;
        // Note the double ?? - first for JoinError, second for ContentCache::load Result

        Ok(Self {
            config,
            cache: Arc::new(RwLock::new(cache)),
            storage,
        })
    }

    /// Refresh the content cache from disk.
    pub async fn refresh(&self) -> Result<()> {
        // Clone the specific config needed for loading
        let content_config = self.config.content.clone();

        let new_cache = tokio::task::spawn_blocking(move || {
            ContentCache::load(&content_config)
        })
        .await
        .map_err(|e| Error::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))??;

        let mut cache = self.cache.write().await;
        *cache = new_cache;
        Ok(())
    }
}
```

### Key Points
- `spawn_blocking` returns a `JoinHandle<T>` which when `.await`ed gives `Result<T, JoinError>`
- The double `??` handles both the `JoinError` and the inner `Result` from `ContentCache::load`
- `ContentConfig` must implement `Clone` (it likely already does via derive)
- The closure must be `'static`, so we clone the config rather than borrowing

---

## 2. Auth Middleware (Task 1.2)

**Locations:**
- `crates/riley-cms-api/src/middleware.rs` (Implementation)
- `crates/riley-cms-api/src/lib.rs` (Registration)
- `crates/riley-cms-api/src/handlers.rs` (Usage)

### Step A: Middleware Implementation

Use `axum::middleware::from_fn_with_state` for a concise implementation that has access to the app configuration.

**`crates/riley-cms-api/src/middleware.rs`**:

```rust
use axum::{
    extract::{Request, State},
    http::{StatusCode, header},
    middleware::Next,
    response::Response,
};
use std::sync::Arc;
use crate::AppState;

/// Authentication status for the current request
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthStatus {
    Public,
    Admin,
}

pub async fn auth_middleware(
    State(state): State<Arc<AppState>>,
    mut request: Request,
    next: Next,
) -> Response {
    // 1. Default to Public
    let mut auth_status = AuthStatus::Public;

    // 2. Check for configured API token
    if let Some(ref auth_config) = state.config.auth {
        if let Some(ref token_config) = auth_config.api_token {
            // In a real scenario, resolve this once at startup, but for now:
            if let Ok(expected_token) = token_config.resolve() {
                // Check Authorization header
                if let Some(auth_header) = request.headers().get(header::AUTHORIZATION) {
                    if let Ok(auth_str) = auth_header.to_str() {
                        if let Some(provided_token) = auth_str.strip_prefix("Bearer ") {
                            if provided_token.trim() == expected_token {
                                auth_status = AuthStatus::Admin;
                            }
                        }
                    }
                }
            }
        }
    }

    // 3. Insert status into extensions so handlers can read it
    request.extensions_mut().insert(auth_status);

    next.run(request).await
}
```

### Step B: Register Middleware

Update the router builder to include the middleware.

**`crates/riley-cms-api/src/lib.rs`**:

```rust
// Add imports
use crate::middleware::auth_middleware;

pub fn build_router(state: Arc<AppState>) -> Router {
    let cors = build_cors_layer(&state.config);

    Router::new()
        // ... (existing routes) ...
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ))
        .with_state(state) // state must be attached for middleware to access it
        .layer(cors)
        .layer(TraceLayer::new_for_http())
}
```

### Step C: Update Handlers

Modify `list_posts` and `list_series` to verify permissions.

**`crates/riley-cms-api/src/handlers.rs`**:

```rust
// Add imports
use axum::extract::Extension;
use crate::middleware::AuthStatus;

pub async fn list_posts(
    State(state): State<Arc<AppState>>,
    Extension(auth_status): Extension<AuthStatus>, // Extract auth status
    Query(query): Query<ListQuery>,
) -> Response {
    let opts: ListOptions = query.clone().into();

    // Security Check
    if (opts.include_drafts || opts.include_scheduled) && auth_status != AuthStatus::Admin {
        return (
            StatusCode::UNAUTHORIZED,
            Json(ErrorResponse { error: "Authentication required for drafts/scheduled content".into() })
        ).into_response();
    }

    // ... existing logic ...
}

// Apply similar logic to list_series
```

### Key Points
- Middleware runs on **every** request and sets `AuthStatus` in extensions
- Handlers extract the `AuthStatus` and make authorization decisions
- This pattern separates authentication (middleware) from authorization (handlers)
- The token is resolved from config which supports `env:VAR_NAME` syntax for secrets

---

## Testing Checklist

### Task 1.1 (Async Blocking)
- [ ] Existing integration tests still pass
- [ ] Server remains responsive during `refresh()` calls
- [ ] No panics from `JoinError` in normal operation

### Task 1.2 (Auth Middleware)
- [ ] `GET /posts` returns 200 (public access works)
- [ ] `GET /posts?include_drafts=true` returns 401 without token
- [ ] `GET /posts?include_drafts=true` with valid `Authorization: Bearer <token>` returns 200
- [ ] `GET /posts?include_scheduled=true` follows same pattern
- [ ] Invalid tokens still return 401

---

*Generated via collaborative review with Gemini and Claude Code*
