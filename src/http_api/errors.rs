// This file contains modifications generated with assistance from Qwen AI
// Original project: https://github.com/snoack/viewpie
// SPDX-License-Identifier: GPL-3.0-or-later

//! Typed API errors mapped to proper HTTP status codes.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

/// All errors the REST API can produce.
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("{0}")]
    BadRequest(String),
    #[error("resource not found: {0}")]
    NotFound(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("camera is not part of the active panel")]
    CameraNotInActivePanel,
    #[error("camera has no fullscreen_url configured")]
    NoFullscreenUrl,
    #[error("cannot delete the last remaining panel")]
    LastPanel,
    #[error("invalid settings: {0}")]
    InvalidSettings(String),
    #[error("config reload failed: {0}")]
    ReloadFailed(String),
    #[error("display error: {0}")]
    Display(#[from] crate::kms::DisplayError),
    #[error("feed pool error: {0}")]
    Feed(#[from] crate::feed::PoolError),
    #[error("panel error: {0}")]
    Panel(#[from] crate::panel_manager::PanelError),
    #[error("internal error: {0}")]
    Internal(String),
}

impl ApiError {
    pub fn bad_request(msg: impl Into<String>) -> Self {
        Self::BadRequest(msg.into())
    }
    pub fn not_found(msg: impl Into<String>) -> Self {
        Self::NotFound(msg.into())
    }
    pub fn conflict(msg: impl Into<String>) -> Self {
        Self::Conflict(msg.into())
    }
}

impl From<crate::config::ConfigError> for ApiError {
    fn from(e: crate::config::ConfigError) -> Self {
        match e {
            crate::config::ConfigError::Validation(s) => Self::InvalidSettings(s),
            other => Self::ReloadFailed(other.to_string()),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, message) = match &self {
            ApiError::BadRequest(m) => (StatusCode::BAD_REQUEST, m.clone()),
            ApiError::NotFound(m) => (StatusCode::NOT_FOUND, m.clone()),
            ApiError::Conflict(m) => (StatusCode::CONFLICT, m.clone()),
            ApiError::CameraNotInActivePanel => {
                (StatusCode::UNPROCESSABLE_ENTITY, self.to_string())
            }
            ApiError::NoFullscreenUrl => (StatusCode::UNPROCESSABLE_ENTITY, self.to_string()),
            ApiError::LastPanel => (StatusCode::CONFLICT, self.to_string()),
            ApiError::InvalidSettings(m) => (StatusCode::BAD_REQUEST, m.clone()),
            ApiError::ReloadFailed(m) => (StatusCode::INTERNAL_SERVER_ERROR, m.clone()),
            ApiError::Feed(crate::feed::PoolError::NotFound(m)) => {
                (StatusCode::NOT_FOUND, format!("feed {m:?} not found"))
            }
            ApiError::Feed(crate::feed::PoolError::InUse(m)) => {
                (StatusCode::CONFLICT, format!("feed {m:?} in use"))
            }
            ApiError::Feed(crate::feed::PoolError::NoFullscreenUrl(m)) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("feed {m:?} has no fullscreen_url"),
            ),
            ApiError::Panel(crate::panel_manager::PanelError::NotFound(m)) => {
                (StatusCode::NOT_FOUND, format!("panel {m:?} not found"))
            }
            ApiError::Panel(crate::panel_manager::PanelError::AlreadyExists(m)) => {
                (StatusCode::CONFLICT, format!("panel {m:?} already exists"))
            }
            ApiError::Panel(crate::panel_manager::PanelError::LastPanel) => (
                StatusCode::CONFLICT,
                "cannot delete the last remaining panel".into(),
            ),
            ApiError::Panel(crate::panel_manager::PanelError::Validation(m)) => {
                (StatusCode::BAD_REQUEST, m.clone())
            }
            ApiError::Panel(crate::panel_manager::PanelError::Feed(fe)) => {
                return ApiError::Feed(clone_pool(fe)).into_response()
            }
            ApiError::Panel(crate::panel_manager::PanelError::Display(de)) => {
                (StatusCode::INTERNAL_SERVER_ERROR, de.to_string())
            }
            ApiError::Display(d) => (StatusCode::INTERNAL_SERVER_ERROR, d.to_string()),
            ApiError::Internal(m) => (StatusCode::INTERNAL_SERVER_ERROR, m.clone()),
        };
        let body = json!({ "ok": false, "error": message });
        (status, axum::Json(body)).into_response()
    }
}

fn clone_pool(e: &crate::feed::PoolError) -> crate::feed::PoolError {
    // PoolError implements Debug but not Clone; rebuild via string matching.
    let s = e.to_string();
    if s.contains("not found") {
        crate::feed::PoolError::NotFound(extract_quoted(&s))
    } else if s.contains("in use") {
        crate::feed::PoolError::InUse(extract_quoted(&s))
    } else {
        crate::feed::PoolError::NoFullscreenUrl(extract_quoted(&s))
    }
}

fn extract_quoted(s: &str) -> String {
    s.split('"').nth(1).unwrap_or_default().to_string()
}
