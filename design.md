```markdown
# riley_cms

A minimal, self-hosted CMS for personal blogs. Rust, no database, no GUI. Git is your content database, S3 is your asset database, and riley_cms is the stateless glue that serves it.

## Philosophy

- Git is the database for content
- S3/R2 is the database for assets
- The API is stateless glue
- The CLI is for you, the API is for your website
- Opinionated by design — less config, less bikeshedding
- Publishable as a lib and bin — use the whole thing or embed the parts you need

## Architecture

Single Rust binary with subcommands:

```
riley_cms serve              # run the HTTP API
riley_cms init               # initialize a new content repo with example structure
riley_cms push               # git add/commit/push content
riley_cms upload <file>      # upload asset to R2, print URL
riley_cms ls posts           # list posts
riley_cms ls assets          # list assets in bucket
riley_cms validate           # check content structure and configs for errors
```

Workspace structure:
```
riley_cms/
├── Cargo.toml              # workspace
├── crates/
│   ├── riley-cms-core/     # git ops, s3 ops, content parsing, shared types
│   │                       # publishable as a lib for embedding
│   ├── riley-cms-api/      # axum server
│   └── riley-cms-cli/      # clap CLI
├── examples/
│   └── content/            # example content repo structure
├── README.md
└── riley_cms.example.toml
```

## Configuration

All config in `riley_cms.toml`:

```toml
[content]
repo_path = "/data/repo"
content_dir = "content"  # relative to repo root

[storage]
backend = "s3"  # only option for now, but structured for future backends
bucket = "my-assets"
region = "auto"  # R2 uses "auto"
endpoint = "https://xxx.r2.cloudflarestorage.com"
public_url_base = "https://assets.mydomain.com"  # for generating asset URLs

[server]
host = "0.0.0.0"
port = 8080
cors_origins = ["https://mysite.com"]  # allowed CORS origins, or ["*"] for any
cache_max_age = 60                      # Cache-Control max-age in seconds
cache_stale_while_revalidate = 300      # stale-while-revalidate in seconds

[webhooks]
# Called after successful git push (content update)
on_content_update = ["https://mysite.com/api/revalidate"]
secret = "env:WEBHOOK_SECRET"  # HMAC-SHA256 signing (X-Riley-Cms-Signature header)

[auth]
# Values can be literals or "env:VAR_NAME" to read from environment
git_token = "env:GIT_AUTH_TOKEN"      # for git push auth
api_token = "env:API_TOKEN"           # optional, for accessing drafts/scheduled posts
```

Config resolution (first match wins):
1. `--config <path>` flag if provided
2. `RILEY_CMS_CONFIG` env var if set
3. `riley_cms.toml` in current directory
4. Walk up ancestors looking for `riley_cms.toml`
5. `~/.config/riley_cms/config.toml` (user default)
6. `/etc/riley_cms/config.toml` (system default, mainly for containerized deployments)

## Content Structure (Opinionated)

This is **the** structure. Not configurable. Use it or don't use riley_cms.

```
content/
├── standalone-post/
│   ├── config.toml
│   └── content.mdx
├── another-post/
│   ├── config.toml
│   └── content.mdx
└── my-rust-series/
    ├── series.toml
    ├── getting-started/
    │   ├── config.toml
    │   └── content.mdx
    └── ownership/
        ├── config.toml
        └── content.mdx
```

Rules:
- Directory has `series.toml` → it's a series, recurse into children
- Directory has `config.toml` + `content.mdx` → it's a post
- Slug = folder name (lowercase, kebab-case recommended)
- Series post ordering = explicit `order` field in each post's `config.toml` (alphabetical fallback for ties)
- No frontmatter in MDX — all metadata in TOML
- Deeply nested series (series within series) are not supported
- MDX is served raw — the frontend is responsible for parsing/rendering

## Types

```rust
// === Config types (deserialized from TOML) ===

#[derive(Deserialize)]
struct PostConfig {
    title: String,
    subtitle: Option<String>,
    preview_text: String,
    preview_image: Option<String>,  // URL to asset (usually R2)
    tags: Option<Vec<String>>,
    goes_live_at: Option<DateTime<Utc>>,  // None = draft, Some(past) = live, Some(future) = scheduled
    order: Option<i32>,  // for series posts; alphabetical fallback for ties/missing
}

#[derive(Deserialize)]
struct SeriesConfig {
    title: String,
    description: Option<String>,
    preview_image: Option<String>,
    goes_live_at: Option<DateTime<Utc>>,  // None = draft, Some(past) = live, Some(future) = scheduled
}

// === Domain types (constructed by riley-cms-core) ===

struct Post {
    slug: String,
    config: PostConfig,
    content: String,            // raw MDX (frontend parses)
    series_slug: Option<String>,
}

struct Series {
    slug: String,
    config: SeriesConfig,
    posts: Vec<Post>,           // ordered by `order` field, alphabetical fallback
}

