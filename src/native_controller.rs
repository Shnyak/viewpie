use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{watch, Mutex, RwLock};

struct RunningChild {
    child: Child,
    stdin: ChildStdin,
}

pub struct NativeRendererController {
    config_path: String,
    binary_path: PathBuf,
    child: Mutex<Option<RunningChild>>,
    operation: Mutex<()>,
    decoder_status: Arc<RwLock<HashMap<String, serde_json::Value>>>,
    display_on: AtomicBool,
    renderer_state: watch::Sender<Option<serde_json::Value>>,
}

impl NativeRendererController {
    pub fn new(config_path: impl Into<String>, initial_display_on: bool) -> Self {
        let binary_path = std::env::var_os("VIEWPIE_NATIVE_RENDERER")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::current_exe()
                    .ok()
                    .and_then(|p| p.parent().map(|d| d.join("viewpie-native")))
            })
            .unwrap_or_else(|| PathBuf::from("viewpie-native"));

        let (renderer_state, _) = watch::channel(None);
        Self {
            config_path: config_path.into(),
            binary_path,
            child: Mutex::new(None),
            operation: Mutex::new(()),
            decoder_status: Arc::new(RwLock::new(HashMap::new())),
            display_on: AtomicBool::new(initial_display_on),
            renderer_state,
        }
    }

    /// Shared live decoder telemetry consumed by the HTTP status endpoint.
    pub fn decoder_status_handle(&self) -> Arc<RwLock<HashMap<String, serde_json::Value>>> {
        self.decoder_status.clone()
    }

    pub async fn select_panel(&self, panel: &str) -> anyhow::Result<()> {
        self.renderer_state.send_replace(None);
        if panel.contains('\n') || panel.contains('\r') {
            anyhow::bail!("panel id contains a line break");
        }
        let _operation = self.operation.lock().await;
        let mut guard = self.child.lock().await;
        let exited = match guard.as_mut() {
            Some(running) => running.child.try_wait()?.is_some(),
            None => true,
        };
        if exited {
            // try_wait reaps an exited child. Only a missing/dead renderer
            // needs a new process; ordinary panel changes stay in-process.
            self.decoder_status.write().await.clear();
            guard.take();
            drop(guard);
            return self.start_inner(panel, None).await;
        }

        // The native renderer already owns every configured Source and
        // supports PANEL over stdin. Keep all grid decoder sessions alive and
        // let its render loop switch to the incoming scene's existing frames.
        let running = guard.as_mut().expect("live renderer was checked above");
        let command = format!("PANEL {panel}\n");
        running.stdin.write_all(command.as_bytes()).await?;
        running.stdin.flush().await?;
        Ok(())
    }

    /// Reload the YAML-backed scene set after a configuration mutation.
    /// Unlike ordinary panel selection, this intentionally restarts the
    /// renderer so newly added cameras, panel definitions and settings are read.
    pub async fn reload(&self, panel: &str, fullscreen: Option<&str>) -> anyhow::Result<()> {
        self.restart(panel, fullscreen).await
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
        {
            let mut guard = self.child.lock().await;
            let exited = match guard.as_mut() {
                Some(running) => running.child.try_wait()?.is_some(),
                None => false,
            };
            if exited {
                guard.take();
            }
            let current_renderer_state = self.renderer_state.subscribe().borrow().clone();
            let already_visible = current_renderer_state.as_ref().is_some_and(|state| {
                state.get("status").and_then(|value| value.as_str()) == Some("committed")
                    && state.get("mode").and_then(|value| value.as_str()) == Some("fullscreen")
                    && state.get("camera").and_then(|value| value.as_str()) == Some(camera)
            });
            if already_visible && !exited && guard.is_some() {
                return Ok(());
            }
            // Keep the last committed renderer state while a new camera
            // connects. The native renderer retains its current KMS frame
            // until the requested camera has produced a replacement.
            if let Some(running) = guard.as_mut() {
                let command = format!("FULLSCREEN {camera}\n");
                running.stdin.write_all(command.as_bytes()).await?;
                running.stdin.flush().await?;
            } else {
                drop(guard);
                self.start_inner(panel, Some(camera)).await?;
            }
        }
        // Do not serialize later camera selections behind this bounded wait:
        // another request can replace an unavailable camera immediately.
        drop(_operation);
        self.wait_for_fullscreen(camera).await
    }

    async fn wait_for_fullscreen(&self, camera: &str) -> anyhow::Result<()> {
        let mut receiver = self.renderer_state.subscribe();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(6);
        loop {
            if let Some(state) = receiver.borrow().clone() {
                let reported_camera = state.get("camera").and_then(|value| value.as_str());
                let status = state.get("status").and_then(|value| value.as_str());
                if reported_camera == Some(camera) && status == Some("committed") {
                    return Ok(());
                }
                if reported_camera == Some(camera) && status == Some("error") {
                    let message = state
                        .get("error")
                        .and_then(|value| value.as_str())
                        .unwrap_or("native renderer rejected the fullscreen frame");
                    anyhow::bail!("{message}");
                }
            }
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                anyhow::bail!(
                    "native renderer did not confirm a visible frame for camera {camera} within 6 seconds"
                );
            }
            tokio::time::timeout(remaining, receiver.changed())
                .await
                .map_err(|_| anyhow::anyhow!(
                    "native renderer did not confirm a visible frame for camera {camera} within 10 seconds"
                ))?
                .map_err(|_| anyhow::anyhow!("native renderer status channel closed"))?;
        }
    }

    pub async fn set_display_power(&self, on: bool) -> anyhow::Result<()> {
        // Keep the requested state even if the child is currently unavailable;
        // the next renderer start must honor the API's persisted preference.
        self.display_on.store(on, Ordering::Release);
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
        self.renderer_state.send_replace(None);
        let _operation = self.operation.lock().await;
        self.decoder_status.write().await.clear();
        self.stop_inner().await;
        self.start_inner(panel, fullscreen).await
    }

    async fn start_inner(&self, panel: &str, fullscreen: Option<&str>) -> anyhow::Result<()> {
        let mut command = Command::new(&self.binary_path);
        command
            .arg(&self.config_path)
            .env("VIEWPIE_PANEL", panel)
            .env(
                "VIEWPIE_SCREEN_ON",
                if self.display_on.load(Ordering::Acquire) {
                    "1"
                } else {
                    "0"
                },
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
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
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("failed to open native renderer stdout"))?;
        let decoder_status = self.decoder_status.clone();
        let renderer_state = self.renderer_state.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if let Some(payload) = line
                    .strip_prefix("VIEWPIE_RENDERER_STATE ")
                    .or_else(|| line.strip_prefix("VIEWPIE_RENDERER_ERROR "))
                {
                    match serde_json::from_str::<serde_json::Value>(payload) {
                        Ok(status) => {
                            renderer_state.send_replace(Some(status));
                        }
                        Err(error) => {
                            tracing::warn!("invalid native renderer state event: {error}")
                        }
                    }
                } else if let Some(payload) = line.strip_prefix("VIEWPIE_DECODER_STATUS ") {
                    match serde_json::from_str::<serde_json::Value>(payload) {
                        Ok(status) => {
                            if let Some(camera) = status.get("camera").and_then(|v| v.as_str()) {
                                decoder_status
                                    .write()
                                    .await
                                    .insert(camera.to_string(), status);
                            }
                        }
                        Err(error) => {
                            tracing::warn!("invalid native decoder status event: {error}")
                        }
                    }
                } else {
                    // Preserve the renderer's existing journal output while
                    // reserving the prefixed line for machine-readable events.
                    tracing::info!(target: "viewpie::native", "{}", line);
                }
            }
        });

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
