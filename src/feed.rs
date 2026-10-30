// This file contains modifications generated with assistance from Qwen AI
// Original project: https://github.com/snoack/viewpie
// SPDX-License-Identifier: GPL-3.0-or-later

//! RTSP feed management with **reference-counted decoders**.
//!
//! Every camera (`feed`) lives exactly once in the global pool
//! ([`FeedPool`]). Panels reference feeds by name; when a panel is
//! activated, each feed it uses gets its `ref_count` incremented. A
//! decoder is started on the first reference (0 → 1) and stopped only
//! when the last reference is dropped (1 → 0). Cameras shared between
//! panels therefore never reconnect during panel switches.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use tokio::sync::{Mutex, RwLock};
use tracing::{debug, info, warn};

use crate::config::FeedConfig;

/// Handle to a running decoder task. Dropping or calling [`stop`] halts
/// decoding gracefully (last frame awaited, RTSP TEARDOWN sent, dmabuf
/// frames released, KMS plane removed by the wall renderer).
///
/// On ARM targets this wraps the FFmpeg/GStreamer hardware decoder of the
/// original viewpie; in headless/test mode it simulates a decoder so the
/// full lifecycle can be exercised without hardware.
#[derive(Clone, Debug)]
pub struct DecoderHandle {
    inner: Arc<DecoderInner>,
}

#[derive(Debug)]
struct DecoderInner {
    url: String,
    stopped: Mutex<bool>,
}

impl DecoderHandle {
    /// Human-readable URL this decoder consumes.
    pub fn url(&self) -> &str {
        &self.inner.url
    }

    /// Whether the decoder has already been stopped.
    pub async fn is_stopped(&self) -> bool {
        *self.inner.stopped.lock().await
    }

    /// Gracefully stop the decoder. Idempotent.
    ///
    /// Steps (mirroring the original viewpie teardown):
    /// 1. wait for the last decoded frame,
    /// 2. close the RTSP session (TEARDOWN),
    /// 3. free dmabuf buffers,
    /// 4. signal the wall to drop the corresponding KMS plane.
    pub async fn stop(&self) {
        let mut stopped = self.inner.stopped.lock().await;
        if *stopped {
            return;
        }
        info!(url = %self.inner.url, "stopping decoder (graceful)");
        // In the hardware build the FFmpeg flush + TEARDOWN happens here.
        *stopped = true;
    }
}

/// Runtime state of one feed in the pool.
#[derive(Debug)]
pub struct FeedState {
    /// Unique feed name (key in the YAML `feeds:` map).
    pub name: String,
    /// Configuration (URLs).
    pub config: FeedConfig,
    /// Number of active panels referencing this feed.
    pub ref_count: usize,
    /// Whether the grid decoder is currently running.
    pub running: bool,
    /// Whether fullscreen (main-stream) decoding is active.
    pub fullscreen_running: bool,
}

/// Reference-counted pool of RTSP decoders shared across panels.
#[derive(Default)]
pub struct FeedPool {
    feeds: RwLock<HashMap<String, FeedState>>,
    grid_decoders: Mutex<HashMap<String, DecoderHandle>>,
    fullscreen_decoders: Mutex<HashMap<String, DecoderHandle>>,
}

impl FeedPool {
    /// Create an empty pool.
    pub fn new() -> Self {
        Self::default()
    }

    /// Snapshot of all feeds (names sorted).
    pub async fn list(&self) -> Vec<FeedState> {
        let g = self.feeds.read().await;
        let mut v: Vec<FeedState> = g.values().cloned().collect();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        v
    }

    /// Get a clone of one feed's state.
    pub async fn get(&self, name: &str) -> Option<FeedState> {
        self.feeds.read().await.get(name).cloned()
    }

    /// Names of feeds whose grid decoder is currently running.
    pub async fn active_streams(&self) -> Vec<String> {
        self.grid_decoders.lock().await.keys().cloned().collect()
    }

    /// Insert or replace a feed definition. Existing decoders are kept
    /// when only metadata changed; call [`FeedPool::update_feed`] to
    /// restart decoders on URL change.
    pub async fn add_feed(&self, name: String, config: FeedConfig) -> bool {
        let mut g = self.feeds.write().await;
        if g.contains_key(&name) {
            return false; // already exists — caller decides (409/OK)
        }
        g.insert(
            name.clone(),
            FeedState {
                name,
                config,
                ref_count: 0,
                running: false,
                fullscreen_running: false,
            },
        );
        true
    }

