//! `virtual: true` — a managed Xvfb server (Linux only).
//!
//! When the launch config asks for a virtual display and neither `DISPLAY`
//! nor `WAYLAND_DISPLAY` is set, [`VirtualDisplay::start_if_needed`] finds
//! `Xvfb` on `PATH` (the error names `apt install xvfb` when it is
//! missing), picks the first free display number from `:99` by probing
//! `/tmp/.X<N>-lock` and `/tmp/.X11-unix/X<N>`, starts
//! `Xvfb :N -screen 0 WxHx24 -nolisten tcp`, waits up to 5 s for the socket
//! to appear, and hands back the `DISPLAY` value to put in the **child's**
//! environment only (the parent's environment is never modified). The
//! server is killed on [`VirtualDisplay::stop`] or drop.
//!
//! On macOS and Windows `virtual: true` is a no-op with one `info` log line.

use crate::error::BrowserError;
use crate::launch::LaunchConfig;

/// Whether a display is available for a headed browser. Used to decide the
/// `headless` default: macOS and Windows always have one; Linux needs
/// `DISPLAY` / `WAYLAND_DISPLAY`, or a virtual display Surf will start.
pub fn has_display() -> bool {
    if cfg!(any(target_os = "macos", target_os = "windows")) {
        return true;
    }
    env_set("DISPLAY") || env_set("WAYLAND_DISPLAY")
}

fn env_set(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|v| !v.is_empty())
}

/// A running Xvfb server (only ever constructed on Linux).
#[derive(Debug)]
pub struct VirtualDisplay {
    display: String,
    #[cfg(target_os = "linux")]
    child: std::sync::Mutex<Option<tokio::process::Child>>,
}

impl VirtualDisplay {
    /// Start Xvfb if `cfg.virtual_display` is set, the platform is Linux and
    /// no display is already available. `Ok(None)` otherwise.
    pub async fn start_if_needed(
        cfg: &LaunchConfig,
    ) -> Result<Option<VirtualDisplay>, BrowserError> {
        if !cfg.virtual_display {
            return Ok(None);
        }
        #[cfg(not(target_os = "linux"))]
        {
            tracing::info!("virtual: true has no effect on this platform (no Xvfb needed)");
            Ok(None)
        }
        #[cfg(target_os = "linux")]
        {
            if env_set("DISPLAY") || env_set("WAYLAND_DISPLAY") {
                tracing::info!("virtual: true ignored — a display is already available");
                return Ok(None);
            }
            linux::start(cfg.size.0, cfg.size.1).await.map(Some)
        }
    }

    /// The `DISPLAY` value (e.g. `:99`).
    pub fn display(&self) -> &str {
        &self.display
    }

    /// Kill the server and wait for it to exit. Idempotent.
    pub async fn stop(&self) {
        #[cfg(target_os = "linux")]
        {
            let child = self.child.lock().expect("xvfb child poisoned").take();
            if let Some(mut child) = child {
                let _ = child.start_kill();
                let _ = child.wait().await;
                tracing::debug!("Xvfb {} stopped", self.display.as_str());
            }
        }
    }
}

impl Drop for VirtualDisplay {
    fn drop(&mut self) {
        #[cfg(target_os = "linux")]
        if let Ok(mut guard) = self.child.lock() {
            if let Some(child) = guard.as_mut() {
                // `kill_on_drop` covers the process; this just makes the
                // intent explicit when the runtime is still alive.
                let _ = child.start_kill();
            }
        }
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::VirtualDisplay;
    use crate::discovery::which;
    use crate::error::BrowserError;
    use std::path::Path;
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    /// First display number to probe.
    const FIRST_DISPLAY: u32 = 99;
    /// Last display number to probe.
    const LAST_DISPLAY: u32 = 299;
    /// How long to wait for the X socket after starting Xvfb.
    const SOCKET_TIMEOUT: Duration = Duration::from_secs(5);

    fn lock_path(n: u32) -> std::path::PathBuf {
        Path::new("/tmp").join(format!(".X{n}-lock"))
    }

    fn socket_path(n: u32) -> std::path::PathBuf {
        Path::new("/tmp/.X11-unix").join(format!("X{n}"))
    }

    fn free_display() -> Option<u32> {
        (FIRST_DISPLAY..=LAST_DISPLAY).find(|&n| !lock_path(n).exists() && !socket_path(n).exists())
    }

    pub(super) async fn start(width: u32, height: u32) -> Result<VirtualDisplay, BrowserError> {
        let xvfb = which("Xvfb").ok_or_else(|| {
            BrowserError::Launch(
                "virtual: true needs Xvfb, which is not on PATH — install it with `apt install xvfb` \
                 (or set DISPLAY to an existing X server)"
                    .into(),
            )
        })?;
        let n = free_display().ok_or_else(|| {
            BrowserError::Launch(format!(
                "no free X display between :{FIRST_DISPLAY} and :{LAST_DISPLAY} (stale /tmp/.X*-lock files?)"
            ))
        })?;
        let name = format!(":{n}");
        let mut child = tokio::process::Command::new(&xvfb)
            .arg(&name)
            .arg("-screen")
            .arg("0")
            .arg(format!("{width}x{height}x24"))
            .arg("-nolisten")
            .arg("tcp")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| {
                BrowserError::Launch(format!("could not start {}: {e}", xvfb.display()))
            })?;

        let deadline = Instant::now() + SOCKET_TIMEOUT;
        loop {
            if socket_path(n).exists() {
                break;
            }
            if let Ok(Some(status)) = child.try_wait() {
                return Err(BrowserError::Launch(format!(
                    "Xvfb {name} exited during startup ({status})"
                )));
            }
            if Instant::now() >= deadline {
                let _ = child.start_kill();
                let _ = child.wait().await;
                return Err(BrowserError::Launch(format!(
                    "Xvfb {name} did not create {} within {SOCKET_TIMEOUT:?}",
                    socket_path(n).display()
                )));
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        tracing::info!("virtual display {name} ({width}x{height}) started");
        Ok(VirtualDisplay {
            display: name,
            child: std::sync::Mutex::new(Some(child)),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn virtual_is_a_noop_when_not_requested() {
        let cfg = LaunchConfig {
            virtual_display: false,
            ..LaunchConfig::for_path("/nonexistent")
        };
        assert!(VirtualDisplay::start_if_needed(&cfg)
            .await
            .unwrap()
            .is_none());
    }

    #[cfg(not(target_os = "linux"))]
    #[tokio::test]
    async fn virtual_is_a_noop_off_linux() {
        let cfg = LaunchConfig {
            virtual_display: true,
            ..LaunchConfig::for_path("/nonexistent")
        };
        assert!(VirtualDisplay::start_if_needed(&cfg)
            .await
            .unwrap()
            .is_none());
        assert!(has_display());
    }
}
