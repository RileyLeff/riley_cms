# Riley CMS - Gemini Code Review Fix Plan

**Generated:** 2026-01-22
**Source:** Gemini comprehensive code review session
**Status:** Draft

---

## Phase 1: Critical Security Fixes (Immediate Priority)

**Goal:** Secure the application against arbitrary file access, command injection, and unauthorized content access.

### 1.1 Fix Content Visibility Bypass

**Severity:** Critical
**Files:** `crates/riley-cms-api/src/handlers.rs`

The `get_post` and `get_series` handlers do not check if content is a draft or scheduled for the future. Anyone who guesses a slug can view unpublished content.

**Implementation:**
Enforce visibility checks in individual resource handlers, not just list endpoints.

```rust
// crates/riley-cms-api/src/handlers.rs

pub async fn get_post(
    State(state): State<Arc<AppState>>,
    Path(slug): Path<String>,
    auth: AuthStatus,
) -> Result<impl IntoResponse, ApiError> {
    let post = state.riley_cms.get_post(&slug)?;

    // Visibility Check: drafts/future posts require admin auth
    if let Some(goes_live) = post.metadata.goes_live_at {
        if goes_live > Utc::now() && !auth.is_admin() {
            return Err(ApiError::NotFound);
        }
    }

    Ok(Json(post))
}
```

### 1.2 Validate Git Handler Paths

**Severity:** Critical
**Files:** `crates/riley-cms-api/src/handlers.rs`

The `git_handler` passes user-supplied path input directly to `PATH_INFO` for `git-http-backend` without strict validation.

**Implementation:**
Add regex validation for the `path` parameter.

```rust
use regex::Regex;
use once_cell::sync::Lazy;

static SAFE_GIT_PATH: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"^[a-zA-Z0-9\-_\.\/]+$").unwrap()
});

pub async fn git_handler(
    Path(path): Path<String>,
    // ...
) -> Result<impl IntoResponse, ApiError> {
    // Validate path contains no traversal or injection
    if path.contains("..") || !SAFE_GIT_PATH.is_match(&path) {
        eprintln!("Security Alert: Invalid git path attempted: {:?}", path);
        return Err(ApiError::BadRequest("Invalid path".into()));
    }
    // ... existing logic
}
```

### 1.3 Stream Git Operations (DoS Prevention)

**Severity:** High
**Files:** `crates/riley-cms-core/src/git.rs`

`GitBackend::run_cgi` buffers the entire request body and response into memory. A large push could cause OOM.

**Implementation:**
Refactor to stream stdin/stdout instead of buffering.

```rust
// crates/riley-cms-core/src/git.rs

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use axum::body::Body;
use futures::stream::TryStreamExt;

pub async fn run_cgi_streaming(
    &self,
    env_vars: Vec<(String, String)>,
    body_stream: Body,
) -> Result<Body, CmsError> {
    let mut child = Command::new(&self.backend_path)
        .envs(env_vars)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .map_err(CmsError::GitIoError)?;

    // Stream request body to child stdin
    let mut stdin = child.stdin.take().unwrap();
    let mut body_reader = StreamReader::new(
        body_stream.into_data_stream().map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))
    );
    tokio::io::copy(&mut body_reader, &mut stdin).await?;
    drop(stdin);

    // Stream child stdout as response body
    let stdout = child.stdout.take().unwrap();
    let stream = ReaderStream::new(stdout);
    Ok(Body::from_stream(stream))
}
```

### 1.4 Path Traversal Prevention in Storage

**Severity:** High
**Files:** `crates/riley-cms-core/src/content.rs` or storage layer

Ensure all file access is constrained to the content root directory.

