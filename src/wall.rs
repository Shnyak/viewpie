// This file contains modifications generated with assistance from Qwen AI
// Original project: https://github.com/snoack/viewpie
// SPDX-License-Identifier: GPL-3.0-or-later

//! Video wall renderer: turns a panel layout + runtime state into the
//! concrete KMS frame that is committed to the display, and computes the
//! pixel geometry of every viewport (including `spacing` separators).
//!
//! All geometry functions are pure so they can be unit-tested without any
//! display hardware.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::config::PanelConfig;
use crate::kms::FrameLayout;

/// Screen resolution in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resolution {
    pub width: u32,
    pub height: u32,
}

/// Computed rectangle of one viewport on screen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewportRect {
    pub slot: String,
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// Compute per-slot rectangles for a panel grid on a `res` screen with
/// `spacing` pixel separators between cells (and around the border).
///
/// Cells may span multiple grid squares (e.g. `yard yard` merges two
/// columns); merged regions get the combined size.
pub fn compute_layout(panel: &PanelConfig, res: Resolution, spacing: u32) -> Vec<ViewportRect> {
    let grid = panel.grid();
    if grid.is_empty() {
        return vec![];
    }
    let rows = grid.len() as u32;
    let cols = grid[0].len() as u32;
    // Usable area after separators: (rows+1) horizontal gaps.
    let cell_w = ((res.width as i64 - spacing as i64 * (cols as i64 + 1)) / cols.max(1) as i64)
        .max(0) as u32;
    let cell_h = ((res.height as i64 - spacing as i64 * (rows as i64 + 1)) / rows.max(1) as i64)
        .max(0) as u32;

    // Find bounding box of each slot token (handles merged spans).
    let mut spans: HashMap<&str, (u32, u32, u32, u32)> = HashMap::new(); // min_col,min_row,max_col,max_row
    for (r, row) in grid.iter().enumerate() {
        for (c, tok) in row.iter().enumerate() {
            if tok == "." {
                continue;
            }
            let e = spans
                .entry(tok.as_str())
                .or_insert((c as u32, r as u32, c as u32, r as u32));
            e.0 = e.0.min(c as u32);
            e.1 = e.1.min(r as u32);
            e.2 = e.2.max(c as u32);
            e.3 = e.3.max(r as u32);
        }
    }
    let mut out: Vec<ViewportRect> = spans
        .into_iter()
        .map(|(slot, (c0, r0, c1, r1))| {
            let x = spacing + c0 * (cell_w + spacing);
            let y = spacing + r0 * (cell_h + spacing);
            let w = (c1 - c0 + 1) * cell_w + (c1 - c0) * spacing;
            let h = (r1 - r0 + 1) * cell_h + (r1 - r0) * spacing;
            ViewportRect {
                slot: slot.to_string(),
                x,
                y,
                width: w,
                height: h,
            }
        })
        .collect();
    out.sort_by(|a, b| a.slot.cmp(&b.slot));
    out
}

/// Build the [`FrameLayout`] to commit for a grid view of `panel`.
pub fn grid_frame(
    panel: &PanelConfig,
    spacing: u32,
    background: &str,
    active_feed_per_slot: &HashMap<String, String>,
) -> FrameLayout {
    let mut planes: Vec<String> = Vec::new();
    for vp in &panel.viewports {
        if let Some(feed) = active_feed_per_slot.get(&vp.slot) {
            planes.push(feed.clone());
        }
    }
    planes.sort();
    planes.dedup();
    FrameLayout {
        spacing,
        background: background.to_string(),
        planes,
        fullscreen: false,
    }
}

/// Build the [`FrameLayout`] for a single fullscreen camera.
pub fn fullscreen_frame(camera: &str, background: &str, spacing: u32) -> FrameLayout {
    FrameLayout {
        spacing,
        background: background.to_string(),
        planes: vec![camera.to_string()],
        fullscreen: true,
    }
}

/// Render the separator grid into an RGB framebuffer (software path used
/// when the composed background/spacing plane must be refreshed, e.g.
/// immediately after a `spacing` change — no decoder reconnect needed).
///
/// Returns `width*height*3` bytes.
pub fn render_background_framebuffer(
    rects: &[ViewportRect],
    res: Resolution,
    spacing: u32,
    background_rgb: (u8, u8, u8),
) -> Vec<u8> {
    let _ = spacing; // separators are simply "not covered by any viewport"
    let mut buf = vec![0u8; (res.width * res.height * 3) as usize];
    for y in 0..res.height {
        for x in 0..res.width {
            let inside = rects
                .iter()
                .any(|r| x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height);
            let idx = ((y * res.width + x) * 3) as usize;
            if inside {
                // viewport area: dark grey placeholder (planes overwrite on HW)
                buf[idx] = 0x10;
                buf[idx + 1] = 0x10;
                buf[idx + 2] = 0x10;
            } else {
                buf[idx] = background_rgb.0;
                buf[idx + 1] = background_rgb.1;
                buf[idx + 2] = background_rgb.2;
            }
        }
    }
    buf
}

