// This file contains modifications generated with assistance from Qwen AI
// Original project: https://github.com/snoack/viewpie
// SPDX-License-Identifier: GPL-3.0-or-later

//! Serde request/response models for the REST API.
//!
//! These wire types are deliberately separate from internal domain types
//! so the HTTP contract can evolve without breaking YAML compatibility.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::config::{
    DecoderPreference, DisplaySettings, FeedConfig, PanelConfig, ScreenBootMode, ViewportConfig,
};
use crate::feed::FeedState;

/// `POST /api/fullscreen` body.
#[derive(Debug, Clone, Deserialize)]
pub struct FullscreenRequest {
    pub camera: String,
    #[serde(default)]
    pub duration_seconds: Option<u64>,
}

/// Camera resource (GET/POST/PUT `/api/cameras`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CameraDto {
    pub name: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fullscreen_url: Option<String>,
    /// Requested decoder path for this camera.
    #[serde(default)]
    pub decoder: DecoderPreference,
    /// Number of panels currently referencing this camera.
    #[serde(default)]
    pub ref_count: usize,
    /// Whether a decoder is running for this camera.
    #[serde(default)]
    pub running: bool,
}

impl CameraDto {
    pub fn from_state(s: &FeedState) -> Self {
        Self {
            name: s.name.clone(),
            url: s.config.url.clone(),
            fullscreen_url: s.config.fullscreen_url.clone(),
            decoder: s.config.decoder,
            ref_count: s.ref_count,
            running: s.running,
        }
    }

    pub fn to_feed_config(&self) -> FeedConfig {
        FeedConfig {
            url: self.url.clone(),
            fullscreen_url: self.fullscreen_url.clone(),
            decoder: self.decoder,
        }
    }
}

/// Panel summary item (`GET /api/panels`).
#[derive(Debug, Clone, Serialize)]
pub struct PanelSummary {
    pub id: String,
    pub name: String,
    pub camera_count: usize,
}

/// `PUT /api/panels/order` request.
#[derive(Debug, Clone, Deserialize)]
pub struct PanelOrderRequest {
    pub order: Vec<String>,
}

/// `GET /api/panels` response.
#[derive(Debug, Clone, Serialize)]
pub struct PanelListResponse {
    pub panels: Vec<PanelSummary>,
    pub active: String,
}

/// `GET /api/panels/active` response.
#[derive(Debug, Clone, Serialize)]
pub struct ActivePanelResponse {
    pub id: String,
    pub name: String,
}

/// Panel over the wire: YAML shape plus an optional `id` used by
/// `POST /api/panels` (the id is otherwise the map key in the config file).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PanelDto {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(flatten)]
    pub panel: PanelConfig,
}

impl PanelDto {
    /// Strip the transport-level id and return the domain config.
    pub fn into_config(self) -> PanelConfig {
        self.panel
    }
}

impl From<PanelConfig> for PanelDto {
    fn from(p: PanelConfig) -> Self {
        Self { id: None, panel: p }
    }
}

/// Wire viewport == config viewport.
pub type ViewportDto = ViewportConfig;

/// Partial settings update (`PUT /api/settings`): every field optional.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct SettingsPatch {
    pub spacing: Option<u32>,
    pub hardware_decoder_limit: Option<u32>,
    pub pause_off_screen: Option<bool>,
    pub debug_overlay: Option<bool>,
    pub startup_panel: Option<String>,
    pub screen_boot_mode: Option<ScreenBootMode>,
    pub background: Option<String>,
    pub rotation_interval: Option<u64>,
    pub fullscreen_timeout: Option<u64>,
}

impl SettingsPatch {
    /// Apply the patch on top of `base`, producing the candidate settings.
    pub fn merge(&self, base: &DisplaySettings) -> DisplaySettings {
        DisplaySettings {
            spacing: self.spacing.unwrap_or(base.spacing),
            hardware_decoder_limit: self
                .hardware_decoder_limit
                .unwrap_or(base.hardware_decoder_limit),
            pause_off_screen: self.pause_off_screen.unwrap_or(base.pause_off_screen),
            debug_overlay: self.debug_overlay.unwrap_or(base.debug_overlay),
            startup_panel: self
                .startup_panel
                .clone()
                .or_else(|| base.startup_panel.clone()),
            screen_boot_mode: self.screen_boot_mode.unwrap_or(base.screen_boot_mode),
            last_screen_on: base.last_screen_on,
            background: self
                .background
                .clone()
                .unwrap_or_else(|| base.background.clone()),
            rotation_interval: self.rotation_interval.unwrap_or(base.rotation_interval),
            fullscreen_timeout: self.fullscreen_timeout.unwrap_or(base.fullscreen_timeout),
        }
    }
}

