// This file contains modifications generated with assistance from Qwen AI
// Original project: https://github.com/snoack/viewpie
// SPDX-License-Identifier: GPL-3.0-or-later

//! Embedded HTTP REST API (axum, port 8080 by default).
//!
//! Responsibilities of this module:
//! * build the [`AppState`] shared by all handlers,
//! * run background tasks: screen-state monitor (auto-pause when the
//!   display switches off — works without CEC via DPMS sysfs), rotation
//!   timer and fullscreen timeout enforcement,
//! * serve the router with CORS restricted to private network origins.

pub mod errors;
pub mod handlers;
pub mod models;
pub mod routes;
pub mod state;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tower_http::cors::{AllowOrigin, CorsLayer};
use tracing::{error, info, warn};

pub use state::{AppState, PanelRuntimeState, WallCommand, WallState};

use crate::config::Config;
use crate::kms::{check_screen_via_kms, DisplayBackend};
use crate::wall::Resolution;

/// Start the complete application: state bootstrap + HTTP server +
/// background tasks. Commands sent on the internal channel are applied by
/// a serialized pump so that CEC events (hardware build) and REST calls
/// share one code path.
pub async fn run(
    cfg: Config,
    backend: Arc<dyn DisplayBackend>,
    resolution: Resolution,
    config_path: &str,
    bind: SocketAddr,
    native_renderer: Option<Arc<crate::native_controller::NativeRendererController>>,
) -> anyhow::Result<AppState> {
    let (state, mut rx) = AppState::bootstrap(cfg, backend.clone(), resolution, config_path).await;

    // Command pump.
    let st = state.clone();
    let command_renderer = native_renderer.clone();
    tokio::spawn(async move {
        while let Some(cmd) = rx.recv().await {
            match cmd {
                WallCommand::SwitchPanel(id) => {
                    match st.manager.switch_to(&id).await {
                        Ok(()) => {
                            if let Some(renderer) = &command_renderer {
                                if let Err(e) = renderer.select_panel(&id).await {
                                    warn!("native renderer failed to switch to panel {id}: {e}");
                                }
                            }
                        }
                        Err(e) => warn!("cmd switch {id}: {e}"),
                    }
                }
                WallCommand::SetFullscreen(cam, dur) => {
                    match st.manager.enter_fullscreen(&cam).await {
                        Ok(()) => {
                            if let Some(renderer) = &command_renderer {
                                let panel = st.manager.active_id().await;
                                if let Err(e) = renderer.select_fullscreen(&cam, &panel).await {
                                    warn!("native renderer failed to enter fullscreen for {cam}: {e}");
                                }
                            }
                        }
                        Err(e) => warn!("cmd fullscreen {cam}: {e}"),
                    }
                    if let Some(sec) = dur {
                        let s2 = st.clone();
                        let timeout_renderer = command_renderer.clone();
                        tokio::spawn(async move {
                            tokio::time::sleep(Duration::from_secs(sec)).await;
                            if crate::http_api::state::current_fullscreen()
                                .await
                                .as_deref()
                                .is_none()
                            {
                                return;
                            }
                            let _ = s2.manager.leave_fullscreen().await;
                            if let Some(renderer) = &timeout_renderer {
                                let panel = s2.manager.active_id().await;
                                if let Err(e) = renderer.select_panel(&panel).await {
                                    warn!("native renderer failed to exit fullscreen: {e}");
                                }
                            }
                        });
                    }
                }
                WallCommand::ReturnToGrid => {
                    let _ = st.manager.leave_fullscreen().await;
                    if let Some(renderer) = &command_renderer {
                        let panel = st.manager.active_id().await;
                        if let Err(e) = renderer.select_panel(&panel).await {
                            warn!("native renderer failed to return to panel {panel}: {e}");
                        }
                    }
                }
                WallCommand::Pause => {
                    if !crate::http_api::state::is_paused_now().await {
                        let _ = st.manager.pause().await;
                        if let Some(renderer) = &command_renderer {
                            renderer.stop().await;
                        }
                    }
                }
                WallCommand::Resume => {
                    if crate::http_api::state::is_paused_now().await {
                        let _ = st.manager.resume().await;
                        if let Some(renderer) = &command_renderer {
                            let panel = st.manager.active_id().await;
                            if let Err(e) = renderer.select_panel(&panel).await {
                                warn!("native renderer failed to resume panel {panel}: {e}");
                            }
                        }
                    }
                }
                WallCommand::UpdateSettings(_) | WallCommand::ReloadConfig => {
                    // Already applied synchronously by the originating handler.
                    if let Some(renderer) = &command_renderer {
                        let panel = st.manager.active_id().await;
                        if let Err(e) = renderer.select_panel(&panel).await {
                            warn!("native renderer failed to apply updated settings: {e}");
                        }
                    }
                }
            }
        }
    });

    spawn_background_tasks(state.clone(), native_renderer.clone());

    let app = routes::router(state.clone()).layer(cors_layer());
    let listener = tokio::net::TcpListener::bind(bind).await?;
    info!(%bind, "HTTP API listening");
    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            error!("HTTP server error: {e}");
        }
    });
    Ok(state)
}

