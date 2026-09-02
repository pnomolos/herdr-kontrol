use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tracing::{debug, info};

use super::types::{HerdrEvent, SessionSnapshot, SnapshotResult, WireLine};

/// Herdr 0.8 closes the API socket after each non-subscribe RPC; events use a dedicated conn.
pub struct HerdrClient {
    socket: PathBuf,
    events: Option<BufReader<UnixStream>>,
}

impl HerdrClient {
    pub fn new(socket: impl AsRef<Path>) -> Self {
        Self {
            socket: socket.as_ref().to_path_buf(),
            events: None,
        }
    }

    pub async fn ping(&self) -> Result<()> {
        let v = rpc(&self.socket, "ping", serde_json::json!({})).await?;
        debug!(?v, "pong");
        Ok(())
    }

    pub async fn snapshot(&self) -> Result<SessionSnapshot> {
        let v = rpc(&self.socket, "session.snapshot", serde_json::json!({})).await?;
        if let Ok(s) = serde_json::from_value::<SnapshotResult>(v.clone()) {
            return Ok(s.snapshot);
        }
        serde_json::from_value(v).context("session.snapshot shape")
    }

    pub async fn subscribe(&mut self, pane_ids: &[String]) -> Result<()> {
        let mut subs: Vec<serde_json::Value> = [
            "pane.updated",
            "pane.created",
            "pane.closed",
            "pane.moved",
            "pane.exited",
            "pane.focused",
            "pane.agent_detected",
            "workspace.focused",
            "workspace.created",
            "workspace.closed",
            "workspace.updated",
            "tab.focused",
            "tab.created",
            "tab.closed",
        ]
        .into_iter()
        .map(|t| serde_json::json!({"type": t}))
        .collect();
        for id in pane_ids {
            subs.push(serde_json::json!({
                "type": "pane.agent_status_changed",
                "pane_id": id,
            }));
        }

        let mut stream = UnixStream::connect(&self.socket)
            .await
            .with_context(|| format!("connect {}", self.socket.display()))?;
        let req = serde_json::json!({
            "id": "sub",
            "method": "events.subscribe",
            "params": { "subscriptions": subs },
        });
        let mut line = serde_json::to_vec(&req)?;
        line.push(b'\n');
        stream.write_all(&line).await?;
        stream.flush().await?;

        let mut reader = BufReader::new(stream);
        let mut resp = String::new();
        reader.read_line(&mut resp).await?;
        match serde_json::from_str::<WireLine>(resp.trim()) {
            Ok(WireLine::Response {
                error: Some(err), ..
            }) => {
                anyhow::bail!("subscribe: {} ({})", err.message, err.code);
            }
            Ok(WireLine::Response { .. }) => {
                info!("subscribed");
            }
            other => anyhow::bail!("subscribe ack: {other:?}"),
        }
        self.events = Some(reader);
        Ok(())
    }

    pub async fn focus_agent(&self, target: &str) -> Result<()> {
        let _ = rpc(
            &self.socket,
            "agent.focus",
            serde_json::json!({ "target": target }),
        )
        .await?;
        Ok(())
    }

    pub async fn focus_workspace(&self, workspace_id: &str) -> Result<()> {
        let _ = rpc(
            &self.socket,
            "workspace.focus",
            serde_json::json!({ "workspace_id": workspace_id }),
        )
        .await?;
        Ok(())
    }

    pub async fn next_event(&mut self) -> Result<HerdrEvent> {
        let reader = self
            .events
            .as_mut()
            .ok_or_else(|| anyhow!("not subscribed"))?;
        loop {
            let mut line = String::new();
            let n = reader.read_line(&mut line).await?;
            if n == 0 {
                anyhow::bail!("herdr event socket closed");
            }
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            debug!(%line, "herdr rx");
            match serde_json::from_str::<WireLine>(line) {
                Ok(WireLine::Event { event, data }) => {
                    return Ok(HerdrEvent::from_wire(&event, data));
                }
                Ok(WireLine::Response {
                    error: Some(err), ..
                }) => {
                    anyhow::bail!("herdr error {}: {}", err.code, err.message);
                }
                Ok(_) => {}
                Err(e) => debug!(%e, %line, "skip unparsed herdr line"),
            }
        }
    }
}

async fn rpc(socket: &Path, method: &str, params: serde_json::Value) -> Result<serde_json::Value> {
    let mut stream = UnixStream::connect(socket)
        .await
        .with_context(|| format!("connect {}", socket.display()))?;
    let req = serde_json::json!({
        "id": format!("k-{method}"),
        "method": method,
        "params": params,
    });
    let mut line = serde_json::to_vec(&req)?;
    line.push(b'\n');
    stream.write_all(&line).await?;
    stream.flush().await?;
    let mut reader = BufReader::new(stream);
    let mut resp = String::new();
    let n = tokio::time::timeout(Duration::from_secs(2), reader.read_line(&mut resp))
        .await
        .map_err(|_| anyhow!("herdr {method} timed out"))??;
    if n == 0 {
        anyhow::bail!("herdr closed during {method}");
    }
    match serde_json::from_str::<WireLine>(resp.trim()) {
        Ok(WireLine::Response { result, error, .. }) => {
            if let Some(err) = error {
                return Err(anyhow!("{method}: {} ({})", err.message, err.code));
            }
            Ok(result.unwrap_or(serde_json::Value::Null))
        }
        Ok(other) => Err(anyhow!("{method}: unexpected {other:?}")),
        Err(e) => Err(anyhow!("{method} parse: {e}: {}", resp.trim())),
    }
}

pub fn default_socket() -> PathBuf {
    if let Ok(p) = std::env::var("HERDR_SOCKET_PATH") {
        return PathBuf::from(p);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".config/herdr/herdr.sock")
}
