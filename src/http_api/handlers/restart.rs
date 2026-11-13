// SPDX-License-Identifier: GPL-3.0-or-later
//! Token-protected host restart endpoint.
use std::time::Duration;

use axum::http::HeaderMap;
use axum::Json;
use tracing::error;

use crate::http_api::auth::{authorize, token_configured};
use crate::http_api::errors::ApiError;
use crate::http_api::models::OkResponse;

const REBOOT_DELAY: Duration = Duration::from_millis(750);

pub fn restart_available() -> bool {
    token_configured() && has_cap_sys_boot()
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

pub async fn restart(headers: HeaderMap) -> Result<Json<OkResponse>, ApiError> {
    authorize(&headers)?;
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
    fn capability_is_detected_from_proc_status() {
        let with_cap = "Name:\tviewpie\nCapEff:\t0000000000400000\n";
        let without_cap = "Name:\tviewpie\nCapEff:\t0000000000000000\n";
        assert!(has_cap_sys_boot_in_status(with_cap));
        assert!(!has_cap_sys_boot_in_status(without_cap));
        assert!(!has_cap_sys_boot_in_status("Name:\tviewpie\n"));
    }
}
