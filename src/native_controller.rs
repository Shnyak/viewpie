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
        if panel.contains('\n') || panel.contains('\r') {
            anyhow::bail!("panel id contains a line break");
        }
        // A panel change starts a fresh native renderer. This deliberately
        // serializes H.264 decoder sessions: the previous process is killed
        // and reaped before the new panel opens its RTSP streams. In-process
        // prewarming briefly ran both panels' V4L2/MMAL decoders together and
        // caused bcm2835-codec/VCHIQ allocation failures on the Pi 3.
        self.restart(panel, None).await
    }

    /// Reload the YAML-backed scene set after a configuration mutation.
    /// Unlike ordinary panel selection, this intentionally restarts the
    /// renderer so newly added cameras, panel definitions and settings are read.
    pub async fn reload(&self, panel: &str) -> anyhow::Result<()> {
        self.restart(panel, None).await
    }

    pub async fn select_fullscreen(&self, camera: &str, panel: &str) -> anyhow::Result<()> {
        if panel.contains('\n')
            || panel.contains('\r')
            || camera.contains('\n')
            || camera.contains('\r')
        {
            anyhow::bail!("panel or camera id contains a line break");
        }
        let _operation = self.operation.lock().await;
        let mut guard = self.child.lock().await;
        let exited = match guard.as_mut() {
            Some(running) => running.child.try_wait()?.is_some(),
            None => false,
        };
        if exited {
            guard.take();
        }
        if let Some(running) = guard.as_mut() {
            let commands = format!("FULLSCREEN {camera}\n");
            running.stdin.write_all(commands.as_bytes()).await?;
            running.stdin.flush().await?;
            return Ok(());
        }
        drop(guard);
        self.start_inner(panel, Some(camera)).await
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
        self.start_inner(panel, fullscreen).await
    }

    async fn start_inner(&self, panel: &str, fullscreen: Option<&str>) -> anyhow::Result<()> {
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
            // A graceful STOP asks every FFmpeg decoder to drain and join, but
            // decoder reads can remain blocked on a live RTSP stream. Terminate
            // the renderer directly and wait for exit before starting another:
            // its decoder threads are in the same process, and the kernel
            // releases its DRM/FFmpeg/V4L2 resources when the process exits.
            // This is also what prevents overlapping hardware decoder sessions
            // during a panel change on resource-constrained Raspberry Pi boards.
            let _ = running.child.kill().await;
            let _ = running.child.wait().await;
        }
    }
}