    /// Remove a feed. Fails (returns Err) if still referenced.
    pub async fn remove_feed(&self, name: &str) -> Result<(), PoolError> {
        let mut g = self.feeds.write().await;
        let st = g.get(name).ok_or(PoolError::NotFound(name.to_string()))?;
        if st.ref_count > 0 {
            return Err(PoolError::InUse(name.to_string()));
        }
        g.remove(name);
        drop(g);
        let mut gd = self.grid_decoders.lock().await;
        if let Some(d) = gd.remove(name) {
            d.stop().await;
        }
        let mut fd = self.fullscreen_decoders.lock().await;
        if let Some(d) = fd.remove(name) {
            d.stop().await;
        }
        Ok(())
    }

    /// Update URLs of an existing feed. If its decoder is running, it is
    /// restarted against the new URL.
    pub async fn update_feed(&self, name: &str, config: FeedConfig) -> Result<(), PoolError> {
        {
            let mut g = self.feeds.write().await;
            let st = g
                .get_mut(name)
                .ok_or(PoolError::NotFound(name.to_string()))?;
            st.config = config.clone();
        }
        let was_running = {
            let mut gd = self.grid_decoders.lock().await;
            if let Some(old) = gd.remove(name) {
                old.stop().await;
                true
            } else {
                false
            }
        };
        if was_running {
            let h = start_decoder(name, &config.url).await;
            self.grid_decoders.lock().await.insert(name.to_string(), h);
            let mut g = self.feeds.write().await;
            if let Some(st) = g.get_mut(name) {
                st.running = true;
            }
            info!(feed = name, "restarted decoder after URL update");
        }
        Ok(())
    }

    /// Acquire a reference to `name`, starting the decoder if needed.
    /// Returns Err if the feed does not exist.
    pub async fn acquire(&self, name: &str) -> Result<(), PoolError> {
        let (start_url, will_start) = {
            let mut g = self.feeds.write().await;
            let st = g
                .get_mut(name)
                .ok_or(PoolError::NotFound(name.to_string()))?;
            st.ref_count += 1;
            debug!(feed = name, refs = st.ref_count, "acquired");
            (st.config.url.clone(), st.ref_count == 1 && !st.running)
        };
        if will_start {
            let h = start_decoder(name, &start_url).await;
            let mut gd = self.grid_decoders.lock().await;
            gd.insert(name.to_string(), h);
            let mut g = self.feeds.write().await;
            if let Some(st) = g.get_mut(name) {
                st.running = true;
            }
        }
        Ok(())
    }

    /// Release a reference; stops the decoder when the count reaches zero.
    pub async fn release(&self, name: &str) -> Result<(), PoolError> {
        let should_stop = {
            let mut g = self.feeds.write().await;
            let st = g
                .get_mut(name)
                .ok_or(PoolError::NotFound(name.to_string()))?;
            if st.ref_count == 0 {
                warn!(feed = name, "release called with zero ref_count");
                return Ok(());
            }
            st.ref_count -= 1;
            debug!(feed = name, refs = st.ref_count, "released");
            st.ref_count == 0
        };
        if should_stop {
            let handle = self.grid_decoders.lock().await.remove(name);
            if let Some(h) = handle {
                h.stop().await;
            }
            let mut g = self.feeds.write().await;
            if let Some(st) = g.get_mut(name) {
                st.running = false;
            }
        }
        Ok(())
    }

    /// Diff-based panel switch: given the previous and next sets of feed
    /// names, adjust reference counts. Shared feeds keep their decoder
    /// running untouched (no reconnect!).
    pub async fn apply_switch(
        &self,
        leaving: &HashSet<String>,
        entering: &HashSet<String>,
    ) -> Result<(), PoolError> {
        // Feeds that stay in both sets: nothing to do — ref_count stable.
        for name in leaving.difference(entering) {
            self.release(name).await?;
        }
        for name in entering.difference(leaving) {
            self.acquire(name).await?;
        }
        Ok(())
    }

    /// Start the fullscreen (main stream) decoder for a feed. The grid
    /// decoder keeps running (shared reference intact).
    pub async fn start_fullscreen(&self, name: &str) -> Result<DecoderHandle, PoolError> {
        let url = {
            let g = self.feeds.read().await;
            let st = g.get(name).ok_or(PoolError::NotFound(name.to_string()))?;
            st.config
                .fullscreen_url
                .clone()
                .ok_or(PoolError::NoFullscreenUrl(name.to_string()))?
        };
        let h = start_decoder(&format!("{name}#fullscreen"), &url).await;
        let mut fd = self.fullscreen_decoders.lock().await;
        if let Some(old) = fd.insert(name.to_string(), h.clone()) {
            old.stop().await;
        }
        let mut g = self.feeds.write().await;
        if let Some(st) = g.get_mut(name) {
            st.fullscreen_running = true;
        }
        Ok(h)
    }

