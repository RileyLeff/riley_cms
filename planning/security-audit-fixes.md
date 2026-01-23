# Security Audit Fixes

Comprehensive plan to address findings from Gemini security review (Jan 2025).

## Priority 1: Critical - SSRF Prevention

### 1a. Redirect-Based SSRF Bypass

**Problem:** `send_webhook` pins the resolved IP for the initial request, but `reqwest` follows redirects by default (up to 10). An attacker's server can respond with `302 Location: http://127.0.0.1:8080/...`, bypassing all IP validation since the redirect creates a new connection without the pinning.

**File:** `crates/riley-cms-core/src/lib.rs` (line ~378)

**Fix:** Disable redirect following on the webhook client.

```rust
// Before:
let client = reqwest::Client::builder()
    .resolve(&host, safe_addr)
    .timeout(std::time::Duration::from_secs(10))
    .build()
    .unwrap_or_else(|_| reqwest::Client::new());

// After:
let client = reqwest::Client::builder()
    .resolve(&host, safe_addr)
    .redirect(reqwest::redirect::Policy::none())
    .timeout(std::time::Duration::from_secs(10))
    .build()
    .unwrap_or_else(|_| reqwest::Client::new());
```

**Test:** Unit test with a mock HTTP server that returns 302 to a private IP; assert the webhook does NOT follow it.

---

### 1b. IPv4-Mapped IPv6 SSRF Bypass

**Problem:** `http://[::ffff:127.0.0.1]:8080/` parses as IPv6, passes all current checks (not loopback `::1`, not ULA, not link-local), but the OS routes it to `127.0.0.1`.

**File:** `crates/riley-cms-core/src/lib.rs` (function `is_safe_ip`, line ~314)

**Fix:** Extract IP safety checks into a dedicated `security` module with proper IPv4-mapped handling.

**New file:** `crates/riley-cms-core/src/security.rs`

```rust
use std::net::IpAddr;

/// Check if an IP address is safe for outbound connections.
/// Rejects loopback, private (RFC 1918), link-local, carrier-grade NAT,
/// IPv4-mapped IPv6 addresses that map to unsafe IPs, multicast, and
/// deprecated site-local IPv6.
pub fn is_safe_ip(ip: &IpAddr) -> bool {
    if ip.is_loopback() || ip.is_unspecified() || ip.is_multicast() {
        return false;
    }

    match ip {
        IpAddr::V4(v4) => {
            let octets = v4.octets();
            // 10.0.0.0/8
            if octets[0] == 10 { return false; }
            // 172.16.0.0/12
            if octets[0] == 172 && (16..=31).contains(&octets[1]) { return false; }
            // 192.168.0.0/16
            if octets[0] == 192 && octets[1] == 168 { return false; }
            // 169.254.0.0/16 (link-local, includes AWS metadata 169.254.169.254)
            if octets[0] == 169 && octets[1] == 254 { return false; }
            // 100.64.0.0/10 (carrier-grade NAT)
            if octets[0] == 100 && (64..=127).contains(&octets[1]) { return false; }
            true
        }
        IpAddr::V6(v6) => {
            // IPv4-mapped (::ffff:0:0/96) and IPv4-compatible (deprecated)
            // Canonicalize to IPv4 and re-check
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_safe_ip(&IpAddr::V4(v4));
            }

            let segments = v6.segments();
            // Unique Local (fc00::/7)
            if (segments[0] & 0xfe00) == 0xfc00 { return false; }
            // Link-local (fe80::/10)
            if (segments[0] & 0xffc0) == 0xfe80 { return false; }
            // Site-local (fec0::/10) - deprecated but block anyway
            if (segments[0] & 0xffc0) == 0xfec0 { return false; }

            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_ipv4_mapped_loopback() {
        let ip: IpAddr = "::ffff:127.0.0.1".parse().unwrap();
        assert!(!is_safe_ip(&ip));
    }

    #[test]
    fn rejects_ipv4_mapped_private() {
        assert!(!is_safe_ip(&"::ffff:10.0.0.1".parse().unwrap()));
        assert!(!is_safe_ip(&"::ffff:192.168.1.1".parse().unwrap()));
        assert!(!is_safe_ip(&"::ffff:172.16.0.1".parse().unwrap()));
    }

    #[test]
    fn rejects_ipv4_mapped_link_local() {
        assert!(!is_safe_ip(&"::ffff:169.254.169.254".parse().unwrap()));
    }

    #[test]
    fn rejects_multicast() {
        assert!(!is_safe_ip(&"ff02::1".parse().unwrap()));
    }

    #[test]
    fn rejects_site_local() {
        assert!(!is_safe_ip(&"fec0::1".parse().unwrap()));
    }

    #[test]
    fn rejects_private_ipv4() {
        assert!(!is_safe_ip(&"10.0.0.1".parse().unwrap()));
        assert!(!is_safe_ip(&"192.168.1.1".parse().unwrap()));
        assert!(!is_safe_ip(&"172.16.0.1".parse().unwrap()));
        assert!(!is_safe_ip(&"100.64.0.1".parse().unwrap()));
    }

    #[test]
    fn rejects_loopback() {
        assert!(!is_safe_ip(&"127.0.0.1".parse().unwrap()));
        assert!(!is_safe_ip(&"::1".parse().unwrap()));
    }

    #[test]
    fn allows_public_ipv4() {
        assert!(is_safe_ip(&"8.8.8.8".parse().unwrap()));
        assert!(is_safe_ip(&"1.1.1.1".parse().unwrap()));
    }

    #[test]
    fn allows_public_ipv6() {
        assert!(is_safe_ip(&"2606:4700:4700::1111".parse().unwrap()));
    }
}
```

