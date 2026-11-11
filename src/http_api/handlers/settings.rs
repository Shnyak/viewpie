// This file contains modifications generated with assistance from Qwen AI
// Original project: https://github.com/snoack/viewpie
// SPDX-License-Identifier: GPL-3.0-or-later

//! Dynamic display settings endpoints.
//!
//! `GET /api/settings` returns the current [`DisplaySettings`];
//! `PUT /api/settings` accepts a *partial* patch — only the provided
//! fields are changed, everything else is kept. Effects:
//!
//! * `spacing`, `background` → immediate re-composite (grid redraw without
//!   any decoder reconnect),
//! * `pause_off_screen` → picked up by the screen monitor task on its next
//!   poll (≤ 5 s),
//! * `rotation_interval`, `fullscreen_timeout` → applied to future timers.

use axum::extract::Json;
use axum::extract::State;
use tracing::info;

use crate::http_api::errors::ApiError;
use crate::http_api::models::{OkResponse, SettingsPatch};
use crate::http_api::state::{AppState, WallCommand};

pub async fn get_settings(
    State(state): State<AppState>,
) -> Result<Json<crate::config::DisplaySettings>, ApiError> {
    let s = state.settings.read().await.clone();
    Ok(Json(s))
}

pub async fn put_settings(
    State(state): State<AppState>,
    Json(patch): Json<SettingsPatch>,
) -> Result<Json<OkResponse>, ApiError> {
    let _config_guard = state.config_update_lock.lock().await;
    let old_config = state.config.read().await.clone();
    let mut candidate_config = old_config.clone();
    let candidate = patch.merge(&candidate_config.display);
    candidate
        .validate()
        .map_err(|e| ApiError::InvalidSettings(e.to_string()))?;

    candidate_config.display = candidate.clone();
    // display.rotation_interval is authoritative after an API edit. Clear
    // the legacy defaults override so it cannot undo this value on restart.
    candidate_config.defaults.rotation_interval = None;
    candidate_config.validate()?;
    state.persist_config(&candidate_config)?;
    let old = {
        let mut s = state.settings.write().await;
        std::mem::replace(&mut *s, candidate.clone())
    };

    // Immediate visual changes: spacing / background require a redraw of
    // the composed grid frame. Decoders keep running untouched.
    if old.spacing != candidate.spacing || old.background != candidate.background {
        info!(
            spacing = candidate.spacing,
            background = %candidate.background,
            "settings changed — recompositing"
        );
        if !crate::http_api::state::is_paused_now().await {
            if let Err(error) = state.manager.recomposite().await {
                *state.settings.write().await = old.clone();
                state.persist_config(&old_config).map_err(|rollback| {
                    ApiError::Internal(format!(
                        "settings apply failed ({error}) and config rollback failed: {rollback}"
                    ))
                })?;
                return Err(error.into());
            }
        }
    }
    if old.pause_off_screen != candidate.pause_off_screen {
        info!(
            pause_off_screen = candidate.pause_off_screen,
            "auto-pause policy updated"
        );
    }
    if old.rotation_interval != candidate.rotation_interval
        || old.fullscreen_timeout != candidate.fullscreen_timeout
    {
        info!("timer settings updated — effective for upcoming ticks");
    }

    state.publish_config(candidate_config).await;
    // Startup-only choices must not restart the renderer or change the
    // currently displayed scene/power state. Only live rendering settings
    // are forwarded to the native child.
    let renderer_changed = old.spacing != candidate.spacing
        || old.hardware_decoder_limit != candidate.hardware_decoder_limit
        || old.pause_off_screen != candidate.pause_off_screen
        || old.debug_overlay != candidate.debug_overlay
        || old.background != candidate.background
        || old.rotation_interval != candidate.rotation_interval
        || old.fullscreen_timeout != candidate.fullscreen_timeout;
    if renderer_changed {
        let _ = state
            .command_tx
            .send(WallCommand::UpdateSettings(candidate))
            .await;
    }
    Ok(Json(OkResponse::ok()))
}
