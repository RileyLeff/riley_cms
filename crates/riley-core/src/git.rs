//! Git Smart HTTP protocol support via git-http-backend CGI
//!
//! This module provides functionality to serve Git repositories over HTTP
//! using the Git Smart HTTP protocol. It works by invoking the system's
//! `git http-backend` CGI binary.

use crate::error::{Error, Result};
use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

/// Result of running a Git CGI operation
#[derive(Debug)]
pub struct GitCgiResponse {
    /// HTTP status code (parsed from CGI Status header, defaults to 200)
    pub status: u16,
    /// Response headers from the CGI script
    pub headers: HashMap<String, String>,
    /// Response body
    pub body: Vec<u8>,
}

/// Git HTTP backend wrapper
///
/// Handles Git Smart HTTP protocol by invoking `git http-backend` CGI.
pub struct GitBackend {
    /// Path to the Git repository
    repo_path: std::path::PathBuf,
    /// Optional explicit path to git-http-backend binary
    configured_backend_path: Option<std::path::PathBuf>,
}

impl GitBackend {
    /// Create a new Git backend for the given repository path
    pub fn new(repo_path: impl AsRef<Path>) -> Self {
        Self {
            repo_path: repo_path.as_ref().to_path_buf(),
            configured_backend_path: None,
        }
    }

    /// Create a new Git backend with an explicit backend binary path
    pub fn with_backend_path(
        repo_path: impl AsRef<Path>,
        backend_path: Option<std::path::PathBuf>,
    ) -> Self {
        Self {
            repo_path: repo_path.as_ref().to_path_buf(),
            configured_backend_path: backend_path,
        }
    }

    /// Run a Git CGI request
    ///
    /// This invokes `git http-backend` with the appropriate CGI environment
    /// variables and returns the response.
    ///
    /// # Arguments
    ///
    /// * `method` - HTTP method (GET, POST)
    /// * `path_info` - Path after the Git URL prefix (e.g., "/info/refs")
    /// * `query_string` - Query string (e.g., "service=git-upload-pack")
    /// * `content_type` - Content-Type header value (if any)
    /// * `body` - Request body (for POST requests)
    ///
    /// # Returns
    ///
    /// Returns the CGI response including status, headers, and body.
    pub async fn run_cgi(
        &self,
        method: &str,
        path_info: &str,
        query_string: Option<&str>,
        content_type: Option<&str>,
        body: &[u8],
    ) -> Result<GitCgiResponse> {
        // Build CGI environment variables
        let mut env = HashMap::new();
        env.insert("GIT_PROJECT_ROOT".to_string(), self.repo_path.to_string_lossy().to_string());
        env.insert("GIT_HTTP_EXPORT_ALL".to_string(), "1".to_string());
        env.insert("PATH_INFO".to_string(), path_info.to_string());
        env.insert("REQUEST_METHOD".to_string(), method.to_string());

        if let Some(qs) = query_string {
            env.insert("QUERY_STRING".to_string(), qs.to_string());
        }

        if let Some(ct) = content_type {
            env.insert("CONTENT_TYPE".to_string(), ct.to_string());
        }

        env.insert("CONTENT_LENGTH".to_string(), body.len().to_string());

        // Find git-http-backend (use configured path if available)
        let git_backend = match &self.configured_backend_path {
            Some(path) => path.to_string_lossy().to_string(),
            None => find_git_http_backend()?,
        };

        // Spawn the git-http-backend process
        let mut child = Command::new(&git_backend)
            .envs(&env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| Error::Git(format!("Failed to spawn git-http-backend: {}", e)))?;

        // Write request body to stdin
        if !body.is_empty() {
            if let Some(mut stdin) = child.stdin.take() {
                stdin.write_all(body).await?;
            }
        }

        // Read output
        let output = child
            .wait_with_output()
            .await
            .map_err(|e| Error::Git(format!("git-http-backend failed: {}", e)))?;

        if !output.stderr.is_empty() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            tracing::warn!("git-http-backend stderr: {}", stderr);
        }

        // Parse CGI response (headers + body)
        parse_cgi_response(&output.stdout)
    }

    /// Check if the repository exists and is a valid Git repository
    pub fn is_valid_repo(&self) -> bool {
        self.repo_path.join(".git").exists() || self.repo_path.join("HEAD").exists()
    }
}