```rust
use std::path::{Path, PathBuf};

/// Securely joins a base path and a relative path, preventing traversal.
fn secure_join(base: &Path, relative: &str) -> Result<PathBuf, CmsError> {
    let base = base.canonicalize().map_err(|_| CmsError::StorageError)?;
    let joined = base.join(relative);

    // Canonicalize to resolve ".." and symlinks
    let canonical = joined.canonicalize().map_err(|_| CmsError::NotFound)?;

    if canonical.starts_with(&base) {
        Ok(canonical)
    } else {
        eprintln!("Security Alert: Path traversal attempt: {:?}", joined);
        Err(CmsError::SecurityError("Invalid path access".into()))
    }
}
```

---

## Phase 2: Error Handling Improvements

**Goal:** Replace generic 500 errors with typed, informative responses.

### 2.1 Differentiate Client vs Server Errors

**Files:** `crates/riley-cms-api/src/handlers.rs`

The current `internal_error` helper wraps all errors into 500. Map errors to appropriate status codes.

```rust
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        match self {
            ApiError::NotFound => (StatusCode::NOT_FOUND, "Not found").into_response(),
            ApiError::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg).into_response(),
            ApiError::Unauthorized => (StatusCode::UNAUTHORIZED, "Unauthorized").into_response(),
            ApiError::SecurityError(_) => (StatusCode::FORBIDDEN, "Forbidden").into_response(),
            ApiError::Internal(e) => {
                eprintln!("Internal error: {:?}", e);
                (StatusCode::INTERNAL_SERVER_ERROR, "Internal server error").into_response()
            }
        }
    }
}
```

### 2.2 Replace `.unwrap()` with Proper Error Propagation

**Files:** `crates/riley-cms-api/src/handlers.rs` (header parsing)

```rust
// Before
headers.insert("X-Custom", "value".parse().unwrap());

// After
headers.insert("X-Custom", "value".parse().expect("valid static header value"));
// Or for dynamic values:
headers.insert("X-Custom", value.parse().map_err(|_| ApiError::Internal("header parse error"))?);
```

---

## Phase 3: API Design Improvements

**Goal:** Consistent API contract and future-proof versioning.

### 3.1 API Route Versioning

**Files:** `crates/riley-cms-api/src/lib.rs` (router setup)

Prefix all API routes with `/api/v1`.

```rust
let app = Router::new()
    .nest("/api/v1", api_routes())
    .route("/git/*path", get(git_handler).post(git_handler))
    .with_state(state);

fn api_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/posts", get(list_posts))
        .route("/posts/:slug", get(get_post))
        .route("/series", get(list_series))
        .route("/series/:slug", get(get_series))
}
```

### 3.2 Full ETag Hash

**Files:** `crates/riley-cms-core/src/content.rs` (or wherever `compute_etag` is)

Use the full SHA256 hash instead of truncating to 8 bytes.

```rust
// Before: truncated to 16 hex chars
fn compute_etag(content: &[u8]) -> String {
    let hash = Sha256::digest(content);
    format!("\"{}\"", hex::encode(&hash[..8]))
}

// After: full 64 hex chars
fn compute_etag(content: &[u8]) -> String {
    let hash = Sha256::digest(content);
    format!("\"{}\"", hex::encode(hash))
}
```

---

## Phase 4: Testing Gaps

**Goal:** Prevent regression of security fixes and verify concurrent behavior.

### 4.1 Security Regression Tests

**Files:** `crates/riley-cms-core/tests/security_tests.rs` (new)

```rust
#[test]
fn test_path_traversal_blocked() {
    let cache = ContentCache::new("./test_content");
    assert!(cache.get_post("../Cargo.toml").is_err());
    assert!(cache.get_post("../../etc/passwd").is_err());
    assert!(cache.get_post("valid-slug").is_ok()); // control
}

#[test]
fn test_dotdot_in_slug_rejected() {
    let cache = ContentCache::new("./test_content");
    assert!(cache.get_post("foo/../bar").is_err());
}
```

### 4.2 Git Handler Integration Tests

**Files:** `crates/riley-cms-api/tests/git_handler.rs` (new)

