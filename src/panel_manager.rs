// This file contains modifications generated with assistance from Qwen AI
// Original project: https://github.com/snoack/viewpie
// SPDX-License-Identifier: GPL-3.0-or-later

//! Panel manager: owns the panel registry, the active panel and performs
//! **graceful, reference-counted panel switches**.
//!
//! The switch algorithm follows the fork specification:
//!
//! 1. diff feed sets of current vs. new panel,
//! 2. decrement ref_count for leaving feeds (may stop decoders),
//! 3. increment ref_count for entering feeds (may start decoders),
//! 4. shared feeds keep their running decoder untouched,
//! 5. reset fullscreen state,
//! 6. atomic KMS commit of the new layout,
//! 7. publish the new active panel id.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::config::{PanelConfig, ViewportConfig};
use crate::feed::FeedPool;
use crate::kms::{DisplayBackend, DisplayError};
use crate::wall::{self, Resolution};

/// Errors produced by panel operations.
#[derive(Debug, thiserror::Error)]
pub enum PanelError {
    #[error("panel {0:?} not found")]
    NotFound(String),
    #[error("panel id {0:?} already exists")]
    AlreadyExists(String),
    #[error("cannot delete the last remaining panel")]
    LastPanel,
    #[error("panel validation failed: {0}")]
    Validation(String),
    #[error(transparent)]
    Feed(#[from] crate::feed::PoolError),
    #[error(transparent)]
    Display(#[from] DisplayError),
}

/// Registry + runtime for panels.
pub struct PanelManager {
    panels: RwLock<HashMap<String, PanelConfig>>,
    active: RwLock<String>,
    /// `Some(id)` once a panel layout has actually been committed to the
    /// display and its feeds hold references. `None` right after startup —
    /// the first `switch_to` then performs a full acquire pass.
    displayed: RwLock<Option<String>>,
    pool: Arc<FeedPool>,
    backend: Arc<dyn DisplayBackend>,
    resolution: Resolution,
    /// Currently displayed feed per viewport slot (for rotation support).
    slot_feed: RwLock<HashMap<String, String>>,
}

impl PanelManager {
    pub fn new(
        panels: HashMap<String, PanelConfig>,
        active: String,
        pool: Arc<FeedPool>,
        backend: Arc<dyn DisplayBackend>,
        resolution: Resolution,
    ) -> Self {
        // Pre-compute the initial slot→feed map *before* constructing the
        // locks so that `new` never needs `blocking_read/write`, which would
        // panic when called from inside a tokio runtime (e.g. in tests).
        let mut slot_map = HashMap::new();
        if let Some(p) = panels.get(&active) {
            for vp in &p.viewports {
                if let Some(f) = vp.feeds.first() {
                    slot_map.insert(vp.slot.clone(), f.clone());
                }
            }
        }
        Self {
            panels: RwLock::new(panels),
            active: RwLock::new(active),
            displayed: RwLock::new(None),
            pool,
            backend,
            resolution,
            slot_feed: RwLock::new(slot_map),
        }
    }

    /// Id of the currently active panel.
    pub async fn active_id(&self) -> String {
        self.active.read().await.clone()
    }

    /// Clone of a panel definition.
    pub async fn get_panel(&self, id: &str) -> Result<PanelConfig, PanelError> {
        self.panels
            .read()
            .await
            .get(id)
            .cloned()
            .ok_or_else(|| PanelError::NotFound(id.to_string()))
    }

    /// List all panels (id sorted).
    pub async fn list(&self) -> Vec<(String, PanelConfig)> {
        let g = self.panels.read().await;
        let mut v: Vec<(String, PanelConfig)> =
            g.iter().map(|(k, p)| (k.clone(), p.clone())).collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
    }

    /// Number of registered panels.
    pub async fn count(&self) -> usize {
        self.panels.read().await.len()
    }

    /// Which panels (ids) use the given camera?
    pub async fn panels_using_camera(&self, camera: &str) -> Vec<String> {
        let g = self.panels.read().await;
        let mut ids: Vec<String> = g
            .iter()
            .filter(|(_, p)| p.has_camera(camera))
            .map(|(id, _)| id.clone())
            .collect();
        ids.sort();
        ids
    }

    /// Add a new panel. Validates against the feed pool.
    pub async fn add_panel(&self, id: String, panel: PanelConfig) -> Result<(), PanelError> {
        let known: Vec<String> = self.pool.list().await.into_iter().map(|f| f.name).collect();
        panel
            .validate(&known)
            .map_err(|e| PanelError::Validation(e.to_string()))?;
        let mut g = self.panels.write().await;
        if g.contains_key(&id) {
            return Err(PanelError::AlreadyExists(id));
        }
        g.insert(id, panel);
        Ok(())
    }

    /// Update (or replace) a panel. If it is active, re-commit the layout.
    pub async fn update_panel(&self, id: &str, panel: PanelConfig) -> Result<(), PanelError> {
        let known: Vec<String> = self.pool.list().await.into_iter().map(|f| f.name).collect();
        panel
            .validate(&known)
            .map_err(|e| PanelError::Validation(e.to_string()))?;
        {
            let g = self.panels.read().await;
            if !g.contains_key(id) {
                return Err(PanelError::NotFound(id.to_string()));
            }
        }
        self.panels.write().await.insert(id.to_string(), panel);
        if self.active_id().await == id {
            // Active panel changed → recompute refs & commit immediately.
            let current_feeds = self.feeds_of(id).await;
            let mut slot_map = HashMap::new();
            for vp in &self.get_panel(id).await.unwrap().viewports {
                if let Some(f) = vp.feeds.first() {
                    slot_map.insert(vp.slot.clone(), f.clone());
                }
            }
            *self.slot_feed.write().await = slot_map;
            self.backend
                .commit(&self.build_grid_frame(id).await?)
                .await?;
            info!(panel = id, "updated active panel in place");
            let _ = current_feeds;
        }
        Ok(())
    }

    /// Delete a panel. Refuses to remove the last one or the active one's
    /// only alternative — when deleting the active panel, another panel is
    /// activated first.
    pub async fn delete_panel(&self, id: &str) -> Result<(), PanelError> {
        let mut g = self.panels.write().await;
        if !g.contains_key(id) {
            return Err(PanelError::NotFound(id.to_string()));
        }
        if g.len() == 1 {
            return Err(PanelError::LastPanel);
        }
        let was_active = *self.active.read().await == *id;
        g.remove(id);
        drop(g);
        if was_active {
            let fallback = {
                let g = self.panels.read().await;
                let mut keys: Vec<&String> = g.keys().collect();
                keys.sort();
                keys[0].clone()
            };
            info!(new_active = %fallback, "active panel deleted — activating fallback");
            self.switch_to(&fallback).await?;
        }
        Ok(())
    }

    /// Feeds used by a panel.
    pub async fn feeds_of(&self, id: &str) -> HashSet<String> {
        let p = self.panels.read().await;
        match p.get(id) {
            Some(panel) => panel.feed_names().into_iter().collect(),
            None => HashSet::new(),
        }
    }

    /// Build the grid frame for a panel using current slot→feed mapping.
    async fn build_grid_frame(&self, id: &str) -> Result<crate::kms::FrameLayout, PanelError> {
        let panels = self.panels.read().await;
        let panel = panels
            .get(id)
            .ok_or_else(|| PanelError::NotFound(id.to_string()))?;
        let slots = self.slot_feed.read().await;
        // global settings live in http_api::state; per-panel override wins
        let spacing = crate::http_api::state::global_spacing_for(panel.spacing).await;
        let bg = crate::http_api::state::global_background().await;
        Ok(wall::grid_frame(panel, spacing, &bg, &slots))
    }

    /// Switch the active panel gracefully using reference counting.
    pub async fn switch_to(&self, new_id: &str) -> Result<(), PanelError> {
        let old_displayed = self.displayed.read().await.clone();
        // Idempotent fast path: same panel already displayed → just
        // re-commit (allows refresh after in-place edits).
        if old_displayed.as_deref() == Some(new_id) {
            let frame = self.build_grid_frame(new_id).await?;
            self.backend.commit(&frame).await?;
            return Ok(());
        }
        // Validate target exists.
        self.get_panel(new_id).await?;

        let leaving = match &old_displayed {
            Some(old) => self.feeds_of(old).await,
            None => HashSet::new(),
        };
        let entering = self.feeds_of(new_id).await;

        // 1..5 — reference-counted diff on the shared decoder pool.
        self.pool.apply_switch(&leaving, &entering).await?;

        // 6 — reset fullscreen before committing the new grid.
        let fs = crate::http_api::state::clear_fullscreen_state().await;
        if let Some(cam) = fs {
            self.pool.stop_fullscreen(&cam).await;
        }

        // Recompute slot→feed map for the new panel.
        {
            let panels = self.panels.read().await;
            let panel = panels.get(new_id).unwrap();
            let mut m = HashMap::new();
            for vp in &panel.viewports {
                if let Some(f) = vp.feeds.first() {
                    m.insert(vp.slot.clone(), f.clone());
                }
            }
            *self.slot_feed.write().await = m;
        }

        // 7 — atomic KMS commit of the new layout.
        let frame = self.build_grid_frame(new_id).await?;
        self.backend.commit(&frame).await?;

        {
            let mut d = self.displayed.write().await;
            *d = Some(new_id.to_string());
        }
        *self.active.write().await = new_id.to_string();
        info!(from = ?old_displayed, to = %new_id, "panel switched");
        Ok(())
    }

    /// Commit the current state (used after settings changes like spacing).
    pub async fn recomposite(&self) -> Result<(), PanelError> {
        let id = self.active_id().await;
        let frame = self.build_grid_frame(&id).await?;
        self.backend.commit(&frame).await?;
        Ok(())
    }

    /// Advance viewport rotation (called by the rotation timer): each
    /// multi-feed viewport switches to its next feed; then recomposite.
    pub async fn rotate_step(&self) -> Result<(), PanelError> {
        let id = self.active_id().await;
        let panel = self.get_panel(&id).await?;
        let mut slots = self.slot_feed.write().await;
        let mut changed = false;
        for vp in &panel.viewports {
            if vp.feeds.len() > 1 {
                let cur = slots
                    .get(&vp.slot)
                    .and_then(|f| vp.feeds.iter().position(|x| x == f))
                    .unwrap_or(0);
                let next = (cur + 1) % vp.feeds.len();
                slots.insert(vp.slot.clone(), vp.feeds[next].clone());
                changed = true;
            }
        }
        drop(slots);
        if changed && !crate::http_api::state::is_paused_now().await {
            self.recomposite().await?;
        }
        Ok(())
    }

    /// Enter fullscreen on `camera` (must be part of the active panel).
    pub async fn enter_fullscreen(&self, camera: &str) -> Result<(), PanelError> {
        let id = self.active_id().await;
        let panel = self.get_panel(&id).await?;
        if !panel.has_camera(camera) {
            return Err(PanelError::Validation(format!(
                "camera {camera:?} is not part of active panel {id:?}"
            )));
        }
        // Stop previous fullscreen stream if any.
        if let Some(prev) =
            crate::http_api::state::set_fullscreen_state(Some(camera.to_string())).await
        {
            if prev != camera {
                self.pool.stop_fullscreen(&prev).await;
            }
        }
        // Switch stream: sub → main.
        self.pool.start_fullscreen(camera).await?;
        let bg = crate::http_api::state::global_background().await;
        let frame = wall::fullscreen_frame(camera, &bg, 0);
        self.backend.commit(&frame).await?;
        info!(camera, "entered fullscreen");
        Ok(())
    }

    /// Leave fullscreen back to the grid of the active panel.
    pub async fn leave_fullscreen(&self) -> Result<(), PanelError> {
        if let Some(cam) = crate::http_api::state::clear_fullscreen_state().await {
            self.pool.stop_fullscreen(&cam).await;
        }
        self.recomposite().await
    }

    /// Whether we are currently in fullscreen mode.
    pub async fn fullscreen_camera(&self) -> Option<String> {
        crate::http_api::state::current_fullscreen().await
    }

    /// Pause everything: stop decoders, black screen. Pool preserved.
    pub async fn pause(&self) -> Result<(), PanelError> {
        crate::http_api::state::set_paused(true).await;
        self.pool.pause_all().await;
        self.backend.black_screen().await?;
        info!("paused: all decoders stopped, black screen committed");
        Ok(())
    }

    /// Resume: restart decoders of the active panel, restore mode.
    pub async fn resume(&self) -> Result<(), PanelError> {
        crate::http_api::state::set_paused(false).await;
        let id = self.active_id().await;
        let feeds = self.feeds_of(&id).await;
        self.pool.resume_feeds(&feeds).await?;
        if let Some(cam) = crate::http_api::state::current_fullscreen().await {
            self.pool.start_fullscreen(&cam).await?;
            let bg = crate::http_api::state::global_background().await;
            self.backend
                .commit(&wall::fullscreen_frame(&cam, &bg, 0))
                .await?;
        } else {
            let frame = self.build_grid_frame(&id).await?;
            self.backend.commit(&frame).await?;
        }
        info!(panel = id, "resumed");
        Ok(())
    }

    /// Current display resolution.
    pub fn resolution(&self) -> Resolution {
        self.resolution
    }

    /// Computed viewport rectangles of the active panel.
    pub async fn active_rects(&self) -> Vec<wall::ViewportRect> {
        let id = self.active_id().await;
        let panels = self.panels.read().await;
        match panels.get(&id) {
            Some(p) => {
                let spacing = crate::http_api::state::global_spacing_for(p.spacing).await;
                wall::compute_layout(p, self.resolution, spacing)
            }
            None => vec![],
        }
    }

    /// Replace the whole registry (used by /api/reload).
    pub async fn reload_from(
        &self,
        panels: HashMap<String, PanelConfig>,
        preferred_active: Option<String>,
    ) -> Result<(), PanelError> {
        let old_ids: HashSet<String> = self.panels.read().await.keys().cloned().collect();
        let new_ids: HashSet<String> = panels.keys().cloned().collect();
        // Release feeds of removed panels fully.
        {
            let g = self.panels.read().await;
            for id in old_ids.difference(&new_ids) {
                if let Some(p) = g.get(id) {
                    for f in p.feed_names() {
                        // best-effort: ignore feeds already gone from pool
                        let _ = self.pool.release(&f).await;
                    }
                }
            }
        }
        *self.panels.write().await = panels;
        let active = self.active.read().await.clone();
        drop(active);
        let cur = self.active_id().await;
        let still_exists = self.panels.read().await.contains_key(&cur);
        let target = match (&preferred_active, still_exists) {
            (Some(p), _) if self.panels.read().await.contains_key(p) => p.clone(),
            (_, true) => cur.clone(),
            _ => {
                let g = self.panels.read().await;
                let mut keys: Vec<&String> = g.keys().collect();
                keys.sort();
                keys.first().cloned().cloned().unwrap_or_default()
            }
        };
        if !target.is_empty() {
            if let Err(e) = self.switch_to(&target).await {
                warn!("reload switch_to failed: {e}");
                return Err(e);
            }
        }
        Ok(())
    }
}

/// Convenience: construct default single-feed viewports from a flat list.
pub fn viewports_from(feeds: &[String]) -> Vec<ViewportConfig> {
    feeds
        .iter()
        .map(|f| ViewportConfig {
            slot: f.clone(),
            feeds: vec![f.clone()],
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::kms::HeadlessBackend;

    const SAMPLE: &str = r#"
feeds:
  a: { url: "rtsp://c/a", fullscreen_url: "rtsp://c/a_main" }
  b: { url: "rtsp://c/b", fullscreen_url: "rtsp://c/b_main" }
  c: { url: "rtsp://c/c", fullscreen_url: "rtsp://c/c_main" }
panels:
  p1:
    name: "P1"
    layout: |
      a a b
      a a b
      . . b
    viewports:
      - { slot: a, feeds: [a] }
      - { slot: b, feeds: [b] }
  p2:
    name: "P2"
    layout: |
      b b c
      b b c
      . . c
    viewports:
      - { slot: b, feeds: [b] }
      - { slot: c, feeds: [c] }
defaults:
  panel: p1
"#;

    async fn make_mgr() -> (PanelManager, Arc<FeedPool>, Arc<HeadlessBackend>) {
        let cfg = Config::from_yaml(SAMPLE).unwrap();
        let pool = Arc::new(FeedPool::new());
        for (name, fc) in &cfg.feeds {
            pool.add_feed(name.clone(), fc.clone()).await;
        }
        let backend = Arc::new(HeadlessBackend::new());
        let backend_dyn: Arc<dyn DisplayBackend> = backend.clone();
        let mgr = PanelManager::new(
            cfg.panels.clone(),
            cfg.initial_panel(),
            pool.clone(),
            backend_dyn,
            Resolution {
                width: 900,
                height: 600,
            },
        );
        mgr.switch_to("p1").await.unwrap(); // acquire initial refs
        (mgr, pool, backend)
    }

    #[tokio::test]
    async fn switch_keeps_shared_decoder() {
        let (mgr, pool, backend) = make_mgr().await;
        assert!(pool.is_running("a").await);
        assert!(pool.is_running("b").await);
        assert!(!pool.is_running("c").await);

        let b_handle_before = mgr.feeds_of("p1").await; // just to touch API
        assert_eq!(b_handle_before, ["a".to_string(), "b".to_string()].into());

        mgr.switch_to("p2").await.unwrap();

        assert_eq!(mgr.active_id().await, "p2");
        assert!(!pool.is_running("a").await, "leaving feed stopped");
        assert!(pool.is_running("b").await, "shared feed keeps running");
        assert!(pool.is_running("c").await, "entering feed started");
        assert_eq!(pool.ref_count("b").await, Some(1));
        assert!(backend
            .last_frame()
            .await
            .unwrap()
            .planes
            .contains(&"b".to_string()));
    }

    #[tokio::test]
    async fn delete_active_panel_falls_back() {
        let (mgr, _pool, _be) = make_mgr().await;
        mgr.delete_panel("p1").await.unwrap();
        assert_eq!(mgr.active_id().await, "p2");
        assert_eq!(mgr.count().await, 1);
    }

    #[tokio::test]
    async fn cannot_delete_last_panel() {
        let (mgr, _pool, _be) = make_mgr().await;
        mgr.delete_panel("p1").await.unwrap();
        let err = mgr.delete_panel("p2").await.unwrap_err();
        assert!(matches!(err, PanelError::LastPanel));
    }

    #[tokio::test]
    async fn fullscreen_requires_camera_in_panel() {
        let (mgr, pool, _be) = make_mgr().await;
        let err = mgr.enter_fullscreen("c").await.unwrap_err();
        assert!(matches!(err, PanelError::Validation(_)));
        // Fullscreen state is process-global and shared across tests.
        // Its exact value is racy while the test suite runs in parallel.
        // This test checks validation, successful entry, and feed activation.
        mgr.enter_fullscreen("a").await.unwrap();
        assert!(pool.is_running("a").await);
        mgr.leave_fullscreen().await.unwrap();
    }

    #[tokio::test]
    async fn pause_resume_roundtrip() {
        let (mgr, pool, be) = make_mgr().await;
        mgr.pause().await.unwrap();
        assert!(!pool.is_running("a").await);
        assert_eq!(be.last_frame().await.unwrap().planes.len(), 0);
        assert_eq!(pool.ref_count("a").await, Some(1));
        mgr.resume().await.unwrap();
        assert!(pool.is_running("a").await);
        assert!(!be.last_frame().await.unwrap().planes.is_empty());
    }

    #[tokio::test]
    async fn panels_using_camera() {
        let (mgr, _pool, _be) = make_mgr().await;
        assert_eq!(mgr.panels_using_camera("b").await, vec!["p1", "p2"]);
        assert_eq!(mgr.panels_using_camera("a").await, vec!["p1"]);
    }

    #[tokio::test]
    async fn add_invalid_panel_rejected() {
        let (mgr, _pool, _be) = make_mgr().await;
        let bad = PanelConfig {
            name: "bad".into(),
            spacing: None,
            layout: "zz zz zz\nzz zz zz\nzz zz zz\n".into(),
            viewports: vec![ViewportConfig {
                slot: "zz".into(),
                feeds: vec!["nonexistent".into()],
            }],
        };
        let err = mgr.add_panel("bad".into(), bad).await.unwrap_err();
        assert!(matches!(err, PanelError::Validation(_)));
    }
}