/// Parse `#RRGGBB` into a tuple. Falls back to black.
pub fn parse_hex_color(c: &str) -> (u8, u8, u8) {
    let s = c.strip_prefix('#').unwrap_or(c);
    if s.len() != 6 || !s.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return (0, 0, 0);
    }
    (
        u8::from_str_radix(&s[0..2], 16).unwrap_or(0),
        u8::from_str_radix(&s[2..4], 16).unwrap_or(0),
        u8::from_str_radix(&s[4..6], 16).unwrap_or(0),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{PanelConfig, ViewportConfig};

    fn panel() -> PanelConfig {
        PanelConfig {
            name: "test".into(),
            spacing: None,
            layout: "a a b\na a b\nc . .\n".into(),
            viewports: vec![
                ViewportConfig {
                    slot: "a".into(),
                    feeds: vec!["fa".into()],
                },
                ViewportConfig {
                    slot: "b".into(),
                    feeds: vec!["fb".into()],
                },
                ViewportConfig {
                    slot: "c".into(),
                    feeds: vec!["fc".into()],
                },
            ],
        }
    }

    #[test]
    fn merged_slots_get_combined_rect() {
        let res = Resolution {
            width: 1000,
            height: 700,
        };
        let rects = compute_layout(&panel(), res, 0);
        let a = rects.iter().find(|r| r.slot == "a").unwrap();
        // 'a' spans 2 cols × 2 rows of a 3×3 grid → 666×466 (± rounding)
        assert!(a.width > 600 && a.width <= 667, "a.w = {}", a.width);
        assert!(a.height > 400 && a.height <= 467, "a.h = {}", a.height);
        let b = rects.iter().find(|r| r.slot == "b").unwrap();
        // 'b' occupies column index 2 → x = spacing + 2*(cell_w + spacing);
        // with spacing = 0 this is simply 2 * cell_w. Note: a.width is the
        // merged span (2 cells), so compare against the single-cell width.
        assert_eq!(b.x, a.width / 2 * 2);
        assert_eq!(a.width % 2, 0); // 666 for 1000/3 rounding
    }

    #[test]
    fn spacing_shrinks_cells_evenly() {
        let res = Resolution {
            width: 1004,
            height: 1004,
        };
        let p = PanelConfig {
            name: "q".into(),
            spacing: None,
            layout: "x x\nx x\n".into(),
            viewports: vec![ViewportConfig {
                slot: "x".into(),
                feeds: vec!["f".into()],
            }],
        };
        // validate not required here; compute directly
        let rects = compute_layout(&p, res, 2);
        let x = &rects[0];
        // usable = 1004 - 3*2 = 998 → cell 499 wide; merged 2 cells: 499*2+2=1000
        assert_eq!(x.width, 1000);
        assert_eq!(x.x, 2);
    }

    #[test]
    fn zero_spacing_ok() {
        let res = Resolution {
            width: 900,
            height: 600,
        };
        let rects = compute_layout(&panel(), res, 0);
        assert_eq!(rects.len(), 3);
        let c = rects.iter().find(|r| r.slot == "c").unwrap();
        assert_eq!(c.x, 0);
        assert_eq!(c.y, 2 * (600 / 3));
    }

    #[test]
    fn grid_frame_lists_unique_feeds() {
        let mut active = HashMap::new();
        active.insert("a".to_string(), "fa".to_string());
        active.insert("b".to_string(), "fb".to_string());
        active.insert("c".to_string(), "fa".to_string()); // same feed twice
        let f = grid_frame(&panel(), 1, "#000000", &active);
        assert_eq!(f.planes, vec!["fa".to_string(), "fb".to_string()]);
        assert_eq!(f.spacing, 1);
        assert!(!f.fullscreen);
    }

    #[test]
    fn fullscreen_frame_single_plane() {
        let f = fullscreen_frame("pool", "#0000ff", 0);
        assert_eq!(f.planes, vec!["pool"]);
        assert!(f.fullscreen);
    }

    #[test]
    fn hex_color_parse() {
        assert_eq!(parse_hex_color("#ff8000"), (255, 128, 0));
        assert_eq!(parse_hex_color("bogus"), (0, 0, 0));
        assert_eq!(parse_hex_color("#000000"), (0, 0, 0));
    }

    #[test]
    fn background_fb_size_and_color() {
        let res = Resolution {
            width: 4,
            height: 2,
        };
        let rects = vec![ViewportRect {
            slot: "a".into(),
            x: 0,
            y: 0,
            width: 2,
            height: 2,
        }];
        let buf = render_background_framebuffer(&rects, res, 1, (255, 0, 0));
        assert_eq!(buf.len(), 4 * 2 * 3);
        // pixel (3,0) outside viewport → red background
        assert_eq!(&buf[(3 * 3)..(3 * 3 + 3)], &[255, 0, 0]);
        // pixel (0,0) inside → placeholder grey
        assert_eq!(&buf[0..3], &[0x10, 0x10, 0x10]);
    }
}
