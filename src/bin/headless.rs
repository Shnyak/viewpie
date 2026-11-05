// This file contains modifications generated with assistance from Qwen AI
// Original project: https://github.com/snoack/viewpie
// SPDX-License-Identifier: GPL-3.0-or-later

//! Headless variant of viewpie: identical application logic (config,
//! panels, reference-counted pool, HTTP API) but with the in-memory
//! display backend and no CEC. Used for integration testing and as a
//! dry-run mode (`viewpie-headless /path/to/viewpie.yaml`).

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Context;
use tracing::info;
use tracing_subscriber::EnvFilter;

use viewpie::config::Config;
use viewpie::http_api;
use viewpie::kms::{DisplayBackend, HeadlessBackend};
use viewpie::wall::Resolution;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,viewpie=debug"));
    tracing_subscriber::fmt().with_env_filter(filter).init();

    let config_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/tmp/viewpie.headless.yaml".to_string());
    let bind: SocketAddr = std::env::var("VIEWPIE_API_BIND")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| "127.0.0.1:8080".parse().unwrap());

    let cfg = Config::load(&config_path)
        .with_context(|| format!("loading configuration from {config_path}"))?;

    let backend: Arc<dyn DisplayBackend> = Arc::new(HeadlessBackend::new());
    let resolution = Resolution {
        width: 1920,
        height: 1080,
    };

    let state = http_api::run(cfg, backend, resolution, &config_path, bind, None).await?;
    info!(%bind, "headless viewpie ready");

    tokio::signal::ctrl_c().await?;
    let _ = state.manager.pause().await;
    Ok(())
}
