# Security Fixes Plan (5 Issues from Gemini Review)

## Overview

Fix all 5 security/production issues identified in the Gemini code review, ordered by severity.

---

## Fix 1: Symlink Traversal in Content Loading (HIGH)

**Problem:** `fs::read_dir` entries checked with `path.is_dir()` follow symlinks. A malicious git committer could create a symlink `content.mdx -> /etc/passwd` and expose arbitrary files via the API.

**Files:** `crates/riley-core/src/content.rs`

**Approach:** Use `entry.file_type()` (which does NOT follow symlinks on the DirEntry) to detect and skip symlinks with a warning. Apply in two locations:
1. `ContentCache::load()` loop (line ~46)
2. `ContentCache::load_series()` loop (line ~168)

Insert symlink check immediately after getting the entry, before the `is_dir()` check:
```rust
let file_type = entry.file_type()?;
if file_type.is_symlink() {
    tracing::warn!("Skipping symlink in content directory: {:?}", entry.path());
    continue;
}
if !file_type.is_dir() {
    continue;
}
```

Also add a symlink check inside `load_post()` for the `config.toml` and `content.mdx` files themselves (a directory could be real but contain symlinked files):
```rust
fn is_safe_file(path: &Path) -> bool {
    match fs::symlink_metadata(path) {
        Ok(meta) => !meta.file_type().is_symlink(),
        Err(_) => false,
    }
}
```

**Dependencies:** None
**Tests:** Add test with `std::os::unix::fs::symlink` creating a symlink in a temp content dir, assert it's skipped.

---

## Fix 2: DNS Rebinding (TOCTOU) in Webhooks (MEDIUM)

**Problem:** `validate_webhook_url()` resolves DNS and checks IPs, but `send_webhook()` passes the original URL to reqwest which resolves again. DNS could change between checks.

**Files:** `crates/riley-core/src/lib.rs`

**Approach:** Merge validation into `send_webhook()`. Resolve DNS once, validate all IPs, then use `reqwest::ClientBuilder::resolve(host, validated_addr)` to pin the connection to the validated IP. Remove the separate `validate_webhook_url()` call from `fire_webhooks()`.

Flow:
1. Parse URL, extract host/port
2. Resolve to socket addrs
3. Find first non-private IP (reject if none)
4. Build reqwest client with `.resolve(host, safe_addr)`
5. Send using original URL (reqwest uses pinned IP, TLS/SNI still works)

Keep `is_private_ip()`, `is_link_local()` as private helpers but restructure so `send_webhook` does the full validation+send atomically.

**Dependencies:** None (reqwest 0.12 has `.resolve()`)
**Tests:** Existing SSRF tests remain valid. Add a test that verifies `localhost` webhooks are rejected.

---

## Fix 3: Rate Limiting (MEDIUM)

**Problem:** No rate limiting on auth endpoints. Brute-force attacks possible on API tokens and git credentials.

**Files:**
- `Cargo.toml` (workspace)
- `crates/riley-api/Cargo.toml`
- `crates/riley-api/src/lib.rs`

**Approach:** Use `tower-governor` 0.8.0 (compatible with tower 0.5 + axum 0.8). Apply per-IP rate limiting as a layer on the router.

Limits: 50 burst capacity, replenish 10/second. This allows normal browsing (page + assets) but blocks brute-force scripts.

```rust
use tower_governor::{GovernorLayer, governor::GovernorConfigBuilder};

let governor_conf = GovernorConfigBuilder::default()
    .per_second(10)
    .burst_size(50)
    .finish()
    .unwrap();

// Add as layer before auth middleware
.layer(GovernorLayer { config: &governor_conf })
```

**Dependencies:**
- Workspace: `tower-governor = "0.8"`
- riley-api: `tower-governor = { workspace = true }`

**Tests:** Verify the server still starts and responds. Rate limit testing is best done manually with a load tool.

---

## Fix 4: Graceful Shutdown (LOW)

**Problem:** `axum::serve(listener, app).await` has no signal handler. Process kills sever in-flight requests.

**Files:** `crates/riley-api/src/lib.rs`

**Approach:** Add `with_graceful_shutdown(shutdown_signal())` to the serve call. The signal handler listens for SIGINT (Ctrl+C) and SIGTERM (Docker/K8s stop).

```rust
axum::serve(listener, app)
    .with_graceful_shutdown(shutdown_signal())
    .await?;

async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();
    #[cfg(unix)]
    let mut sigterm = tokio::signal::unix::signal(
        tokio::signal::unix::SignalKind::terminate()
    ).expect("failed to install SIGTERM handler");

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        #[cfg(unix)]
        _ = sigterm.recv() => {},
        #[cfg(not(unix))]
        _ = terminate => {},
    }
    tracing::info!("Shutdown signal received, draining connections...");
}
```

**Dependencies:** None (tokio "full" includes signal)
**Tests:** Existing tests unaffected. Manual verification: start server, send SIGTERM, confirm clean exit.

---

## Fix 5: Token Length Leak (INFORMATIONAL)

**Problem:** `provided.len() == expected.len()` check before `ct_eq` leaks token length via timing.

**Files:**
- `crates/riley-api/src/middleware.rs`
- `crates/riley-api/Cargo.toml`

**Approach:** Hash both tokens with SHA-256 before comparing. Hashes are always 32 bytes, so length is never leaked. This is the standard approach (used by Django, Rails, etc.).

```rust
use sha2::{Sha256, Digest};

let provided_hash = Sha256::digest(provided_token.trim().as_bytes());
let expected_hash = Sha256::digest(expected_token.as_bytes());
if provided_hash.ct_eq(&expected_hash).into() {
    auth_status = AuthStatus::Admin;
}
```

Remove the `provided.len() == expected.len()` check entirely.

**Dependencies:**
- Workspace: `sha2` already present
- riley-api: add `sha2 = { workspace = true }`

**Tests:** Existing auth tests verify correct behavior. Add test that wrong-length token still fails.

---

## Dependency Summary

| Crate | Add to workspace | Add to riley-api | Add to riley-core |
|-------|-----------------|-----------------|-------------------|
| tower-governor | `"0.8"` | yes | no |
| sha2 | already there | yes (new) | already there |

---

## Verification

1. `cargo build` - clean compilation
2. `cargo test --workspace` - all tests pass
3. `cargo clippy --workspace` - no warnings
4. `cargo fmt --check` - formatted
5. Manual: start server, verify rate limit returns 429 after burst
6. Manual: SIGTERM → clean shutdown log message
7. Test: symlink in content dir is skipped with warning
