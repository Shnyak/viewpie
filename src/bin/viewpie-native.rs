//! Standalone hardware renderer using the upstream FFmpeg + DRM/KMS pipeline.
//! The selected fork panel is adapted to the upstream display schema.

#[path = "../native/mod.rs"]
mod native;

use serde::Serialize;
use std::collections::HashMap;
use viewpie::config::{Config, FeedConfig, PanelConfig, ViewportConfig};

#[derive(Serialize)]
struct NativeConfig<'a> {
    rotation_interval: u64,
    spacing: u32,
    background: &'a str,
    pause_off_screen: bool,
    feeds: &'a HashMap<String, FeedConfig>,
    displays: Vec<NativeDisplay<'a>>,
}

#[derive(Serialize)]
struct NativeDisplay<'a> {
    spacing: u32,
    background: &'a str,
    layout: String,
    viewports: &'a [ViewportConfig],
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| viewpie::DEFAULT_CONFIG_PATH.to_string());
    let cfg = Config::load(&path)?;
    let panel_id = std::env::var("VIEWPIE_PANEL").unwrap_or_else(|_| cfg.initial_panel());
    let panel = cfg
        .panels
        .get(&panel_id)
        .ok_or_else(|| format!("panel {panel_id:?} does not exist"))?;
    let layout = square_layout(panel)?;
    // The upstream parser rejects feeds that are not shown by this display,
    // so include only feeds referenced by the selected panel.
    let feeds: HashMap<String, FeedConfig> = panel
        .feed_names()
        .into_iter()
        .filter_map(|name| cfg.feeds.get(&name).cloned().map(|feed| (name, feed)))
        .collect();
    let native_cfg = NativeConfig {
        rotation_interval: cfg.display.rotation_interval,
        spacing: cfg.display.spacing,
        background: &cfg.display.background,
        pause_off_screen: cfg.display.pause_off_screen,
        feeds: &feeds,
        displays: vec![NativeDisplay {
            spacing: panel.spacing.unwrap_or(cfg.display.spacing),
            background: &cfg.display.background,
            layout,
            viewports: &panel.viewports,
        }],
    };
    let yaml = serde_yaml::to_string(&native_cfg)?;
    eprintln!(
        "viewpie-native: using panel {panel_id:?} from {path}; starting upstream FFmpeg/DRM renderer"
    );
    native::run_from_config_text(&yaml)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    Ok(())
}

fn square_layout(panel: &PanelConfig) -> Result<String, Box<dyn std::error::Error>> {
    let mut rows = panel.grid();
    if rows.is_empty() || rows.iter().any(Vec::is_empty) {
        return Err("panel layout is empty".into());
    }
    let side = rows
        .len()
        .max(rows.iter().map(Vec::len).max().unwrap_or(0));
    if side > 8 {
        return Err(format!(
            "panel grid is {side} cells wide/high; maximum supported is 8"
        )
        .into());
    }
    for row in &mut rows {
        row.resize(side, ".".to_string());
    }
    while rows.len() < side {
        rows.push(vec![".".to_string(); side]);
    }
    Ok(rows
        .into_iter()
        .map(|r| r.join(" "))
        .collect::<Vec<_>>()
        .join("\n"))
}
