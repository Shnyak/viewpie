// This file contains modifications generated with assistance from Qwen AI
// Original project: https://github.com/snoack/viewpie
// SPDX-License-Identifier: GPL-3.0-or-later

//! `GET /api/status` — runtime snapshot for dashboards (Home Assistant).

use axum::extract::State;
use axum::Json;

use crate::http_api::errors::ApiError;
use crate::http_api::models::StatusResponse;
use crate::http_api::state::AppState;

pub async fn status(State(state): State<AppState>) -> Result<Json<StatusResponse>, ApiError> {
    let wall = state.wall_state.read().await;
    let settings = state.settings.read().await;
    // Drop read guards before awaiting anything else to keep lock scopes tight.
    let paused = wall.paused;
    let fullscreen_camera = wall.fullscreen_camera.clone();
    drop(wall);

    let screen_on = state.backend.screen_on().await;

    let active_panel = state.manager.active_id().await;
    let panel = state.manager.get_panel(&active_panel).await.ok();
    let active_panel_name = panel.as_ref().map(|p| p.name.clone()).unwrap_or_default();
    let layout = panel.map(|p| p.layout).unwrap_or_default();

    let cameras = state.camera_names().await;
    let active_streams = state.feeds_pool.active_streams().await.len();
    let panels_count = state.manager.count().await;
    let uptime_seconds = chrono::Utc::now()
        .signed_duration_since(state.uptime_started)
        .num_seconds();
    let cpu_temp_c = tokio::task::spawn_blocking(|| crate::http_api::state::read_cpu_temp(None))
        .await
        .ok()
        .flatten();

    Ok(Json(StatusResponse {
        paused,
        pause_off_screen: settings.pause_off_screen,
        screen_off: !screen_on,
        active_panel,
        active_panel_name,
        fullscreen_camera,
        cameras,
        layout,
        spacing: settings.spacing,
        uptime_seconds,
        cpu_temp_c,
        active_streams,
        panels_count,
    }))
}
