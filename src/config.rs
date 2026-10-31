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

static CONFIG_TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

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

/// A single RTSP feed (camera) definition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeedConfig {
    /// Sub-stream URL used in grid view (low resolution).
    pub url: String,
    /// Main-stream URL used in fullscreen view (high resolution).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fullscreen_url: Option<String>,
}

/// Display-wide settings that can be changed at runtime via the HTTP API.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DisplaySettings {
    /// Grid separator thickness in pixels (0-20).
    pub spacing: u32,
    /// Automatically pause decoding when the screen is switched off (CEC/KMS).
    pub pause_off_screen: bool,
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
            pause_off_screen: true,
            background: "#000000".to_string(),
            rotation_interval: 20,
            fullscreen_timeout: 600,
        }
    }
}

impl DisplaySettings {
    /// Validate value ranges; returns the first error found.
    pub fn validate(&self) -> Result<(), ConfigError> {
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

    /// Validate the panel: square grid 1x1..8x8, all slots defined,
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
        let Some(first_row) = grid.first() else {
            return Err(ConfigError::Validation(
                "panel layout must contain at least one cell".into(),
            ));
        };
        let cols = first_row.len();
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
        if rows != cols {
            return Err(ConfigError::Validation(format!(
                "panel layout must be square, got {rows}x{cols}"
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
        // Every defined slot should appear as one solid rectangular region.
        // Repeating a token across adjacent cells makes one enlarged viewport;
        // "." remains an unassigned placeholder and is intentionally unrestricted.
        for vp in &self.viewports {
            let mut min_row = usize::MAX;
            let mut min_col = usize::MAX;
            let mut max_row = 0usize;
            let mut max_col = 0usize;
            let mut count = 0usize;

            for (row_idx, row) in grid.iter().enumerate() {
                for (col_idx, token) in row.iter().enumerate() {
                    if token == &vp.slot {
                        min_row = min_row.min(row_idx);
                        min_col = min_col.min(col_idx);
                        max_row = max_row.max(row_idx);
                        max_col = max_col.max(col_idx);
                        count += 1;
                    }
                }
            }

            if count == 0 {
                return Err(ConfigError::Validation(format!(
                    "viewport slot {:?} does not appear in layout",
                    vp.slot
                )));
            }

            let rectangle_area = (max_row - min_row + 1) * (max_col - min_col + 1);
            if count != rectangle_area {
                return Err(ConfigError::Validation(format!(
                    "viewport slot {:?} must occupy one contiguous rectangle",
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
    /// Panel activated on startup / resume.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub panel: Option<String>,
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

    /// Validate cross-references between panels and feeds.
    pub fn validate(&self) -> Result<(), ConfigError> {
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
        Ok(())
    }

    /// Atomically persist this configuration as YAML.
    ///
    /// Validation happens before touching the destination. The temporary file is
    /// created beside the destination so rename stays on the same filesystem.
    pub fn save_atomic(&self, path: &str) -> Result<(), ConfigError> {
        self.validate()?;

        let destination = Path::new(path);
        let parent = destination
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let file_name = destination
            .file_name()
            .ok_or_else(|| ConfigError::Validation("config path has no filename".into()))?;
        let yaml = serde_yaml::to_string(self)?;

        let mut temp_path = PathBuf::new();
        let mut temp_file = None;
        for _ in 0..32 {
            let sequence = CONFIG_TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
            let mut temp_name = file_name.to_os_string();
            temp_name.push(format!(".{}.{}.tmp", std::process::id(), sequence));
            let candidate = parent.join(temp_name);
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&candidate)
            {
                Ok(file) => {
                    temp_path = candidate;
                    temp_file = Some(file);
                    break;
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(source) => {
                    return Err(ConfigError::Io {
                        path: candidate.display().to_string(),
                        source,
                    });
                }
            }
        }

        let mut file = temp_file.ok_or_else(|| {
            ConfigError::Validation("could not allocate a unique temporary config file".into())
        })?;
        let write_result = (|| -> std::io::Result<()> {
            file.write_all(yaml.as_bytes())?;
            file.sync_all()
        })();
        drop(file);
        if let Err(source) = write_result {
            let _ = fs::remove_file(&temp_path);
            return Err(ConfigError::Io {
                path: temp_path.display().to_string(),
                source,
            });
        }

        if let Err(source) = fs::rename(&temp_path, destination) {
            let _ = fs::remove_file(&temp_path);
            return Err(ConfigError::Io {
                path: destination.display().to_string(),
                source,
            });
        }

        // Persist the directory entry where supported. Some platforms/filesystems
        // do not allow opening or syncing directories, so the atomic rename remains
        // the primary guarantee and this best-effort sync is intentionally ignored.
        if let Ok(directory) = OpenOptions::new().read(true).open(parent) {
            let _ = directory.sync_all();
        }
        Ok(())
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
    fn accepts_one_by_one_and_two_by_two_grids() {
        let known_feeds = vec!["a".to_string()];
        for layout in ["a", "a a\na a"] {
            let panel = PanelConfig {
                name: "small".into(),
                spacing: None,
                layout: layout.into(),
                viewports: vec![ViewportConfig {
                    slot: "a".into(),
                    feeds: vec!["a".into()],
                }],
            };
            assert!(
                panel.validate(&known_feeds).is_ok(),
                "rejected layout: {layout}"
            );
        }
    }

    #[test]
    fn rejects_non_square_grid() {
        let invalid = r#"
feeds:
  a: { url: "rtsp://x/a" }
panels:
  p:
    layout: |
      a a a
      a a a
      a a a
      a a a
    viewports:
      - { slot: a, feeds: [a] }
"#;
        let err = Config::from_yaml(invalid).unwrap_err();
        assert!(
            matches!(err, ConfigError::Validation(message) if message.contains("must be square"))
        );
    }

    #[test]
    fn accepts_multiple_rectangular_viewports_and_empty_cells() {
        let valid = r#"
feeds:
  c1: { url: "rtsp://x/c1" }
  c2: { url: "rtsp://x/c2" }
  c3: { url: "rtsp://x/c3" }
panels:
  p:
    layout: |
      c1 c1 c2 .
      c1 c1 c3 .
      .  .  .  .
      .  .  .  .
    viewports:
      - { slot: c1, feeds: [c1] }
      - { slot: c2, feeds: [c2] }
      - { slot: c3, feeds: [c3] }
"#;
        let cfg =
            Config::from_yaml(valid).expect("rectangular regions and dot placeholders are valid");
        assert_eq!(cfg.panels["p"].camera_count(), 3);
    }

    #[test]
    fn rejects_non_rectangular_repeated_slot() {
        let invalid = r#"
feeds:
  a: { url: "rtsp://x/a" }
panels:
  p:
    layout: |
      a a .
      a . .
      . . .
    viewports:
      - { slot: a, feeds: [a] }
"#;
        let err = Config::from_yaml(invalid).unwrap_err();
        assert!(matches!(err, ConfigError::Validation(_)));
    }

    #[test]
    fn save_atomic_round_trips_valid_config() {
        let cfg = Config::from_yaml(SAMPLE).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("viewpie.yaml");
        cfg.save_atomic(path.to_str().unwrap()).unwrap();

        let loaded = Config::load(path.to_str().unwrap()).unwrap();
        assert_eq!(loaded, cfg);
        assert!(!dir.path().read_dir().unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp")
        }));
    }

    #[test]
    fn save_atomic_rejects_invalid_config_without_overwriting_file() {
        let cfg = Config::from_yaml(SAMPLE).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("viewpie.yaml");
        std::fs::write(&path, "keep existing contents").unwrap();

        let mut invalid = cfg;
        invalid.panels.clear();
        assert!(invalid.save_atomic(path.to_str().unwrap()).is_err());
        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            "keep existing contents"
        );
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
}
