//! Git Smart HTTP protocol support via git-http-backend CGI
//!
//! This module provides functionality to serve Git repositories over HTTP
//! using the Git Smart HTTP protocol. It works by invoking the system's
//! `git http-backend` CGI binary.
//!
//! The implementation streams request bodies to the CGI process and streams
//! CGI output back to the client, avoiding buffering large payloads in memory.

use crate::error::{Error, Result};
use bytes::Bytes;
use futures_util::StreamExt;
use std::collections::HashMap;
use std::io;
use std::path::Path;
use std::pin::Pin;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::task::JoinHandle;
use tokio::time::{Duration, timeout};

/// Default maximum time to wait for a git-http-backend process to complete.
pub const DEFAULT_GIT_CGI_TIMEOUT: Duration = Duration::from_secs(300); // 5 minutes
use tokio_util::io::ReaderStream;

/// Maximum size of CGI headers before we give up (16 KB)
const MAX_CGI_HEADER_SIZE: usize = 16 * 1024;

/// Stream of response body bytes from a CGI process.
pub type BodyStream =
    Pin<Box<dyn futures_util::Stream<Item = std::result::Result<Bytes, io::Error>> + Send>>;

/// Parsed CGI headers (status code + response headers, no body).
#[derive(Debug)]
pub struct GitCgiHeaders {
    /// HTTP status code (parsed from CGI Status header, defaults to 200)
    pub status: u16,
    /// Response headers from the CGI script
    pub headers: HashMap<String, String>,
}

/// A streaming CGI response.
///
/// The headers have already been parsed from stdout. The `body_stream` yields
/// the remaining stdout bytes. Call `completion.wait()` after the stream is
/// consumed to reap the child process and check its exit status.
pub struct GitCgiStreamResponse {
    /// Parsed CGI headers
    pub headers: GitCgiHeaders,
    /// Stream of body bytes from stdout (after the header separator)
    pub body_stream: BodyStream,
    /// Handle to await process completion (for webhook timing)
    pub completion: GitCgiCompletion,
}

/// Handle to monitor CGI process completion and collect stderr.
///
/// The caller should await `wait()` after the body stream has been consumed
/// (or dropped) to ensure the process is reaped and stderr is logged.
pub struct GitCgiCompletion {
    child: Child,
    stderr_task: JoinHandle<String>,
    stdin_task: Option<JoinHandle<std::result::Result<(), Error>>>,
}

impl GitCgiCompletion {
    /// Wait for the process to exit. Returns the exit status.
    ///
    /// Also joins the stdin streaming task and logs stderr output.
    /// The `cgi_timeout` parameter controls how long to wait before killing the process.
    pub async fn wait(mut self, cgi_timeout: Duration) -> Result<std::process::ExitStatus> {
        // Wait for stdin to finish (if still running)
        if let Some(stdin_task) = self.stdin_task.take() {
            match stdin_task.await {
                Ok(Err(e)) => {
                    tracing::warn!("stdin streaming error (non-fatal): {}", e);
                }
                Err(e) => tracing::warn!("stdin task panicked: {}", e),
                Ok(Ok(())) => {}
            }
        }

        let status = match timeout(cgi_timeout, self.child.wait()).await {
            Ok(result) => result
                .map_err(|e| Error::Git(format!("Failed to wait on git-http-backend: {}", e)))?,
            Err(_elapsed) => {
                tracing::error!(
                    "Git CGI process timed out after {}s, killing",
                    cgi_timeout.as_secs()
                );
                let _ = self.child.kill().await;
                return Err(Error::Git("Git operation timed out".to_string()));
            }
        };

        let stderr = self
            .stderr_task
            .await
            .unwrap_or_else(|_| String::from("<stderr task panicked>"));

        if !stderr.is_empty() {
            tracing::warn!("git-http-backend stderr: {}", stderr);
        }

        Ok(status)
    }
}

/// Result of running a Git CGI operation (buffered variant, used in tests only)
#[cfg(test)]
#[derive(Debug)]
pub(crate) struct GitCgiResponse {
    pub status: u16,
    pub headers: HashMap<String, String>,
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

