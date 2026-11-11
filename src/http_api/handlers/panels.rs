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
    ActivePanelResponse, OkResponse, PanelDto, PanelListResponse, PanelOrderRequest, PanelSummary,
};
use crate::http_api::state::{AppState, WallCommand};

/// `GET /api/panels`
pub async fn list_panels(
    State(state): State<AppState>,
) -> Result<Json<PanelListResponse>, ApiError> {
    let all = state.manager.list().await;
    let active = state.manager.active_id().await;
    let config = state.config.read().await.clone();
    let panels = config.ordered_panel_ids().into_iter().filter_map(|id| {
        all.iter().find(|(panel_id, _)| panel_id == &id).map(|(_, p)| PanelSummary {
            id,
            name: p.name.clone(),
            camera_count: p.camera_count(),
        })
    }).collect();
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
    let _config_guard = state.config_update_lock.lock().await;
    let mut dto = panel;
    dto.id = None;
    let panel_config = dto.into_config();
    let old_config = state.config.read().await.clone();
    let mut candidate = old_config.clone();
    if candidate.panels.contains_key(&id) {
        return Err(ApiError::conflict(format!("panel {id:?} already exists")));
    }
    candidate.panels.insert(id.clone(), panel_config.clone());
    candidate.defaults.panel_order = old_config.ordered_panel_ids();
    if !candidate.defaults.panel_order.contains(&id) {
        candidate.defaults.panel_order.push(id.clone());
    }
    candidate.validate()?;
    state.persist_config(&candidate)?;
    if let Err(error) = state.manager.add_panel(id.clone(), panel_config).await {
        state.persist_config(&old_config).map_err(|rollback| {
            ApiError::Internal(format!(
                "panel creation failed ({error}) and config rollback failed: {rollback}"
            ))
        })?;
        return Err(error.into());
    }
    state.publish_config(candidate).await;
    let mut g = state.panels.write().await;
    if let Ok(c) = state.manager.get_panel(&id).await {
        g.insert(id.clone(), c);
    }
    drop(g);
    let _ = state.command_tx.send(WallCommand::ReloadConfig).await;
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
    let _config_guard = state.config_update_lock.lock().await;
    let mut dto = panel;
    dto.id = Some(id.clone());
    let new_panel = dto.into_config();
    let old_config = state.config.read().await.clone();
    let old_panel = old_config
        .panels
        .get(&id)
        .cloned()
        .ok_or_else(|| ApiError::not_found(format!("panel {id:?} not found")))?;
    let mut candidate = old_config.clone();
    candidate.panels.insert(id.clone(), new_panel.clone());
    candidate.validate()?;
    state.persist_config(&candidate)?;
    if let Err(error) = state.manager.update_panel(&id, new_panel).await {
        let runtime_rollback = state.manager.update_panel(&id, old_panel).await;
        let disk_rollback = state.persist_config(&old_config);
        if let Err(rollback) = runtime_rollback {
            return Err(ApiError::Internal(format!(
                "panel update failed ({error}); runtime rollback failed: {rollback}"
            )));
        }
        if let Err(rollback) = disk_rollback {
            return Err(ApiError::Internal(format!(
                "panel update failed ({error}); config rollback failed: {rollback}"
            )));
        }
        return Err(error.into());
    }
    state.publish_config(candidate).await;
    let mut g = state.panels.write().await;
    if let Ok(c) = state.manager.get_panel(&id).await {
        g.insert(id.clone(), c);
    }
    drop(g);
    let _ = state.command_tx.send(WallCommand::ReloadConfig).await;
    Ok(Json(OkResponse::ok()))
}

/// `DELETE /api/panels/{id}`
pub async fn delete_panel(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<OkResponse>, ApiError> {
    let _config_guard = state.config_update_lock.lock().await;
    let old_config = state.config.read().await.clone();
    let removed = old_config
        .panels
        .get(&id)
        .cloned()
        .ok_or_else(|| ApiError::not_found(format!("panel {id:?} not found")))?;
    let mut candidate = old_config.clone();
    candidate.panels.remove(&id);
    candidate.defaults.panel_order.retain(|panel_id| panel_id != &id);
    if candidate.panels.is_empty() {
        return Err(ApiError::LastPanel);
    }
    if candidate.defaults.panel.as_deref() == Some(id.as_str()) {
        candidate.defaults.panel = None;
    }
    if candidate.display.startup_panel.as_deref() == Some(id.as_str()) {
        candidate.display.startup_panel = None;
    }
    if candidate.display.startup_panel.is_none() {
        candidate.display.startup_panel = Some(candidate.initial_panel());
    }
    candidate.validate()?;
    state.persist_config(&candidate)?;
    if let Err(error) = state.manager.delete_panel(&id).await {
        state.persist_config(&old_config).map_err(|rollback| {
            ApiError::Internal(format!(
                "panel deletion failed ({error}) and config rollback failed: {rollback}"
            ))
        })?;
        return Err(error.into());
    }
    state.publish_config(candidate).await;
    let _ = removed;
    let mut g = state.panels.write().await;
    g.remove(&id);
    let active = state.manager.active_id().await;
    *state.active_panel.write().await = active.clone();
    let _ = state.command_tx.send(WallCommand::ReloadConfig).await;
    info!(panel = id, "panel deleted");
    Ok(Json(OkResponse::ok()))
}


/// `PUT /api/panels/order` — persist the exact display order.
pub async fn reorder_panels(
    State(state): State<AppState>,
    Json(request): Json<PanelOrderRequest>,
) -> Result<Json<OkResponse>, ApiError> {
    let _config_guard = state.config_update_lock.lock().await;
    let old_config = state.config.read().await.clone();
    let requested = request.order;
    let mut expected = old_config.ordered_panel_ids();
    expected.sort();
    let mut actual = requested.clone();
    actual.sort();
    if actual != expected {
        return Err(ApiError::bad_request(
            "order must contain every panel id exactly once",
        ));
    }
    let mut candidate = old_config;
    candidate.defaults.panel_order = requested;
    candidate.validate()?;
    state.persist_config(&candidate)?;
    state.publish_config(candidate).await;
    Ok(Json(OkResponse::ok()))
}

/// `POST /api/panels/{id}/activate` — graceful reference-counted switch.
pub async fn activate_panel(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<OkResponse>, ApiError> {
    // In native-pipeline mode the serialized command pump is the sole
    // activation owner. The API-side manager must not start its own RTSP
    // decoders before the child renderer receives the switch command.
    // Keep the existing synchronous manager path for the non-native backend.
    if state.native_pipeline {
        // Preserve the REST contract (unknown panel → 404) without performing
        // the decoder-owning switch in the request handler.
        state.manager.get_panel(&id).await?;
    } else {
        state.manager.switch_to(&id).await?;
        *state.active_panel.write().await = id.clone();
    }
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
