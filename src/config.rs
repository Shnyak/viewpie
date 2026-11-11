// This file contains modifications generated with assistance from Qwen AI
// Original project: https://github.com/snoack/viewpie
// SPDX-License-Identifier: GPL-3.0-or-later

//! YAML configuration for viewpie (fork with panels & dynamic settings).
//!
//! The configuration format is a superset of the original `viewpie` format:
//!
//! ```yaml
//! display:
//!   spacing: 1
//!   background: "#000000"
//!   pause_off_screen: true
//!   fullscreen_timeout: 600
//!   hardware_decoder_limit: 6
//!
//! feeds:
//!   corridor:
//!     url: rtsp://10.0.0.5/corridor_sub
//!     fullscreen_url: rtsp://10.0.0.5/corridor_main
//!
//! panels:
//!   outside:
//!     name: "Улица"
//!     spacing: 2
//!     layout: |
//!       yard yard gate pool
//!       yard yard street .
//!     viewports:
//!       - slot: yard
//!         feeds: [yard]
//!
//! defaults:
//!   panel: outside
//!   rotation_interval: 20
//! ```

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Errors produced while loading or validating configuration.
#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("failed to read config file {path}: {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
    #[error("failed to parse YAML config: {0}")]
    Yaml(#[from] serde_yaml::Error),
    #[error("validation failed: {0}")]
    Validation(String),
}

/// Requested decoder policy for one camera.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DecoderPreference {
    Hardware,
    Software,
    #[default]
    Auto,
}

/// A single RTSP feed (camera) definition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeedConfig {
    /// Sub-stream URL used in grid view (low resolution).
    pub url: String,
    /// Main-stream URL used in fullscreen view (high resolution).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fullscreen_url: Option<String>,
    /// Preferred decode path. Auto fills unused hardware slots after explicit preferences.
    #[serde(default)]
    pub decoder: DecoderPreference,
}

/// Display-wide settings that can be changed at runtime via the HTTP API.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DisplaySettings {
    /// Grid separator thickness in pixels (0-20).
    pub spacing: u32,
    /// Maximum simultaneous hardware-decoded grid streams (0-16).
    pub hardware_decoder_limit: u32,
    /// Automatically pause decoding when the screen is switched off (CEC/KMS).
    pub pause_off_screen: bool,
    /// Render readable per-stream diagnostics over the camera wall.
    pub debug_overlay: bool,
    /// Background color, `#RRGGBB`.
    pub background: String,
    /// Seconds between fullscreen rotations inside a viewport.
    pub rotation_interval: u64,
    /// Seconds after which fullscreen automatically returns to grid.
    pub fullscreen_timeout: u64,
}

impl Default for DisplaySettings {
    fn default() -> Self {
        Self {
            spacing: 1,
            hardware_decoder_limit: 6,
            pause_off_screen: true,
            debug_overlay: false,
            background: "#000000".to_string(),
            rotation_interval: 20,
            fullscreen_timeout: 600,
        }
    }
}

impl DisplaySettings {
    /// Validate value ranges; returns the first error found.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.hardware_decoder_limit > 16 {
            return Err(ConfigError::Validation(
                "hardware_decoder_limit must be between 0 and 16".into(),
            ));
        }
        if self.spacing > 20 {
            return Err(ConfigError::Validation(
                "spacing must be between 0 and 20".into(),
            ));
        }
        if self.rotation_interval < 5 || self.rotation_interval > 3600 {
            return Err(ConfigError::Validation(
                "rotation_interval must be between 5 and 3600 seconds".into(),
            ));
        }
        if self.fullscreen_timeout < 10 || self.fullscreen_timeout > 3600 {
            return Err(ConfigError::Validation(
                "fullscreen_timeout must be between 10 and 3600 seconds".into(),
            ));
        }
        check_hex_color(&self.background)?;
        Ok(())
    }
}

fn check_hex_color(c: &str) -> Result<(), ConfigError> {
    let ok = c.len() == 7 && c.starts_with('#') && c[1..].chars().all(|ch| ch.is_ascii_hexdigit());
    if ok {
        Ok(())
    } else {
        Err(ConfigError::Validation(format!(
            "background must be a #RRGGBB hex color, got {c:?}"
        )))
    }
}

