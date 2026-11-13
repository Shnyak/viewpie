// This file contains modifications generated with assistance from Qwen AI
// Original project: https://github.com/snoack/viewpie
// SPDX-License-Identifier: GPL-3.0-or-later

//! Control endpoints: pause / resume / grid / fullscreen / reload.

use std::time::Duration;
use tokio::sync::oneshot;

use axum::extract::Json;
use axum::extract::State;
use axum::http::StatusCode;
use tracing::{error, info, warn};

use crate::config::Config;
use crate::http_api::errors::ApiError;
use crate::http_api::models::{FullscreenRequest, OkResponse};
use crate::http_api::state::{AppState, WallCommand};

/// `POST /api/pause` — switch off the physical display without stopping decoders.
pub async fn pause(State(state): State<AppState>) -> Result<Json<OkResponse>, ApiError> {
    state
        .persist_screen_state(false)
        .await
        .map_err(|e| ApiError::Internal(format!("could not persist screen state: {e}")))?;
    crate::http_api::state::set_paused(true).await;
    let _ = state.command_tx.send(WallCommand::Pause).await;
    Ok(Json(OkResponse::ok()))
}

/// `POST /api/resume` — switch the physical display back on.
pub async fn resume(State(state): State<AppState>) -> Result<Json<OkResponse>, ApiError> {
    state
        .persist_screen_state(true)
        .await
        .map_err(|e| ApiError::Internal(format!("could not persist screen state: {e}")))?;
    crate::http_api::state::set_paused(false).await;
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
    if state.feeds_pool.get(&req.camera).await.is_none() {
        return Err(ApiError::not_found(format!(
            "camera {:?} not found",
            req.camera
        )));
    }

    // Preserve the previous selection so a failed physical KMS transition
    // cannot leave HA showing a fullscreen state that is not on the screen.
    let previous = state.manager.fullscreen_camera().await;
    let selection_started = state
        .manager
        .enter_fullscreen_with_timestamp(&req.camera)
        .await
        .map_err(ApiError::Panel)?;

    let (ack_tx, ack_rx) = oneshot::channel();
    if state
        .command_tx
        .send(WallCommand::SetFullscreen(
            req.camera.clone(),
            req.duration_seconds,
            ack_tx,
        ))
        .await
        .is_err()
    {
        restore_fullscreen_selection(&state, previous.clone()).await;
        return Err(ApiError::Unavailable(
            "renderer command channel is closed".into(),
        ));
    }

    let renderer_result = match tokio::time::timeout(Duration::from_secs(12), ack_rx).await {
        Ok(Ok(result)) => result,
        Ok(Err(_)) => Err("renderer acknowledgement channel closed".to_string()),
        Err(_) => Err("timed out waiting for the camera to appear on the display".to_string()),
    };
    if let Err(error) = renderer_result {
        restore_fullscreen_selection(&state, previous.clone()).await;
        return Err(ApiError::Unavailable(format!(
            "fullscreen transition was not confirmed by the native renderer: {error}"
        )));
    }

    // Start the timeout after the renderer accepts the selection. If the
    // first fullscreen frame is still reconnecting, the previous picture stays
    // visible and the renderer continues retrying without rolling the request
    // back; this timer still bounds how long fullscreen remains selected.
    let timeout = match req.duration_seconds {
        Some(seconds) => Some(seconds),
        None => Some(state.settings.read().await.fullscreen_timeout),
    };
    if let Some(seconds) = timeout {
        let st = state.clone();
        let cam = req.camera.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(seconds)).await;
            let current = st.wall_state.read().await;
            let is_current = fullscreen_selection_is_current(
                current.fullscreen_camera.as_deref(),
                current.fullscreen_started.clone(),
                &cam,
                selection_started,
            );
            drop(current);

            if is_current {
                info!(camera = %cam, secs = seconds, "fullscreen timeout — returning to grid");
                if let Err(error) = st.manager.leave_fullscreen().await {
                    error!("auto grid failed: {error}");
                } else {
                    let _ = st.command_tx.send(WallCommand::ReturnToGrid).await;
                }
            }
        });
    }

    Ok((StatusCode::OK, Json(OkResponse::ok())))
}

/// A timeout is valid only for the exact fullscreen selection that created it.
/// Comparing the camera alone is insufficient when the same camera is selected
/// again after visiting another one.
fn fullscreen_selection_is_current(
    current_camera: Option<&str>,
    current_started: Option<chrono::DateTime<chrono::Utc>>,
    requested_camera: &str,
    requested_started: Option<chrono::DateTime<chrono::Utc>>,
) -> bool {
    requested_started.is_some()
        && current_camera == Some(requested_camera)
        && current_started == requested_started
}

async fn restore_fullscreen_selection(state: &AppState, previous: Option<String>) {
    if let Some(camera) = previous {
        let _ = state.manager.enter_fullscreen(&camera).await;
        let (acknowledgement, _receiver) = oneshot::channel();
        let _ = state
            .command_tx
            .send(WallCommand::SetFullscreen(camera, None, acknowledgement))
            .await;
    } else {
        let _ = state.manager.leave_fullscreen().await;
        let _ = state.command_tx.send(WallCommand::ReturnToGrid).await;
    }
}

/// `POST /api/reload` — re-read the YAML config and apply without restart.
pub async fn reload(State(state): State<AppState>) -> Result<Json<OkResponse>, ApiError> {
    let _config_guard = state.config_update_lock.lock().await;
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
    state.publish_config(cfg).await;
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

#[cfg(test)]
mod fullscreen_timeout_tests {
    use super::*;
    use chrono::Utc;

    #[test]
    fn old_timeout_does_not_match_a_later_selection_of_the_same_camera() {
        let first_selection = Utc::now();
        let second_selection = first_selection + chrono::Duration::milliseconds(1);

        assert!(fullscreen_selection_is_current(
            Some("camera_a"),
            Some(first_selection.clone()),
            "camera_a",
            Some(first_selection),
        ));
        assert!(!fullscreen_selection_is_current(
            Some("camera_a"),
            Some(second_selection.clone()),
            "camera_a",
            Some(first_selection),
        ));
    }

    #[test]
    fn timeout_does_not_match_a_different_camera_or_grid() {
        let selected_at = Utc::now();

        assert!(!fullscreen_selection_is_current(
            Some("camera_b"),
            Some(selected_at.clone()),
            "camera_a",
            Some(selected_at),
        ));
        assert!(!fullscreen_selection_is_current(
            None,
            None,
            "camera_a",
            Some(selected_at),
        ));
    }
}
