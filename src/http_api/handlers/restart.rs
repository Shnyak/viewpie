// SPDX-License-Identifier: GPL-3.0-or-later
//! Token-protected host restart endpoint.
use std::time::Duration;

use axum::http::{header::AUTHORIZATION, HeaderMap};
use axum::Json;
use tracing::error;

use crate::http_api::errors::ApiError;
use crate::http_api::models::OkResponse;

const RESTART_TOKEN_ENV: &str = "VIEWPIE_RESTART_TOKEN";
const REBOOT_DELAY: Duration = Duration::from_millis(750);

pub fn restart_available() -> bool {
    let token_configured = std::env::var(RESTART_TOKEN_ENV)
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false);
    token_configured && has_cap_sys_boot()
}

fn has_cap_sys_boot() -> bool {
    let Ok(status) = std::fs::read_to_string("/proc/self/status") else {
        return false;
    };
    has_cap_sys_boot_in_status(&status)
}

fn has_cap_sys_boot_in_status(status: &str) -> bool {
    const CAP_SYS_BOOT: u32 = 22;
    let Some(value) = status
        .lines()
        .find_map(|line| line.strip_prefix("CapEff:"))
        .map(str::trim)
    else {
        return false;
    };
    u64::from_str_radix(value, 16)
        .map(|effective| effective & (1_u64 << CAP_SYS_BOOT) != 0)
        .unwrap_or(false)
}

fn constant_time_eq(expected: &[u8], supplied: &[u8]) -> bool {
    let mut difference = expected.len() ^ supplied.len();
    for index in 0..expected.len().max(supplied.len()) {
        difference |=
            usize::from(*expected.get(index).unwrap_or(&0) ^ *supplied.get(index).unwrap_or(&0));
    }
    difference == 0
}

pub async fn restart(headers: HeaderMap) -> Result<Json<OkResponse>, ApiError> {
    let expected = std::env::var(RESTART_TOKEN_ENV).unwrap_or_default();
    if expected.trim().is_empty() {
        return Err(ApiError::Unavailable(
            "restart token is not configured on the Viewpie service".into(),
        ));
    }
    let supplied = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .unwrap_or("");
    if !constant_time_eq(expected.trim().as_bytes(), supplied.as_bytes()) {
        return Err(ApiError::Unauthorized);
    }
    if !has_cap_sys_boot() {
        return Err(ApiError::Unavailable(
            "Viewpie service does not have CAP_SYS_BOOT; restart is disabled".into(),
        ));
    }

    // Respond to Home Assistant before the host starts shutting down.
    tokio::spawn(async {
        tokio::time::sleep(REBOOT_DELAY).await;
        let result = tokio::task::spawn_blocking(|| {
            #[cfg(target_os = "linux")]
            {
                // CAP_SYS_BOOT is explicitly granted by the packaged systemd unit.
                unsafe { libc::reboot(libc::LINUX_REBOOT_CMD_RESTART) }
            }
            #[cfg(not(target_os = "linux"))]
            {
                -1
            }
        })
        .await;
        match result {
            Ok(0) => {}
            Ok(_) => error!(error = %std::io::Error::last_os_error(), "host reboot syscall failed"),
            Err(err) => error!(error = %err, "host reboot task failed"),
        }
    });

    Ok(Json(OkResponse::ok()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bearer_token_comparison_is_exact() {
        assert!(constant_time_eq(b"abc123", b"abc123"));
        assert!(!constant_time_eq(b"abc123", b"abc124"));
        assert!(!constant_time_eq(b"abc123", b"abc123 "));
        assert!(!constant_time_eq(b"abc123", b""));
    }

    #[test]
    fn capability_is_detected_from_proc_status() {
        let with_cap = "Name:\tviewpie\nCapEff:\t0000000000400000\n";
        let without_cap = "Name:\tviewpie\nCapEff:\t0000000000000000\n";
        assert!(has_cap_sys_boot_in_status(with_cap));
        assert!(!has_cap_sys_boot_in_status(without_cap));
        assert!(!has_cap_sys_boot_in_status("Name:\tviewpie\n"));
    }
}
