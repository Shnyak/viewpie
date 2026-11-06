use std::path::PathBuf;
use std::process::Stdio;
use tokio::io::AsyncWriteExt;
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::Mutex;

struct RunningChild {
    child: Child,
    stdin: ChildStdin,
}

pub struct NativeRendererController {
    config_path: String,
    binary_path: PathBuf,
    child: Mutex<Option<RunningChild>>,
    operation: Mutex<()>,
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
            operation: Mutex::new(()),
        }
    }

    pub async fn select_panel(&self, panel: &str) -> anyhow::Result<()> {
        self.restart(panel, None).await
    }

    pub async fn select_fullscreen(&self, camera: &str, panel: &str) -> anyhow::Result<()> {
        self.restart(panel, Some(camera)).await
    }

    pub async fn set_display_power(&self, on: bool) -> anyhow::Result<()> {
        let _operation = self.operation.lock().await;
        let mut guard = self.child.lock().await;

        let running = guard
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("native renderer is not running"))?;

        let command = if on { "ON\n" } else { "OFF\n" };
        running.stdin.write_all(command.as_bytes()).await?;
        running.stdin.flush().await?;

        Ok(())
    }

    async fn restart(&self, panel: &str, fullscreen: Option<&str>) -> anyhow::Result<()> {
        let _operation = self.operation.lock().await;
        self.stop_inner().await;

        let mut command = Command::new(&self.binary_path);
        command
            .arg(&self.config_path)
            .env("VIEWPIE_PANEL", panel)
            .stdin(Stdio::piped())
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

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow::anyhow!("failed to open native renderer stdin"))?;

        tokio::time::sleep(std::time::Duration::from_millis(250)).await;

        if let Some(status) = child.try_wait()? {
            anyhow::bail!("native renderer exited during startup with status {status}");
        }

        *self.child.lock().await = Some(RunningChild { child, stdin });
        Ok(())
    }

    pub async fn stop(&self) {
        let _operation = self.operation.lock().await;
        self.stop_inner().await;
    }

    async fn stop_inner(&self) {
        if let Some(mut running) = self.child.lock().await.take() {
            // Ask the renderer to drain page flips and join every decoder
            // before considering a forced termination.
            if let Err(error) = running.stdin.write_all(b"STOP\n").await {
                tracing::warn!("could not send graceful STOP to native renderer: {error}");
            } else if let Err(error) = running.stdin.flush().await {
                tracing::warn!("could not flush graceful STOP to native renderer: {error}");
            }
            drop(running.stdin);

            match tokio::time::timeout(
                std::time::Duration::from_secs(12),
                running.child.wait(),
            )
            .await
            {
                Ok(Ok(status)) => {
                    tracing::info!("native renderer stopped cleanly: {status}");
                }
                Ok(Err(error)) => {
                    tracing::warn!("waiting for native renderer failed: {error}");
                    let _ = running.child.kill().await;
                    let _ = running.child.wait().await;
                }
                Err(_) => {
                    tracing::warn!(
                        "native renderer did not finish graceful shutdown in 12s; forcing termination"
                    );
                    let _ = running.child.kill().await;
                    let _ = running.child.wait().await;
                }
            }
        }
    }
}
