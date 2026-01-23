# Implementation Plan: Remediation & Feature Completion

This document outlines the plan to address architecture, security, and feature gaps identified in the code review. The primary goals are securing the API, fixing async runtime blocking, and fulfilling the promise of a self-hosted Git server.

## Summary of Priorities

1.  **Critical**: Security vulnerabilities (Auth) and Runtime Stability (Blocking I/O).
2.  **High**: Functional gaps (Git Server) required to match documentation/design.
3.  **Medium**: Test coverage and code hygiene.

---

## Phase 1: Security & Stability (Critical)

**Goal**: Ensure the application is secure by default and does not block the async executor.

### 1.1 Fix Async Blocking I/O
**Priority**: Critical
**Complexity**: Moderate
**Location**: `crates/riley-cms-core/src/lib.rs`

*   **Issue**: `ContentCache::load` performs synchronous filesystem I/O. Calling this directly in `Riley::refresh` (an async function) blocks the Tokio thread, potentially stalling the server.
*   **Changes**:
    *   Modify `Riley::from_config` and `Riley::refresh` to use `tokio::task::spawn_blocking`.
    *   Ensure `ContentConfig` is cloned/moved into the closure correctly (requires `'static` lifetime).
*   **Testing**:
    *   Existing integration tests should pass.
    *   Add a load test (optional but recommended) to verify server responsiveness during a refresh.

### 1.2 Implement Authentication Middleware
**Priority**: Critical
**Complexity**: Moderate
**Location**: `crates/riley-cms-api/src/middleware.rs`, `crates/riley-cms-api/src/lib.rs`, `crates/riley-cms-api/src/handlers.rs`

*   **Issue**: Drafts and scheduled posts are currently accessible by anyone simply by adding `?include_drafts=true`. `AuthConfig` is defined but unused.
*   **Changes**:
    *   **Middleware**: Implement a Tower middleware layer in `middleware.rs`.
        *   Check for `Authorization: Bearer <token>`.
        *   Compare against `state.config.auth.api_token`.
        *   Store an `AuthStatus` (enum: `Public`, `Admin`) in request extensions.
    *   **Handlers**: Update `list_posts` and `list_series` in `handlers.rs`.
        *   Retrieve `AuthStatus` from extensions.
        *   If `ListQuery` requests drafts/scheduled items AND `AuthStatus` is not `Admin`, return `401 Unauthorized`.
*   **Dependencies**: 1.1 (Stable foundation).
*   **Testing**: Manual `curl` tests with and without tokens.

---

## Phase 2: Testing Infrastructure (High)

**Goal**: Establish confidence in the API layer before adding complex Git features.

### 2.1 API Integration Tests
**Priority**: High
**Complexity**: Moderate
**Location**: `crates/riley-cms-api/tests/api.rs` (New File)

*   **Issue**: No tests for the HTTP layer.
*   **Changes**:
    *   Add `dev-dependencies`: `tower`, `hyper`, `mime`.
    *   Create a test harness that initializes `Riley` with a temp directory.
    *   **Test Cases**:
        *   `GET /posts` (Public) -> 200 OK.
        *   `GET /posts?include_drafts=true` (No Auth) -> 401 Unauthorized.
        *   `GET /posts?include_drafts=true` (With Auth) -> 200 OK + Drafts included.
        *   `GET /health` -> 200 OK.
        *   Verify `Cache-Control` headers are present on public routes and absent/private on auth routes.
*   **Dependencies**: 1.2 (Need auth implementation to test it).

---

## Phase 3: Git Server Implementation (High)

**Goal**: Fulfill the "Git is your database" architectural promise by implementing the Git Smart HTTP protocol.

### 3.1 Core Git Operations
**Priority**: High
**Complexity**: Complex
**Location**: `crates/riley-cms-core/src/git.rs` (New File), `crates/riley-cms-core/src/lib.rs`

