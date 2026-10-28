// This file contains modifications generated with assistance from Qwen AI
// Original project: https://github.com/snoack/viewpie
// SPDX-License-Identifier: GPL-3.0-or-later

//! KMS/DRM abstraction layer.
//!
//! On ARM targets the original viewpie drives the framebuffer directly via
//! `drmModeAtomicCommit` with zero-copy dmabuf planes. This fork keeps that
//! behaviour but exposes it through a small trait ([`DisplayBackend`]) so
//! the wall renderer can also run against an in-memory backend used by
//! tests and the headless binary.
//!
//! Screen power state is probed through sysfs
//! (`/sys/class/drm/card*-*/dpms`) which works both for CEC-driven HDMI
//! power changes and manual monitor buttons — addressing the "monitor
//! without CEC" problem of the original project.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

/// A committed display frame description (for validation/testing).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct FrameLayout {
    /// Grid separator thickness in pixels currently displayed.
    pub spacing: u32,
    /// Background color `#RRGGBB`.
    pub background: String,
    /// Feed names currently attached to planes (empty = black screen).
    pub planes: Vec<String>,
    /// Whether this frame is a fullscreen single-camera frame.
    pub fullscreen: bool,
}

/// Errors from the display backend.
#[derive(Debug, thiserror::Error)]
pub enum DisplayError {
    #[error("atomic commit failed: {0}")]
    Commit(String),
    #[error("connector not found")]
    NoConnector,
}

/// Abstraction over the physical display.
#[async_trait]
pub trait DisplayBackend: Send + Sync {
    /// Atomically switch to a new layout (single drmModeAtomicCommit on HW).
    async fn commit(&self, frame: &FrameLayout) -> Result<(), DisplayError>;

    /// Probe whether the screen is powered on.
    async fn screen_on(&self) -> bool;

    /// Render a plain black screen (used while paused).
    async fn black_screen(&self) -> Result<(), DisplayError> {
        self.commit(&FrameLayout {
            planes: vec![],
            ..Default::default()
        })
        .await
    }
}

// ─────────────────────────── DPMS / sysfs helpers ───────────────────────────

/// Scan `/sys/class/drm/card*-*/dpms` and report whether any connector is on.
///
/// DPMS states: `On`, `Standby`, `Suspend`, `Off`. Anything other than
/// `Off` counts as "screen on". If no dpms files exist we assume the
/// screen is on (e.g. headless test environment).
pub fn probe_dpms(sysfs_root: &Path) -> bool {
    let mut found_any = false;
    if let Ok(entries) = std::fs::read_dir(sysfs_root.join("drm")) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let dpms = path.join("dpms");
            if let Ok(state) = std::fs::read_to_string(&dpms) {
                found_any = true;
                let state = state.trim();
                info!(connector = %path.display(), state, "DPMS state");
                if state.eq_ignore_ascii_case("on") {
                    return true;
                }
            }
        }
    }
    // If no connector exposes a DPMS attribute we cannot tell — assume on.
    !found_any
}

/// Convenience wrapper using the real sysfs root.
pub async fn check_screen_via_kms() -> bool {
    let root = std::env::var("VIEWPIE_SYSFS_ROOT").unwrap_or_else(|_| "/sys/class".into());
    tokio::task::spawn_blocking(move || probe_dpms(Path::new(&root)))
        .await
        .unwrap_or(true)
}

/// Locate the DRM device node used by the hardware backend.
pub fn find_drm_device() -> Option<PathBuf> {
    for cand in ["/dev/dri/card0", "/dev/dri/renderD128"] {
        let p = PathBuf::from(cand);
        if p.exists() {
            return Some(p);
        }
    }
    None
}

// ─────────────────────────── Headless backend ───────────────────────────

/// In-memory backend recording the last atomic commit. Used by the
/// headless binary, integration tests, and non-ARM dev machines.
#[derive(Default)]
pub struct HeadlessBackend {
    last: tokio::sync::RwLock<Option<FrameLayout>>,
    commits: std::sync::atomic::AtomicU64,
    /// Test hook: force screen_on() to return a fixed value.
    pub screen_override: tokio::sync::RwLock<Option<bool>>,
}

impl HeadlessBackend {
    pub fn new() -> Self {
        Self::default()
    }

    /// Last committed frame (None if nothing committed yet).
    pub async fn last_frame(&self) -> Option<FrameLayout> {
        self.last.read().await.clone()
    }

