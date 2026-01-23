# Security Review #4 — Implementation Plan

Findings from fresh Gemini security audit (Jan 2025), after streaming/temp-env/TraceLayer/file-size/git-config fixes were applied.

---

## Fix 1: Empty Token Bypass (Medium — Exploitable Bug)

### Problem

Both authentication paths accept empty strings as valid tokens:

1. **Git Basic Auth** (`handlers.rs:408-410`): If `git_token` is configured as `""`, then `provided.ct_eq(expected)` on two empty byte slices returns true. Any request with empty password (e.g., `Basic Z2l0Og==` = `git:`) would authenticate.

2. **API Bearer Auth** (`middleware.rs:59-61`): If `api_token` is `""`, then `Sha256::digest(b"")` equals `Sha256::digest(b"")`. Any request with `Authorization: Bearer ` (empty after trimming) would get `AuthStatus::Admin`.

This is exploitable if an operator accidentally sets a token to an empty string (misconfiguration, empty env var).

### Approach

Reject empty tokens at resolution time. After `ConfigValue::resolve()` returns, check that the resolved value is non-empty. If empty, treat as "not configured" (deny access, log warning).

### Files to modify

| File | Changes |
|------|---------|
| `crates/riley-cms-api/src/handlers.rs` | Add empty check after resolving git_token |
| `crates/riley-cms-api/src/middleware.rs` | Add empty check after resolving api_token |
| `crates/riley-cms-api/tests/api.rs` | Add test for empty token rejection |

### Implementation

**handlers.rs** — in `check_git_basic_auth`, after line 366:
```rust
Ok(token) => {
    if token.is_empty() {
        tracing::warn!("Git token resolves to empty string. Git auth disabled.");
        return false;
    }
    token
}
```

**middleware.rs** — in `auth_middleware`, after line 49:
```rust
Ok(expected_token) => {
    if expected_token.is_empty() {
        tracing::warn!("API token resolves to empty string. Admin auth disabled.");
        // fall through to insert Public status
    } else {
        // existing Bearer token check logic
        if let Some(auth_header) = request.headers().get(header::AUTHORIZATION)
            && let Ok(auth_str) = auth_header.to_str()
            && let Some(provided_token) = auth_str.strip_prefix("Bearer ")
        {
            let provided_hash = Sha256::digest(provided_token.trim().as_bytes());
            let expected_hash = Sha256::digest(expected_token.as_bytes());
            if provided_hash.ct_eq(&expected_hash).into() {
                auth_status = AuthStatus::Admin;
            }
        }
    }
}
```

---

## Fix 2: X-Content-Type-Options: nosniff (Low — Defense in Depth)

### Problem

The `get_post_raw` handler returns `text/plain` content but does not set `X-Content-Type-Options: nosniff`. While modern browsers mostly respect the Content-Type, older IE versions and some edge cases may MIME-sniff the response body, potentially interpreting injected HTML/JS in MDX content.

### Approach

Add `X-Content-Type-Options: nosniff` as a global middleware layer. This applies to all responses (API JSON responses, raw content, git responses) and is best practice for any HTTP server. Using a middleware avoids duplicating the header in every handler.

### Files to modify

| File | Changes |
|------|---------|
| `crates/riley-cms-api/src/lib.rs` | Add `SetResponseHeader` layer for nosniff |

### Implementation

In `build_router`, add a `tower_http::set_header::SetResponseHeaderLayer` after the CORS layer:

```rust
use tower_http::set_header::SetResponseHeaderLayer;
use axum::http::HeaderValue;

// In build_router:
.layer(SetResponseHeaderLayer::overriding(
    header::X_CONTENT_TYPE_OPTIONS,
    HeaderValue::from_static("nosniff"),
))
```

`tower_http` already provides this — no new dependencies needed. The `header::X_CONTENT_TYPE_OPTIONS` constant is available from `axum::http::header` (which re-exports from the `http` crate).

---

## Fix 3: Rate Limiter Proxy Awareness (High — Deployment Concern)

### Problem

`tower_governor` with `GovernorConfigBuilder::default()` extracts the client IP from the TCP peer address. When deployed behind a reverse proxy (nginx, Cloudflare, etc.), ALL requests appear to come from the proxy's IP, meaning:
- All legitimate users share one rate-limit bucket
- An attacker who can bypass the proxy rate-limits the entire site

### Approach

Add an optional `trusted_proxies` list to `ServerConfig`. When configured, extract the client IP from `X-Forwarded-For` (skipping trusted proxy IPs from the right). When not configured, continue using peer IP (correct for direct-to-internet deployments).

This uses `tower_governor`'s `GovernorConfigBuilder::key_extractor()` with a custom extractor.

### Files to modify

| File | Changes |
|------|---------|
| `crates/riley-cms-core/src/config.rs` | Add `trusted_proxies` to `ServerConfig` |
| `crates/riley-cms-api/src/lib.rs` | Custom key extractor that respects X-Forwarded-For |
| `riley_cms.example.toml` | Document the new option |

### Config change

```rust
// In ServerConfig:
/// Trusted proxy IP addresses/CIDRs. When set, client IP is extracted from
/// X-Forwarded-For header (rightmost untrusted IP). When empty (default),
/// uses TCP peer address directly (correct for non-proxied deployments).
#[serde(default)]
pub trusted_proxies: Vec<String>,
```

### Key extractor implementation