/// Build an application state + router without binding a socket. Used by
/// integration tests (`oneshot`) and by embedders wanting custom serving.
pub async fn build_app(
    cfg: Config,
    backend: Arc<dyn DisplayBackend>,
    resolution: Resolution,
    config_path: &str,
) -> (AppState, axum::Router) {
    let (state, _rx) = AppState::bootstrap(cfg, backend, resolution, config_path).await;
    let app = routes::router(state.clone()).layer(cors_layer());
    (state, app)
}

/// Background loops: auto-pause on screen off, viewport rotation,
/// fullscreen watchdog.
fn spawn_background_tasks(
    state: AppState,
    native_renderer: Option<Arc<crate::native_controller::NativeRendererController>>,
) {
    // ── Screen state monitor (pause_off_screen) ──
    let st = state.clone();
    let screen_renderer = native_renderer.clone();
    tokio::spawn(async move {
        loop {
            let (pause_off, paused, auto_paused) = {
                let s = st.settings.read().await;
                let w = st.wall_state.read().await;
                (s.pause_off_screen, w.paused, w.auto_paused)
            };
            if pause_off {
                let screen_on = check_screen_via_kms().await;
                if !screen_on && !paused {
                    info!("screen switched off — auto-pausing wall");
                    if let Err(e) = st.manager.pause().await {
                        error!("auto-pause failed: {e}");
                    }
                    if let Some(renderer) = &screen_renderer {
                        renderer.stop().await;
                    }
                    crate::http_api::state::set_auto_paused(true).await;
                } else if screen_on && auto_paused {
                    if paused {
                        info!("screen back on — auto-resuming wall");
                        if let Err(e) = st.manager.resume().await {
                            error!("auto-resume failed: {e}");
                        }
                        if let Some(renderer) = &screen_renderer {
                            let panel = st.manager.active_id().await;
                            if let Err(e) = renderer.select_panel(&panel).await {
                                error!("native renderer auto-resume failed: {e}");
                            }
                        }
                    }
                    crate::http_api::state::set_auto_paused(false).await;
                }
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    });

    // ── Viewport rotation timer ──
    let st = state.clone();
    tokio::spawn(async move {
        loop {
            let interval = st.settings.read().await.rotation_interval.max(5);
            tokio::time::sleep(Duration::from_secs(interval)).await;
            if crate::http_api::state::is_paused_now().await {
                continue;
            }
            if crate::http_api::state::current_fullscreen().await.is_some() {
                continue; // don't rotate behind a fullscreen view
            }
            if let Err(e) = st.manager.rotate_step().await {
                warn!("rotation step failed: {e}");
            }
        }
    });

    // ── Fullscreen watchdog (enforces fullscreen_timeout robustly) ──
    let st = state.clone();
    let fullscreen_renderer = native_renderer.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            let started = {
                let w = st.wall_state.read().await;
                match (&w.fullscreen_camera, w.fullscreen_started) {
                    (Some(_), Some(t0)) => Some(t0),
                    _ => None,
                }
            };
            if let Some(t0) = started {
                let timeout = st.settings.read().await.fullscreen_timeout;
                let age = chrono::Utc::now()
                    .signed_duration_since(t0)
                    .num_seconds()
                    .max(0) as u64;
                if age > timeout {
                    info!("fullscreen watchdog: returning to grid after {age}s");
                    let _ = st.manager.leave_fullscreen().await;
                    if let Some(renderer) = &fullscreen_renderer {
                        let panel = st.manager.active_id().await;
                        if let Err(e) = renderer.select_panel(&panel).await {
                            warn!("native renderer failed to exit fullscreen: {e}");
                        }
                    }
                }
            }
        }
    });
}

/// CORS: allow requests from private LAN origins only (the RPi serves a
/// local dashboard; public-internet origins are rejected).
fn cors_layer() -> CorsLayer {
    let allow_private = AllowOrigin::predicate(|origin: &axum::http::HeaderValue, _| {
        let Some(o) = origin.to_str().ok() else {
            return false;
        };
        let Some(rest) = o.split("://").nth(1) else {
            return false;
        };
        let host = rest.split(':').next().unwrap_or("");
        is_private_host(host)
    });
    CorsLayer::new()
        .allow_origin(allow_private)
        .allow_methods(tower_http::cors::Any)
        .allow_headers(tower_http::cors::Any)
}

fn is_private_host(host: &str) -> bool {
    if host == "localhost" || host.ends_with(".local") || host.ends_with(".lan") {
        return true;
    }
    let Ok(ip) = host.parse::<std::net::IpAddr>() else {
        return false;
    };
    match ip {
        std::net::IpAddr::V4(v4) => {
            let o = v4.octets();
            o[0] == 10
                || (o[0] == 172 && (16..=31).contains(&o[1]))
                || (o[0] == 192 && o[1] == 168)
                || o[0] == 127
        }
        std::net::IpAddr::V6(v6) => v6.is_loopback(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_host_detection() {
        assert!(is_private_host("192.168.1.10"));
        assert!(is_private_host("10.0.0.5"));
        assert!(is_private_host("172.16.0.1"));
        assert!(!is_private_host("172.32.0.1"));
        assert!(is_private_host("127.0.0.1"));
        assert!(is_private_host("localhost"));
        assert!(is_private_host("homeassistant.local"));
        assert!(!is_private_host("evil.example.com"));
        assert!(!is_private_host("8.8.8.8"));
    }
}
