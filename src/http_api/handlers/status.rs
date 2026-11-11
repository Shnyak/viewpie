// This file contains modifications generated with assistance from Qwen AI
// Original project: https://github.com/snoack/viewpie
// SPDX-License-Identifier: GPL-3.0-or-later

//! `GET /api/status` — runtime snapshot for dashboards (Home Assistant).

use axum::extract::State;
use axum::Json;
use chrono::DateTime;

use crate::http_api::errors::ApiError;
use crate::http_api::handlers::restart::restart_available;
use crate::http_api::models::StatusResponse;
use crate::http_api::state::AppState;

fn format_build_date(value: &str) -> String {
    DateTime::parse_from_rfc3339(value)
        .map(|date| date.format("%d.%m.%y %H:%M:%S").to_string())
        .unwrap_or_else(|_| value.to_string())
}

fn format_uptime(seconds: i64) -> String {
    let seconds = seconds.max(0);
    let days = seconds / 86_400;
    let hours = (seconds % 86_400) / 3_600;
    let minutes = (seconds % 3_600) / 60;
    if days > 0 {
        format!("{days} дн. {hours} ч. {minutes} мин.")
    } else if hours > 0 {
        format!("{hours} ч. {minutes} мин.")
    } else if minutes > 0 {
        format!("{minutes} мин.")
    } else {
        format!("{seconds} сек.")
    }
}

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
    let layout = panel.as_ref().map(|p| p.layout.clone()).unwrap_or_default();

    let cameras = state.camera_names().await;
    // In native-pipeline mode the API process deliberately owns no decoders;
    // report the configured stream count for the active panel instead of the
    // empty API-side FeedPool. The child renderer is the sole RTSP owner.
    let active_streams = if state.native_pipeline {
        panel.as_ref().map(|p| p.camera_count()).unwrap_or(0)
    } else {
        state.feeds_pool.active_streams().await.len()
    };
    let panels_count = state.manager.count().await;
    let mut decoder_status: Vec<serde_json::Value> = state
        .decoder_status
        .read()
        .await
        .values()
        .cloned()
        .collect();
    // Include cameras that have not been opened yet as well: HA can show the
    // complete inventory immediately, while the active panel's cameras begin
    // as "starting" and the rest are explicitly "paused".
    for camera in &cameras {
        let reported = decoder_status
            .iter()
            .any(|item| item.get("camera").and_then(|v| v.as_str()) == Some(camera));
        if !reported {
            let active = panel.as_ref().is_some_and(|p| p.has_camera(camera));
            decoder_status.push(serde_json::json!({
                "camera": camera,
                "decoder": null,
                "mode": null,
                "state": if active { "starting" } else { "paused" },
                "fallback_reason": null,
            }));
        }
    }
    decoder_status.sort_by(|a, b| {
        a.get("camera")
            .and_then(|v| v.as_str())
            .cmp(&b.get("camera").and_then(|v| v.as_str()))
    });
    let uptime_seconds = chrono::Utc::now()
        .signed_duration_since(state.uptime_started)
        .num_seconds();
    let (cpu_temp_c, cpu_load_percent, load_average_1m) = tokio::task::spawn_blocking(|| {
        (
            crate::http_api::state::read_cpu_temp(None),
            crate::http_api::state::read_cpu_load_percent(None),
            crate::http_api::state::read_load_average_1m(None),
        )
    })
    .await
    .unwrap_or((None, None, None));

    Ok(Json(StatusResponse {
        software_version: env!("CARGO_PKG_VERSION"),
        software_build_date: format_build_date(env!("VIEWPIE_BUILD_DATE")),
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
        uptime: format_uptime(uptime_seconds),
        cpu_temp_c,
        cpu_load_percent,
        load_average_1m,
        restart_available: restart_available(),
        active_streams,
        panels_count,
        decoder_status,
    }))
}

#[cfg(test)]
mod tests {
    use super::{format_build_date, format_uptime};

    #[test]
    fn build_date_uses_day_month_short_year_and_time() {
        assert_eq!(
            format_build_date("2026-11-11T16:37:16+03:00"),
            "11.11.26 16:37:16"
        );
    }

    #[test]
    fn uptime_uses_days_hours_and_minutes() {
        assert_eq!(format_uptime(0), "0 сек.");
        assert_eq!(format_uptime(125), "2 мин.");
        assert_eq!(format_uptime(3_661), "1 ч. 1 мин.");
        assert_eq!(format_uptime(90_061), "1 дн. 1 ч. 1 мин.");
    }
}