```rust
#[tokio::test]
async fn test_git_path_validation_rejects_traversal() {
    let app = create_test_app().await;
    let resp = app.get("/git/../../../etc/passwd").await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_git_path_validation_rejects_special_chars() {
    let app = create_test_app().await;
    let resp = app.get("/git/; rm -rf /").await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_valid_git_info_refs() {
    let app = create_test_app().await;
    let resp = app.get("/git/info/refs?service=git-upload-pack").await;
    assert!(resp.status().is_success() || resp.status() == StatusCode::NOT_FOUND);
}
```

### 4.3 Visibility Bypass Tests

**Files:** `crates/riley-cms-api/tests/api.rs` (extend existing)

```rust
#[tokio::test]
async fn test_draft_post_not_visible_without_auth() {
    let app = create_test_app_with_draft_post().await;
    let resp = app.get("/api/v1/posts/my-draft-post").await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_draft_post_visible_with_admin_auth() {
    let app = create_test_app_with_draft_post().await;
    let resp = app
        .get("/api/v1/posts/my-draft-post")
        .header("Authorization", "Bearer admin-token")
        .await;
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_future_post_not_visible() {
    let app = create_test_app_with_future_post().await;
    let resp = app.get("/api/v1/posts/future-post").await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}
```

### 4.4 Concurrency Tests

**Files:** `crates/riley-cms-core/tests/concurrency.rs` (new)

```rust
#[tokio::test]
async fn test_concurrent_reads_during_refresh() {
    let cache = Arc::new(ContentCache::new("./test_content"));

    let mut handles = vec![];

    // Spawn a refresh task
    let cache_clone = cache.clone();
    handles.push(tokio::spawn(async move {
        cache_clone.refresh().await.unwrap();
    }));

    // Spawn concurrent readers
    for _ in 0..50 {
        let cache_clone = cache.clone();
        handles.push(tokio::spawn(async move {
            let _ = cache_clone.list_posts(ListOptions::default());
        }));
    }

    // All should complete without panic
    for handle in handles {
        handle.await.unwrap();
    }
}
```

---

## Phase 5: Minor Improvements

### 5.1 Configurable Git Backend Path

**Files:** `crates/riley-cms-core/src/git.rs`, config struct

```toml
# config.toml
[git]
backend_path = "/usr/lib/git-core/git-http-backend"
```

```rust
pub struct GitConfig {
    pub backend_path: Option<PathBuf>,
}

impl GitBackend {
    pub fn new(config: &GitConfig) -> Result<Self, CmsError> {
        let path = config.backend_path.clone()
            .or_else(|| find_git_backend())  // existing discovery logic as fallback
            .ok_or(CmsError::GitError("git-http-backend not found".into()))?;
        Ok(Self { backend_path: path })
    }
}
```

### 5.2 Add `thiserror` Dependency

**Files:** `crates/riley-cms-core/Cargo.toml`

```toml
[dependencies]
thiserror = "2"
```

---

## Implementation Order (Dependency Graph)

```
Phase 2.1 (CmsError type)
    ├── Phase 1.1 (Visibility bypass fix)
    ├── Phase 1.2 (Git path validation)
    ├── Phase 1.3 (Streaming git ops)
    ├── Phase 1.4 (Path traversal prevention)
    ├── Phase 2.2 (Remove unwraps)
    └── Phase 3.1 (API versioning)
            └── Phase 4.* (All tests)
                    └── Phase 5.* (Minor improvements)
```

**Recommended execution:**

1. Add `thiserror` and define `CmsError` enum (Phase 5.2 + 2.1)
2. Implement `secure_join` in storage (Phase 1.4)
3. Add git path validation (Phase 1.2)
4. Fix visibility bypass in handlers (Phase 1.1)
5. Refactor git streaming (Phase 1.3)
6. Improve error mapping in API layer (Phase 2.2)
7. Add API versioning prefix (Phase 3.1)
8. Write all security/integration/concurrency tests (Phase 4)
9. Minor cleanups - ETag, git config, clippy (Phase 5)
