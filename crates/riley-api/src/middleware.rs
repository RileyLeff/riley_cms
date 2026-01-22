//! Middleware for riley-api
//!
//! Authentication middleware for protected endpoints.

use axum::{
    extract::{Request, State},
    http::header,
    middleware::Next,
    response::Response,
};
use std::sync::Arc;

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
    if let Some(ref auth_config) = state.config.auth {
        if let Some(ref token_config) = auth_config.api_token {
            // Resolve the token (supports "env:VAR_NAME" syntax)
            if let Ok(expected_token) = token_config.resolve() {
                // Check Authorization header for Bearer token
                if let Some(auth_header) = request.headers().get(header::AUTHORIZATION) {
                    if let Ok(auth_str) = auth_header.to_str() {
                        if let Some(provided_token) = auth_str.strip_prefix("Bearer ") {
                            if provided_token.trim() == expected_token {
                                auth_status = AuthStatus::Admin;
                            }
                        }
                    }
                }
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
