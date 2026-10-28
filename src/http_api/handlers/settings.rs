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

use axum::extract::State;
use axum::Json;
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
    // Compute candidate under the read lock, upgrade to write.
    let candidate = {
        let cur = state.settings.read().await;
        patch.merge(&cur)
    };
    candidate
        .validate()
        .map_err(|e| ApiError::InvalidSettings(e.to_string()))?;

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
            state.manager.recomposite().await?;
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

    let _ = state
        .command_tx
        .send(WallCommand::UpdateSettings(candidate))
        .await;
    Ok(Json(OkResponse::ok()))
}