    /// Run a Git CGI request with streaming I/O.
    ///
    /// The request body is provided as an async stream of byte chunks.
    /// Returns parsed CGI headers plus a stream of the response body.
    ///
    /// # Arguments
    ///
    /// * `method` - HTTP method (GET, POST)
    /// * `path_info` - Path after the Git URL prefix (e.g., "/info/refs")
    /// * `query_string` - Query string (e.g., "service=git-upload-pack")
    /// * `content_type` - Content-Type header value (if any)
    /// * `content_length` - Content-Length from the request (if known)
    /// * `body_stream` - Stream of request body chunks
    /// * `max_body_size` - Maximum allowed body size in bytes
    #[allow(clippy::too_many_arguments)]
    pub async fn run_cgi(
        &self,
        method: &str,
        path_info: &str,
        query_string: Option<&str>,
        content_type: Option<&str>,
        content_length: Option<u64>,
        body_stream: impl futures_util::Stream<Item = std::result::Result<Bytes, io::Error>>
        + Send
        + Unpin
        + 'static,
        max_body_size: u64,
    ) -> Result<GitCgiStreamResponse> {
        // Build CGI environment variables
        let mut env = HashMap::new();
        env.insert(
            "GIT_PROJECT_ROOT".to_string(),
            self.repo_path.to_string_lossy().to_string(),
        );
        env.insert("GIT_HTTP_EXPORT_ALL".to_string(), "1".to_string());
        env.insert("PATH_INFO".to_string(), path_info.to_string());
        env.insert("REQUEST_METHOD".to_string(), method.to_string());

        if let Some(qs) = query_string {
            env.insert("QUERY_STRING".to_string(), qs.to_string());
        }

        if let Some(ct) = content_type {
            env.insert("CONTENT_TYPE".to_string(), ct.to_string());
        }

        if let Some(cl) = content_length {
            env.insert("CONTENT_LENGTH".to_string(), cl.to_string());
        }

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

        // === STDIN STREAMING ===
        // Spawn a task to stream the request body to stdin with size enforcement.
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| Error::Git("Failed to open stdin pipe".to_string()))?;

        let stdin_task: JoinHandle<std::result::Result<(), Error>> = tokio::spawn(async move {
            let mut body_stream = std::pin::pin!(body_stream);
            let mut total_bytes: u64 = 0;

            while let Some(chunk_result) = body_stream.next().await {
                let chunk =
                    chunk_result.map_err(|e| Error::Git(format!("Body stream error: {}", e)))?;
                total_bytes += chunk.len() as u64;
                if total_bytes > max_body_size {
                    return Err(Error::Git(format!(
                        "Request body too large ({} bytes exceeds max {} bytes)",
                        total_bytes, max_body_size
                    )));
                }
                if let Err(e) = stdin.write_all(&chunk).await {
                    // Broken pipe is expected if the child doesn't need all input
                    // (e.g., GET requests with empty body, or child errored early)
                    if e.kind() == io::ErrorKind::BrokenPipe {
                        break;
                    }
                    return Err(Error::Git(format!(
                        "Failed to write to git-http-backend stdin: {}",
                        e
                    )));
                }
            }
            // Close stdin to signal EOF to the child
            let _ = stdin.shutdown().await;
            Ok(())
        });

        // === STDERR COLLECTION ===
        // Spawn a task to buffer stderr (capped at 64KB, typically small).
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| Error::Git("Failed to open stderr pipe".to_string()))?;

        let stderr_task: JoinHandle<String> = tokio::spawn(async move {
            let mut buf = String::new();
            let mut limited = BufReader::new(stderr).take(64 * 1024);
            let _ = limited.read_to_string(&mut buf).await;
            buf
        });

        // === STDOUT HEADER PARSING ===
        // Read from stdout until we find the header/body separator.
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| Error::Git("Failed to open stdout pipe".to_string()))?;

        let mut stdout_reader = BufReader::new(stdout);
        let headers = read_cgi_headers(&mut stdout_reader).await?;

        // Check if stdin_task already finished with an error (e.g., body too large).
        // If so, abort before streaming begins so we can return a proper error response.
        if stdin_task.is_finished() {
            match stdin_task.await {
                Ok(Err(e)) => return Err(e),
                Err(join_err) => {
                    return Err(Error::Git(format!("stdin task panicked: {}", join_err)));
                }
                Ok(Ok(())) => {
                    // stdin completed successfully, continue with no stdin_task to track
                    let body_stream: BodyStream = Box::pin(ReaderStream::new(stdout_reader));

                    return Ok(GitCgiStreamResponse {
                        headers,
                        body_stream,
                        completion: GitCgiCompletion {
                            child,
                            stderr_task,
                            stdin_task: None,
                        },
                    });
                }
            }
        }

        // === STDOUT BODY STREAMING ===
        // The remaining bytes in stdout_reader become the body stream.
        let body_stream: BodyStream = Box::pin(ReaderStream::new(stdout_reader));

        Ok(GitCgiStreamResponse {
            headers,
            body_stream,
            completion: GitCgiCompletion {
                child,
                stderr_task,
                stdin_task: Some(stdin_task),
            },
        })
    }

    /// Check if the repository exists and is a valid Git repository
    pub fn is_valid_repo(&self) -> bool {
        self.repo_path.join(".git").exists() || self.repo_path.join("HEAD").exists()
    }
}