/// `GET /api/status` response — full runtime snapshot.
#[derive(Debug, Clone, Serialize)]
pub struct StatusResponse {
    /// Version of the Viewpie package used to build this running binary.
    pub software_version: &'static str,
    /// ISO 8601 timestamp of the source commit used for this build.
    pub software_build_date: String,
    pub paused: bool,
    pub pause_off_screen: bool,
    pub screen_off: bool,
    pub active_panel: String,
    pub active_panel_name: String,
    pub fullscreen_camera: Option<String>,
    pub cameras: Vec<String>,
    pub layout: String,
    pub spacing: u32,
    pub uptime_seconds: i64,
    /// Human-readable uptime, suitable for dashboards.
    pub uptime: String,
    pub cpu_temp_c: Option<f64>,
    pub cpu_load_percent: Option<f64>,
    pub load_average_1m: Option<f64>,
    pub restart_available: bool,
    pub active_streams: usize,
    pub panels_count: usize,
    /// Actual per-camera decoder path reported by the native renderer.
    #[serde(default)]
    pub decoder_status: Vec<serde_json::Value>,
}

/// Generic success envelope.
#[derive(Debug, Clone, Serialize)]
pub struct OkResponse {
    pub ok: bool,
}

impl OkResponse {
    pub fn ok() -> Self {
        Self { ok: true }
    }
}

/// `POST /api/cameras/{name}/test` response.
#[derive(Debug, Clone, Serialize)]
pub struct CameraTestResponse {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Map of slot → feeds helper used by tooling.
pub type SlotMap = HashMap<String, Vec<String>>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_patch_merge_partial() {
        let base = DisplaySettings::default();
        let patch: SettingsPatch = serde_json::from_str(r#"{"spacing": 5}"#).unwrap();
        let merged = patch.merge(&base);
        assert_eq!(merged.spacing, 5);
        assert_eq!(merged.hardware_decoder_limit, 6);
        assert_eq!(merged.background, base.background);
        assert_eq!(merged.pause_off_screen, base.pause_off_screen);
    }

    #[test]
    fn settings_patch_merge_full() {
        let base = DisplaySettings::default();
        let patch: SettingsPatch = serde_json::from_str(
            r##"{"spacing":2,"hardware_decoder_limit":4,"pause_off_screen":false,"background":"#ff0000","rotation_interval":30,"fullscreen_timeout":90}"##,
        )
        .unwrap();
        let merged = patch.merge(&base);
        assert_eq!(merged.spacing, 2);
        assert_eq!(merged.hardware_decoder_limit, 4);
        assert!(!merged.pause_off_screen);
        assert_eq!(merged.background, "#ff0000");
        assert_eq!(merged.rotation_interval, 30);
        assert_eq!(merged.fullscreen_timeout, 90);
    }

    #[test]
    fn camera_dto_roundtrip() {
        let dto = CameraDto {
            name: "yard".into(),
            url: "rtsp://x/y".into(),
            fullscreen_url: Some("rtsp://x/ym".into()),
            decoder: DecoderPreference::Auto,
            ref_count: 2,
            running: true,
        };
        let json = serde_json::to_string(&dto).unwrap();
        let back: CameraDto = serde_json::from_str(&json).unwrap();
        assert_eq!(back.name, "yard");
        assert_eq!(back.decoder, DecoderPreference::Auto);
        assert_eq!(back.ref_count, 2);
    }

    #[test]
    fn fullscreen_request_defaults() {
        let req: FullscreenRequest = serde_json::from_str(r#"{"camera":"pool"}"#).unwrap();
        assert_eq!(req.duration_seconds, None);
    }

    #[test]
    fn panel_dto_deserializes_yaml_shape() {
        let p: PanelDto = serde_json::from_value(serde_json::json!({
            "name": "Outside",
            "layout": "a a b\na a b\nb b b\n",
            "viewports": [{"slot":"a","feeds":["cam_a"]},{"slot":"b","feeds":["cam_b"]}]
        }))
        .unwrap();
        assert_eq!(p.panel.camera_count(), 2);
        assert_eq!(p.id, None);
        let with_id: PanelDto = serde_json::from_value(serde_json::json!({
            "id": "outside",
            "name": "Outside",
            "layout": "a a b\na a b\nb b b\n",
            "viewports": [{"slot":"a","feeds":["cam_a"]},{"slot":"b","feeds":["cam_b"]}]
        }))
        .unwrap();
        assert_eq!(with_id.id.as_deref(), Some("outside"));
    }

    #[test]
    fn status_response_includes_build_package_version() {
        let status = StatusResponse {
            software_version: env!("CARGO_PKG_VERSION"),
            software_build_date: env!("VIEWPIE_BUILD_DATE").to_string(),
            paused: false,
            pause_off_screen: true,
            screen_off: false,
            active_panel: "main".into(),
            active_panel_name: "Main".into(),
            fullscreen_camera: None,
            cameras: vec![],
            layout: String::new(),
            spacing: 2,
            uptime_seconds: 1,
            uptime: "менее минуты".into(),
            cpu_temp_c: None,
            cpu_load_percent: None,
            load_average_1m: None,
            restart_available: false,
            active_streams: 0,
            panels_count: 1,
            decoder_status: vec![],
        };
        let json = serde_json::to_value(status).unwrap();
        assert_eq!(json["software_version"], env!("CARGO_PKG_VERSION"));
        assert!(json["software_build_date"].as_str().is_some());
        assert_eq!(json["uptime"], "менее минуты");
    }
}