/// One viewport (cell region) of a panel, bound to one or more feeds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewportConfig {
    /// Layout slot identifier (a token appearing in `layout`).
    pub slot: String,
    /// Feeds shown in this viewport (rotated by `rotation_interval`).
    pub feeds: Vec<String>,
}

/// A panel (scene): a named camera wall layout.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PanelConfig {
    /// Human readable name.
    #[serde(default)]
    pub name: String,
    /// Optional per-panel spacing override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spacing: Option<u32>,
    /// Text grid: whitespace-separated slot tokens, `.` marks empty cells.
    #[serde(default)]
    pub layout: String,
    /// Viewports referenced by the layout.
    #[serde(default)]
    pub viewports: Vec<ViewportConfig>,
}

impl PanelConfig {
    /// Parse `layout` into rows of tokens.
    pub fn grid(&self) -> Vec<Vec<String>> {
        self.layout
            .lines()
            .map(|line| line.split_whitespace().map(|s| s.to_string()).collect())
            .filter(|row: &Vec<String>| !row.is_empty())
            .collect()
    }

    /// All distinct feed names used by this panel.
    pub fn feed_names(&self) -> Vec<String> {
        let mut seen: Vec<String> = Vec::new();
        for vp in &self.viewports {
            for f in &vp.feeds {
                if !seen.contains(f) {
                    seen.push(f.clone());
                }
            }
        }
        seen.sort();
        seen
    }

    /// Number of cameras (feeds) in the panel.
    pub fn camera_count(&self) -> usize {
        self.feed_names().len()
    }

    /// Returns true if the given camera is used by this panel.
    pub fn has_camera(&self, camera: &str) -> bool {
        self.feed_names().iter().any(|f| f == camera)
    }

    /// Validate the panel: grid 1x1..8x8, all slots defined,
    /// every viewport slot referenced in the layout.
    /// Unique panel id when embedded in a JSON request body (YAML uses the map key).
    pub fn id_hint(&self) -> Option<&str> {
        None
    }

    pub fn validate(&self, known_feeds: &[String]) -> Result<(), ConfigError> {
        let grid = self.grid();
        let rows = grid.len();
        if !(1..=8).contains(&rows) {
            return Err(ConfigError::Validation(format!(
                "panel layout must have 1-8 rows, got {rows}"
            )));
        }
        let cols = grid[0].len();
        for row in &grid {
            if row.len() != cols {
                return Err(ConfigError::Validation(
                    "panel layout rows must all have the same width".into(),
                ));
            }
        }
        if !(1..=8).contains(&cols) {
            return Err(ConfigError::Validation(format!(
                "panel layout must have 1-8 columns, got {cols}"
            )));
        }
        let mut slots: HashMap<&str, &ViewportConfig> = HashMap::new();
        for vp in &self.viewports {
            if slots.insert(vp.slot.as_str(), vp).is_some() {
                return Err(ConfigError::Validation(format!(
                    "duplicate viewport slot {:?}",
                    vp.slot
                )));
            }
        }
        // Every non-empty token in the layout must reference a defined slot.
        for row in &grid {
            for tok in row {
                if tok == "." {
                    continue;
                }
                if !slots.contains_key(tok.as_str()) {
                    return Err(ConfigError::Validation(format!(
                        "layout slot {tok:?} has no viewport definition"
                    )));
                }
            }
        }
        // Every defined slot should appear in the layout.
        for vp in &self.viewports {
            let present = grid.iter().flatten().any(|t| *t == vp.slot);
            if !present {
                return Err(ConfigError::Validation(format!(
                    "viewport slot {:?} does not appear in layout",
                    vp.slot
                )));
            }
        }
        // All feeds must exist.
        for vp in &self.viewports {
            if vp.feeds.is_empty() {
                return Err(ConfigError::Validation(format!(
                    "viewport {:?} has no feeds",
                    vp.slot
                )));
            }
            for f in &vp.feeds {
                if !known_feeds.contains(f) {
                    return Err(ConfigError::Validation(format!(
                        "viewport {:?} references unknown feed {f:?}",
                        vp.slot
                    )));
                }
            }
        }
        Ok(())
    }
}