/// Find the git-http-backend binary
fn find_git_http_backend() -> Result<String> {
    // Common locations for git-http-backend
    let candidates = [
        "/usr/lib/git-core/git-http-backend",
        "/usr/libexec/git-core/git-http-backend",
        "/opt/homebrew/libexec/git-core/git-http-backend",
        "/usr/local/libexec/git-core/git-http-backend",
    ];

    for path in &candidates {
        if std::path::Path::new(path).exists() {
            return Ok(path.to_string());
        }
    }

    // Try to find it via `git --exec-path`
    let output = std::process::Command::new("git")
        .arg("--exec-path")
        .output()
        .map_err(|e| Error::Git(format!("Failed to run git --exec-path: {}", e)))?;

    if output.status.success() {
        let exec_path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let backend_path = format!("{}/git-http-backend", exec_path);
        if std::path::Path::new(&backend_path).exists() {
            return Ok(backend_path);
        }
    }

    Err(Error::Git(
        "git-http-backend not found. Ensure Git is installed with HTTP support.".to_string(),
    ))
}

/// Parse CGI response into status, headers, and body
fn parse_cgi_response(data: &[u8]) -> Result<GitCgiResponse> {
    // CGI response format:
    // Header1: Value1\r\n
    // Header2: Value2\r\n
    // \r\n
    // body...

    let mut headers = HashMap::new();
    let mut status = 200u16;
    let mut body_start = 0;

    // Find the header/body separator (\r\n\r\n or \n\n)
    let mut i = 0;
    while i < data.len() {
        // Check for \r\n\r\n
        if i + 3 < data.len() && &data[i..i + 4] == b"\r\n\r\n" {
            body_start = i + 4;
            break;
        }
        // Check for \n\n (some CGI scripts use this)
        if i + 1 < data.len() && &data[i..i + 2] == b"\n\n" {
            body_start = i + 2;
            break;
        }
        i += 1;
    }

    // Parse headers
    if body_start > 0 {
        let header_bytes = &data[..body_start];
        let header_str = String::from_utf8_lossy(header_bytes);

        for line in header_str.lines() {
            if line.is_empty() {
                continue;
            }
            if let Some((key, value)) = line.split_once(':') {
                let key = key.trim().to_lowercase();
                let value = value.trim().to_string();

                // Parse Status header for HTTP status code
                if key == "status" {
                    if let Some(code_str) = value.split_whitespace().next() {
                        if let Ok(code) = code_str.parse::<u16>() {
                            status = code;
                        }
                    }
                } else {
                    headers.insert(key, value);
                }
            }
        }
    }

    let body = if body_start > 0 && body_start < data.len() {
        data[body_start..].to_vec()
    } else {
        Vec::new()
    };

    Ok(GitCgiResponse {
        status,
        headers,
        body,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_cgi_response_basic() {
        let data = b"Content-Type: application/x-git-upload-pack-advertisement\r\n\r\nHello";
        let response = parse_cgi_response(data).unwrap();

        assert_eq!(response.status, 200);
        assert_eq!(
            response.headers.get("content-type"),
            Some(&"application/x-git-upload-pack-advertisement".to_string())
        );
        assert_eq!(response.body, b"Hello");
    }

    #[test]
    fn test_parse_cgi_response_with_status() {
        let data = b"Status: 404 Not Found\r\nContent-Type: text/plain\r\n\r\nNot found";
        let response = parse_cgi_response(data).unwrap();

        assert_eq!(response.status, 404);
        assert_eq!(response.body, b"Not found");
    }

    #[test]
    fn test_parse_cgi_response_unix_newlines() {
        let data = b"Content-Type: text/plain\n\nBody here";
        let response = parse_cgi_response(data).unwrap();

        assert_eq!(response.status, 200);
        assert_eq!(response.body, b"Body here");
    }

    #[test]
    fn test_git_backend_is_valid_repo_bare() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let repo_path = temp_dir.path();

        // Not a repo yet
        let backend = GitBackend::new(repo_path);
        assert!(!backend.is_valid_repo());

        // Create a bare repo indicator
        std::fs::write(repo_path.join("HEAD"), "ref: refs/heads/main").unwrap();
        assert!(backend.is_valid_repo());
    }

    #[test]
    fn test_git_backend_is_valid_repo_normal() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let repo_path = temp_dir.path();

        // Create .git directory
        std::fs::create_dir(repo_path.join(".git")).unwrap();

        let backend = GitBackend::new(repo_path);
        assert!(backend.is_valid_repo());
    }
}
