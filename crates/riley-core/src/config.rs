//! Configuration parsing and resolution for riley_cms

use crate::error::{Error, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// Full configuration for riley_cms
#[derive(Debug, Clone, Deserialize)]
pub struct RileyConfig {
    pub content: ContentConfig,
    pub storage: StorageConfig,
    pub server: Option<ServerConfig>,
    pub webhooks: Option<WebhooksConfig>,
    pub auth: Option<AuthConfig>,
}

/// Content repository configuration
#[derive(Debug, Clone, Deserialize)]
pub struct ContentConfig {
    pub repo_path: PathBuf,
    #[serde(default = "default_content_dir")]
    pub content_dir: String,
}

fn default_content_dir() -> String {
    "content".to_string()
}

/// Storage backend configuration
#[derive(Debug, Clone, Deserialize)]
pub struct StorageConfig {
    #[serde(default = "default_backend")]
    pub backend: String,
    pub bucket: String,
    #[serde(default = "default_region")]
    pub region: String,
    pub endpoint: Option<String>,
    pub public_url_base: String,
}

fn default_backend() -> String {
    "s3".to_string()
}

fn default_region() -> String {
    "auto".to_string()
}

/// Server configuration
#[derive(Debug, Clone, Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default)]
    pub cors_origins: Vec<String>,
    #[serde(default = "default_cache_max_age")]
    pub cache_max_age: u32,
    #[serde(default = "default_cache_stale_while_revalidate")]
    pub cache_stale_while_revalidate: u32,
}

fn default_host() -> String {
    "0.0.0.0".to_string()
}

fn default_port() -> u16 {
    8080
}

fn default_cache_max_age() -> u32 {
    60
}

fn default_cache_stale_while_revalidate() -> u32 {
    300
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: default_host(),
            port: default_port(),
            cors_origins: vec![],
            cache_max_age: default_cache_max_age(),
            cache_stale_while_revalidate: default_cache_stale_while_revalidate(),
        }
    }
}

/// Webhook configuration
#[derive(Debug, Clone, Deserialize)]
pub struct WebhooksConfig {
    #[serde(default)]
    pub on_content_update: Vec<String>,
}

/// Authentication configuration
#[derive(Debug, Clone, Deserialize)]
pub struct AuthConfig {
    pub git_token: Option<ConfigValue>,
    pub api_token: Option<ConfigValue>,
}

/// A config value that can be a literal or env var reference
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum ConfigValue {
    Literal(String),
}

impl ConfigValue {
    /// Resolve the value, reading from env if it starts with "env:"
    pub fn resolve(&self) -> Result<String> {
        match self {
            ConfigValue::Literal(s) => {
                if let Some(var_name) = s.strip_prefix("env:") {
                    std::env::var(var_name).map_err(|_| {
                        Error::Config(format!("Environment variable {} not set", var_name))
                    })
                } else {
                    Ok(s.clone())
                }
            }
        }
    }
}

/// Wrapper for loading config from file
pub struct Config;

impl Config {
    /// Load config from a specific path
    pub fn from_path(path: &Path) -> Result<RileyConfig> {
        let content = std::fs::read_to_string(path)?;
        toml::from_str(&content).map_err(|e| Error::ConfigParse {
            path: path.to_path_buf(),
            source: e,
        })
    }
}

