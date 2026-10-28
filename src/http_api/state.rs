// This file contains modifications generated with assistance from Qwen AI
// Original project: https://github.com/snoack/viewpie
// SPDX-License-Identifier: GPL-3.0-or-later

//! Shared application state for the HTTP API and background tasks.
//!
//! Design notes (async correctness):
//! * All locks are `tokio::sync::RwLock` — never held across `await`
//!   points unless explicitly intended, and lock order is always
//!   `SETTINGS → WALL → PANELS → ACTIVE` to avoid deadlocks.
//! * Handlers perform their mutations through this module's helpers so
//!   the panel manager and the API can never diverge.
//! * A [`WallCommand`] mpsc channel exists for consumers that prefer a
//!   serialized command pipeline (CEC handler); the REST handlers apply
//!   changes directly through the shared singletons below.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock};

use chrono::{DateTime, Utc};
use tokio::sync::{mpsc, RwLock};

use crate::config::{DisplaySettings, FeedConfig, PanelConfig};
use crate::feed::FeedPool;
use crate::kms::DisplayBackend;
use crate::panel_manager::PanelManager;
use crate::wall::Resolution;

/// Commands accepted by the wall renderer loop (CEC / timers / API).
#[derive(Debug, Clone)]
pub enum WallCommand {
    SwitchPanel(String),
    SetFullscreen(String, Option<u64>),
    ReturnToGrid,
    Pause,
    Resume,
    UpdateSettings(DisplaySettings),
    ReloadConfig,
}

/// Per-panel runtime state (pause/fullscreen tracked independently per scene).
#[derive(Debug, Clone, Default)]
pub struct PanelRuntimeState {
    pub paused: bool,
    pub fullscreen_camera: Option<String>,
}

/// Global wall runtime state.
#[derive(Debug, Clone, Default)]
pub struct WallState {
    pub paused: bool,
    /// Set when pause was triggered automatically by screen-off.
    pub auto_paused: bool,
    pub fullscreen_camera: Option<String>,
    pub fullscreen_started: Option<DateTime<Utc>>,
    pub panel_states: HashMap<String, PanelRuntimeState>,
}

/// The complete application state, cheaply clonable (`Arc` fields).
#[derive(Clone)]
pub struct AppState {
    pub wall_state: Arc<RwLock<WallState>>,
    pub feeds_pool: Arc<FeedPool>,
    pub panels: Arc<RwLock<HashMap<String, PanelConfig>>>,
    pub active_panel: Arc<RwLock<String>>,
    pub settings: Arc<RwLock<DisplaySettings>>,
    pub command_tx: mpsc::Sender<WallCommand>,
    pub uptime_started: DateTime<Utc>,
    pub config_path: Arc<String>,
    pub manager: Arc<PanelManager>,
    pub backend: Arc<dyn DisplayBackend>,
}

// ─────────────────────── process-wide singletons ───────────────────────
//
// The panel manager needs access to live display settings when compositing
// frames. To keep every component cheaply constructible in tests we hold
// the current settings + wall state in process-wide registries that are
// initialised exactly once by `init_singletons` (the first App construction
// wins; tests construct one app at a time per process group).

struct Singletons {
    settings: RwLock<Option<Arc<RwLock<DisplaySettings>>>>,
    wall: RwLock<Option<Arc<RwLock<WallState>>>>,
}

static SINGLETONS: LazyLock<Singletons> = LazyLock::new(|| Singletons {
    settings: RwLock::new(None),
    wall: RwLock::new(None),
});

/// Register the global settings/wall-state objects used by the compositor.
pub async fn init_singletons(settings: Arc<RwLock<DisplaySettings>>, wall: Arc<RwLock<WallState>>) {
    let mut s = SINGLETONS.settings.write().await;
    if s.is_none() {
        *s = Some(settings);
    }
    let mut w = SINGLETONS.wall.write().await;
    if w.is_none() {
        *w = Some(wall);
    }
}

async fn settings_arc() -> Arc<RwLock<DisplaySettings>> {
    let guard = SINGLETONS.settings.read().await;
    guard.clone().expect("singletons not initialised")
}

async fn wall_arc() -> Arc<RwLock<WallState>> {
    let guard = SINGLETONS.wall.read().await;
    guard.clone().expect("singletons not initialised")
}

/// Effective spacing: per-panel override wins over the global setting.
pub async fn global_spacing_for(panel_override: Option<u32>) -> u32 {
    match panel_override {
        Some(v) => v,
        None => settings_arc().await.read().await.spacing,
    }
}

/// Current background color.
pub async fn global_background() -> String {
    settings_arc().await.read().await.background.clone()
}

/// Whether the wall is currently paused.
pub async fn is_paused_now() -> bool {
    wall_arc().await.read().await.paused
}

/// Set the paused flag.
pub async fn set_paused(v: bool) {
    wall_arc().await.write().await.paused = v;
}

/// Mark/unmark automatic (screen-off triggered) pause.
pub async fn set_auto_paused(v: bool) {
    wall_arc().await.write().await.auto_paused = v;
}

/// Read the fullscreen camera without modifying anything.
pub async fn current_fullscreen() -> Option<String> {
    wall_arc().await.read().await.fullscreen_camera.clone()
}