/// Defaults section.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct DefaultsConfig {
    /// Panel activated on startup / resume (legacy YAML setting).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub panel: Option<String>,
    /// User-defined panel order, from first to last.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub panel_order: Vec<String>,
    /// Override for display.rotation_interval.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotation_interval: Option<u64>,
}

/// Top-level configuration document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    pub display: DisplaySettings,
    #[serde(default)]
    pub feeds: HashMap<String, FeedConfig>,
    #[serde(default)]
    pub panels: HashMap<String, PanelConfig>,
    #[serde(default)]
    pub defaults: DefaultsConfig,
}

impl Config {
    /// Load and validate configuration from a YAML string.
    pub fn from_yaml(src: &str) -> Result<Self, ConfigError> {
        let mut cfg: Config = serde_yaml::from_str(src)?;
        // defaults.rotation_interval overrides display.rotation_interval.
        if let Some(ri) = cfg.defaults.rotation_interval {
            cfg.display.rotation_interval = ri;
        }
        cfg.validate()?;
        Ok(cfg)
    }

    /// Load configuration from a file path.
    pub fn load(path: &str) -> Result<Self, ConfigError> {
        let src = std::fs::read_to_string(path).map_err(|e| ConfigError::Io {
            path: path.to_string(),
            source: e,
        })?;
        Self::from_yaml(&src)
    }

    /// Persist this validated configuration without exposing a partially written YAML file.
    ///
    /// The temporary file is created beside the destination so rename is atomic
    /// on the same filesystem. It is private because feed URLs may contain credentials.
    pub fn save_atomic(&self, path: &str) -> Result<(), ConfigError> {
        self.validate()?;
        let yaml = serde_yaml::to_string(self)?;
        let destination = Path::new(path);
        let parent = destination
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let file_name = destination
            .file_name()
            .ok_or_else(|| ConfigError::Validation(format!("invalid config path: {path}")))?
            .to_string_lossy();
        static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
        let sequence = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let temporary: PathBuf = parent.join(format!(
            ".{file_name}.{}.{}.tmp",
            std::process::id(),
            sequence
        ));

        let write_result = (|| -> std::io::Result<()> {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&temporary)?;
            file.write_all(yaml.as_bytes())?;
            file.sync_all()?;
            drop(file);
            fs::rename(&temporary, destination)?;
            // Persist the directory entry as well as the file contents.
            let dir = fs::File::open(parent)?;
            dir.sync_all()?;
            Ok(())
        })();