**Changes to `lib.rs`:**
- Add `mod security;` declaration
- Add `pub use security::is_safe_ip;` (or keep private, just use `security::is_safe_ip`)
- Remove old `is_private_ip`, `is_link_local`, `is_safe_ip` functions (lines 275-316)
- Update `send_webhook` to call `security::is_safe_ip`

---

## Priority 2: High - Git CGI Zombie Processes

**Problem:** Read operations (clone/fetch) never call `wait()` on the `git-http-backend` child process. Dropped `tokio::process::Child` does NOT kill or reap the process, leaving zombies that accumulate and eventually exhaust PIDs.

**File:** `crates/riley-cms-api/src/handlers.rs` (lines 557-578)

**Fix:** Always spawn the cleanup task, regardless of operation type. Only trigger refresh/webhooks for successful write operations.

```rust
// Before (line 558):
if is_write_operation {
    let state_clone = state.clone();
    tokio::spawn(async move {
        match cgi_response.completion.wait().await {
            Ok(exit_status) => {
                if exit_status.success() {
                    if let Err(e) = state_clone.riley_cms.refresh().await {
                        tracing::error!("Failed to refresh content after git push: {}", e);
                    }
                    state_clone.riley_cms.fire_webhooks().await;
                }
            }
            Err(e) => {
                tracing::error!("Git CGI completion error: {}", e);
            }
        }
    });
}

// After:
let state_clone = state.clone();
tokio::spawn(async move {
    match cgi_response.completion.wait().await {
        Ok(exit_status) => {
            if is_write_operation && exit_status.success() {
                if let Err(e) = state_clone.riley_cms.refresh().await {
                    tracing::error!("Failed to refresh content after git push: {}", e);
                }
                state_clone.riley_cms.fire_webhooks().await;
            }
        }
        Err(e) => {
            tracing::error!("Git CGI completion error: {}", e);
        }
    }
});
```

**Test:** Existing integration tests should still pass. Could add a stress test that performs many clones and checks `/proc` for zombie processes, but this is hard to unit-test in CI.

---

## Priority 3: High - Git CGI Timeout

**Problem:** No timeout on `git-http-backend` execution. A stalling process holds connections open indefinitely and can exhaust file descriptors.

**File:** `crates/riley-cms-core/src/git.rs` (function `GitCgiCompletion::wait`, line 67)

**Fix:** Wrap the child `wait()` in `tokio::time::timeout`. Use 5 minutes (300s) as default — generous enough for large repos but prevents infinite hangs.