/// Read CGI headers from a buffered reader.
///
/// Reads line by line until an empty line (the header/body separator) is found.
/// The reader is left positioned at the start of the body.
/// Fails if headers exceed `MAX_CGI_HEADER_SIZE` bytes.
async fn read_cgi_headers<R: tokio::io::AsyncBufRead + Unpin>(
    reader: &mut R,
) -> Result<GitCgiHeaders> {
    let mut headers = HashMap::new();
    let mut status: u16 = 200;
    let mut total_header_bytes: usize = 0;
    let mut line_buf = String::new();

    loop {
        line_buf.clear();
        let bytes_read = reader
            .read_line(&mut line_buf)
            .await
            .map_err(|e| Error::Git(format!("Failed to read CGI headers: {}", e)))?;

        if bytes_read == 0 {
            // EOF before finding separator — treat as headers-only response
            break;
        }

        total_header_bytes += bytes_read;
        if total_header_bytes > MAX_CGI_HEADER_SIZE {
            return Err(Error::Git(format!(
                "CGI headers too large (>{} bytes). Possible malformed response.",
                MAX_CGI_HEADER_SIZE
            )));
        }

        // Check for the empty line separator (after trimming \r\n or \n)
        let trimmed = line_buf.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break; // Found the header/body separator
        }

        // Parse "Key: Value" header line
        if let Some((key, value)) = trimmed.split_once(':') {
            let key = key.trim().to_lowercase();
            let value = value.trim().to_string();

            if key == "status"
                && let Some(code_str) = value.split_whitespace().next()
                && let Ok(code) = code_str.parse::<u16>()
            {
                status = code;
            } else {
                headers.insert(key, value);
            }
        }
    }

    Ok(GitCgiHeaders { status, headers })
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

/// Parse CGI response into status, headers, and body (buffered variant for tests)
#[cfg(test)]
fn parse_cgi_response(data: &[u8]) -> Result<GitCgiResponse> {
    let mut headers = HashMap::new();
    let mut status = 200u16;
    let mut body_start = 0;

    // Find the header/body separator (\r\n\r\n or \n\n)
    let mut i = 0;
    while i < data.len() {
        if i + 3 < data.len() && &data[i..i + 4] == b"\r\n\r\n" {
            body_start = i + 4;
            break;
        }
        if i + 1 < data.len() && &data[i..i + 2] == b"\n\n" {
            body_start = i + 2;
            break;
        }
        i += 1;
    }

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

                if key == "status"
                    && let Some(code_str) = value.split_whitespace().next()
                    && let Ok(code) = code_str.parse::<u16>()
                {
                    status = code;
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

    #[tokio::test]
    async fn test_read_cgi_headers_basic() {
        let data = b"Content-Type: application/x-git-upload-pack-advertisement\r\n\r\n";
        let mut reader = tokio::io::BufReader::new(std::io::Cursor::new(data.to_vec()));
        let headers = read_cgi_headers(&mut reader).await.unwrap();

        assert_eq!(headers.status, 200);
        assert_eq!(
            headers.headers.get("content-type"),
            Some(&"application/x-git-upload-pack-advertisement".to_string())
        );
    }

    #[tokio::test]
    async fn test_read_cgi_headers_with_status() {
        let data = b"Status: 403 Forbidden\r\nContent-Type: text/plain\r\n\r\n";
        let mut reader = tokio::io::BufReader::new(std::io::Cursor::new(data.to_vec()));
        let headers = read_cgi_headers(&mut reader).await.unwrap();

        assert_eq!(headers.status, 403);
        assert_eq!(
            headers.headers.get("content-type"),
            Some(&"text/plain".to_string())
        );
    }

    #[tokio::test]
    async fn test_read_cgi_headers_unix_newlines() {
        let data = b"Content-Type: text/plain\n\nBody here";
        let mut reader = tokio::io::BufReader::new(std::io::Cursor::new(data.to_vec()));
        let headers = read_cgi_headers(&mut reader).await.unwrap();

        assert_eq!(headers.status, 200);
        assert_eq!(
            headers.headers.get("content-type"),
            Some(&"text/plain".to_string())
        );
    }
}
