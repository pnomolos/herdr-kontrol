//! herdr 0.9 driver: per-RPC connections plus one subscribe connection.

use std::collections::HashSet;
use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use super::client::HerdrClient;
use super::types::HerdrEvent;
use crate::backend::{Caps, Command, Commands, StatusPatch, Update, Updates};
use crate::model::SessionSnapshot;

pub enum RunError {
    Connect(anyhow::Error),
    Lost(anyhow::Error),
}

const CAPS: Caps = Caps {
    name: "herdr",
    respond: false,
    screen: false,
};

struct Events {
    rx: mpsc::UnboundedReceiver<Result<HerdrEvent>>,
    tx: mpsc::UnboundedSender<Result<HerdrEvent>>,
    task: Option<tokio::task::JoinHandle<()>>,
    pane_ids: Vec<String>,
}

impl Drop for Events {
    fn drop(&mut self) {
        if let Some(t) = self.task.take() {
            t.abort();
        }
    }
}

impl Events {
    fn new() -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        Self {
            rx,
            tx,
            task: None,
            pane_ids: Vec::new(),
        }
    }

    async fn resubscribe(&mut self, socket: &Path, pane_ids: &[String]) -> Result<()> {
        let mut sub = HerdrClient::new(socket);
        sub.subscribe(pane_ids).await?;
        if let Some(t) = self.task.take() {
            t.abort();
        }
        let tx = self.tx.clone();
        self.task = Some(tokio::spawn(async move {
            loop {
                match sub.next_event().await {
                    Ok(ev) => {
                        if tx.send(Ok(ev)).is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(Err(e));
                        break;
                    }
                }
            }
        }));
        Ok(())
    }

    /// Status events are per-pane subscriptions; follow the pane set.
    async fn follow(&mut self, socket: &Path, ids: Vec<String>) {
        if same_pane_ids(&ids, &self.pane_ids) {
            return;
        }
        if let Err(e) = self.resubscribe(socket, &ids).await {
            warn!(%e, "resubscribe");
            return;
        }
        self.pane_ids = ids;
    }
}

pub async fn run(socket: &Path, tx: &Updates, cmds: &mut Commands) -> Result<(), RunError> {
    let rpc = HerdrClient::new(socket);
    rpc.ping().await.map_err(RunError::Connect)?;

    let mut events = Events::new();
    // 0.9 does not replay retained events; subscribe before snapshot.
    if let Err(e) = events.resubscribe(socket, &[]).await {
        warn!(%e, "subscribe");
    }
    let snap = rpc.snapshot().await.map_err(RunError::Connect)?;
    let ids = pane_ids(&snap);
    let _ = tx.send(Update::Connected(CAPS));
    let _ = tx.send(Update::Snapshot(snap));
    events.follow(socket, ids).await;
    info!(socket = %socket.display(), "herdr connected");

    // herdr does not always emit pane.agent_status_changed for done/idle; poll.
    let mut refresh = tokio::time::interval(Duration::from_secs(2));
    refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    refresh.tick().await;

    loop {
        tokio::select! {
            ev = events.rx.recv() => match ev {
                Some(Ok(HerdrEvent::Refresh)) | Some(Ok(HerdrEvent::AgentDetected { .. })) => {
                    match rpc.snapshot().await {
                        Ok(snap) => publish(socket, &mut events, tx, snap).await,
                        Err(e) => warn!(%e, "refresh snapshot"),
                    }
                }
                Some(Ok(HerdrEvent::Snapshot(s))) => {
                    let _ = tx.send(Update::Snapshot(s));
                }
                Some(Ok(HerdrEvent::AgentStatus {
                    pane_id,
                    workspace_id,
                    agent_status,
                    agent,
                    title,
                    display_agent,
                })) => {
                    let _ = tx.send(Update::Status(StatusPatch {
                        pane_id,
                        workspace_id,
                        agent_status,
                        title,
                        display_agent,
                        agent,
                    }));
                }
                Some(Ok(HerdrEvent::PaneClosed { pane_id })) => {
                    let _ = tx.send(Update::PaneClosed(pane_id));
                }
                Some(Ok(HerdrEvent::Other { kind })) => debug!(kind, "unhandled herdr event"),
                Some(Err(e)) => return Err(RunError::Lost(e)),
                None => return Ok(()),
            },
            _ = refresh.tick() => match rpc.snapshot().await {
                Ok(snap) => publish(socket, &mut events, tx, snap).await,
                Err(e) => return Err(RunError::Lost(e)),
            },
            cmd = cmds.recv() => {
                let Some(cmd) = cmd else { return Ok(()) };
                if let Err(e) = dispatch(&rpc, cmd).await {
                    warn!(%e, "herdr command failed");
                }
            }
        }
    }
}

async fn publish(socket: &Path, events: &mut Events, tx: &Updates, snap: SessionSnapshot) {
    let ids = pane_ids(&snap);
    let _ = tx.send(Update::Snapshot(snap));
    events.follow(socket, ids).await;
}

async fn dispatch(herdr: &HerdrClient, cmd: Command) -> Result<()> {
    match cmd {
        Command::FocusPane { pane_id, hint } => {
            // 0.9.0: only pane.focus projects the attached TUI. agent.focus
            // updates the server record and is a no-op on the viewport.
            info!(pane = %pane_id, "focus pane");
            herdr.focus_pane(&pane_id).await?;
            raise(hint);
        }
        Command::FocusWorkspace {
            workspace_id,
            pane_id,
            hint,
        } => {
            info!(ws = %workspace_id, "focus workspace");
            herdr.focus_workspace(&workspace_id).await?;
            if let Some(pane_id) = pane_id {
                herdr.focus_pane(&pane_id).await?;
            }
            raise(hint);
        }
        Command::Respond { .. } => debug!("herdr: respond unsupported"),
        Command::WatchScreen(_) => {}
    }
    Ok(())
}

fn raise(hint: Option<String>) {
    std::mem::drop(tokio::task::spawn_blocking(move || {
        crate::host::raise(hint.as_deref())
    }));
}

fn pane_ids(snap: &SessionSnapshot) -> Vec<String> {
    snap.panes.iter().map(|p| p.pane_id.clone()).collect()
}

fn same_pane_ids(a: &[String], b: &[String]) -> bool {
    a.len() == b.len() && a.iter().collect::<HashSet<_>>() == b.iter().collect::<HashSet<_>>()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pane_ids_ignore_order() {
        assert!(same_pane_ids(
            &["b".into(), "a".into()],
            &["a".into(), "b".into()]
        ));
        assert!(!same_pane_ids(&["a".into()], &["a".into(), "b".into()]));
        assert!(same_pane_ids(&[], &[]));
    }
}
