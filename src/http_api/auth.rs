// SPDX-License-Identifier: GPL-3.0-or-later
//! Shared Bearer-token authorization for the complete HTTP API.
use axum::{
    body::Body,
    http::{header::AUTHORIZATION, HeaderMap, Request},
    middleware::Next,
    response::{IntoResponse, Response},
};

use crate::http_api::errors::ApiError;

pub const TOKEN_ENV: &str = "VIEWPIE_RESTART_TOKEN";

pub fn token_configured() -> bool {
    std::env::var(TOKEN_ENV)
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false)
}

pub fn authorize(headers: &HeaderMap) -> Result<(), ApiError> {
    let expected = std::env::var(TOKEN_ENV).unwrap_or_default();
    if expected.trim().is_empty() {
        return Err(ApiError::Unavailable(
            "API token is not configured on the Viewpie service".into(),
        ));
    }
    let supplied = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .unwrap_or("");
    if !constant_time_eq(expected.trim().as_bytes(), supplied.as_bytes()) {
        return Err(ApiError::Unauthorized);
    }
    Ok(())
}

pub async fn require_api_token(request: Request<Body>, next: Next) -> Response {
    match authorize(request.headers()) {
        Ok(()) => next.run(request).await,
        Err(error) => error.into_response(),
    }
}

fn constant_time_eq(expected: &[u8], supplied: &[u8]) -> bool {
    let mut difference = expected.len() ^ supplied.len();
    for index in 0..expected.len().max(supplied.len()) {
        difference |=
            usize::from(*expected.get(index).unwrap_or(&0) ^ *supplied.get(index).unwrap_or(&0));
    }
    difference == 0
}

#[cfg(test)]
mod tests {
    use super::constant_time_eq;

    #[test]
    fn bearer_token_comparison_is_exact() {
        assert!(constant_time_eq(b"secret", b"secret"));
        assert!(!constant_time_eq(b"secret", b"secret "));
        assert!(!constant_time_eq(b"secret", b""));
        assert!(!constant_time_eq(b"secret", b"secreT"));
    }
}
