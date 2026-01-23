# Final Cleanup: Pre-Release Checklist

Consolidated from Gemini review, Claude review-of-review, and codebase audit.
Prioritized by real-world impact for public internet deployment and crates.io publishing.

---

## Phase 1: Security (Must-Fix Before Publishing)

### 1.1 Sanitize error messages exposed to HTTP clients

**Files:** `crates/riley-cms-api/src/handlers.rs`

**Problem:** Internal error details (git backend messages, S3 SDK errors, filesystem paths) are returned verbatim to clients via `internal_error()` and inline error responses. An attacker probing endpoints can learn infrastructure details.

**Fix:**
- `internal_error()` should return a generic `"Internal server error"` message
- Log the actual error server-side with `tracing::error!`
- Git handler error responses should not include the `e` from `run_cgi`
- Keep specific messages only for client-caused errors (400, 404, 413)

---

### 1.2 Fix CGI response panic

**File:** `crates/riley-cms-api/src/handlers.rs:524-526`

**Problem:**
```rust
.body(axum::body::Body::from(cgi_response.body))
.expect("valid response from CGI headers");
```
If `git-http-backend` returns malformed headers, this panics and crashes the server.

**Fix:** Replace `.expect(...)` with proper error handling:
```rust
.body(axum::body::Body::from(cgi_response.body))
.unwrap_or_else(|e| {
    tracing::error!("Failed to build response from CGI output: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR.into_response()
})
```

---

### 1.3 Webhook SSRF protection

**File:** `crates/riley-cms-core/src/lib.rs` (`fire_webhooks`)

**Problem:** Webhook URLs are POSTed to without validation. A misconfigured or malicious URL could hit internal services (AWS metadata, localhost databases, etc.).

**Fix options (pick one):**
- **Option A (recommended):** Validate webhook URLs at config load time — reject URLs resolving to private/link-local IP ranges (10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16, 169.254.0.0/16, 127.0.0.0/8, ::1)
- **Option B (minimal):** Document the SSRF risk prominently in config docs and README, add a warning log when webhooks are configured

---

### 1.4 Secure CORS defaults

**File:** `crates/riley-cms-api/src/lib.rs:51-66`

**Problem:** When `cors_origins` is omitted from config, CORS defaults to `Any` (allow all origins). Users who forget to configure CORS get a wide-open deployment.

**Fix:** Default to no cross-origin access when `cors_origins` is not configured:
```rust
None => CorsLayer::new(), // deny all cross-origin by default
```
Document that users must explicitly set `cors_origins = ["*"]` or specific origins.

---

### 1.5 Constant-time token comparison

**Files:** `crates/riley-cms-api/src/middleware.rs:50`, `crates/riley-cms-api/src/handlers.rs` (git basic auth)

**Problem:** Token comparison uses `==`, which is theoretically vulnerable to timing attacks. While not practically exploitable over a network, security scanners will flag it and it's a one-line fix.

**Fix:** Add `subtle` crate, use `ConstantTimeEq`:
```rust
use subtle::ConstantTimeEq;
if provided_token.as_bytes().ct_eq(expected_token.as_bytes()).into() {
    auth_status = AuthStatus::Admin;
}
```

---

## Phase 2: Robustness (Should-Fix Before Publishing)

### 2.1 Content loading resilience

**File:** `crates/riley-cms-core/src/content.rs` (`ContentCache::load`)

**Problem:** A single malformed `config.toml` or unreadable file prevents the entire cache from loading. The server won't start or refresh.

**Fix:** Wrap individual post/series loading in error handling:
```rust
match Self::load_post(&path, &slug, None) {
    Ok(post) => { posts.insert(slug, post); }
    Err(e) => { tracing::error!("Skipping post at {:?}: {}", path, e); }
}
```
Same pattern for `load_series`. Consider adding a startup log summary: "Loaded X posts, Y series (Z errors skipped)".

---

### 2.2 Cap pagination limit

**File:** `crates/riley-cms-core/src/content.rs` (or `crates/riley-cms-api/src/handlers.rs`)

**Problem:** `limit` query param accepts any `usize` value with no upper bound.

**Fix:** Clamp limit to a maximum (e.g., 500):
```rust
let limit = opts.limit.unwrap_or(50).min(500);
```

---

### 2.3 Warn on token resolution failure

**File:** `crates/riley-cms-api/src/middleware.rs:45`

**Problem:** If the env var holding the API token is unset, `token_config.resolve()` returns `Err` and auth silently degrades to public-only with no indication.

**Fix:** Add a warning on the `Err` branch:
```rust
match token_config.resolve() {
    Ok(expected_token) => { /* existing auth check */ }
    Err(e) => {
        tracing::warn!("Failed to resolve API token: {}. Admin auth disabled.", e);
    }
}
```

---

## Phase 3: Cleanup (First Week After Publishing)

### 3.1 Remove unused `git2` dependency

**Files:** `Cargo.toml`, `crates/riley-cms-core/Cargo.toml`, `crates/riley-cms-core/src/error.rs`

**Problem:** `git2` is declared as a dependency and has a `From<git2::Error>` impl, but no runtime code uses it. It adds ~30s to clean compile time.

**Fix:** Remove from both Cargo.toml files, remove the `From<git2::Error>` impl in error.rs. If you plan to use it later, re-add when needed.

---

### 3.2 Add `cargo-audit` to CI

**File:** `.github/workflows/ci.yml`

**Fix:** Add a job:
```yaml
audit:
  runs-on: ubuntu-latest
  steps:
    - uses: actions/checkout@v4
    - uses: rustsec/audit-check@v2
      with:
        token: ${{ secrets.GITHUB_TOKEN }}
```

---

### 3.3 S3 connectivity check at startup

**File:** `crates/riley-cms-core/src/storage.rs`

**Problem:** S3 misconfig is only discovered on the first API call, not at startup.

**Fix:** Add a `StorageBackend::health_check()` method that does a `HeadBucket` call. Invoke during `Riley::new()` and log a warning (not a hard failure) if it fails.

---

### 3.4 Add SECURITY.md

**File:** `SECURITY.md` (repo root)

Standard open-source vulnerability reporting policy. Template:
- Supported versions
- How to report (email or GitHub security advisories)
- Expected response time
- Disclosure policy

---

## Phase 4: Nice-to-Have (Post-Launch)

These are not blockers. Implement if/when they cause real problems:

- **Asset listing pagination** — Add `limit`/`continuation_token` params to `list_assets`
- **Git payload streaming** — Replace buffered body with streaming if repos grow large
- **Arc<Post> in cache** — Profile first; only matters under significant load
- **Webhook signing** — HMAC signatures so receivers can verify authenticity
- **Webhook retry with backoff** — Currently fire-and-forget

---

## Not Doing (Explicitly Rejected)

| Item | Reason |
|------|--------|
| Rate limiting | Reverse proxy concern (nginx/Cloudflare), not library's job |
| HSTS/CSP/X-Frame-Options | Reverse proxy concern; document recommendation instead |
| Token rotation support | Env var reload on SIGHUP would work, but overkill for v0.1 |
| Request body streaming for git | 100MB cap is sufficient for content repos |
| Edition change from 2024 | Edition 2024 is correct (stabilized in Rust 1.85) |

---

## Execution Order

1. Phase 1 (security) — all items before any publish
2. Phase 2 (robustness) — all items before any publish
3. Publish to crates.io
4. Phase 3 (cleanup) — first week
5. Phase 4 (nice-to-have) — as needed
