//! Middleware for riley-cms-api
//!
//! Authentication middleware for protected endpoints.

use axum::{
    extract::{Request, State},
    http::header,
    middleware::Next,
    response::Response,
};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use subtle::ConstantTimeEq;

use crate::AppState;

/// Authentication status for the current request.
///
/// This is inserted into request extensions by the auth middleware
/// and can be extracted by handlers to make authorization decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthStatus {
    /// Unauthenticated public request
    Public,
    /// Authenticated admin request (valid Bearer token provided)
    Admin,
}

/// Authentication middleware that validates Bearer tokens.
///
/// This middleware runs on every request and:
/// 1. Checks for an `Authorization: Bearer <token>` header
/// 2. Validates the token against the configured `auth.api_token`
/// 3. Sets `AuthStatus::Admin` if valid, `AuthStatus::Public` otherwise
/// 4. Inserts the status into request extensions for handlers to check
pub async fn auth_middleware(
    State(state): State<Arc<AppState>>,
    mut request: Request,
    next: Next,
) -> Response {
    let mut auth_status = AuthStatus::Public;

    // Check for configured API token
    if let Some(ref auth_config) = state.config.auth
        && let Some(ref token_config) = auth_config.api_token
    {
        // Resolve the token (supports "env:VAR_NAME" syntax)
        match token_config.resolve() {
            Ok(expected_token) => {
                // Check Authorization header for Bearer token
                if let Some(auth_header) = request.headers().get(header::AUTHORIZATION)
                    && let Ok(auth_str) = auth_header.to_str()
                    && let Some(provided_token) = auth_str.strip_prefix("Bearer ")
                {
                    // Hash both tokens before comparing to prevent
                    // leaking token length via timing side-channel.
                    // SHA-256 produces fixed 32-byte hashes regardless
                    // of input length.
                    let provided_hash = Sha256::digest(provided_token.trim().as_bytes());
                    let expected_hash = Sha256::digest(expected_token.as_bytes());
                    if provided_hash.ct_eq(&expected_hash).into() {
                        auth_status = AuthStatus::Admin;
                    }
                }
            }
            Err(e) => {
                tracing::warn!("Failed to resolve API token: {}. Admin auth disabled.", e);
            }
        }
    }

    // Insert status into extensions so handlers can read it
    request.extensions_mut().insert(auth_status);

    next.run(request).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_auth_status_equality() {
        assert_eq!(AuthStatus::Public, AuthStatus::Public);
        assert_eq!(AuthStatus::Admin, AuthStatus::Admin);
        assert_ne!(AuthStatus::Public, AuthStatus::Admin);
    }
}
