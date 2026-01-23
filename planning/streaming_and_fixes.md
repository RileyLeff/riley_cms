# Streaming Git Handler + Test Safety Fix

## Overview

Fix the two remaining issues from the Gemini security review:
1. **Medium: DoS risk** — Git handler buffers up to 100MB in memory per request (both request body and CGI response). Convert to streaming.
2. **Low: Test safety** — `unsafe { env::set_var }` in tests is unsound in multithreaded context. Replace with `temp-env`.

---

## Fix 1: Streaming Git Handler (Medium)

### Problem
- `handlers.rs:456` — `request.into_body().collect().await` buffers entire request body (up to 100MB)
- `git.rs:121` — `child.wait_with_output().await` buffers entire CGI stdout
- Under concurrent large pushes, this can OOM the server

### Approach: Stream both directions

**Request body → stdin:** Convert axum's body into a stream, pipe chunks directly to the child process stdin via a spawned task. Enforce size limit incrementally (count bytes as they flow, abort if exceeded).

**CGI stdout → response body:** Read headers from stdout first (bounded at 16KB), then stream the remaining stdout directly to the HTTP response via `Body::from_stream(ReaderStream::new(...))`.

### Files to modify

| File | Changes |
|------|---------|
| `Cargo.toml` (workspace) | Add `tokio-util = { version = "0.7", features = ["io"] }`, `futures-util = "0.3"` |
| `crates/riley-cms-core/Cargo.toml` | Add `tokio-util`, `futures-util`, `bytes` deps |
| `crates/riley-cms-api/Cargo.toml` | Add `tokio-util`, `futures-util` deps |
| `crates/riley-cms-core/src/git.rs` | New types + rewrite `run_cgi` to streaming |
| `crates/riley-cms-api/src/handlers.rs` | Rewrite `git_handler` to use streaming |

### New types in `git.rs`

```rust
/// Parsed CGI headers (status + headers, no body)
pub struct GitCgiHeaders {
    pub status: u16,
    pub headers: HashMap<String, String>,
}

/// Streaming CGI response — headers parsed, body available as a stream
pub struct GitCgiStreamResponse {
    pub headers: GitCgiHeaders,
    pub body_stream: Pin<Box<dyn Stream<Item = Result<Bytes, io::Error>> + Send>>,
    pub completion: GitCgiCompletion,
}

/// Handle to await process completion (for webhook timing)
pub struct GitCgiCompletion {
    child: Child,
    stderr_task: JoinHandle<String>,
    stdin_task: Option<JoinHandle<Result<(), Error>>>,
}
```

### New `run_cgi` signature

```rust
pub async fn run_cgi(
    &self,
    method: &str,
    path_info: &str,
    query_string: Option<&str>,
    content_type: Option<&str>,
    content_length: Option<u64>,
    body_stream: impl Stream<Item = Result<Bytes, io::Error>> + Send + Unpin + 'static,
    max_body_size: u64,
) -> Result<GitCgiStreamResponse>
```

### Implementation flow

1. **Spawn child process** with piped stdin/stdout/stderr
2. **Spawn stdin_task** — reads chunks from `body_stream`, writes to stdin, counts bytes, aborts if over `max_body_size`
3. **Spawn stderr_task** — buffers stderr (capped at 64KB) for logging
4. **Read CGI headers** from stdout via `read_cgi_headers()` — reads line-by-line until empty line separator, bounded at 16KB
5. **Check stdin_task** — if already finished with error (size exceeded), return error before streaming begins
6. **Return `GitCgiStreamResponse`** — body_stream wraps remaining stdout via `ReaderStream`

### New `read_cgi_headers` helper (generic over `AsyncBufRead`)

```rust
async fn read_cgi_headers<R: AsyncBufRead + Unpin>(reader: &mut R) -> Result<GitCgiHeaders>
```

Reads line-by-line, parses `Key: Value` headers, extracts Status code. Stops at empty line (header/body separator). Fails if headers exceed 16KB.

### Handler changes (`git_handler`)

