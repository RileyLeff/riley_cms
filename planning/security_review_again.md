# Security Review Follow-up Fixes

Findings from Gemini security review (Jan 2025). All streaming, temp-env, and SSRF fixes are already applied.

---

## Fix 1: Sanitize TraceLayer Header Logging (Medium)

### Problem

`TraceLayer::new_for_http()` in `crates/riley-cms-api/src/lib.rs:56` uses defaults. While the default `DefaultMakeSpan` does NOT include headers at `INFO` level, if a user sets `RUST_LOG=tower_http=debug` (common during development), the `Authorization` header values will appear in logs — leaking tokens.

### Approach

Replace the bare `TraceLayer::new_for_http()` with a configured version that explicitly sets `include_headers(false)` on the span maker. This ensures headers are never logged regardless of log level.

### Files to modify

| File | Changes |
|------|---------|
| `crates/riley-cms-api/src/lib.rs` | Configure `TraceLayer` with `DefaultMakeSpan::new().include_headers(false)` |

### Implementation

```rust
// Before
.layer(TraceLayer::new_for_http())

// After
.layer(
    TraceLayer::new_for_http()
        .make_span_with(
            tower_http::trace::DefaultMakeSpan::new()
                .level(tracing::Level::INFO)
                .include_headers(false),
        )
)
```

This is a one-line change (expanded for readability). No new dependencies.

---

## Fix 2: Content File Size Limit (Medium)

### Problem

`ContentCache::load_post()` at `crates/riley-cms-core/src/content.rs:172` calls `fs::read_to_string(&content_path)` with no size check. A malicious actor with git push access could add a 2GB `.mdx` file, causing OOM on the next `refresh()`.

Similarly, `config.toml` and `series.toml` files are read without size limits (lines 166, 192).

### Approach

Add a `max_content_file_size` field to `ContentConfig` (defaulting to 5MB). Before reading any file, check its metadata size and skip files that exceed the limit with a warning.

### Files to modify

| File | Changes |
|------|---------|
| `crates/riley-cms-core/src/config.rs` | Add `max_content_file_size` to `ContentConfig` with default |
| `crates/riley-cms-core/src/content.rs` | Add size check before `fs::read_to_string` calls |
| `riley_cms.example.toml` | Document the new option |

### Config change

```rust
// In ContentConfig
#[derive(Debug, Clone, Deserialize)]
pub struct ContentConfig {
    pub repo_path: PathBuf,
    #[serde(default = "default_content_dir")]
    pub content_dir: String,
    /// Maximum size in bytes for any single content file (config.toml, content.mdx, series.toml).
    /// Files exceeding this limit are skipped with a warning. Default: 5MB.
    #[serde(default = "default_max_content_file_size")]
    pub max_content_file_size: u64,
}

fn default_max_content_file_size() -> u64 {
    5 * 1024 * 1024 // 5 MB
}
```

### Content loading change

Add a helper that checks file size before reading:

```rust
/// Read a file to string, rejecting files larger than max_size.
fn read_file_bounded(path: &Path, max_size: u64) -> Result<String> {
    let meta = fs::metadata(path)?;
    if meta.len() > max_size {
        return Err(Error::Content {
            path: path.to_path_buf(),
            message: format!(
                "File size {} bytes exceeds limit of {} bytes",
                meta.len(),
                max_size
            ),
        });
    }
    Ok(fs::read_to_string(path)?)
}
```

Replace all `fs::read_to_string` calls in `load_post` and `load_series` with `read_file_bounded(path, config.max_content_file_size)`. This requires threading the `ContentConfig` (or just the limit) through `load_post` and `load_series`.

### Signature changes

```rust
fn load_post(path: &Path, slug: &str, series_slug: Option<&str>, max_file_size: u64) -> Result<Post>
fn load_series(path: &Path, slug: &str, max_file_size: u64) -> Result<(SeriesData, Vec<Post>)>
```

Callers in `ContentCache::load` already have access to `config` so they pass `config.max_content_file_size`.

