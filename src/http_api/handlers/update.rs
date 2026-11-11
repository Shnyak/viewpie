// SPDX-License-Identifier: GPL-3.0-or-later
//! GitHub release discovery and narrowly-scoped package installation.
use axum::http::{header::AUTHORIZATION, HeaderMap};
use axum::Json;
use serde_json::{json, Value};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
    time::{timeout, Duration},
};

use crate::http_api::errors::ApiError;

const RESTART_TOKEN_ENV: &str = "VIEWPIE_RESTART_TOKEN";
const RELEASE_API: &str = "https://api.github.com/repos/Shnyak/viewpie/releases/latest";
const UPDATE_SOCKET: &str = "/run/viewpie-update.sock";

fn package_architecture() -> &'static str {
    #[cfg(target_arch = "arm")]
    {
        "armhf"
    }
    #[cfg(target_arch = "aarch64")]
    {
        "arm64"
    }
    #[cfg(not(any(target_arch = "arm", target_arch = "aarch64")))]
    {
        "unsupported"
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

fn authorize(headers: &HeaderMap) -> Result<(), ApiError> {
    let expected = std::env::var(RESTART_TOKEN_ENV).unwrap_or_default();
    if expected.trim().is_empty() {
        return Err(ApiError::Unavailable(
            "update token is not configured on the Viewpie service".into(),
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

/// Return the latest stable release and whether this architecture has a
/// package whose checksum is published by GitHub.
pub async fn check() -> Result<Json<Value>, ApiError> {
    let response = reqwest::Client::builder()
        .user_agent("Viewpie-Home-Assistant-Integration")
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|err| ApiError::Unavailable(err.to_string()))?
        .get(RELEASE_API)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|err| ApiError::Unavailable(format!("GitHub release check failed: {err}")))?;

    if !response.status().is_success() {
        return Err(ApiError::Unavailable(format!(
            "GitHub release check returned HTTP {}",
            response.status()
        )));
    }

    let release: Value = response
        .json()
        .await
        .map_err(|err| ApiError::Unavailable(format!("Invalid GitHub release response: {err}")))?;
    let tag = release
        .get("tag_name")
        .and_then(Value::as_str)
        .unwrap_or("");
    let latest = tag.strip_prefix('v').unwrap_or(tag);
    if latest.is_empty() {
        return Err(ApiError::Unavailable(
            "GitHub latest release has no version tag".into(),
        ));
    }

    let arch = package_architecture();
    let prefix = format!("viewpie_{latest}-");
    let suffix = format!("_{arch}.deb");
    let asset = release
        .get("assets")
        .and_then(Value::as_array)
        .and_then(|assets| {
            assets.iter().find(|asset| {
                let name = asset.get("name").and_then(Value::as_str).unwrap_or("");
                name.starts_with(&prefix) && name.ends_with(&suffix)
            })
        });
    let digest_available = asset
        .and_then(|item| item.get("digest"))
        .and_then(Value::as_str)
        .is_some_and(|digest| {
            digest.strip_prefix("sha256:").is_some_and(|hex| {
                hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
        });

    let installed = env!("CARGO_PKG_VERSION");
    Ok(Json(json!({
        "installed_version": installed,
        "latest_version": latest,
        "update_available": latest != installed,
        "architecture": arch,
        "package_available": asset.is_some(),
        "checksum_available": digest_available,
        "can_install": asset.is_some() && digest_available && latest != installed,
        "release_url": release.get("html_url").and_then(Value::as_str).unwrap_or(""),
        "release_notes": release.get("body").and_then(Value::as_str).unwrap_or(""),
    })))
}

/// Ask the root-owned socket-activated helper to install the latest verified
/// package. The helper independently resolves the release and validates its
/// SHA-256 digest; the HTTP client cannot supply an arbitrary URL or package.
pub async fn install(headers: HeaderMap) -> Result<Json<Value>, ApiError> {
    authorize(&headers)?;
    let mut socket = timeout(
        Duration::from_secs(3),
        UnixStream::connect(UPDATE_SOCKET),
    )
    .await
    .map_err(|_| ApiError::Unavailable("update helper connection timed out".into()))?
    .map_err(|err| {
        ApiError::Unavailable(format!(
            "update helper is not installed or active: {err}"
        ))
    })?;

    timeout(Duration::from_secs(2), socket.write_all(b"INSTALL\n"))
        .await
        .map_err(|_| ApiError::Unavailable("could not send update request".into()))?
        .map_err(|err| {
            ApiError::Unavailable(format!("could not send update request: {err}"))
        })?;

    let mut acknowledgement = [0_u8; 256];
    let received = timeout(Duration::from_secs(3), socket.read(&mut acknowledgement))
        .await
        .map_err(|_| {
            ApiError::Unavailable("update helper did not acknowledge the request".into())
        })?
        .map_err(|err| {
            ApiError::Unavailable(format!("update helper acknowledgement failed: {err}"))
        })?;
    if received == 0
        || !String::from_utf8_lossy(&acknowledgement[..received]).contains("started")
    {
        return Err(ApiError::Unavailable(
            "update helper rejected the request".into(),
        ));
    }

    Ok(Json(json!({
        "ok": true,
        "status": "started",
        "message": "Verified Viewpie update has been started"
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_comparison_is_exact() {
        assert!(constant_time_eq(b"secret", b"secret"));
        assert!(!constant_time_eq(b"secret", b"secret "));
        assert!(!constant_time_eq(b"secret", b""));
    }

    #[test]
    fn release_asset_digest_must_be_sha256() {
        let valid =
            "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        assert!(valid.strip_prefix("sha256:").is_some_and(|hex| {
            hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit())
        }));
        assert!(!"sha1:abc".starts_with("sha256:"));
    }
}
