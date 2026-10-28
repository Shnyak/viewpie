// This file contains modifications generated with assistance from Qwen AI
// Original project: https://github.com/snoack/viewpie
// SPDX-License-Identifier: GPL-3.0-or-later

//! viewpie — RTSP camera wall for Raspberry Pi (fork with HTTP API).
//!
//! Library root exposing the modules of both the original application and
//! this fork's additions:
//!
//! * [`config`] — YAML configuration (feeds, panels, display settings),
//! * [`feed`] — reference-counted RTSP decoder pool,
//! * [`wall`] — pure video-wall geometry/composition helpers,
//! * [`kms`] — display backend abstraction (DRM atomic / headless),
//! * [`panel_manager`] — panel registry + graceful switching,
//! * [`http_api`] — embedded axum REST server on port 8080,
//! * [`cec`] — HDMI-CEC integration (full implementation on ARM targets,
//!   stub elsewhere so builds/tests work everywhere).

pub mod cec;
pub mod config;
pub mod feed;
pub mod http_api;
pub mod kms;
pub mod panel_manager;
pub mod wall;

pub use config::{Config, DisplaySettings};

/// Default path of the system-wide configuration file.
pub const DEFAULT_CONFIG_PATH: &str = "/etc/viewpie/viewpie.yaml";
/// Default HTTP API bind address.
pub const DEFAULT_API_BIND: &str = "0.0.0.0:8080";