struct Asset {
    key: String,                // path in bucket
    url: String,                // public URL
    size: u64,
    last_modified: DateTime<Utc>,
}
```

## API Routes

```
GET  /posts                     # list all live posts
GET  /posts/:slug               # single post with content
GET  /posts/:slug/raw           # raw MDX only (no wrapper JSON)
GET  /series                    # list all live series
GET  /series/:slug              # series metadata + ordered posts
GET  /assets                    # list assets in bucket
GET  /health                    # healthcheck

# Git smart HTTP protocol
POST /git/git-receive-pack
POST /git/git-upload-pack
GET  /git/info/refs
```

Query params:
- `?include_drafts=true` — include posts/series where `goes_live_at` is null (requires auth)
- `?include_scheduled=true` — include posts/series where `goes_live_at` is in the future (requires auth)
- `?limit=N` — return at most N results (default: 50)
- `?offset=N` — skip first N results (default: 0)

Auth:
- Git endpoints: `Authorization: Bearer <git_token>` or basic auth with token as password
- API `?include_*` params: `Authorization: Bearer <api_token>`
- Public reads (no query params): no auth required

HTTP caching:
- All content endpoints return `Cache-Control: public, max-age=N, stale-while-revalidate=M` (configurable)
- `ETag` header based on content hash
- Supports `If-None-Match` for conditional requests (returns 304 if unchanged)
- Authenticated requests (`?include_drafts` etc.) are not cached (`Cache-Control: private, no-store`)

## API Response Shapes

```json
// GET /posts
{
  "posts": [
    {
      "slug": "standalone-post",
      "title": "My Post",
      "subtitle": null,
      "preview_text": "A short preview...",
      "preview_image": "https://assets.mydomain.com/img/preview.jpg",
      "tags": ["rust", "programming"],
      "series_slug": null,
      "goes_live_at": "2025-01-15T00:00:00Z"
    }
  ],
  "total": 42,
  "limit": 50,
  "offset": 0
}

// GET /posts/:slug
{
  "slug": "standalone-post",
  "title": "My Post",
  "subtitle": null,
  "preview_text": "A short preview...",
  "preview_image": "https://assets.mydomain.com/img/preview.jpg",
  "tags": ["rust", "programming"],
  "series_slug": null,
  "goes_live_at": "2025-01-15T00:00:00Z",
  "content": "# Hello\n\nThis is my MDX content..."
}

// GET /series/:slug
{
  "slug": "my-rust-series",
  "title": "Learning Rust",
  "description": "A series about Rust",
  "preview_image": null,
  "goes_live_at": "2025-01-15T00:00:00Z",
  "posts": [
    { "slug": "getting-started", "title": "Getting Started", "order": 1, ... },
    { "slug": "ownership", "title": "Ownership", "order": 2, ... }
  ]
}
```

## Git Hosting

The service hosts the git repo itself via git-over-HTTP (smart protocol). No GitHub dependency.

- Push: `git push https://cms.mydomain.com/git main`
- Clone: `git clone https://cms.mydomain.com/git` (if you want, can disable public clone)
- Auth via bearer token or basic auth (username ignored, password = token)
- Implementation: invokes `git http-backend` CGI with streaming I/O
- Repo lives on a persistent volume at the configured `repo_path`
- On successful push: refresh content cache, then fire webhooks (async, non-blocking)

On first run, if `repo_path` doesn't exist or is empty, initialize a bare repo.

## S3/R2 Integration

- Use `aws-sdk-s3` crate (works with any S3-compatible storage)
- Assets uploaded via CLI, served directly from R2/S3
- The API lists bucket contents but doesn't proxy files — return URLs to the actual assets
- Presigned URLs: optional future feature for private buckets

CLI upload flow:
```
$ riley_cms upload ./diagram.png
Uploading diagram.png...
https://assets.mydomain.com/diagram.png

$ riley_cms upload ./diagram.png --path images/2025/
Uploading diagram.png...
https://assets.mydomain.com/images/2025/diagram.png
```

## Deployment

Containerized, designed for a VPS (Hetzner, etc).

```dockerfile
FROM rust:1.83 AS builder
WORKDIR /app
COPY . .
RUN cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y git ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/riley_cms /usr/local/bin/
VOLUME /data
EXPOSE 8080
CMD ["riley_cms", "serve", "--config", "/etc/riley_cms/config.toml"]
```

```yaml
# docker-compose.yml
services:
  riley_cms:
    build: .
    volumes:
      - riley_cms_data:/data
      - ./riley_cms.toml:/etc/riley_cms/config.toml:ro
    environment:
      - GIT_AUTH_TOKEN=${GIT_AUTH_TOKEN}
      - API_TOKEN=${API_TOKEN}
      - R2_ACCESS_KEY_ID=${R2_ACCESS_KEY_ID}
      - R2_SECRET_ACCESS_KEY=${R2_SECRET_ACCESS_KEY}
    ports:
      - "8080:8080"
    restart: unless-stopped

volumes:
  riley_cms_data:
```

## Crates

### riley-cms-core (lib)

Publishable, embeddable. No HTTP, no CLI — just the domain logic.