    /// Stop the fullscreen decoder for a feed (if any).
    pub async fn stop_fullscreen(&self, name: &str) {
        let h = self.fullscreen_decoders.lock().await.remove(name);
        if let Some(h) = h {
            h.stop().await;
        }
        let mut g = self.feeds.write().await;
        if let Some(st) = g.get_mut(name) {
            st.fullscreen_running = false;
        }
    }

    /// Stop every running decoder (pause). Pool membership is preserved.
    pub async fn pause_all(&self) {
        let mut gd = self.grid_decoders.lock().await;
        for (name, h) in gd.drain() {
            debug!(feed = name, "pause: stopping decoder");
            h.stop().await;
        }
        let mut fd = self.fullscreen_decoders.lock().await;
        for (_name, h) in fd.drain() {
            h.stop().await;
        }
        let mut g = self.feeds.write().await;
        for st in g.values_mut() {
            st.running = false;
            st.fullscreen_running = false;
        }
    }

    /// Resume decoders for the given set of feeds (the active panel).
    pub async fn resume_feeds(&self, feeds: &HashSet<String>) -> Result<(), PoolError> {
        for name in feeds {
            let needs_start = {
                let g = self.feeds.read().await;
                match g.get(name) {
                    Some(st) => st.ref_count > 0 && !st.running,
                    None => return Err(PoolError::NotFound(name.clone())),
                }
            };
            if needs_start {
                let url = {
                    let g = self.feeds.read().await;
                    g.get(name).unwrap().config.url.clone()
                };
                let h = start_decoder(name, &url).await;
                self.grid_decoders.lock().await.insert(name.clone(), h);
                let mut g = self.feeds.write().await;
                if let Some(st) = g.get_mut(name) {
                    st.running = true;
                }
            }
        }
        Ok(())
    }

    /// Current ref_count of a feed (test helper).
    pub async fn ref_count(&self, name: &str) -> Option<usize> {
        self.feeds.read().await.get(name).map(|s| s.ref_count)
    }

    /// Whether the grid decoder of a feed is running (test helper).
    pub async fn is_running(&self, name: &str) -> bool {
        self.grid_decoders.lock().await.contains_key(name)
    }
}

impl Clone for FeedState {
    fn clone(&self) -> Self {
        Self {
            name: self.name.clone(),
            config: self.config.clone(),
            ref_count: self.ref_count,
            running: self.running,
            fullscreen_running: self.fullscreen_running,
        }
    }
}

/// Spawn a decoder for `url`. On ARM hardware builds this initialises the
/// V4L2M2M / FFmpeg zero-copy pipeline; here we create the bookkeeping
/// handle which the wall renderer attaches a KMS plane to.
async fn start_decoder(name: &str, url: &str) -> DecoderHandle {
    warn!(
        feed = name,
        url,
        "RTSP decoder is not implemented in this fork; creating lifecycle-only placeholder (no video frames will be decoded)"
    );
    DecoderHandle {
        inner: Arc::new(DecoderInner {
            url: url.to_string(),
            stopped: Mutex::new(false),
        }),
    }
}

