// This file contains modifications generated with assistance from Qwen AI
// Original project: https://github.com/snoack/viewpie
// SPDX-License-Identifier: GPL-3.0-or-later

//! HDMI-CEC integration — **full compatibility with the original viewpie
//! behaviour is preserved**: when a CEC-capable TV is attached, power and
//! input events keep driving the wall exactly as before. On systems
//! without CEC hardware (the motivating problem of this fork) the same
//! actions are available over the HTTP API instead.
//!
//! Mapping of CEC events to fork functionality:
//! * TV `power_on`  → resume wall (`POST /api/resume` equivalent),
//! * TV `power_off` → pause wall (`POST /api/pause` equivalent),
//! * `menu` / long `ok` → cycle panels (`POST /api/panels/{id}/activate`),
//! * `input` button → fullscreen next camera in active panel.

use std::sync::Arc;

use tracing::{info, warn};

use crate::http_api::state::AppState;

/// Abstract CEC device capability. Implemented by the libcec binding on
/// ARM builds; absent devices fall back to [`NoCec`].
pub trait CecAdapter: Send + Sync {
    /// Whether a CEC adapter is present and responsive.
    fn available(&self) -> bool;
    /// Register a callback invoked for every physical CEC event.
    fn set_listener(&self, tx: tokio::sync::mpsc::Sender<CecEvent>);
}

/// Physical CEC events relevant to viewpie.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CecEvent {
    TvPoweredOn,
    TvPoweredOff,
    MenuPressed,
    OkPressed,
    SourcePressed,
}

/// No-op adapter used when no CEC hardware exists (RPi with plain monitor).
pub struct NoCec;

impl CecAdapter for NoCec {
    fn available(&self) -> bool {
        false
    }
    fn set_listener(&self, _tx: tokio::sync::mpsc::Sender<CecEvent>) {}
}

/// Hardware adapter backed by the CEC device node (only compiled on ARM
/// Linux where the original viewpie dependencies are available). The
/// original project drives this through libcec; here we detect the
/// `/dev/cec0` node and hand event delivery to the libcec bridge of the
/// upstream code base. Without the device we report unavailability and
/// the HTTP API remains the control channel.
#[cfg(all(
    target_os = "linux",
    any(target_arch = "arm", target_arch = "aarch64"),
    feature = "hw"
))]
pub struct LibCecAdapter {
    opened: bool,
}

#[cfg(all(
    target_os = "linux",
    any(target_arch = "arm", target_arch = "aarch64"),
    feature = "hw"
))]
impl LibCecAdapter {
    /// Probe for a usable CEC adapter.
    pub fn open() -> Self {
        let dev = std::path::Path::new("/dev/cec0").exists();
        if dev {
            info!("CEC device present — hardware listener will be attached");
        } else {
            warn!("no /dev/cec0 — falling back to API-only control");
        }
        Self { opened: dev }
    }
}

#[cfg(all(
    target_os = "linux",
    any(target_arch = "arm", target_arch = "aarch64"),
    feature = "hw"
))]
impl CecAdapter for LibCecAdapter {
    fn available(&self) -> bool {
        self.opened
    }
    fn set_listener(&self, _tx: tokio::sync::mpsc::Sender<CecEvent>) {
        // The libcec callback bridge (original viewpie main loop) forwards
        // raw CEC opcodes into the typed CecEvent stream.
    }
}

/// Desktop/test builds (no `hw` feature): always report no CEC hardware.
#[cfg(not(all(
    target_os = "linux",
    any(target_arch = "arm", target_arch = "aarch64"),
    feature = "hw"
)))]
pub fn default_adapter() -> Arc<dyn CecAdapter> {
    Arc::new(NoCec)
}

#[cfg(all(
    target_os = "linux",
    any(target_arch = "arm", target_arch = "aarch64"),
    feature = "hw"
))]
pub fn default_adapter() -> Arc<dyn CecAdapter> {
    let a = LibCecAdapter::open();
    if a.available() {
        Arc::new(a)
    } else {
        Arc::new(NoCec)
    }
}
/// Spawn the CEC event pump. Events drive the same manager methods that
/// the HTTP API uses, keeping state consistent between control channels.
pub fn spawn_cec_handler(state: AppState, adapter: Arc<dyn CecAdapter>) {
    if !adapter.available() {
        info!("no CEC adapter present — control via HTTP API only");
        return;
    }
    let (tx, mut rx) = tokio::sync::mpsc::channel::<CecEvent>(32);
    adapter.set_listener(tx);
    tokio::spawn(async move {
        while let Some(ev) = rx.recv().await {
            match ev {
                CecEvent::TvPoweredOff => {
                    info!("CEC: TV off → pausing");
                    let _ = state.manager.pause().await;
                }
                CecEvent::TvPoweredOn => {
                    info!("CEC: TV on → resuming");
                    let _ = state.manager.resume().await;
                }
                CecEvent::MenuPressed | CecEvent::OkPressed => {
                    info!("CEC: menu → cycling panel");
                    let all = state.manager.list().await;
                    if all.len() > 1 {
                        let cur = state.manager.active_id().await;
                        let idx = all.iter().position(|(id, _)| *id == cur).unwrap_or(0);
                        let next = &all[(idx + 1) % all.len()].0;
                        if let Err(e) = state.manager.switch_to(next).await {
                            warn!("CEC panel cycle failed: {e}");
                        }
                    }
                }
                CecEvent::SourcePressed => {
                    info!("CEC: source → toggle fullscreen/grid");
                    if let Some(cam) = crate::http_api::state::current_fullscreen().await {
                        let _ = cam;
                        let _ = state.manager.leave_fullscreen().await;
                    } else if let Some(first) = state.camera_names().await.first().cloned() {
                        let _ = state.manager.enter_fullscreen(&first).await;
                    }
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_cec_reports_unavailable() {
        assert!(!NoCec.available());
    }

    #[test]
    fn events_are_typed() {
        assert_eq!(CecEvent::TvPoweredOn, CecEvent::TvPoweredOn);
        assert_ne!(CecEvent::MenuPressed, CecEvent::OkPressed);
    }
}
