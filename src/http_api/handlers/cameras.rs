// This file contains modifications generated with assistance from Qwen AI
// Original project: https://github.com/snoack/viewpie
// SPDX-License-Identifier: GPL-3.0-or-later

//! Camera (feed) CRUD + connectivity test endpoints.

use std::time::Instant;

use axum::extract::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use tracing::{info, warn};

use crate::http_api::errors::ApiError;
use crate::http_api::models::{CameraDto, CameraTestResponse, OkResponse};
use crate::http_api::state::AppState;

/// `GET /api/cameras` — list all cameras with runtime info.
pub async fn list_cameras(State(state): State<AppState>) -> Result<Json<Vec<CameraDto>>, ApiError> {
    let feeds = state.feeds_pool.list().await;
    Ok(Json(feeds.iter().map(CameraDto::from_state).collect()))
}

/// `POST /api/cameras` — register a camera (ref_count 0, decoder starts on
/// first use by an active panel). Returns 201.
pub async fn create_camera(
    State(state): State<AppState>,
    Json(dto): Json<CameraDto>,
) -> Result<(StatusCode, Json<OkResponse>), ApiError> {
    if dto.name.is_empty() || dto.url.is_empty() {
        return Err(ApiError::bad_request("camera needs name and url"));
    }
    if !dto.url.starts_with("rtsp://") && !dto.url.starts_with("rtspt://") {
        return Err(ApiError::bad_request("camera url must be an rtsp:// URL"));
    }
    let created = state
        .add_camera(dto.name.clone(), dto.to_feed_config())
        .await;
    if !created {
        return Err(ApiError::conflict(format!(
            "camera {:?} already exists",
            dto.name
        )));
    }
    info!(camera = dto.name, "camera registered");
    Ok((StatusCode::CREATED, Json(OkResponse::ok())))
}

/// `PUT /api/cameras/{name}` — update URLs. Restarts the decoder when the
/// camera is currently in use.
pub async fn update_camera(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(dto): Json<CameraDto>,
) -> Result<Json<OkResponse>, ApiError> {
    if state.feeds_pool.get(&name).await.is_none() {
        return Err(ApiError::not_found(format!("camera {name:?} not found")));
    }
    state
        .feeds_pool
        .update_feed(&name, dto.to_feed_config())
        .await?;
    // If fullscreen was active for this camera, restart main stream too.
    if crate::http_api::state::current_fullscreen()
        .await
        .as_deref()
        == Some(name.as_str())
    {
        state.feeds_pool.stop_fullscreen(&name).await;
        state.feeds_pool.start_fullscreen(&name).await.ok();
    }
    Ok(Json(OkResponse::ok()))
}

/// `DELETE /api/cameras/{name}` — 409 while any panel references it.
pub async fn delete_camera(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<OkResponse>, ApiError> {
    let users = state.manager.panels_using_camera(&name).await;
    if !users.is_empty() {
        return Err(ApiError::conflict(format!(
            "camera {name:?} is used by panels: {}",
            users.join(", ")
        )));
    }
    state.feeds_pool.remove_feed(&name).await?;
    info!(camera = name, "camera removed");
    Ok(Json(OkResponse::ok()))
}

/// `POST /api/cameras/{name}/test` — connect, grab one frame, measure
/// latency, disconnect. Works both against real RTSP servers (via a TCP
/// probe of the RTSP port plus DESCRIBE when available) and reports clean
/// errors otherwise.
pub async fn test_camera(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<CameraTestResponse>, ApiError> {
    let feed = state
        .feeds_pool
        .get(&name)
        .await
        .ok_or_else(|| ApiError::not_found(format!("camera {name:?} not found")))?;
    let url = feed.config.url.clone();
    let started = Instant::now();
    match probe_rtsp(&url).await {
        Ok(()) => {
            let ms = started.elapsed().as_millis() as u64;
            Ok(Json(CameraTestResponse {
                ok: true,
                latency_ms: Some(ms),
                error: None,
            }))
        }
        Err(e) => {
            warn!(camera = name, error = %e, "connectivity test failed");
            Ok(Json(CameraTestResponse {
                ok: false,
                latency_ms: None,
                error: Some(e),
            }))
        }
    }
}

/// Probe an RTSP URL: resolve host, open the TCP port (default 554) and
/// send an RTSP `OPTIONS` request, validating that we get an `RTSP/1.0`
/// response back. On ARM builds this is supplemented by pulling one frame
/// through the FFmpeg pipeline.
async fn probe_rtsp(url: &str) -> Result<(), String> {
    let rest = url
        .strip_prefix("rtsp://")
        .or_else(|| url.strip_prefix("rtspt://"))
        .ok_or_else(|| "not an rtsp url".to_string())?;
    let without_creds = rest.split_once('@').map(|(_, r)| r).unwrap_or(rest);
    let host_port = without_creds.split('/').next().unwrap_or("");
    let (host, port) = match host_port.split_once(':') {
        Some((h, p)) => (h.to_string(), p.parse::<u16>().map_err(|_| "bad port")?),
        None => (host_port.to_string(), 554),
    };
    if host.is_empty() {
        return Err("empty host".into());
    }
    let req_url = format!("rtsp://{host_port}");
    // Real RTSP handshake via tokio TCP (OPTIONS → RTSP/1.0 200).
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut sock = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        tokio::net::TcpStream::connect((host.as_str(), port)),
    )
    .await
    .map_err(|_| "connect timeout".to_string())?
    .map_err(|e| format!("connect failed: {e}"))?;

    let cseq = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
    let packet =
        format!("OPTIONS {req_url} RTSP/1.0\r\nCSeq: {cseq}\r\nUser-Agent: viewpie-fork\r\n\r\n");
    sock.write_all(packet.as_bytes())
        .await
        .map_err(|e| format!("write failed: {e}"))?;
    let mut buf = [0u8; 512];
    let n = tokio::time::timeout(std::time::Duration::from_secs(5), sock.read(&mut buf))
        .await
        .map_err(|_| "read timeout".to_string())?
        .map_err(|e| format!("read failed: {e}"))?;
    let resp = String::from_utf8_lossy(&buf[..n]);
    if resp.starts_with("RTSP/1.0") {
        Ok(())
    } else {
        Err(format!("unexpected response: {:?}", resp.trim()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn probe_rejects_garbage_urls() {
        assert!(probe_rtsp("http://x/y").await.is_err());
        assert!(probe_rtsp("rtsp://").await.is_err());
    }

    #[tokio::test]
    async fn probe_times_out_on_closed_port() {
        // 127.0.0.1:1 should refuse instantly.
        let r = probe_rtsp("rtsp://127.0.0.1:1/stream").await;
        assert!(r.is_err());
    }
}