```rust
// Convert body to stream
let body_stream = request.into_body().into_data_stream()
    .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()));

// Call streaming run_cgi
let cgi_response = backend.run_cgi(
    method.as_str(), &path_info, query_string.as_deref(),
    content_type.as_deref(), content_length,
    body_stream, GIT_MAX_BODY_SIZE as u64,
).await?;

// Build streaming response
let response_body = Body::from_stream(cgi_response.body_stream);

// Spawn completion task for webhooks
tokio::spawn(async move {
    if let Ok((exit_status, _)) = cgi_response.completion.wait().await {
        if is_write_operation && exit_status.success() {
            state_clone.riley_cms.refresh().await.ok();
            state_clone.riley_cms.fire_webhooks().await;
        }
    }
});
```

### Webhook timing

With streaming, the response starts before the process exits. The `GitCgiCompletion` handle awaits process exit in a background task. Webhooks fire only after the process successfully completes. This is correct because:
- Stream completion → stdout closes → child exits → `completion.wait()` resolves → webhooks fire

### Edge cases

| Scenario | Handling |
|----------|----------|
| Body too large | `stdin_task` returns error, stdin pipe closes, child may error. Check before streaming response. |
| Client disconnect mid-body | `stdin_task` sees stream error, stdin closes, child handles gracefully |
| CGI headers > 16KB | `read_cgi_headers` returns error → 500 response |
| CGI headers malformed (EOF) | Parse what's available, empty body |
| Process exits with error | Detected in `completion.wait()`, webhooks skipped |

### Keep `parse_cgi_response` and `GitCgiResponse`

The existing `parse_cgi_response` function and `GitCgiResponse` struct stay as internal helpers — used by existing unit tests. They just become non-pub.

---

## Fix 2: Replace `unsafe { env::set_var }` (Low)

### Problem
`crates/riley-cms-core/src/config.rs:231,236` — uses `unsafe { std::env::set_var(...) }` and `unsafe { std::env::remove_var(...) }` in test code. This is unsound when tests run in parallel (the default).

### Approach
Add `temp-env` as a dev-dependency and use its scoped API.

### Files to modify

| File | Changes |
|------|---------|
| `crates/riley-cms-core/Cargo.toml` | Add `temp-env = "0.3"` to `[dev-dependencies]` |
| `crates/riley-cms-core/src/config.rs` | Rewrite `test_config_value_env` test |

### Before
```rust
#[test]
fn test_config_value_env() {
    unsafe { std::env::set_var("TEST_RILEY_VAR", "from_env"); }
    let val = ConfigValue::Literal("env:TEST_RILEY_VAR".to_string());
    assert_eq!(val.resolve().unwrap(), "from_env");
    unsafe { std::env::remove_var("TEST_RILEY_VAR"); }
}
```

### After
```rust
#[test]
fn test_config_value_env() {
    temp_env::with_var("TEST_RILEY_VAR", Some("from_env"), || {
        let val = ConfigValue::Literal("env:TEST_RILEY_VAR".to_string());
        assert_eq!(val.resolve().unwrap(), "from_env");
    });
}
```

---

## Dependency Summary

| Crate | Workspace | riley-cms-core | riley-cms-api |
|-------|-----------|------------|-----------|
| `tokio-util` (0.7, features=["io"]) | add | add | add |
| `futures-util` (0.3) | add | add | add |
| `bytes` (1) | add | add | — |
| `temp-env` (0.3) | — | dev-dep | — |

Note: `tokio-util`, `futures-util`, and `bytes` are already transitive deps (in Cargo.lock), so no new crates to compile.

---

## Verification

1. `cargo build --all` — clean compilation
2. `cargo test --all` — all 69+ tests pass
3. `cargo clippy --all -- -D warnings` — no warnings
4. `cargo fmt --all -- --check` — formatted
5. Verify existing API integration tests still pass (they use `build_router` with `oneshot`, no streaming needed)
6. The `GIT_MAX_BODY_SIZE` constant stays at 100MB but is now enforced incrementally during streaming
