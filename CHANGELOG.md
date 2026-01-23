# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] - 2025-01-22

### Added

- Initial release of riley_cms
- **riley-cms-core**: Core library with content parsing, S3 storage, and caching
  - `RileyCms` struct for all CMS operations
  - Content loading from Git-based directory structure
  - Support for standalone posts and series
  - Visibility model: drafts, scheduled, and live content
  - In-memory caching with refresh support
  - ETag generation for HTTP caching
  - Content validation
  - S3/R2 asset storage integration
  - Webhook firing on content updates
- **riley-cms-api**: HTTP API server built on Axum
  - REST endpoints for posts, series, and assets
  - Pagination support with limit/offset
  - Cache-Control and ETag headers
  - CORS configuration
  - Health check endpoint
- **riley-cms-cli**: Command-line interface
  - `serve` - Run the HTTP API server
  - `init` - Initialize content structure
  - `upload` - Upload assets to S3/R2
  - `ls` - List posts, series, or assets
  - `validate` - Check content for errors
- Configuration system with directory-based resolution
- Example content structure

[Unreleased]: https://github.com/rileyleff/riley_cms/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/rileyleff/riley_cms/releases/tag/v0.1.0
