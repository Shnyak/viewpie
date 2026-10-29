// This file contains modifications generated with assistance from Qwen AI
// Original project: https://github.com/snoack/viewpie
// SPDX-License-Identifier: GPL-3.0-or-later

//! viewpie entry point.
//!
//! Loads the YAML configuration, chooses a display backend (real KMS/DRM on
//! Raspberry Pi, headless elsewhere), starts the embedded HTTP API on
//! port 8080 and wires HDMI-CEC events into the same control path.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Context;
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

use viewpie::config::Config;
use viewpie::http_api;
use viewpie::kms::{DisplayBackend, HeadlessBackend};
use viewpie::native_controller::NativeRendererController;
use viewpie::wall::Resolution;
use viewpie::{cec, DEFAULT_API_BIND, DEFAULT_CONFIG_PATH};

fn main() -> anyhow::Result<()> {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,viewpie=debug"));
    tracing_subscriber::fmt().with_env_filter(filter).init();

    let config_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| DEFAULT_CONFIG_PATH.to_string());
    let bind: SocketAddr = std::env::var("VIEWPIE_API_BIND")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| DEFAULT_API_BIND.parse().unwrap());

    info!(path = %config_path, %bind, "starting viewpie fork");

    let cfg = Config::load(&config_path)
        .with_context(|| format!("loading configuration from {config_path}"))?;

    info!(
        feeds = cfg.feeds.len(),
        panels = cfg.panels.len(),
        initial_panel = %cfg.initial_panel(),
        "configuration loaded and validated"
    );
    for (name, panel) in &cfg.panels {
        info!(
            panel = %name,
            label = %panel.name,
            viewports = panel.viewports.len(),
            cameras = panel.camera_count(),
            rows = panel.grid().len(),
            "configured panel"
        );
        for viewport in &panel.viewports {
            tracing::debug!(
                panel = %name,
                slot = %viewport.slot,
                feeds = ?viewport.feeds,
                "configured viewport"
            );
        }
    }

    // Resolution: from env override or default HDMI 1080p; the hardware
    // backend replaces it with the detected connector mode.
    let resolution = Resolution {
        width: std::env::var("VIEWPIE_WIDTH")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1920),
        height: std::env::var("VIEWPIE_HEIGHT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1080),
    };

    let backend: Arc<dyn DisplayBackend> = create_backend()?;
    let native_renderer = if cfg!(feature = "native-pipeline") {
        Some(Arc::new(NativeRendererController::new(config_path.clone())))
    } else {
        None
    };

    let rt = tokio::runtime::Runtime::new().context("initialising tokio runtime")?;
    rt.block_on(async move {
        if let Some(renderer) = &native_renderer {
            let initial_panel = cfg.initial_panel();
            if let Err(e) = renderer.select_panel(&initial_panel).await {
                error!(
                    panel = %initial_panel,
                    error = %e,
                    "native renderer failed to start; keeping HTTP API available"
                );
            }
        }

        let state = http_api::run(
            cfg,
            backend.clone(),
            resolution,
            &config_path,
            bind,
            native_renderer.clone(),
        )
        .await
        .context("starting HTTP API")?;

        // CEC keeps working exactly like the original project when a TV
        // is attached; harmless no-op otherwise.
        let adapter: Arc<dyn cec::CecAdapter> = cec::default_adapter();
        cec::spawn_cec_handler(state.clone(), adapter);

        info!("viewpie running — press Ctrl-C to stop");
        tokio::signal::ctrl_c()
            .await
            .context("waiting for ctrl-c")?;
        info!("shutting down: stopping renderer and pausing decoders cleanly");
        if let Some(renderer) = &native_renderer {
            renderer.stop().await;
        }
        if let Err(e) = state.manager.pause().await {
            error!("shutdown pause failed: {e}");
        }
        Ok::<(), anyhow::Error>(())
    })
}

/// The API manager tracks layout state; the native renderer child owns the DRM card.
fn create_backend() -> anyhow::Result<Arc<dyn DisplayBackend>> {
    if cfg!(feature = "native-pipeline") {
        info!("using headless layout backend; native renderer owns physical DRM/KMS output");
    } else {
        tracing::warn!("using headless display backend: no video will be shown on a physical monitor");
    }
    Ok(Arc::new(HeadlessBackend::new()))
}
