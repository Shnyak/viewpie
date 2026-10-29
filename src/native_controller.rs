//! Supervises the real FFmpeg/DRM renderer as a child process.
//! Restarting on scene changes releases DRM state cleanly and applies the new panel.
use std::path::PathBuf;
use std::process::Stdio;
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

pub struct NativeRendererController {
    config_path: String,
    binary_path: PathBuf,
    child: Mutex<Option<Child>>,
}

impl NativeRendererController {
    pub fn new(config_path: impl Into<String>) -> Self {
        let binary_path = std::env::var_os("VIEWPIE_NATIVE_RENDERER")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::current_exe()
                    .ok()
                    .and_then(|p| p.parent().map(|d| d.join("viewpie-native")))
            })
            .unwrap_or_else(|| PathBuf::from("viewpie-native"));
        Self {
            config_path: config_path.into(),
            binary_path,
            child: Mutex::new(None),
        }
    }

    pub async fn select_panel(&self, panel: &str) -> anyhow::Result<()> {
        self.restart(panel, None).await
    }

    pub async fn select_fullscreen(&self, camera: &str, panel: &str) -> anyhow::Result<()> {
        self.restart(panel, Some(camera)).await
    }

    async fn restart(&self, panel: &str, fullscreen: Option<&str>) -> anyhow::Result<()> {
        self.stop().await;
        let mut command = Command::new(&self.binary_path);
        command
            .arg(&self.config_path)
            .env("VIEWPIE_PANEL", panel)
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        if let Some(camera) = fullscreen {
            command.env("VIEWPIE_FULLSCREEN", camera);
        } else {
            command.env_remove("VIEWPIE_FULLSCREEN");
        }
        let mut child = command.spawn().map_err(|e| {
            anyhow::anyhow!(
                "failed to start native renderer {}: {e}",
                self.binary_path.display()
            )
        })?;

        // Catch immediate configuration/DRM failures instead of silently
        // retaining a dead child and claiming that the display is active.
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        if let Some(status) = child.try_wait()? {
            anyhow::bail!("native renderer exited during startup with status {status}");
        }
        *self.child.lock().await = Some(child);
        Ok(())
    }

    pub async fn stop(&self) {
        if let Some(mut child) = self.child.lock().await.take() {
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
    }
}