```rust
use tower_governor::key_extractor::KeyExtractor;
use std::net::IpAddr;

#[derive(Clone)]
struct ProxyAwareKeyExtractor {
    trusted_proxies: Vec<IpAddr>,
}

impl KeyExtractor for ProxyAwareKeyExtractor {
    type Key = IpAddr;

    fn extract<T>(&self, req: &ServiceRequest<T>) -> Result<Self::Key, GovernorError> {
        if self.trusted_proxies.is_empty() {
            // No proxies configured — use peer IP (existing behavior)
            return SmartIpKeyExtractor.extract(req);
        }

        // Parse X-Forwarded-For from right, skip trusted proxies
        if let Some(xff) = req.headers().get("x-forwarded-for") {
            if let Ok(xff_str) = xff.to_str() {
                let ips: Vec<&str> = xff_str.split(',').map(|s| s.trim()).collect();
                // Walk from right to find first untrusted IP
                for ip_str in ips.iter().rev() {
                    if let Ok(ip) = ip_str.parse::<IpAddr>() {
                        if !self.trusted_proxies.contains(&ip) {
                            return Ok(ip);
                        }
                    }
                }
            }
        }

        // Fallback to peer IP
        SmartIpKeyExtractor.extract(req)
    }
}
```

### Example config

```toml
[server]
# When behind a reverse proxy, list proxy IPs so rate limiting uses the real client IP.
# trusted_proxies = ["10.0.0.1", "172.16.0.0/12"]
```

**Note**: CIDR support would require adding the `ipnet` crate. For v1, we can support individual IPs only. CIDR can be added later if needed.

---

## Fix 4: Unbounded Total Cache Size (Medium — Resource Exhaustion)

### Problem

The per-file size limit (Fix 2 from previous review, 5MB default) prevents a single giant file from causing OOM. However, there is no limit on the *total* size of the content cache. An attacker with git push access could add thousands of 5MB files (e.g., 1000 × 5MB = 5GB), exhausting server memory on the next `refresh()`.

### Approach

Add a `max_total_content_size` config option (default: 100MB). Track cumulative bytes read during `ContentCache::load()`. If the total exceeds the limit, stop loading and return an error (or log a warning and serve what was loaded).

### Files to modify

| File | Changes |
|------|---------|
| `crates/riley-cms-core/src/config.rs` | Add `max_total_content_size` to `ContentConfig` |
| `crates/riley-cms-core/src/content.rs` | Track cumulative size during `load()`, stop if exceeded |
| `riley_cms.example.toml` | Document the new option |

### Config change

```rust
// In ContentConfig:
/// Maximum total size in bytes for all content files combined.
/// If exceeded during loading, remaining files are skipped with a warning.
/// Default: 100MB.
#[serde(default = "default_max_total_content_size")]
pub max_total_content_size: u64,

fn default_max_total_content_size() -> u64 {
    100 * 1024 * 1024 // 100 MB
}
```

### Content loading change

In `ContentCache::load()`, add a running total:

```rust
let mut total_bytes: u64 = 0;

// In the loop, after each successful read_file_bounded:
total_bytes += content.len() as u64;
if total_bytes > config.max_total_content_size {
    tracing::error!(
        "Total content size ({} bytes) exceeds limit ({} bytes). Stopping content load.",
        total_bytes, config.max_total_content_size
    );
    break;
}
```

This requires `read_file_bounded` to return the content so we can measure it (it already does — the caller just needs to track the cumulative size).

### Example config

```toml
[content]
repo_path = "/data/repo"
# max_total_content_size = 104857600  # 100MB default
```

---

## Fix 5: TOCTOU in Symlink Check (Info — Accepted Risk with Documentation)

### Problem

The symlink check in `ContentCache::load()` uses `DirEntry::file_type()` which does not follow symlinks — this correctly identifies symlinks. However, between the `file_type()` check and the subsequent `read_file_bounded()` call, a symlink could theoretically be swapped in (TOCTOU race).

### Assessment

This is effectively unexploitable in the riley_cms threat model:
1. The attacker would need simultaneous git push access AND local filesystem access to the server
2. The race window is microseconds
3. The content directory is a git checkout — git does not create symlinks during normal operations
4. The per-file size limit still applies even if a symlink is followed

### Approach

**No code change.** Add a comment documenting the accepted risk. The defense-in-depth provided by `O_NOFOLLOW`/fd-based reads would add significant complexity for a near-zero-probability attack.

### Files to modify

| File | Changes |
|------|---------|
| `crates/riley-cms-core/src/content.rs` | Add clarifying comment at the symlink check |

### Comment

```rust
// Security: DirEntry::file_type() does NOT follow symlinks,
// preventing symlink traversal attacks (e.g., content.mdx -> /etc/passwd).
// Note: A theoretical TOCTOU race exists between this check and subsequent
// file reads, but it requires local filesystem access during the microsecond
// window and is not exploitable via the git push interface alone.
```

---

## Dependency Changes

| Crate | Type | Purpose |
|-------|------|---------|
| `ipnet` (optional) | riley-cms-core | CIDR parsing for trusted_proxies (defer to v2 if individual IPs suffice) |

All other changes use existing dependencies (`tower_http`, `tower_governor`, `axum::http`).

---

## Priority Order

1. **Fix 1** (Empty token bypass) — exploitable bug, fix immediately
2. **Fix 2** (nosniff header) — one-line fix, trivial
3. **Fix 3** (Rate limiter proxy) — important for proxied deployments
4. **Fix 4** (Total cache size) — defense in depth for compromised git access
5. **Fix 5** (TOCTOU comment) — documentation only

---

## Verification

1. `cargo build --all` — clean compilation
2. `cargo test --all` — all tests pass
3. `cargo clippy --all -- -D warnings` — no warnings
4. Add tests:
   - Empty token returns false / does not grant Admin
   - Nosniff header present on all response types
   - Rate limiter respects X-Forwarded-For when trusted_proxies configured
   - Total content size limit stops loading at threshold
5. Confirm default behavior unchanged (all new fields have safe defaults)