```rust
pub struct RileyCms {
    config: RileyCmsConfig,
}

pub struct ListOptions {
    pub include_drafts: bool,
    pub include_scheduled: bool,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
}

pub struct ListResult<T> {
    pub items: Vec<T>,
    pub total: usize,
}

impl RileyCms {
    pub fn from_config(config: RileyCmsConfig) -> Result<Self>;

    // Content (cached in memory, refreshed on git push)
    pub fn list_posts(&self, opts: &ListOptions) -> Result<ListResult<PostSummary>>;
    pub fn get_post(&self, slug: &str) -> Result<Option<Post>>;
    pub fn list_series(&self, opts: &ListOptions) -> Result<ListResult<SeriesSummary>>;
    pub fn get_series(&self, slug: &str) -> Result<Option<Series>>;
    pub fn validate_content(&self) -> Result<Vec<ValidationError>>;

    // Assets (upload-only; delete via R2 dashboard or rclone)
    pub async fn list_assets(&self) -> Result<Vec<Asset>>;
    pub async fn upload_asset(&self, path: &Path, dest: Option<&str>) -> Result<Asset>;

    // Git (uses git2 or gix)
    pub fn git_receive_pack(&self, input: &[u8]) -> Result<Vec<u8>>;  // refreshes cache, fires webhooks
    pub fn git_upload_pack(&self, input: &[u8]) -> Result<Vec<u8>>;
    pub fn refresh(&self) -> Result<()>;  // manually re-read content from disk

    // Cache support
    pub fn content_etag(&self) -> String;  // hash of current content state
}
```

### riley-cms-api

Axum server. Thin layer over `riley-cms-core`.

### riley-cms-cli

Clap CLI. Thin layer over `riley-cms-core`.

## Crate Dependencies

- `axum` — HTTP server
- `clap` — CLI
- `serde`, `toml` — config and content parsing
- `aws-sdk-s3` — S3/R2 operations
- `chrono` — datetime handling
- `tokio` — async runtime
- `tracing`, `tracing-subscriber` — logging
- `thiserror` — error types
- `tokio-util`, `futures-util`, `bytes` — streaming I/O for git CGI
- `tower-http` — middleware (CORS, tracing, etc)
- `reqwest` — webhook delivery
- `sha2`, `hmac`, `hex` — ETag hashing & webhook HMAC signing

System requirements:
- `git` with `git-http-backend` — serves git repos over HTTP via CGI

Maybe:
- `notify` — for watching content changes (future)

## Documentation Goals

Since this is meant to be published:

1. **README.md** — quick pitch, install, basic usage
2. **docs/getting-started.md** — full walkthrough: install, configure, create content, deploy
3. **docs/content-structure.md** — detailed explanation of the content format
4. **docs/api.md** — API reference
5. **docs/deployment.md** — Docker, docker-compose, Hetzner, Fly.io examples
6. **docs/embedding.md** — using `riley-cms-core` as a library
7. **examples/content/** — example content repo people can copy
8. **riley_cms.example.toml** — annotated example config

## Non-Goals (v1)

- No GUI / admin panel
- No database
- No multi-user / roles
- No image processing / optimization
- No built-in search (do it at the site level or use external service)
- No draft branches / PR workflows
- No comments / reactions
- No analytics
- No RSS (generate it in your site build)

## Future / Maybe

- `riley_cms watch` — rebuild content index on file changes
- Presigned URLs for private assets
- Multiple content directories (e.g., `posts/` and `projects/`)
- Asset manifest file (list of assets with metadata, committed to repo)
- Markdown support (not just MDX)
- SQLite cache for faster queries on large sites
- Asset deletion via CLI (for now, delete via R2 dashboard or rclone)

## Design Decisions

1. **Caching**: Cache content in memory, refresh synchronously on git push (before returning from receive-pack). Manual refresh also available.

2. **Visibility model**: Single `goes_live_at: Option<DateTime>` field. `None` = draft, `Some(past)` = live, `Some(future)` = scheduled. No separate `visible` boolean.

3. **Series ordering**: Explicit `order: i32` field in each post's config.toml. Allows inserting posts without renaming folders. Alphabetical fallback for ties/missing.

4. **Error format**: Simple `{ "error": "message" }`. problem+json is overkill for this use case.

5. **CORS**: Built-in config via `cors_origins` in server section.

6. **Git implementation**: Invoke `git http-backend` CGI with streaming I/O. Simpler than embedding `git2`/`gix`, and inherits git's own protocol handling.

7. **MDX handling**: Serve raw MDX. Frontend is responsible for parsing/rendering.

8. **Asset deletion**: Upload-only for v1. Delete assets via R2 dashboard or rclone.

9. **CLI name**: `riley_cms` to avoid namespace collision.

10. **Config resolution**: Directory-based detection (like cargo/git). Look for `riley_cms.toml` in cwd and ancestors, fall back to `~/.config/riley_cms/config.toml`. Override with `--config` flag or `RILEY_CMS_CONFIG` env var.

11. **HTTP caching**: Return `Cache-Control` and `ETag` headers on content endpoints. Support conditional requests (`If-None-Match` → 304). Authenticated requests bypass cache.

12. **Webhooks**: Fire HTTP POST to configured URLs after successful git push. Async/non-blocking so push returns quickly. Use case: trigger frontend rebuild/revalidation.
```