/// Install a new fullscreen camera, returning the previous one (if any)
/// and stamping `fullscreen_started`.
pub async fn set_fullscreen_state(cam: Option<String>) -> Option<String> {
    let arc = wall_arc().await;
    let mut w = arc.write().await;
    let prev = w.fullscreen_camera.take();
    w.fullscreen_camera = cam;
    w.fullscreen_started = if w.fullscreen_camera.is_some() {
        Some(Utc::now())
    } else {
        None
    };
    prev
}

/// Clear fullscreen state, returning the previously shown camera.
pub async fn clear_fullscreen_state() -> Option<String> {
    set_fullscreen_state(None).await
}

// ─────────────────────── AppState construction ───────────────────────

impl AppState {
    /// Build the full application from validated configuration.
    pub async fn bootstrap(
        cfg: crate::config::Config,
        backend: Arc<dyn DisplayBackend>,
        resolution: Resolution,
        config_path: &str,
    ) -> (Self, mpsc::Receiver<WallCommand>) {
        let (tx, rx) = mpsc::channel(64);

        let pool = Arc::new(FeedPool::new());
        for (name, fc) in &cfg.feeds {
            pool.add_feed(name.clone(), fc.clone()).await;
        }

        let settings = Arc::new(RwLock::new(cfg.display.clone()));
        let wall_state = Arc::new(RwLock::new(WallState::default()));
        init_singletons(settings.clone(), wall_state.clone()).await;

        let panels = Arc::new(RwLock::new(cfg.panels.clone()));
        let active_panel = Arc::new(RwLock::new(cfg.initial_panel()));

        let manager = Arc::new(PanelManager::new(
            cfg.panels.clone(),
            cfg.initial_panel(),
            pool.clone(),
            backend.clone(),
            resolution,
        ));

        // Acquire refs for the initial panel (starts decoders). The manager
        // starts in "no panel displayed" state, so this first switch_to is a
        // full acquire pass rather than an idempotent re-commit.
        let initial = manager.active_id().await;
        manager.switch_to(&initial).await.ok();

        let state = Self {
            wall_state,
            feeds_pool: pool,
            panels,
            active_panel,
            settings,
            command_tx: tx,
            uptime_started: Utc::now(),
            config_path: Arc::new(config_path.to_string()),
            manager,
            backend,
        };
        (state, rx)
    }

    /// Snapshot of cameras known to the pool.
    pub async fn camera_names(&self) -> Vec<String> {
        self.feeds_pool
            .list()
            .await
            .into_iter()
            .map(|f| f.name)
            .collect()
    }

    /// Add a camera to the pool (ref_count 0 — decoder starts on use).
    pub async fn add_camera(&self, name: String, config: FeedConfig) -> bool {
        self.feeds_pool.add_feed(name, config).await
    }

    /// Layout string of the active panel (for /api/status).
    pub async fn active_layout_text(&self) -> String {
        let id = self.manager.active_id().await;
        let panels = self.panels.read().await;
        match panels.get(&id) {
            Some(p) => p.layout.clone(),
            None => String::new(),
        }
    }
}

/// CPU temperature in °C read from `/sys/class/thermal/thermal_zone0/temp`,
/// or `None` when unavailable (e.g. non-Pi machines / tests).
pub fn read_cpu_temp(sysfs_root: Option<&str>) -> Option<f64> {
    let root = sysfs_root.unwrap_or("/sys/class/thermal/thermal_zone0/temp");
    let raw = std::fs::read_to_string(root).ok()?;
    let milli: i64 = raw.trim().parse().ok()?;
    Some(milli as f64 / 1000.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::kms::HeadlessBackend;

    const SAMPLE: &str = r#"
feeds:
  a: { url: "rtsp://c/a", fullscreen_url: "rtsp://c/a_main" }
panels:
  only:
    layout: |
      a a a
      a a a
      a a a
    viewports:
      - { slot: a, feeds: [a] }
defaults:
  panel: only
"#;

    #[tokio::test]
    async fn bootstrap_and_status_helpers() {
        let cfg = Config::from_yaml(SAMPLE).unwrap();
        let be: Arc<dyn DisplayBackend> = Arc::new(HeadlessBackend::new());
        let (state, _rx) = AppState::bootstrap(
            cfg,
            be,
            Resolution {
                width: 800,
                height: 480,
            },
            "/tmp/nonexistent.yaml",
        )
        .await;
        assert_eq!(state.camera_names().await, vec!["a"]);
        assert_eq!(state.manager.active_id().await, "only");
        assert!(!is_paused_now().await);
        assert!(state.feeds_pool.is_running("a").await);
        // spacing helper honours overrides
        assert_eq!(global_spacing_for(Some(7)).await, 7);
        assert_eq!(global_spacing_for(None).await, 1);
        // fullscreen state transitions
        assert_eq!(set_fullscreen_state(Some("a".into())).await, None);
        assert_eq!(current_fullscreen().await.as_deref(), Some("a"));
        assert_eq!(clear_fullscreen_state().await.as_deref(), Some("a"));
        assert_eq!(current_fullscreen().await, None);
    }

    #[test]
    fn cpu_temp_missing_file_is_none() {
        assert!(read_cpu_temp(Some("/definitely/not/here")).is_none());
    }

    #[test]
    fn cpu_temp_parses_milli() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("temp");
        std::fs::write(&f, "58234\n").unwrap();
        assert_eq!(read_cpu_temp(f.to_str()), Some(58.234));
    }
}
