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
use tokio::sync::{mpsc, oneshot, Mutex, RwLock};

use crate::config::{
    Config, ConfigError, DisplaySettings, FeedConfig, PanelConfig, ScreenBootMode,
};
use crate::feed::FeedPool;
use crate::kms::DisplayBackend;
use crate::panel_manager::PanelManager;
use crate::wall::Resolution;

/// Commands accepted by the wall renderer loop (CEC / timers / API).
#[derive(Debug)]
pub enum WallCommand {
    SwitchPanel(String),
    SetFullscreen(String, Option<u64>, oneshot::Sender<Result<(), String>>),
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
    /// Last validated configuration and the serialization lock for API writes.
    pub config: Arc<RwLock<Config>>,
    pub config_update_lock: Arc<Mutex<()>>,
    pub manager: Arc<PanelManager>,
    pub backend: Arc<dyn DisplayBackend>,
    /// True when the native child process owns all camera decoders.
    pub native_pipeline: bool,
    /// Latest actual decoder state reported by the native renderer child.
    pub decoder_status: Arc<RwLock<HashMap<String, serde_json::Value>>>,
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
        native_pipeline: bool,
        decoder_status: Arc<RwLock<HashMap<String, serde_json::Value>>>,
    ) -> (Self, mpsc::Receiver<WallCommand>) {
        let (tx, rx) = mpsc::channel(64);

        let pool = Arc::new(FeedPool::new());
        for (name, fc) in &cfg.feeds {
            pool.add_feed(name.clone(), fc.clone()).await;
        }

        let settings = Arc::new(RwLock::new(cfg.display.clone()));
        let initial_screen_on = match cfg.display.screen_boot_mode {
            ScreenBootMode::AlwaysOn => true,
            ScreenBootMode::AlwaysOff => false,
            ScreenBootMode::Restore => cfg.display.last_screen_on,
        };
        let wall_state = Arc::new(RwLock::new(WallState {
            paused: !initial_screen_on,
            ..WallState::default()
        }));
        init_singletons(settings.clone(), wall_state.clone()).await;

        let panels = Arc::new(RwLock::new(cfg.panels.clone()));
        let active_panel = Arc::new(RwLock::new(cfg.initial_panel()));

        let manager = Arc::new(PanelManager::new_with_feed_management(
            cfg.panels.clone(),
            cfg.initial_panel(),
            pool.clone(),
            backend.clone(),
            resolution,
            !native_pipeline,
        ));

        // Register the initial panel. In the native pipeline this updates
        // metadata only; the child renderer alone opens camera streams.
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
            config: Arc::new(RwLock::new(cfg.clone())),
            config_update_lock: Arc::new(Mutex::new(())),
            manager,
            backend,
            native_pipeline,
            decoder_status,
        };
        if !initial_screen_on {
            // Queue the pause before the command pump starts; the physical screen
            // stays off after a reboot without requiring a second user action.
            let _ = state.command_tx.send(WallCommand::Pause).await;
        }
        (state, rx)
    }

    /// Persist the last manually selected screen state for abrupt power loss.
    pub async fn persist_screen_state(&self, screen_on: bool) -> Result<(), ConfigError> {
        let _guard = self.config_update_lock.lock().await;
        let mut candidate = self.config.read().await.clone();
        candidate.display.last_screen_on = screen_on;
        self.persist_config(&candidate)?;
        self.settings.write().await.last_screen_on = screen_on;
        self.publish_config(candidate).await;
        Ok(())
    }

    /// Persist a validated candidate before publishing it to API readers.
    /// Callers hold config_update_lock while coordinating runtime mutations.
    pub fn persist_config(&self, candidate: &Config) -> Result<(), ConfigError> {
        candidate.save_atomic(self.config_path.as_str())
    }

    /// Publish the configuration after its runtime counterpart was updated.
    pub async fn publish_config(&self, candidate: Config) {
        *self.config.write().await = candidate;
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

/// Read the one-minute system load average from /proc/loadavg.
pub fn read_load_average_1m(path: Option<&str>) -> Option<f64> {
    let raw = std::fs::read_to_string(path.unwrap_or("/proc/loadavg")).ok()?;
    raw.split_whitespace().next()?.parse().ok()
}

fn parse_cpu_counters(raw: &str) -> Option<(u64, u64)> {
    let line = raw.lines().find(|line| line.starts_with("cpu "))?;
    let values: Vec<u64> = line
        .split_whitespace()
        .skip(1)
        .map(str::parse)
        .collect::<Result<_, _>>()
        .ok()?;
    if values.len() < 4 {
        return None;
    }
    let idle = values[3] + values.get(4).copied().unwrap_or(0);
    // guest and guest_nice are already included in user and nice; do not count them twice.
    let total: u64 = values.iter().take(8).sum();
    Some((total, idle))
}

/// CPU busy percentage since the previous sample. The first sample is None.
pub fn read_cpu_load_percent(path: Option<&str>) -> Option<f64> {
    static PREVIOUS: std::sync::Mutex<Option<(u64, u64)>> = std::sync::Mutex::new(None);
    let raw = std::fs::read_to_string(path.unwrap_or("/proc/stat")).ok()?;
    let current = parse_cpu_counters(&raw)?;
    let mut previous = PREVIOUS.lock().ok()?;
    let result = (*previous).and_then(|(old_total, old_idle)| {
        let total = current.0.saturating_sub(old_total);
        let idle = current.1.saturating_sub(old_idle);
        (total > 0).then_some(
            ((total.saturating_sub(idle)) as f64 / total as f64 * 100.0).clamp(0.0, 100.0),
        )
    });
    *previous = Some(current);
    result
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
            false,
            Arc::new(RwLock::new(HashMap::new())),
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
    fn cpu_counters_parse_and_calculate_busy_time() {
        let first =
            parse_cpu_counters("cpu  100 20 30 400 50 0 0 0 90 10\ncpu0 1 2 3 4 5 0 0 0 0 0")
                .unwrap();
        let second = parse_cpu_counters("cpu  150 20 50 420 60 0 0 0 0 0").unwrap();
        assert_eq!(first, (600, 450));
        assert_eq!(second, (700, 480));
        let total = second.0 - first.0;
        let idle = second.1 - first.1;
        assert!((total - idle) as f64 / total as f64 * 100.0 > 0.0);
    }

    #[test]
    fn load_average_reads_first_value() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("loadavg");
        std::fs::write(&file, "0.42 0.31 0.20 1/100 1234\n").unwrap();
        assert_eq!(read_load_average_1m(file.to_str()), Some(0.42));
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
