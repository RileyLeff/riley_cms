# riley_cms

[![Crates.io](https://img.shields.io/crates/v/riley-core.svg)](https://crates.io/crates/riley-core)
[![Documentation](https://docs.rs/riley-core/badge.svg)](https://docs.rs/riley-core)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

A minimal, self-hosted headless CMS for personal blogs. Rust, no database, no GUI.

**Git is your content database. S3/R2 is your asset database. riley_cms is the stateless glue that serves it.**

## Philosophy

- **Git is the database for content** - version controlled, portable, yours forever
- **S3/R2 is the database for assets** - cheap, fast, globally distributed
- **The API is stateless glue** - easy to deploy, easy to scale
- **Opinionated by design** - less config, less bikeshedding
- **Headless** - you control the frontend, riley_cms serves JSON

## Quick Start

### Install

```bash
cargo install riley-cli
```

Or build from source:

```bash
git clone https://github.com/rileyleff/riley_cms
cd riley_cms
cargo build --release
```

### Initialize Content

```bash
riley_cms init my-blog
cd my-blog
```

This creates an example content structure:

```
my-blog/
├── riley_cms.toml
└── content/
    ├── hello-world/
    │   ├── config.toml
    │   └── content.mdx
    └── my-series/
        ├── series.toml
        ├── part-one/
        │   ├── config.toml
        │   └── content.mdx
        └── part-two/
            ├── config.toml
            └── content.mdx
```

### Configure

Edit `riley_cms.toml`:

```toml
[content]
repo_path = "."
content_dir = "content"

[storage]
backend = "s3"
bucket = "my-assets"
region = "auto"
endpoint = "https://xxx.r2.cloudflarestorage.com"
public_url_base = "https://assets.example.com"

[server]
host = "0.0.0.0"
port = 8080
cors_origins = ["https://mysite.com"]
```

### Run

```bash
riley_cms serve
```

## Content Structure

Posts live in directories with `config.toml` + `content.mdx`:

```
content/
├── my-post/
│   ├── config.toml
│   └── content.mdx
└── my-series/
    ├── series.toml          # Makes this a series
    ├── getting-started/
    │   ├── config.toml
    │   └── content.mdx
    └── advanced-topics/
        ├── config.toml
        └── content.mdx
```

### Post Config

```toml
title = "My Post Title"
subtitle = "Optional subtitle"
preview_text = "A short preview for listings..."
preview_image = "https://assets.example.com/preview.jpg"
tags = ["rust", "programming"]
goes_live_at = 2025-01-15T00:00:00Z  # None = draft, future = scheduled
order = 1  # For series posts
```

### Series Config

```toml
title = "My Series"
description = "Learn something cool"
preview_image = "https://assets.example.com/series.jpg"
goes_live_at = 2025-01-15T00:00:00Z
```

## API

| Endpoint | Description |
|----------|-------------|
| `GET /posts` | List all live posts |
| `GET /posts/:slug` | Get a single post with content |
| `GET /posts/:slug/raw` | Get raw MDX content only |
| `GET /series` | List all live series |
| `GET /series/:slug` | Get series with ordered posts |
| `GET /assets` | List assets in bucket |
| `GET /health` | Health check |
| `* /git/{*path}` | Git Smart HTTP (requires Basic Auth) |

### Query Parameters

- `?include_drafts=true` - Include unpublished posts (requires auth)
- `?include_scheduled=true` - Include future-dated posts (requires auth)
- `?limit=N` - Limit results (default: 50)
- `?offset=N` - Skip results for pagination

## Authentication

riley_cms supports two authentication mechanisms:

### API Token (Bearer)

For accessing drafts and scheduled content via the API:

```bash
curl -H "Authorization: Bearer your-api-token" \
  "http://localhost:8080/posts?include_drafts=true"
```

Configure in `riley_cms.toml`:

```toml
[auth]
api_token = "env:API_TOKEN"  # Read from environment variable
# or
api_token = "your-literal-token"
```

### Git Token (Basic Auth)

For pushing content via Git over HTTP:

```bash
git remote add origin http://git:your-token@localhost:8080/git/content
git push origin main
```

Configure in `riley_cms.toml`:

```toml
[auth]
git_token = "env:GIT_AUTH_TOKEN"
```

## Git Server

riley_cms can serve your content repository over HTTP, allowing you to push content updates directly to the server.

### Setup

1. Initialize a bare git repo in your content directory (or use an existing one)
2. Configure the `git_token` in your config
3. Add the remote to your local clone:

```bash
# On your local machine
git remote add cms http://git:your-token@your-server:8080/git/content
git push cms main
```

### Endpoints

| Endpoint | Description |
|----------|-------------|
| `GET /git/{*path}` | Git read operations (fetch/clone) |
| `POST /git/{*path}` | Git write operations (push) |

After a successful push, riley_cms automatically:
1. Refreshes the content cache
2. Fires any configured webhooks

### Response Example

```json
{
  "posts": [
    {
      "slug": "my-post",
      "title": "My Post Title",
      "subtitle": null,
      "preview_text": "A short preview...",
      "preview_image": "https://assets.example.com/preview.jpg",
      "tags": ["rust"],
      "series_slug": null,
      "goes_live_at": "2025-01-15T00:00:00Z"
    }
  ],
  "total": 42,
  "limit": 50,
  "offset": 0
}
```

## CLI

```bash
riley_cms serve              # Run the HTTP API
riley_cms init <path>        # Initialize content structure
riley_cms upload <file>      # Upload asset to S3/R2
riley_cms ls posts           # List posts
riley_cms ls series          # List series
riley_cms ls assets          # List assets
riley_cms validate           # Check content for errors
```

## Crates

| Crate | Description |
|-------|-------------|
| [`riley-core`](https://crates.io/crates/riley-core) | Core library - embed in your own apps |
| [`riley-api`](https://crates.io/crates/riley-api) | Axum HTTP server |
| [`riley-cli`](https://crates.io/crates/riley-cli) | CLI binary |

### Using riley-core as a Library

```rust
use riley_core::{Riley, resolve_config, ListOptions};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = resolve_config(None)?;
    let riley = Riley::from_config(config).await?;

    let posts = riley.list_posts(&ListOptions::default()).await?;
    for post in posts.items {
        println!("{}: {}", post.slug, post.title);
    }

    Ok(())
}
```

## Deployment

### Docker

```dockerfile
FROM rust:1.85 AS builder
WORKDIR /app
COPY . .
RUN cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/riley_cms /usr/local/bin/
EXPOSE 8080
CMD ["riley_cms", "serve"]
```

### Docker Compose

```yaml
services:
  riley:
    build: .
    volumes:
      - ./content:/data/content:ro
      - ./riley_cms.toml:/etc/riley_cms/config.toml:ro
    environment:
      - AWS_ACCESS_KEY_ID=${R2_ACCESS_KEY_ID}
      - AWS_SECRET_ACCESS_KEY=${R2_SECRET_ACCESS_KEY}
    ports:
      - "8080:8080"
```

## Configuration

Config is loaded from (first match wins):

1. `--config <path>` flag
2. `RILEY_CMS_CONFIG` env var
3. `riley_cms.toml` in current directory
4. Walk up ancestors for `riley_cms.toml`
5. `~/.config/riley_cms/config.toml`
6. `/etc/riley_cms/config.toml`

See [riley_cms.example.toml](riley_cms.example.toml) for all options.

## License

MIT
