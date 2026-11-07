//! Hardware renderer using the upstream FFmpeg + DRM/KMS pipeline.
//! Adapts the fork's panels schema and optionally displays one camera fullscreen.

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
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    panels: HashMap<String, NativeDisplay<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    active_panel: Option<String>,
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

    let (feeds, displays, panels, active_panel) =
        if let Ok(camera) = std::env::var("VIEWPIE_FULLSCREEN") {
            let source = cfg
                .feeds
                .get(&camera)
                .ok_or_else(|| format!("fullscreen camera {camera:?} does not exist"))?;
            let url = source
                .fullscreen_url
                .clone()
                .ok_or_else(|| format!("camera {camera:?} has no fullscreen_url"))?;
            let feeds = HashMap::from([(
                camera.clone(),
                FeedConfig {
                    url,
                    fullscreen_url: None,
                },
            )]);
            let viewports = vec![ViewportConfig {
                slot: "fullscreen".to_string(),
                feeds: vec![camera],
            }];
            let display = NativeDisplay {
                spacing: 0,
                background: &cfg.display.background,
                layout: "fullscreen".to_string(),
                viewports: &viewports,
            };
            // Keep the fullscreen viewport alive until serialization below.
            let native_cfg = NativeConfig {
                rotation_interval: cfg.display.rotation_interval,
                spacing: cfg.display.spacing,
                background: &cfg.display.background,
                pause_off_screen: cfg.display.pause_off_screen,
                feeds: &feeds,
                displays: vec![display],
                panels: HashMap::new(),
                active_panel: None,
            };
            let yaml = serde_yaml::to_string(&native_cfg)?;
            eprintln!(
                "viewpie-native: panel={panel_id:?}, fullscreen={:?}; starting upstream FFmpeg/DRM renderer",
                std::env::var("VIEWPIE_FULLSCREEN").ok()
            );
            viewpie::native::run_from_config_text(&yaml).map_err(anyhow::Error::msg)?;
            return Ok(());
        } else {
            let feeds = cfg.feeds.clone();
            let panels = cfg
                .panels
                .iter()
                .map(|(id, panel)| {
                    Ok((
                        id.clone(),
                        NativeDisplay {
                            spacing: panel.spacing.unwrap_or(cfg.display.spacing),
                            background: &cfg.display.background,
                            layout: square_layout(panel)?,
                            viewports: &panel.viewports,
                        },
                    ))
                })
                .collect::<Result<HashMap<_, _>, Box<dyn std::error::Error>>>()?;
            (feeds, Vec::new(), panels, Some(panel_id.clone()))
        };

    let native_cfg = NativeConfig {
        rotation_interval: cfg.display.rotation_interval,
        spacing: cfg.display.spacing,
        background: &cfg.display.background,
        pause_off_screen: cfg.display.pause_off_screen,
        feeds: &feeds,
        displays,
        panels,
        active_panel,
    };
    let yaml = serde_yaml::to_string(&native_cfg)?;
    eprintln!(
        "viewpie-native: panel={panel_id:?}, fullscreen={:?}; starting upstream FFmpeg/DRM renderer",
        std::env::var("VIEWPIE_FULLSCREEN").ok()
    );
    viewpie::native::run_from_config_text(&yaml).map_err(anyhow::Error::msg)?;
    Ok(())
}

fn square_layout(panel: &PanelConfig) -> Result<String, Box<dyn std::error::Error>> {
    let mut rows = panel.grid();
    if rows.is_empty() || rows.iter().any(Vec::is_empty) {
        return Err("panel layout is empty".into());
    }
    let side = rows.len().max(rows.iter().map(Vec::len).max().unwrap_or(0));
    if side > 8 {
        return Err(format!("panel grid is {side} cells wide/high; maximum supported is 8").into());
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

#[cfg(test)]
mod tests {
    use super::*;
    use viewpie::config::ViewportConfig;

    fn panel(layout: &str) -> PanelConfig {
        PanelConfig {
            name: "test".into(),
            spacing: None,
            layout: layout.into(),
            viewports: Vec::<ViewportConfig>::new(),
        }
    }

    #[test]
    fn square_layout_keeps_single_cell() {
        assert_eq!(square_layout(&panel("a")).unwrap(), "a");
    }

    #[test]
    fn square_layout_keeps_two_by_two() {
        assert_eq!(square_layout(&panel("a b\nc d")).unwrap(), "a b\nc d");
    }

    #[test]
    fn square_layout_pads_two_by_four_to_four_by_four() {
        assert_eq!(
            square_layout(&panel("a b c d\ne f g h")).unwrap(),
            "a b c d\ne f g h\n. . . .\n. . . ."
        );
    }

    #[test]
    fn square_layout_rejects_more_than_eight_cells() {
        let layout = "a b c d e f g h i";
        assert!(square_layout(&panel(layout)).is_err());
    }
}