*   **Issue**: Design docs claim Git server support, but it is missing.
*   **Changes**:
    *   Implement `git_upload_pack` (Read) and `git_receive_pack` (Write).
    *   *Approach*: Use `std::process::Command` to invoke the system `git` binary (specifically `git-http-backend` or direct `git upload-pack` calls) as it is more robust for HTTP transport than pure `git2` implementation, though `git2` is available for repository management.
    *   Ensure `git_receive_pack` triggers `Riley::refresh()` and `fire_webhooks()` upon success.
*   **Dependencies**: 1.1 (Async non-blocking structure).

### 3.2 Git HTTP API Endpoints
**Priority**: High
**Complexity**: Moderate
**Location**: `crates/riley-cms-api/src/handlers.rs`, `crates/riley-cms-api/src/lib.rs`

*   **Issue**: Endpoints defined in design (`/git/*`) are missing.
*   **Changes**:
    *   Add routes:
        *   `GET /git/content/info/refs`
        *   `POST /git/content/git-upload-pack`
        *   `POST /git/content/git-receive-pack`
    *   Implement Basic Auth for these routes using `state.config.auth.git_token`.
    *   Stream request bodies to the Core Git operations and stream stdout back to the response.
*   **Testing**:
    *   Manual `git clone http://localhost:8080/git/content`
    *   Manual `git push`
    *   Verify content updates appear in `GET /posts` after push.

---

## Phase 4: Code Quality & Hygiene (Medium)

**Goal**: Clean up technical debt and edge cases.

### 4.1 Safe Header Parsing
**Priority**: Low
**Complexity**: Simple
**Location**: `crates/riley-cms-api/src/handlers.rs`

*   **Issue**: `unwrap()` used on `header.parse()`.
*   **Changes**:
    *   Replace `.unwrap()` with `map_err` or proper error handling when setting `CACHE_CONTROL` and `ETAG`.
    *   Although inputs are integers (safe), defensive coding is preferred.

### 4.2 Documentation Sync
**Priority**: Medium
**Complexity**: Simple
**Location**: `README.md`, `design.md`

*   **Changes**:
    *   Update documentation to reflect the actual implementation details of the Auth and Git phases once complete.
    *   If Phase 3 (Git Server) is deprioritized, removing mentions of it from `README.md` becomes a Critical task to avoid misleading users.

---

## Detailed Task Dependency Graph

```
Phase 1                    Phase 2              Phase 3                 Phase 4
┌─────────────────┐
│ 1.1 Fix Async   │
│ Blocking I/O    │
└────────┬────────┘
         │
         ├──────────────────────────────────────┐
         │                                      │
         ▼                                      ▼
┌─────────────────┐                   ┌─────────────────┐
│ 1.2 Auth        │                   │ 3.1 Core Git    │
│ Middleware      │                   │ Operations      │
└────────┬────────┘                   └────────┬────────┘
         │                                      │
         ▼                                      ▼
┌─────────────────┐                   ┌─────────────────┐
│ 2.1 API         │                   │ 3.2 Git HTTP    │
│ Integration     │                   │ Endpoints       │
│ Tests           │                   └─────────────────┘
└────────┬────────┘
         │
         ▼
┌─────────────────┐
│ 4.1 Safe Header │
│ Parsing         │
└─────────────────┘
```

---

## Implementation Order (Recommended)

| Order | Task | Est. Complexity | Blocking |
|-------|------|-----------------|----------|
| 1 | 1.1 Fix Async Blocking I/O | Moderate | None |
| 2 | 1.2 Auth Middleware | Moderate | 1.1 |
| 3 | 2.1 API Integration Tests | Moderate | 1.2 |
| 4 | 3.1 Core Git Operations | Complex | 1.1 |
| 5 | 3.2 Git HTTP Endpoints | Moderate | 3.1 |
| 6 | 4.1 Safe Header Parsing | Simple | None |
| 7 | 4.2 Documentation Sync | Simple | 3.2 |

---

*Generated via collaborative review with Gemini and Claude Code*
