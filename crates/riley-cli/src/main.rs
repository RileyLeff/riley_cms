//! riley_cms CLI - Command line interface for riley_cms

use anyhow::Result;
use clap::{Parser, Subcommand};
use riley_core::{Riley, resolve_config};
use std::path::PathBuf;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[derive(Parser)]
#[command(name = "riley_cms")]
#[command(about = "A minimal, self-hosted CMS for personal blogs")]
#[command(version)]
struct Cli {
    /// Path to config file
    #[arg(short, long, global = true)]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Run the HTTP API server
    Serve,

    /// Initialize a new content repo with example structure
    Init {
        /// Directory to initialize (defaults to current directory)
        #[arg(default_value = ".")]
        path: PathBuf,
    },

    /// Upload an asset to R2
    Upload {
        /// File to upload
        file: PathBuf,

        /// Destination path in bucket (optional)
        #[arg(short, long)]
        path: Option<String>,
    },

    /// List content or assets
    Ls {
        #[command(subcommand)]
        what: LsCommands,
    },

    /// Validate content structure and configs
    Validate,
}

#[derive(Subcommand)]
enum LsCommands {
    /// List posts
    Posts {
        /// Include drafts
        #[arg(long)]
        drafts: bool,
    },

    /// List series
    Series {
        /// Include drafts
        #[arg(long)]
        drafts: bool,
    },

    /// List assets in bucket
    Assets,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer())
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,tower_http=debug".into()),
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Serve => cmd_serve(cli.config.as_deref()).await,
        Commands::Init { path } => cmd_init(&path).await,
        Commands::Upload { file, path } => {
            cmd_upload(cli.config.as_deref(), &file, path.as_deref()).await
        }
        Commands::Ls { what } => cmd_ls(cli.config.as_deref(), what).await,
        Commands::Validate => cmd_validate(cli.config.as_deref()).await,
    }
}

async fn cmd_serve(config_path: Option<&std::path::Path>) -> Result<()> {
    let config = resolve_config(config_path)?;
    let riley = Riley::from_config(config).await?;
    riley_api::serve(riley).await?;
    Ok(())
}

async fn cmd_init(path: &std::path::Path) -> Result<()> {
    use std::fs;

    let content_dir = path.join("content");
    let example_post = content_dir.join("hello-world");

    // Create directories
    fs::create_dir_all(&example_post)?;

    // Create example post config
    let config_toml = r#"title = "Hello World"
preview_text = "Welcome to your new blog!"
# goes_live_at = 2025-01-01T00:00:00Z  # Uncomment to publish
"#;
    fs::write(example_post.join("config.toml"), config_toml)?;

    // Create example content
    let content_mdx = r#"# Hello World

Welcome to your new blog powered by **riley_cms**!

This is an example post written in MDX. You can use:

- Regular Markdown syntax
- JSX components (rendered by your frontend)
- Code blocks with syntax highlighting

```rust
fn main() {
    println!("Hello from riley_cms!");
}
```

## Getting Started

1. Edit this file or create new post directories
2. Each post needs a `config.toml` and `content.mdx`
3. Set `goes_live_at` in config.toml to publish

Happy blogging!
"#;
    fs::write(example_post.join("content.mdx"), content_mdx)?;

    // Create example config file
    let riley_config = r#"[content]
repo_path = "."
content_dir = "content"

[storage]
bucket = "your-bucket-name"
endpoint = "https://your-account.r2.cloudflarestorage.com"
public_url_base = "https://assets.yourdomain.com"

[server]
host = "0.0.0.0"
port = 8080
cors_origins = ["*"]

[auth]
# git_token = "env:GIT_AUTH_TOKEN"
# api_token = "env:API_TOKEN"
"#;
    fs::write(path.join("riley_cms.toml"), riley_config)?;

    println!("Initialized riley_cms in {}", path.display());
    println!("  - Created content/hello-world/ with example post");
    println!("  - Created riley_cms.toml (edit with your settings)");
    println!();
    println!("Next steps:");
    println!("  1. Edit riley_cms.toml with your S3/R2 credentials");
    println!("  2. Run `riley_cms serve` to start the API");

    Ok(())
}

async fn cmd_upload(
    config_path: Option<&std::path::Path>,
    file: &std::path::Path,
    dest: Option<&str>,
) -> Result<()> {
    let config = resolve_config(config_path)?;
    let riley = Riley::from_config(config).await?;

    println!("Uploading {}...", file.display());
    let asset = riley.upload_asset(file, dest).await?;
    println!("{}", asset.url);

    Ok(())
}

async fn cmd_ls(config_path: Option<&std::path::Path>, what: LsCommands) -> Result<()> {
    let config = resolve_config(config_path)?;
    let riley = Riley::from_config(config).await?;

    match what {
        LsCommands::Posts { drafts } => {
            let opts = riley_core::ListOptions {
                include_drafts: drafts,
                include_scheduled: drafts,
                limit: None,
                offset: None,
            };
            let result = riley.list_posts(&opts).await?;

            if result.items.is_empty() {
                println!("No posts found.");
            } else {
                for post in result.items {
                    let status = match post.goes_live_at {
                        None => "[draft]",
                        Some(date) if date > chrono::Utc::now() => "[scheduled]",
                        Some(_) => "[live]",
                    };
                    println!("{} {} - {}", status, post.slug, post.title);
                }
                println!("\nTotal: {} posts", result.total);
            }
        }
        LsCommands::Series { drafts } => {
            let opts = riley_core::ListOptions {
                include_drafts: drafts,
                include_scheduled: drafts,
                limit: None,
                offset: None,
            };
            let result = riley.list_series(&opts).await?;

            if result.items.is_empty() {
                println!("No series found.");
            } else {
                for series in result.items {
                    let status = match series.goes_live_at {
                        None => "[draft]",
                        Some(date) if date > chrono::Utc::now() => "[scheduled]",
                        Some(_) => "[live]",
                    };
                    println!(
                        "{} {} - {} ({} posts)",
                        status, series.slug, series.title, series.post_count
                    );
                }
                println!("\nTotal: {} series", result.total);
            }
        }
        LsCommands::Assets => {
            let mut total = 0usize;
            let mut opts = riley_core::AssetListOptions::default();

            loop {
                let result = riley.list_assets(&opts).await?;

                for asset in &result.assets {
                    let size = format_size(asset.size);
                    println!(
                        "{:>8}  {}  {}",
                        size,
                        asset.last_modified.format("%Y-%m-%d"),
                        asset.key
                    );
                }
                total += result.assets.len();

                match result.next_continuation_token {
                    Some(token) => opts.continuation_token = Some(token),
                    None => break,
                }
            }

            if total == 0 {
                println!("No assets found.");
            } else {
                println!("\nTotal: {} assets", total);
            }
        }
    }

    Ok(())
}

async fn cmd_validate(config_path: Option<&std::path::Path>) -> Result<()> {
    let config = resolve_config(config_path)?;
    let riley = Riley::from_config(config).await?;

    let errors = riley.validate_content().await?;

    if errors.is_empty() {
        println!("✓ Content is valid");
        Ok(())
    } else {
        println!("Found {} validation error(s):\n", errors.len());
        for error in &errors {
            println!("  {} : {}", error.path, error.message);
        }
        std::process::exit(1);
    }
}

fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if bytes >= GB {
        format!("{:.1}G", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1}M", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1}K", bytes as f64 / KB as f64)
    } else {
        format!("{}B", bytes)
    }
}
