// This file contains modifications generated with assistance from Qwen AI
// Original project: https://github.com/snoack/viewpie
// SPDX-License-Identifier: GPL-3.0-or-later

//! Control endpoints: pause / resume / grid / fullscreen / reload.

use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::extract::Json;
use tracing::{error, info, warn};

use crate::config::Config;
use crate::http_api::errors::ApiError;
use crate::http_api::models::{FullscreenRequest, OkResponse};
use crate::http_api::state::{AppState, WallCommand};

/// `POST /api/pause` — stop all decoders, black screen, keep the pool.
pub async fn pause(State(state): State<AppState>) -> Result<Json<OkResponse>, ApiError> {
    state.manager.pause().await?;
    let _ = state.command_tx.send(WallCommand::Pause).await;
    Ok(Json(OkResponse::ok()))
}

/// `POST /api/resume` — restart active panel decoders, restore mode.
pub async fn resume(State(state): State<AppState>) -> Result<Json<OkResponse>, ApiError> {
    state.manager.resume().await?;
    let _ = state.command_tx.send(WallCommand::Resume).await;
    Ok(Json(OkResponse::ok()))
}

/// `POST /api/grid` — leave fullscreen back to the grid view.
pub async fn grid(State(state): State<AppState>) -> Result<Json<OkResponse>, ApiError> {
    if crate::http_api::state::is_paused_now().await {
        return Err(ApiError::bad_request(
            "wall is paused — call /api/resume first",
        ));
    }
    state.manager.leave_fullscreen().await?;
    let _ = state.command_tx.send(WallCommand::ReturnToGrid).await;
    Ok(Json(OkResponse::ok()))
}

/// `POST /api/fullscreen` — show one camera full-screen (sub → main stream).
///
/// With `duration_seconds` a tokio task returns to grid after that delay
/// (±1 s accuracy; `tokio::time::sleep` is monotonic and precise).
pub async fn fullscreen(
    State(state): State<AppState>,
    Json(req): Json<FullscreenRequest>,
) -> Result<(StatusCode, Json<OkResponse>), ApiError> {
    if crate::http_api::state::is_paused_now().await {
        return Err(ApiError::bad_request(
            "wall is paused — call /api/resume first",
        ));
    }
    // Camera must exist in the pool.
    if state.feeds_pool.get(&req.camera).await.is_none() {
        return Err(ApiError::not_found(format!(
            "camera {:?} not found",
            req.camera
        )));
    }
    state
        .manager
        .enter_fullscreen(&req.camera)
        .await
        .map_err(|e| match e {
            crate::panel_manager::PanelError::Validation(_) => ApiError::CameraNotInActivePanel,
            crate::panel_manager::PanelError::Feed(crate::feed::PoolError::NoFullscreenUrl(_)) => {
                ApiError::NoFullscreenUrl
            }
            other => ApiError::Panel(other),
        })?;

    // Determine timeout: explicit duration or global fullscreen_timeout.
    let timeout = match req.duration_seconds {
        Some(d) => Some(d),
        None => {
            let t = state.settings.read().await.fullscreen_timeout;
            Some(t)
        }
    };
    if let Some(seconds) = timeout {
        let st = state.clone();
        let cam = req.camera.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(seconds)).await;
            // Only auto-return if we are still showing the same camera
            // (a newer manual action supersedes this timer).
            if crate::http_api::state::current_fullscreen()
                .await
                .as_deref()
                == Some(cam.as_str())
            {
                info!(camera = %cam, secs = seconds, "fullscreen timeout — returning to grid");
                if let Err(e) = st.manager.leave_fullscreen().await {
                    error!("auto grid failed: {e}");
                }
            }
        });
    }
    let _ = state
        .command_tx
        .send(WallCommand::SetFullscreen(req.camera, req.duration_seconds))
        .await;
    Ok((StatusCode::OK, Json(OkResponse::ok())))
}

/// `POST /api/reload` — re-read the YAML config and apply without restart.
pub async fn reload(State(state): State<AppState>) -> Result<Json<OkResponse>, ApiError> {
    let path = state.config_path.as_str().to_string();
    let cfg = load_config_async(&path)
        .await
        .map_err(|e| ApiError::ReloadFailed(e.to_string()))?;

    // 1. Settings.
    {
        let mut s = state.settings.write().await;
        *s = cfg.display.clone();
    }
    // 2. Cameras: add new, update changed, drop unreferenced old ones.
    let existing = state.feeds_pool.list().await;
    for f in &existing {
        match cfg.feeds.get(&f.name) {
            Some(newcfg) if *newcfg != f.config => {
                state
                    .feeds_pool
                    .update_feed(&f.name, newcfg.clone())
                    .await?;
            }
            None => {
                // Feed removed from config — release all refs then delete.
                while state.feeds_pool.ref_count(&f.name).await.unwrap_or(0) > 0 {
                    state.feeds_pool.release(&f.name).await?;
                }
                state.feeds_pool.remove_feed(&f.name).await?;
            }
            _ => {}
        }
    }
    for (name, fc) in &cfg.feeds {
        if state.feeds_pool.get(name).await.is_none() {
            state.feeds_pool.add_feed(name.clone(), fc.clone()).await;
        }
    }
    // 3. Panels + active panel.
    {
        *state.panels.write().await = cfg.panels.clone();
        *state.active_panel.write().await = state.manager.active_id().await;
    }
    state
        .manager
        .reload_from(cfg.panels.clone(), Some(cfg.initial_panel()))
        .await?;
    info!(path, "configuration reloaded");
    let _ = state.command_tx.send(WallCommand::ReloadConfig).await;
    Ok(Json(OkResponse::ok()))
}

async fn load_config_async(path: &str) -> Result<Config, crate::config::ConfigError> {
    let p = path.to_string();
    // `p` is moved into the blocking closure; on a JoinError (the task
    // panicked) reconstruct the path from the caller's argument instead.
    tokio::task::spawn_blocking(move || Config::load(&p))
        .await
        .map_err(|e| {
            warn!("reload join error: {e}");
            crate::config::ConfigError::Io {
                path: path.to_string(),
                source: std::io::Error::other(e.to_string()),
            }
        })?
}