### Example config

```toml
[content]
repo_path = "/data/repo"
content_dir = "content"
# max_content_file_size = 5242880  # 5MB default, increase if needed
```

---

## Fix 3: Configurable Git Limits (Low)

### Problem

`GIT_MAX_BODY_SIZE` (100MB) is hardcoded in `crates/riley-cms-api/src/handlers.rs:417` and `GIT_CGI_TIMEOUT` (300s) is hardcoded in `crates/riley-cms-core/src/git.rs:24`. Operators may want to tune these for their environment (e.g., smaller limits on memory-constrained servers, longer timeouts for large repos on slow connections).

### Approach

Add optional `max_body_size` and `cgi_timeout_secs` fields to the existing `GitConfig` struct. The API handler reads the body size limit from config; the core crate reads the timeout from config.

### Files to modify

| File | Changes |
|------|---------|
| `crates/riley-cms-core/src/config.rs` | Add fields to `GitConfig` with defaults |
| `crates/riley-cms-core/src/git.rs` | Accept timeout as parameter instead of using constant |
| `crates/riley-cms-api/src/handlers.rs` | Read body size limit from config instead of constant |
| `riley_cms.example.toml` | Document the new options |

### Config change

```rust
#[derive(Debug, Clone, Deserialize)]
pub struct GitConfig {
    /// Explicit path to git-http-backend binary (optional, auto-discovered if not set)
    pub backend_path: Option<PathBuf>,
    /// Maximum request body size for git operations in bytes. Default: 100MB.
    #[serde(default = "default_git_max_body_size")]
    pub max_body_size: u64,
    /// Timeout for git-http-backend CGI process in seconds. Default: 300 (5 minutes).
    #[serde(default = "default_git_cgi_timeout")]
    pub cgi_timeout_secs: u64,
}

fn default_git_max_body_size() -> u64 {
    100 * 1024 * 1024 // 100 MB
}

fn default_git_cgi_timeout() -> u64 {
    300 // 5 minutes
}
```

### git.rs changes

Remove the `GIT_CGI_TIMEOUT` constant. Accept timeout as a `Duration` parameter in `GitCgiCompletion::wait()`:

```rust
pub async fn wait(mut self, cgi_timeout: Duration) -> Result<std::process::ExitStatus> {
    // ... existing logic using cgi_timeout instead of GIT_CGI_TIMEOUT ...
}
```

The `run_cgi` function already accepts `max_body_size` as a parameter, so no change needed there.

### handlers.rs changes

Remove the `GIT_MAX_BODY_SIZE` constant. Read both values from `state.config`:

```rust
let git_config = state.config.git.as_ref();
let max_body_size = git_config
    .map(|g| g.max_body_size)
    .unwrap_or(100 * 1024 * 1024);
let cgi_timeout = Duration::from_secs(
    git_config.map(|g| g.cgi_timeout_secs).unwrap_or(300)
);
```

Pass `cgi_timeout` into the spawned completion task:

```rust
tokio::spawn(async move {
    match cgi_response.completion.wait(cgi_timeout).await {
        // ...
    }
});
```

### Example config

```toml
[git]
# backend_path = "/usr/lib/git-core/git-http-backend"
# max_body_size = 104857600    # 100MB default
# cgi_timeout_secs = 300       # 5 minutes default
```

---

## Verification

1. `cargo build --all` — compiles cleanly
2. `cargo test --all` — all tests pass
3. `cargo clippy --all -- -D warnings` — no warnings
4. Confirm existing tests still pass (config parsing tests need updating for new fields)
5. Confirm default behavior is unchanged (all new fields have defaults matching current hardcoded values)

## Dependency Changes

None. All fixes use existing dependencies.

## Risk Assessment

All three fixes are backwards-compatible:
- Fix 1: Changes internal middleware config, no API or config file changes
- Fix 2: New optional config field with a permissive default (5MB per file is generous for blog content)
- Fix 3: New optional config fields with defaults matching current behavior
