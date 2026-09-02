use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{Context, Result};
use tracing::{info, warn};

const AGENTS: &[&str] = &["NIHostIntegrationAgent", "NIHardwareAgent"];

const RESTORE: &[&str] = &[
    "/Library/Application Support/Native Instruments/Hardware/NIHostIntegrationAgent.app",
    "/Library/Application Support/Native Instruments/Hardware/NIHardwareAgent.app",
];

/// Exclusive HID+bulk: join stomp before restore so launchd is not immediately re-killed.
pub struct NihiaGuard {
    restore: bool,
    stop: Option<Arc<AtomicBool>>,
    stomp: Option<JoinHandle<()>>,
}

impl NihiaGuard {
    pub fn noop() -> Self {
        Self {
            restore: false,
            stop: None,
            stomp: None,
        }
    }

    pub fn inhibit() -> Result<Self> {
        kill_agents()?;
        for _ in 0..8 {
            std::thread::sleep(Duration::from_millis(150));
            if agents_running() {
                let _ = kill_agents();
            } else {
                break;
            }
        }
        if agents_running() {
            warn!("NI agents still present after inhibit; screen claim may fail");
        } else {
            info!("NIHostIntegrationAgent / NIHardwareAgent down");
        }
        std::thread::sleep(Duration::from_millis(400));
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let stomp = std::thread::Builder::new()
            .name("nihia-stomp".into())
            .spawn(move || {
                while !flag.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(750));
                    if flag.load(Ordering::SeqCst) {
                        break;
                    }
                    if agents_running() {
                        warn!("NIHIA respawned; stomping");
                        let _ = kill_agents();
                    }
                }
            })?;
        Ok(Self {
            restore: true,
            stop: Some(stop),
            stomp: Some(stomp),
        })
    }

    pub fn forget(&mut self) {
        self.restore = false;
    }

    fn stop_stomp(&mut self) {
        if let Some(stop) = &self.stop {
            stop.store(true, Ordering::SeqCst);
        }
        if let Some(h) = self.stomp.take() {
            let _ = h.join();
        }
    }
}

impl Drop for NihiaGuard {
    fn drop(&mut self) {
        self.stop_stomp();
        if self.restore {
            let _ = restore_agents();
        }
    }
}

pub fn kill_agents() -> Result<()> {
    let status = Command::new("killall")
        .args(AGENTS)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("killall")?;
    // 1 = no matching process, that's fine
    if !status.success() && status.code() != Some(1) {
        anyhow::bail!("killall exited {status}");
    }
    Ok(())
}

pub fn restore_agents() -> Result<()> {
    for path in RESTORE {
        if !std::path::Path::new(path).exists() {
            warn!(path, "NI agent bundle missing; skip restore");
            continue;
        }
        let status = Command::new("open")
            .arg("-g")
            .arg("-a")
            .arg(path)
            .status()
            .with_context(|| format!("open {path}"))?;
        if !status.success() {
            warn!(path, %status, "failed to relaunch NI agent");
        }
    }
    Ok(())
}

fn agents_running() -> bool {
    AGENTS.iter().any(|name| {
        Command::new("pgrep")
            .arg("-x")
            .arg(name)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    })
}
