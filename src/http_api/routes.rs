// This file contains modifications generated with assistance from Qwen AI
// Original project: https://github.com/snoack/viewpie
// SPDX-License-Identifier: GPL-3.0-or-later

//! Axum route table for the REST API.

use axum::routing::{get, post, put};
use axum::Router;

use crate::http_api::handlers::{cameras, control, panels, settings, status};
use crate::http_api::state::AppState;

pub fn router(state: AppState) -> Router {
    Router::new()
        // State control
        .route("/api/pause", post(control::pause))
        .route("/api/resume", post(control::resume))
        .route("/api/grid", post(control::grid))
        .route("/api/fullscreen", post(control::fullscreen))
        .route("/api/reload", post(control::reload))
        // Display settings
        .route(
            "/api/settings",
            get(settings::get_settings).put(settings::put_settings),
        )
        // Panels
        .route(
            "/api/panels",
            get(panels::list_panels).post(panels::create_panel),
        )
        .route("/api/panels/active", get(panels::active_panel))
        .route(
            "/api/panels/{id}",
            get(panels::get_panel)
                .put(panels::update_panel)
                .delete(panels::delete_panel),
        )
        .route("/api/panels/{id}/activate", post(panels::activate_panel))
        // Cameras
        .route(
            "/api/cameras",
            get(cameras::list_cameras).post(cameras::create_camera),
        )
        .route(
            "/api/cameras/{name}",
            put(cameras::update_camera).delete(cameras::delete_camera),
        )
        .route("/api/cameras/{name}/test", post(cameras::test_camera))
        // Status
        .route("/api/status", get(status::status))
        .with_state(state)
}
