// This file contains modifications generated with assistance from Qwen AI
// Original project: https://github.com/snoack/viewpie
// SPDX-License-Identifier: GPL-3.0-or-later

//! Panel CRUD + activation endpoints.

use axum::extract::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use tracing::info;

use crate::http_api::errors::ApiError;
use crate::http_api::models::{
    ActivePanelResponse, OkResponse, PanelDto, PanelListResponse, PanelSummary,
};
use crate::http_api::state::{AppState, WallCommand};

/// `GET /api/panels`
pub async fn list_panels(
    State(state): State<AppState>,
) -> Result<Json<PanelListResponse>, ApiError> {
    let all = state.manager.list().await;
    let active = state.manager.active_id().await;
    let panels = all
        .into_iter()
        .map(|(id, p)| PanelSummary {
            id,
            name: p.name.clone(),
            camera_count: p.camera_count(),
        })
        .collect();
    Ok(Json(PanelListResponse { panels, active }))
}

/// `GET /api/panels/active`
pub async fn active_panel(
    State(state): State<AppState>,
) -> Result<Json<ActivePanelResponse>, ApiError> {
    let id = state.manager.active_id().await;
    let p = state
        .manager
        .get_panel(&id)
        .await
        .map_err(|_| ApiError::NotFound(id.clone()))?;
    Ok(Json(ActivePanelResponse { id, name: p.name }))
}

/// `GET /api/panels/{id}`
pub async fn get_panel(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<PanelDto>, ApiError> {
    let p = state.manager.get_panel(&id).await?;
    Ok(Json(PanelDto {
        id: Some(id),
        panel: p,
    }))
}

/// `POST /api/panels` — create. Body may embed `id`; otherwise the
/// `X-Panel-Id` free-form field `name` is slugified as fallback id.
/// Returns 201 Created.
pub async fn create_panel(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(panel): Json<PanelDto>,
) -> Result<(StatusCode, Json<OkResponse>), ApiError> {
    let id = panel
        .id
        .clone()
        .or_else(|| {
            headers
                .get("x-panel-id")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string())
        })
        .unwrap_or_else(|| slug(&panel.panel.name));
    if id.is_empty() {
        return Err(ApiError::bad_request("cannot derive panel id"));
    }
    let mut dto = panel;
    dto.id = None;
    state
        .manager
        .add_panel(id.clone(), dto.into_config())
        .await?;
    let mut g = state.panels.write().await;
    if let Ok(c) = state.manager.get_panel(&id).await {
        g.insert(id.clone(), c);
    }
    info!(panel = id, "panel created");
    Ok((StatusCode::CREATED, Json(OkResponse::ok())))
}

/// `PUT /api/panels/{id}` — update (creates if missing per REST convention
/// this fork keeps strict: must exist).
pub async fn update_panel(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(panel): Json<PanelDto>,
) -> Result<Json<OkResponse>, ApiError> {
    let mut dto = panel;
    dto.id = Some(id.clone());
    state.manager.update_panel(&id, dto.into_config()).await?;
    let mut g = state.panels.write().await;
    if let Ok(c) = state.manager.get_panel(&id).await {
        g.insert(id.clone(), c);
    }
    drop(g);
    if state.manager.active_id().await == id {
        let _ = state.command_tx.send(WallCommand::SwitchPanel(id.clone())).await;
    }
    Ok(Json(OkResponse::ok()))
}

/// `DELETE /api/panels/{id}`
pub async fn delete_panel(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<OkResponse>, ApiError> {
    state.manager.delete_panel(&id).await?;
    let mut g = state.panels.write().await;
    g.remove(&id);
    let active = state.manager.active_id().await;
    *state.active_panel.write().await = active.clone();
    let _ = state.command_tx.send(WallCommand::SwitchPanel(active)).await;
    info!(panel = id, "panel deleted");
    Ok(Json(OkResponse::ok()))
}

/// `POST /api/panels/{id}/activate` — graceful reference-counted switch.
pub async fn activate_panel(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<OkResponse>, ApiError> {
    state.manager.switch_to(&id).await?;
    *state.active_panel.write().await = id.clone();
    let _ = state.command_tx.send(WallCommand::SwitchPanel(id)).await;
    Ok(Json(OkResponse::ok()))
}

fn slug(name: &str) -> String {
    name.chars()
        .filter_map(|c| {
            if c.is_ascii_alphanumeric() {
                Some(c.to_ascii_lowercase())
            } else if c.is_whitespace() || c == '-' {
                Some('_')
            } else {
                // drop characters that cannot appear in an id
                None
            }
        })
        .collect::<String>()
        // collapse runs of separators, trim leading/trailing ones
        .split('_')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("_")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs() {
        assert_eq!(slug("Внутри дома"), "");
        assert_eq!(slug("Outside Area"), "outside_area");
        assert_eq!(slug("back-yard"), "back_yard");
    }
}
