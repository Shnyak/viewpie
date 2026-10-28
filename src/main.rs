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
#[cfg(all(
    target_os = "linux",
    any(target_arch = "arm", target_arch = "aarch64"),
    feature = "hw"
))]
use viewpie::kms::KmsBackend;
use viewpie::kms::{DisplayBackend, HeadlessBackend};
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

    let rt = tokio::runtime::Runtime::new().context("initialising tokio runtime")?;
    rt.block_on(async move {
        let state = http_api::run(cfg, backend.clone(), resolution, &config_path, bind)
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
        info!("shutting down: pausing decoders cleanly");
        if let Err(e) = state.manager.pause().await {
            error!("shutdown pause failed: {e}");
        }
        Ok::<(), anyhow::Error>(())
    })
}

/// Pick the best display backend for the current platform.
fn create_backend() -> anyhow::Result<Arc<dyn DisplayBackend>> {
    #[cfg(all(
        target_os = "linux",
        any(target_arch = "arm", target_arch = "aarch64"),
        feature = "hw"
    ))]
    {
        if viewpie::kms::find_drm_device().is_some() {
            match KmsBackend::open() {
                Ok(k) => {
                    info!("using hardware KMS backend");
                    return Ok(Arc::new(k));
                }
                Err(e) => {
                    tracing::warn!("KMS open failed ({e}), using headless backend");
                }
            }
        }
    }
    info!("using headless display backend");
    Ok(Arc::new(HeadlessBackend::new()))
}