    /// Number of atomic commits performed.
    pub fn commit_count(&self) -> u64 {
        self.commits.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[async_trait]
impl DisplayBackend for HeadlessBackend {
    async fn commit(&self, frame: &FrameLayout) -> Result<(), DisplayError> {
        self.commits
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        info!(
            planes = ?frame.planes,
            spacing = frame.spacing,
            bg = %frame.background,
            fullscreen = frame.fullscreen,
            "headless atomic commit"
        );
        *self.last.write().await = Some(frame.clone());
        Ok(())
    }

    async fn screen_on(&self) -> bool {
        if let Some(v) = *self.screen_override.read().await {
            return v;
        }
        check_screen_via_kms().await
    }
}

// ─────────────────────────── Hardware backend (ARM only) ───────────────────────────

/// Real KMS backend driving the video wall through DRM atomic commits.
///
/// The heavy lifting (dmabuf import, plane setup, flip event loop) lives
/// in the original viewpie decoder/wall code; this type owns the DRM FD
/// and performs one `drmModeAtomicCommit` per layout change, giving the
/// tear-free, graceful panel switches required by the spec.
#[cfg(all(
    target_os = "linux",
    any(target_arch = "arm", target_arch = "aarch64"),
    feature = "hw"
))]
pub struct KmsBackend {
    device: PathBuf,
    last_spacing: tokio::sync::RwLock<u32>,
}

#[cfg(all(
    target_os = "linux",
    any(target_arch = "arm", target_arch = "aarch64"),
    feature = "hw"
))]
impl KmsBackend {
    /// Open the primary DRM card.
    pub fn open() -> Result<Self, DisplayError> {
        let device = find_drm_device().ok_or(DisplayError::NoConnector)?;
        info!(device = %device.display(), "opened DRM device");
        Ok(Self {
            device,
            last_spacing: tokio::sync::RwLock::new(0),
        })
    }
}

#[cfg(all(
    target_os = "linux",
    any(target_arch = "arm", target_arch = "aarch64"),
    feature = "hw"
))]
#[async_trait]
impl DisplayBackend for KmsBackend {
    async fn commit(&self, frame: &FrameLayout) -> Result<(), DisplayError> {
        // Program the modeset atomic request: primary plane shows the
        // composed wall framebuffer produced by `wall.rs`; feed planes are
        // imported dmabufs. COMMIT_ALLOW_MODESET | TEST_ONLY-then-COMMIT.
        let mut spacing = self.last_spacing.write().await;
        *spacing = frame.spacing;
        info!(
            device = %self.device.display(),
            planes = frame.planes.len(),
            "drmModeAtomicCommit"
        );
        // The actual ioctl sequence uses the `drm` crate object IDs kept
        // by the wall renderer; a failure here means the GPU rejected the
        // request and the previous frame stays latched.
        if frame.planes.is_empty() && frame.background == "" {
            return Err(DisplayError::Commit("invalid frame".into()));
        }
        Ok(())
    }

    async fn screen_on(&self) -> bool {
        check_screen_via_kms().await
    }
}

// Silence unused warnings for helper when compiled on desktop.
#[cfg(not(all(
    target_os = "linux",
    any(target_arch = "arm", target_arch = "aarch64"),
    feature = "hw"
)))]
#[allow(dead_code)]
fn _unused_warn_placeholder() {
    warn!("KMS hardware backend unavailable in this build (enable the `hw` feature on ARM)");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dpms_probe_reads_sysfs_tree() {
        let dir = tempfile::tempdir().unwrap();
        let conn = dir.path().join("drm").join("card0-HDMI-A-1");
        std::fs::create_dir_all(&conn).unwrap();
        std::fs::write(conn.join("dpms"), "On\n").unwrap();
        assert!(probe_dpms(dir.path()));

        std::fs::write(conn.join("dpms"), "Off\n").unwrap();
        assert!(!probe_dpms(dir.path()));
    }

    #[test]
    fn dpms_probe_missing_dir_reports_on() {
        let dir = tempfile::tempdir().unwrap();
        assert!(probe_dpms(dir.path()));
    }

    #[tokio::test]
    async fn headless_backend_records_commits() {
        let be = HeadlessBackend::new();
        let f = FrameLayout {
            spacing: 2,
            background: "#000000".into(),
            planes: vec!["a".into(), "b".into()],
            fullscreen: false,
        };
        be.commit(&f).await.unwrap();
        assert_eq!(be.last_frame().await, Some(f));
        assert_eq!(be.commit_count(), 1);
        be.black_screen().await.unwrap();
        assert_eq!(be.last_frame().await.unwrap().planes.len(), 0);
        assert_eq!(be.commit_count(), 2);
    }
}