        if let Err(source) = write_result {
            let _ = fs::remove_file(&temporary);
            return Err(ConfigError::Io {
                path: path.to_string(),
                source,
            });
        }
        Ok(())
    }

    /// Validate cross-references between panels and feeds.
    pub fn validate(&self) -> Result<(), ConfigError> {
        let explicit_hardware: Vec<&str> = self
            .feeds
            .iter()
            .filter(|(name, feed)| {
                feed.decoder == DecoderPreference::Hardware
                    && self.panels.values().any(|panel| panel.has_camera(name))
            })
            .map(|(name, _)| name.as_str())
            .collect();
        if explicit_hardware.len() > self.display.hardware_decoder_limit as usize {
            return Err(ConfigError::Validation(format!(
                "{} cameras explicitly request hardware decoding ({}), but hardware_decoder_limit is {}; increase the limit or mark some cameras software/auto",
                explicit_hardware.len(),
                explicit_hardware.join(", "),
                self.display.hardware_decoder_limit
            )));
        }
        if self.panels.is_empty() {
            return Err(ConfigError::Validation(
                "at least one panel must be defined".into(),
            ));
        }
        self.display.validate()?;
        let feed_names: Vec<String> = self.feeds.keys().cloned().collect();
        for p in self.panels.values() {
            p.validate(&feed_names)
                .map_err(|e| ConfigError::Validation(format!("panel: {e}")))?;
        }
        if let Some(def) = &self.defaults.panel {
            if !self.panels.contains_key(def) {
                return Err(ConfigError::Validation(format!(
                    "defaults.panel {def:?} does not exist"
                )));
            }
        }
        let mut ordered = std::collections::HashSet::new();
        for id in &self.defaults.panel_order {
            if !self.panels.contains_key(id) {
                return Err(ConfigError::Validation(format!(
                    "defaults.panel_order references unknown panel {id:?}"
                )));
            }
            if !ordered.insert(id) {
                return Err(ConfigError::Validation(format!(
                    "defaults.panel_order contains duplicate panel {id:?}"
                )));
            }
        }
        Ok(())
    }

    /// Return panel ids in configured order, appending any legacy/unlisted ids.
    pub fn ordered_panel_ids(&self) -> Vec<String> {
        let mut ids = Vec::with_capacity(self.panels.len());
        for id in &self.defaults.panel_order {
            if self.panels.contains_key(id) && !ids.contains(id) {
                ids.push(id.clone());
            }
        }
        let mut remaining: Vec<String> = self.panels.keys()
            .filter(|id| !ids.contains(id))
            .cloned()
            .collect();
        remaining.sort();
        ids.extend(remaining);
        ids
    }

    /// Determine the initially active panel.
    pub fn initial_panel(&self) -> String {
        if let Some(p) = &self.defaults.panel {
            if self.panels.contains_key(p) {
                return p.clone();
            }
        }
        let mut keys: Vec<&String> = self.panels.keys().collect();
        keys.sort();
        keys.into_iter().next().cloned().unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
display:
  spacing: 1
  pause_off_screen: true
  fullscreen_timeout: 600

feeds:
  yard:
    url: rtsp://10.0.0.5/yard_sub
    fullscreen_url: rtsp://10.0.0.5/yard_main
  gate:
    url: rtsp://10.0.0.5/gate_sub
  pool:
    url: rtsp://10.0.0.5/pool_sub
    fullscreen_url: rtsp://10.0.0.5/pool_main

panels:
  outside:
    name: "Улица"
    spacing: 2
    layout: |
      yard yard gate pool
      yard yard gate pool
      . . . .
      . . . .
    viewports:
      - slot: yard
        feeds: [yard]
      - slot: gate
        feeds: [gate]
      - slot: pool
        feeds: [pool]

defaults:
  panel: outside
  rotation_interval: 20
"#;

    #[test]
    fn parses_sample_config() {
        let cfg = Config::from_yaml(SAMPLE).expect("valid sample");
        assert_eq!(cfg.display.spacing, 1);
        assert_eq!(cfg.display.background, "#000000");
        assert_eq!(cfg.feeds.len(), 3);
        assert_eq!(cfg.panels.len(), 1);
        let p = &cfg.panels["outside"];
        assert_eq!(p.name, "Улица");
        assert_eq!(p.camera_count(), 3);
        assert!(p.has_camera("yard"));
        assert!(!p.has_camera("gate2"));
        assert_eq!(cfg.initial_panel(), "outside");
    }

    #[test]
    fn rejects_unknown_feed() {
        let bad = SAMPLE.replace("[pool]", "[nonexistent]");
        let err = Config::from_yaml(&bad).unwrap_err();
        assert!(matches!(err, ConfigError::Validation(_)));
    }

    #[test]
    fn rejects_missing_slot_viewport() {
        let bad = SAMPLE.replace("- slot: pool\n        feeds: [pool]\n", "");
        let err = Config::from_yaml(&bad);
        assert!(err.is_err());
    }

    #[test]
    fn accepts_single_cell_grid() {
        let yaml = r#"
feeds:
  a: { url: "rtsp://x/a" }
panels:
  p:
    layout: |
      a
    viewports:
      - { slot: a, feeds: [a] }
"#;
        assert!(Config::from_yaml(yaml).is_ok());
    }

    #[test]
    fn accepts_two_by_two_grid() {
        let yaml = r#"
feeds:
  a: { url: "rtsp://x/a" }
panels:
  p:
    layout: |
      a b
      c d
    viewports:
      - { slot: a, feeds: [a] }
      - { slot: b, feeds: [a] }
      - { slot: c, feeds: [a] }
      - { slot: d, feeds: [a] }
"#;
        assert!(Config::from_yaml(yaml).is_ok());
    }

    #[test]
    fn accepts_rectangular_grid() {
        let yaml = r#"
feeds:
  a: { url: "rtsp://x/a" }
panels:
  p:
    layout: |
      a a a
      a a a
    viewports:
      - { slot: a, feeds: [a] }
"#;
        assert!(Config::from_yaml(yaml).is_ok());
    }

    #[test]
    fn rejects_grid_larger_than_eight() {
        let yaml = r#"
feeds:
  a: { url: "rtsp://x/a" }
panels:
  p:
    layout: |
      a a a a a a a a a
      a a a a a a a a a
      a a a a a a a a a
      a a a a a a a a a
      a a a a a a a a a
      a a a a a a a a a
      a a a a a a a a a
      a a a a a a a a a
      a a a a a a a a a
    viewports:
      - { slot: a, feeds: [a] }
"#;
        assert!(Config::from_yaml(yaml).is_err());
    }

    #[test]
    fn decoder_preferences_default_to_auto_and_hardware_budget_defaults_to_six() {
        let yaml = r#"
feeds:
  a: {url: "rtsp://x/a"}
panels:
  p:
    layout: |
      a
    viewports:
      - {slot: a, feeds: [a]}
"#;
        let cfg = Config::from_yaml(yaml).unwrap();
        assert_eq!(cfg.display.hardware_decoder_limit, 6);
        assert_eq!(cfg.feeds["a"].decoder, DecoderPreference::Auto);
    }

    #[test]
    fn explicit_hardware_preferences_cannot_exceed_budget() {
        let yaml = r#"
display:
  hardware_decoder_limit: 1
feeds:
  a: {url: "rtsp://x/a", decoder: hardware}
  b: {url: "rtsp://x/b", decoder: hardware}
panels:
  p:
    layout: |
      a b
    viewports:
      - {slot: a, feeds: [a]}
      - {slot: b, feeds: [b]}
"#;
        let error = Config::from_yaml(yaml).unwrap_err().to_string();
        assert!(error.contains("2 cameras explicitly request hardware decoding"));
    }

    #[test]
    fn unused_hardware_camera_does_not_consume_budget() {
        let yaml = r#"
display:
  hardware_decoder_limit: 0
feeds:
  a: {url: "rtsp://x/a"}
  spare: {url: "rtsp://x/spare", decoder: hardware}
panels:
  p:
    layout: |
      a
    viewports:
      - {slot: a, feeds: [a]}
"#;
        assert!(Config::from_yaml(yaml).is_ok());
    }

    #[test]
    fn settings_validation() {
        let mut s = DisplaySettings::default();
        assert!(s.validate().is_ok());
        s.spacing = 21;
        assert!(s.validate().is_err());
        s.spacing = 5;
        s.background = "black".into();
        assert!(s.validate().is_err());
        s.background = "#ff00aa".into();
        s.fullscreen_timeout = 1;
        assert!(s.validate().is_err());
    }

    #[test]
    fn defaults_fallback_when_no_display_section() {
        let minimal = r#"
feeds:
  a: { url: "rtsp://x/a" }
panels:
  p:
    layout: |
      a a a
      a a a
      a a a
    viewports:
      - { slot: a, feeds: [a] }
"#;
        let cfg = Config::from_yaml(minimal).unwrap();
        assert_eq!(cfg.display, DisplaySettings::default());
        assert_eq!(cfg.initial_panel(), "p");
    }

    #[test]
    fn atomic_save_round_trips_and_keeps_credentials_private() {
        let cfg = Config::from_yaml(SAMPLE).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("viewpie.yaml");
        cfg.save_atomic(path.to_str().unwrap()).unwrap();

        let loaded = Config::load(path.to_str().unwrap()).unwrap();
        assert_eq!(loaded, cfg);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[test]
    fn atomic_save_rejects_invalid_config_without_replacing_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("viewpie.yaml");
        std::fs::write(&path, "keep this file").unwrap();

        let mut cfg = Config::from_yaml(SAMPLE).unwrap();
        cfg.panels.clear();
        assert!(cfg.save_atomic(path.to_str().unwrap()).is_err());
        assert_eq!(std::fs::read_to_string(path).unwrap(), "keep this file");
    }

    #[test]
    fn atomic_save_does_not_leave_temporary_file_on_failure() {
        let dir = tempfile::tempdir().unwrap();
        let missing_parent = dir.path().join("missing").join("viewpie.yaml");
        let cfg = Config::from_yaml(SAMPLE).unwrap();
        assert!(cfg.save_atomic(missing_parent.to_str().unwrap()).is_err());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }
}