```rust
// Before:
pub async fn wait(mut self) -> Result<std::process::ExitStatus> {
    // ... stdin cleanup ...

    let status = self
        .child
        .wait()
        .await
        .map_err(|e| Error::Git(format!("Failed to wait on git-http-backend: {}", e)))?;

    // ... stderr collection ...
    Ok(status)
}

// After:
use tokio::time::{timeout, Duration};

const GIT_CGI_TIMEOUT: Duration = Duration::from_secs(300); // 5 minutes

pub async fn wait(mut self) -> Result<std::process::ExitStatus> {
    // ... stdin cleanup (unchanged) ...

    let status = match timeout(GIT_CGI_TIMEOUT, self.child.wait()).await {
        Ok(result) => result.map_err(|e| {
            Error::Git(format!("Failed to wait on git-http-backend: {}", e))
        })?,
        Err(_elapsed) => {
            tracing::error!("Git CGI process timed out after {}s, killing", GIT_CGI_TIMEOUT.as_secs());
            let _ = self.child.kill().await;
            return Err(Error::Git("Git operation timed out".to_string()));
        }
    };

    // ... stderr collection (unchanged) ...
    Ok(status)
}
```

**Dependencies:** None (`tokio::time` is already available via the `time` feature).

---

## Priority 4: Medium - ETag Concatenation Collision

**Problem:** Key and content bytes are hashed without delimiters, so `slug="ab" content="c"` produces the same hash input as `slug="a" content="bc"`.

**File:** `crates/riley-cms-core/src/content.rs` (function `compute_etag`, line 267)

**Fix:** Length-prefix each field before hashing to make the byte stream unambiguous.

```rust
// Before:
for key in post_keys {
    if let Some(post) = posts.get(key) {
        hasher.update(key.as_bytes());
        hasher.update(post.content.as_bytes());
    }
}
// ...
for key in series_keys {
    hasher.update(key.as_bytes());
}

// After:
for key in post_keys {
    if let Some(post) = posts.get(key) {
        hasher.update(&(key.len() as u64).to_le_bytes());
        hasher.update(key.as_bytes());
        hasher.update(&(post.content.len() as u64).to_le_bytes());
        hasher.update(post.content.as_bytes());
    }
}
// ...
for key in series_keys {
    hasher.update(&(key.len() as u64).to_le_bytes());
    hasher.update(key.as_bytes());
}
```

**Test:** Add to `content::tests`:

```rust
#[test]
fn test_etag_no_concatenation_collision() {
    // "ab" + "c" should differ from "a" + "bc"
    let mut posts_a = HashMap::new();
    posts_a.insert("ab".to_string(), Post { content: "c".to_string(), /* ... */ });

    let mut posts_b = HashMap::new();
    posts_b.insert("a".to_string(), Post { content: "bc".to_string(), /* ... */ });

    let etag_a = ContentCache::compute_etag(&posts_a, &HashMap::new());
    let etag_b = ContentCache::compute_etag(&posts_b, &HashMap::new());
    assert_ne!(etag_a, etag_b);
}
```

**Note:** This is a breaking change for any CDN/client caching the old ETags. All cached content will be invalidated on deploy (a one-time cache miss, acceptable).

---

## Priority 5: Low - Informational/Future

These are noted but not immediately actionable:

| Issue | Risk | Notes |
|-------|------|-------|
| Unbounded memory (all content in RAM) | Medium (DoS via large push) | Acceptable for a personal blog. Revisit if repo grows. |
| Rate limiting is per-IP only | Low (distributed brute-force) | 10 req/s with constant-time comparison is adequate for a personal CMS. |
| Symlink TOCTOU race | Low (requires filesystem write) | Attacker needs git push access, which already means they're authenticated. |
| No port restriction on webhooks | Low | An attacker would need to control the config file to set webhook URLs. |

---

## Implementation Order

1. Create `security.rs` module with `is_safe_ip` (includes IPv4-mapped + multicast checks)
2. Update `lib.rs`: remove old IP functions, add `mod security`, add `redirect(Policy::none())`
3. Fix zombie processes in `handlers.rs` (move spawn outside `if`)
4. Add timeout to `git.rs` `wait()` function
5. Fix ETag length-prefixing in `content.rs`
6. Run `cargo test --all`, `cargo clippy --all`, `cargo fmt --all`
7. Verify existing tests still pass, add new security unit tests

## New Dependencies

None required. All fixes use existing crate features (`tokio::time`, `reqwest::redirect`, `std::net`).