/// Resolve config file path using the resolution order:
/// 1. Explicit path if provided
/// 2. RILEY_CMS_CONFIG env var
/// 3. riley_cms.toml in current directory
/// 4. Walk up ancestors looking for riley_cms.toml
/// 5. ~/.config/riley_cms/config.toml (user default)
/// 6. /etc/riley_cms/config.toml (system default)
pub fn resolve_config(explicit_path: Option<&Path>) -> Result<RileyConfig> {
    let mut searched = Vec::new();

    // 1. Explicit path
    if let Some(path) = explicit_path {
        if path.exists() {
            return Config::from_path(path);
        }
        searched.push(path.to_path_buf());
    }

    // 2. RILEY_CMS_CONFIG env var
    if let Ok(env_path) = std::env::var("RILEY_CMS_CONFIG") {
        let path = PathBuf::from(&env_path);
        if path.exists() {
            return Config::from_path(&path);
        }
        searched.push(path);
    }

    // 3 & 4. Current directory and ancestors
    if let Ok(cwd) = std::env::current_dir() {
        let mut dir = Some(cwd.as_path());
        while let Some(d) = dir {
            let config_path = d.join("riley_cms.toml");
            if config_path.exists() {
                return Config::from_path(&config_path);
            }
            searched.push(config_path);
            dir = d.parent();
        }
    }

    // 5. User default (~/.config/riley_cms/config.toml)
    if let Some(config_dir) = dirs::config_dir() {
        let user_config = config_dir.join("riley_cms").join("config.toml");
        if user_config.exists() {
            return Config::from_path(&user_config);
        }
        searched.push(user_config);
    }

    // 6. System default (/etc/riley_cms/config.toml)
    let system_config = PathBuf::from("/etc/riley_cms/config.toml");
    if system_config.exists() {
        return Config::from_path(&system_config);
    }
    searched.push(system_config);

    Err(Error::ConfigNotFound { searched })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_config_value_literal() {
        let val = ConfigValue::Literal("test".to_string());
        assert_eq!(val.resolve().unwrap(), "test");
    }

    #[test]
    fn test_config_value_env() {
        // SAFETY: This test runs in isolation and doesn't access the env var from other threads
        unsafe {
            std::env::set_var("TEST_RILEY_VAR", "from_env");
        }
        let val = ConfigValue::Literal("env:TEST_RILEY_VAR".to_string());
        assert_eq!(val.resolve().unwrap(), "from_env");
        unsafe {
            std::env::remove_var("TEST_RILEY_VAR");
        }
    }

    #[test]
    fn test_config_value_env_missing() {
        let val = ConfigValue::Literal("env:NONEXISTENT_RILEY_VAR_12345".to_string());
        assert!(val.resolve().is_err());
    }

    #[test]
    fn test_parse_minimal_config() {
        let toml = r#"
[content]
repo_path = "/data/repo"

[storage]
bucket = "my-bucket"
public_url_base = "https://assets.example.com"
"#;
        let config: RileyConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.content.repo_path, PathBuf::from("/data/repo"));
        assert_eq!(config.content.content_dir, "content"); // default
        assert_eq!(config.storage.bucket, "my-bucket");
        assert_eq!(config.storage.backend, "s3"); // default
        assert_eq!(config.storage.region, "auto"); // default
        assert!(config.server.is_none());
        assert!(config.webhooks.is_none());
        assert!(config.auth.is_none());
    }

    #[test]
    fn test_parse_full_config() {
        let toml = r#"
[content]
repo_path = "/data/repo"
content_dir = "posts"

[storage]
backend = "s3"
bucket = "my-bucket"
region = "us-east-1"
endpoint = "https://s3.amazonaws.com"
public_url_base = "https://assets.example.com"

[server]
host = "127.0.0.1"
port = 3000
cors_origins = ["https://example.com", "https://dev.example.com"]
cache_max_age = 120
cache_stale_while_revalidate = 600

[webhooks]
on_content_update = ["https://example.com/webhook"]

[auth]
git_token = "secret123"
api_token = "env:API_TOKEN"
"#;
        let config: RileyConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.content.content_dir, "posts");
        assert_eq!(config.storage.region, "us-east-1");
        assert_eq!(
            config.storage.endpoint,
            Some("https://s3.amazonaws.com".to_string())
        );

        let server = config.server.unwrap();
        assert_eq!(server.host, "127.0.0.1");
        assert_eq!(server.port, 3000);
        assert_eq!(server.cors_origins.len(), 2);
        assert_eq!(server.cache_max_age, 120);

        let webhooks = config.webhooks.unwrap();
        assert_eq!(webhooks.on_content_update.len(), 1);

        let auth = config.auth.unwrap();
        assert!(auth.git_token.is_some());
        assert!(auth.api_token.is_some());
    }

    #[test]
    fn test_server_config_defaults() {
        let server = ServerConfig::default();
        assert_eq!(server.host, "0.0.0.0");
        assert_eq!(server.port, 8080);
        assert!(server.cors_origins.is_empty());
        assert_eq!(server.cache_max_age, 60);
        assert_eq!(server.cache_stale_while_revalidate, 300);
    }

    #[test]
    fn test_load_config_from_file() {
        let temp_dir = TempDir::new().unwrap();
        let config_path = temp_dir.path().join("riley_cms.toml");
        std::fs::write(
            &config_path,
            r#"
[content]
repo_path = "/test"

[storage]
bucket = "test-bucket"
public_url_base = "https://test.com"
"#,
        )
        .unwrap();

        let config = Config::from_path(&config_path).unwrap();
        assert_eq!(config.content.repo_path, PathBuf::from("/test"));
    }

    #[test]
    fn test_load_config_invalid_toml() {
        let temp_dir = TempDir::new().unwrap();
        let config_path = temp_dir.path().join("invalid.toml");
        std::fs::write(&config_path, "this is not valid toml {{{").unwrap();

        let result = Config::from_path(&config_path);
        assert!(result.is_err());
    }

    #[test]
    fn test_load_config_missing_required_field() {
        let temp_dir = TempDir::new().unwrap();
        let config_path = temp_dir.path().join("incomplete.toml");
        std::fs::write(
            &config_path,
            r#"
[content]
repo_path = "/test"
# Missing [storage] section
"#,
        )
        .unwrap();

        let result = Config::from_path(&config_path);
        assert!(result.is_err());
    }
}