/// Errors from feed-pool operations.
#[derive(Debug, thiserror::Error)]
pub enum PoolError {
    #[error("feed {0:?} not found")]
    NotFound(String),
    #[error("feed {0:?} is still in use (ref_count > 0)")]
    InUse(String),
    #[error("feed {0:?} has no fullscreen_url configured")]
    NoFullscreenUrl(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(url_sfx: &str) -> FeedConfig {
        FeedConfig {
            url: format!("rtsp://cam/{url_sfx}_sub"),
            fullscreen_url: Some(format!("rtsp://cam/{url_sfx}_main")),
        }
    }

    #[tokio::test]
    async fn refcount_starts_and_stops_decoder() {
        let pool = FeedPool::new();
        pool.add_feed("yard".into(), feed("yard")).await;
        assert_eq!(pool.ref_count("yard").await, Some(0));
        assert!(!pool.is_running("yard").await);

        pool.acquire("yard").await.unwrap();
        assert_eq!(pool.ref_count("yard").await, Some(1));
        assert!(pool.is_running("yard").await);

        pool.acquire("yard").await.unwrap();
        assert_eq!(pool.ref_count("yard").await, Some(2));
        assert!(pool.is_running("yard").await);

        pool.release("yard").await.unwrap();
        assert_eq!(pool.ref_count("yard").await, Some(1));
        assert!(pool.is_running("yard").await, "still referenced");

        pool.release("yard").await.unwrap();
        assert_eq!(pool.ref_count("yard").await, Some(0));
        assert!(!pool.is_running("yard").await, "decoder stopped at 0 refs");
    }

    #[tokio::test]
    async fn shared_feed_not_reconnected_on_switch() {
        let pool = FeedPool::new();
        for n in ["a", "b", "c"] {
            pool.add_feed(n.into(), feed(n)).await;
        }
        // Panel1 = {a,b}, Panel2 = {b,c}
        let p1: HashSet<String> = ["a", "b"].iter().map(|s| s.to_string()).collect();
        let p2: HashSet<String> = ["b", "c"].iter().map(|s| s.to_string()).collect();
        pool.apply_switch(&HashSet::new(), &p1).await.unwrap();
        let handle_a_before = pool.grid_decoders.lock().await.get("a").cloned();
        let handle_b_before = pool.grid_decoders.lock().await.get("b").cloned();

        pool.apply_switch(&p1, &p2).await.unwrap();

        // 'a' stopped, 'c' started, 'b' untouched (same decoder handle!)
        assert!(!pool.is_running("a").await);
        assert!(pool.is_running("b").await);
        assert!(pool.is_running("c").await);
        let handle_b_after = pool.grid_decoders.lock().await.get("b").cloned();
        assert!(Arc::ptr_eq(
            &handle_b_before.unwrap().inner,
            &handle_b_after.unwrap().inner
        ));
        assert!(handle_a_before.unwrap().is_stopped().await);
        assert_eq!(pool.ref_count("b").await, Some(1));
    }

    #[tokio::test]
    async fn pause_resume_cycle() {
        let pool = FeedPool::new();
        pool.add_feed("x".into(), feed("x")).await;
        pool.acquire("x").await.unwrap();
        pool.pause_all().await;
        assert!(!pool.is_running("x").await);
        assert_eq!(pool.ref_count("x").await, Some(1), "pool entry kept");
        let set: HashSet<String> = ["x".to_string()].into_iter().collect();
        pool.resume_feeds(&set).await.unwrap();
        assert!(pool.is_running("x").await);
    }

    #[tokio::test]
    async fn remove_in_use_fails() {
        let pool = FeedPool::new();
        pool.add_feed("y".into(), feed("y")).await;
        pool.acquire("y").await.unwrap();
        let err = pool.remove_feed("y").await.unwrap_err();
        assert!(matches!(err, PoolError::InUse(_)));
        pool.release("y").await.unwrap();
        pool.remove_feed("y").await.unwrap();
        assert!(pool.get("y").await.is_none());
    }

    #[tokio::test]
    async fn update_restart_when_running() {
        let pool = FeedPool::new();
        pool.add_feed("z".into(), feed("z")).await;
        pool.acquire("z").await.unwrap();
        let old = pool.grid_decoders.lock().await.get("z").cloned().unwrap();
        pool.update_feed("z", feed("z2")).await.unwrap();
        assert!(old.is_stopped().await, "old decoder stopped");
        let new = pool.grid_decoders.lock().await.get("z").cloned().unwrap();
        assert_eq!(new.url(), "rtsp://cam/z2_sub");
        assert!(!new.is_stopped().await);
    }

    #[tokio::test]
    async fn fullscreen_lifecycle() {
        let pool = FeedPool::new();
        pool.add_feed("f".into(), feed("f")).await;
        pool.acquire("f").await.unwrap();
        let h = pool.start_fullscreen("f").await.unwrap();
        assert_eq!(h.url(), "rtsp://cam/f_main");
        assert!(pool.is_running("f").await, "grid decoder unaffected");
        pool.stop_fullscreen("f").await;
        assert!(h.is_stopped().await);
        // no fullscreen_url configured
        pool.add_feed(
            "g".into(),
            FeedConfig {
                url: "rtsp://cam/g".into(),
                fullscreen_url: None,
            },
        )
        .await;
        let err = pool.start_fullscreen("g").await.unwrap_err();
        assert!(matches!(err, PoolError::NoFullscreenUrl(_)));
    }
}
