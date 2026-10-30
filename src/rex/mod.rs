//! Rex backend: one `rex.control.v1` connection, `session.attach_all{event_scope:"all"}`,
//! a [`mirror::Mirror`] of sessions/windows/blocks, and debounced snapshots out.

mod conn;
mod mirror;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use futures_util::future::join_all;
use serde_json::json;
use tokio::time::Instant;
use tracing::{debug, info, warn};

pub use conn::default_socket;
use conn::{error_code, Conn, Event, Inbound};
use mirror::Mirror;

use crate::backend::{Caps, Command, Commands, Reply, Update, Updates};

pub enum RunError {
    Connect(anyhow::Error),
    Lost(anyhow::Error),
}

const CAPS: Caps = Caps {
    name: "rex",
    respond: true,
    screen: true,
};

const TERM: &str = "com.superlogical.terminal";
const DEBOUNCE: Duration = Duration::from_millis(40);
const SCREEN_EVERY: Duration = Duration::from_millis(1000);
const SCREEN_LINES: usize = 40;
/// Bytes of scrollback to read for the preview. Full output can pass the
/// 1 MiB message limit (`too_large`); 48 KiB covers 40 lines of any width.
const SCREEN_TAIL_BYTES: u64 = 48 * 1024;

struct Driver {
    conn: Arc<Conn>,
    mirror: Mirror,
    /// Rex.app's client id, for `client.perform session.select`.
    app: Option<String>,
    perform_warned: bool,
    watch: Option<String>,
    last_screen: Option<(String, String)>,
}

pub async fn run(socket: &Path, tx: &Updates, cmds: &mut Commands) -> Result<(), RunError> {
    let (conn, mut inbound) = conn::connect(socket).await.map_err(RunError::Connect)?;
    let conn = Arc::new(conn);
    let (_, res) = conn
        .call("session.attach_all", json!({"event_scope": "all"}))
        .await
        .map_err(RunError::Connect)?;
    let mut d = Driver {
        conn,
        mirror: Mirror::default(),
        app: None,
        perform_warned: false,
        watch: None,
        last_screen: None,
    };
    for s in res["sessions"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
    {
        if let (Some(id), Some(label)) = (s["session_id"].as_str(), s["label"].as_str()) {
            d.mirror.add_session(id, label);
        }
    }
    let ids: Vec<String> = d.mirror.sessions.iter().map(|s| s.id.clone()).collect();
    for id in ids {
        d.load_session(&id).await;
    }
    d.find_app().await;
    let _ = tx.send(Update::Connected(CAPS));
    let _ = tx.send(Update::Snapshot(d.mirror.snapshot()));
    info!(
        socket = %socket.display(),
        sessions = d.mirror.sessions.len(),
        blocks = d.mirror.blocks.len(),
        app = d.app.as_deref().unwrap_or("-"),
        "rex connected"
    );

    let mut dirty_at: Option<Instant> = None;
    let mut screen = tokio::time::interval(SCREEN_EVERY);
    screen.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            msg = inbound.recv() => match msg {
                Some(Inbound::Event(ev)) => {
                    if d.on_event(ev).await && dirty_at.is_none() {
                        dirty_at = Some(Instant::now() + DEBOUNCE);
                    }
                }
                Some(Inbound::Closed(reason)) => return Err(RunError::Lost(anyhow!(reason))),
                None => return Err(RunError::Lost(anyhow!("rex reader gone"))),
            },
            _ = tokio::time::sleep_until(dirty_at.unwrap_or_else(Instant::now)), if dirty_at.is_some() => {
                dirty_at = None;
                let _ = tx.send(Update::Snapshot(d.mirror.snapshot()));
            }
            _ = screen.tick(), if d.watch.is_some() => d.poll_screen(tx).await,
            cmd = cmds.recv() => {
                let Some(cmd) = cmd else { return Ok(()) };
                if let Err(e) = d.dispatch(cmd, tx).await {
                    warn!(%e, "rex command failed");
                }
            }
        }
    }
}

impl Driver {
    async fn load_session(&mut self, session_id: &str) {
        match self
            .conn
            .call_value("session.view", json!({"session_id": session_id}))
            .await
        {
            Ok(view) => {
                let fresh = self.mirror.apply_view(session_id, &view);
                self.load_blocks(session_id, fresh).await;
            }
            Err(e) => warn!(%e, session_id, "session.view"),
        }
    }

    /// Program status, title and foreground process for each new block, concurrently.
    async fn load_blocks(&mut self, session_id: &str, blocks: Vec<String>) {
        let calls = blocks.iter().flat_map(|b| {
            ["program_status", "title", "process"].map(|m| {
                let conn = Arc::clone(&self.conn);
                let args = json!({"session_id": session_id, "block_id": b, "args": {}});
                let b = b.clone();
                async move { (b, m, conn.call(&format!("{TERM}.{m}"), args).await) }
            })
        });
        for (b, m, r) in join_all(calls).await {
            match r {
                Ok((seq, v)) => {
                    match m {
                        "program_status" => self.mirror.load_status(&b, seq, &v),
                        "title" => self.mirror.load_title(&b, seq, &v),
                        _ => self.mirror.load_process(&b, seq, &v),
                    };
                }
                Err(e) => debug!(%e, block = %b, m, "block facet"),
            }
        }
    }

    async fn find_app(&mut self) {
        let res = self
            .conn
            .call_value("client.list", json!({"kinds": ["app"]}))
            .await;
        self.app = res.ok().and_then(|v| {
            v["clients"]
                .as_array()?
                .iter()
                .filter(|c| c["connection_state"].as_str().unwrap_or("connected") == "connected")
                .filter(|c| c["info"]["platform"].as_str().unwrap_or("macos") == "macos")
                .filter_map(|c| c["client_id"].as_str())
                .next_back()
                .map(str::to_string)
        });
    }

    /// True if the snapshot may have changed.
    async fn on_event(&mut self, ev: Event) -> bool {
        let p = &ev.payload;
        match ev.kind.as_str() {
            "session_created" => {
                let s = &p["session"];
                let (Some(id), Some(label)) = (s["session_id"].as_str(), s["label"].as_str())
                else {
                    return false;
                };
                let id = id.to_string();
                self.mirror.add_session(&id, label);
                self.load_session(&id).await;
                true
            }
            "session_destroyed" => {
                if let Some(id) = p["session_id"].as_str() {
                    self.mirror.remove_session(id);
                }
                true
            }
            "session_moved" => {
                if let Some(id) = p["session_id"].as_str() {
                    self.mirror
                        .move_session(id, p["before_session_id"].as_str());
                }
                true
            }
            "client_joined" | "client_left" => {
                self.find_app().await;
                false
            }
            "session_event" => {
                let Some(sid) = p["session_id"].as_str().map(str::to_string) else {
                    return false;
                };
                let inner = &p["event"];
                let ip = &inner["payload"];
                match inner["type"].as_str().unwrap_or_default() {
                    "session_view_changed" => {
                        let rev = ip["revision"].as_u64().unwrap_or(u64::MAX);
                        if self.mirror.session_revision(&sid).is_some_and(|r| rev <= r) {
                            return false;
                        }
                        self.load_session(&sid).await;
                        true
                    }
                    "window_label_changed" => {
                        if let (Some(w), Some(l)) = (ip["window_id"].as_str(), ip["label"].as_str())
                        {
                            self.mirror.set_window_label(&sid, w, l);
                        }
                        true
                    }
                    "session_label_changed" => {
                        if let Some(l) = ip["label"].as_str() {
                            self.mirror.set_session_label(&sid, l);
                        }
                        true
                    }
                    "block_closed" => ip["block_id"]
                        .as_str()
                        .is_some_and(|b| self.mirror.remove_block(b)),
                    "block_event" => {
                        let (Some(b), Some(name)) = (ip["block_id"].as_str(), ip["name"].as_str())
                        else {
                            return false;
                        };
                        if ip["creator_name"].as_str().is_some_and(|c| c != TERM) {
                            return false;
                        }
                        self.mirror
                            .apply_block_event(ev.seq, b, name, &ip["payload"])
                    }
                    _ => false,
                }
            }
            _ => false,
        }
    }

    async fn dispatch(&mut self, cmd: Command, tx: &Updates) -> Result<()> {
        match cmd {
            Command::FocusPane { pane_id, .. } => {
                let (sid, wid) = self
                    .mirror
                    .locate(&pane_id)
                    .map(|(s, w)| (s.to_string(), w.to_string()))
                    .ok_or_else(|| anyhow!("unknown block {pane_id}"))?;
                info!(block = %pane_id, "focus block");
                self.conn
                    .call(
                        "session.focus_block",
                        json!({"session_id": sid, "block_id": pane_id}),
                    )
                    .await?;
                // Already-focused blocks produce no view change, so mark here too.
                if self.mirror.mark_seen(&pane_id) {
                    let _ = tx.send(Update::Snapshot(self.mirror.snapshot()));
                }
                self.select_in_app(&sid, Some(&wid)).await;
                raise();
            }
            Command::FocusWorkspace {
                workspace_id,
                pane_id,
                ..
            } => {
                info!(session = %workspace_id, "focus session");
                let wid = pane_id
                    .as_deref()
                    .and_then(|p| self.mirror.locate(p))
                    .map(|(_, w)| w.to_string());
                // The app shows that window (or the active one), at its focused block.
                let shown = self.mirror.session(&workspace_id).and_then(|s| {
                    let w = wid.as_deref().or(s.active_window_id.as_deref())?;
                    s.windows
                        .iter()
                        .find(|x| x.id == w)?
                        .focused_block_id
                        .clone()
                });
                if shown.is_some_and(|b| self.mirror.mark_seen(&b)) {
                    let _ = tx.send(Update::Snapshot(self.mirror.snapshot()));
                }
                self.select_in_app(&workspace_id, wid.as_deref()).await;
                raise();
            }
            Command::Respond {
                pane_id,
                stamp,
                reply,
            } => self.respond(&pane_id, stamp.as_deref(), reply).await?,
            Command::WatchScreen(target) => {
                if target != self.watch {
                    self.watch = target;
                    self.last_screen = None;
                    self.poll_screen(tx).await;
                }
            }
        }
        Ok(())
    }

    /// Move Rex.app to the session. Needs Remote Control in the app's settings;
    /// without it, focus only changes the server's view of the session.
    async fn select_in_app(&mut self, session_id: &str, window_id: Option<&str>) {
        if self.app.is_none() {
            self.find_app().await;
        }
        let Some(app) = self.app.clone() else {
            self.warn_perform("no Rex.app client connected");
            return;
        };
        let mut args = json!({"session_id": session_id});
        if let Some(w) = window_id {
            args["window_id"] = json!(w);
        }
        let r = self
            .conn
            .call(
                "client.perform",
                json!({"client_id": app, "action": "session.select", "args": args,
                       "origin": "api", "timeout_ms": 3000}),
            )
            .await;
        match r {
            Ok(_) => {}
            Err(e) => {
                match error_code(&e) {
                    Some("not_found") | Some("client_unavailable") => self.app = None,
                    _ => {}
                }
                self.warn_perform(&e.to_string());
            }
        }
    }

    fn warn_perform(&mut self, why: &str) {
        if !self.perform_warned {
            warn!(
                why,
                "Rex.app did not switch session; enable Remote Control in Rex settings. \
                 Focus still updates the server (further failures logged at debug)"
            );
            self.perform_warned = true;
        } else {
            debug!(why, "client.perform session.select failed");
        }
    }

    /// Re-read the block's status from the server and type only if it is still the
    /// exact permission prompt the user saw.
    async fn respond(&mut self, pane_id: &str, stamp: Option<&str>, reply: Reply) -> Result<()> {
        let sid = self
            .mirror
            .locate(pane_id)
            .map(|(s, _)| s.to_string())
            .ok_or_else(|| anyhow!("unknown block {pane_id}"))?;
        let args = json!({"session_id": sid, "block_id": pane_id, "args": {}});
        let (seq, v) = self
            .conn
            .call(&format!("{TERM}.program_status"), args)
            .await?;
        self.mirror.load_status(pane_id, seq, &v);
        let args = json!({"session_id": sid, "block_id": pane_id, "args": {}});
        let (seq, v) = self.conn.call(&format!("{TERM}.process"), args).await?;
        self.mirror.load_process(pane_id, seq, &v);
        let live = self
            .mirror
            .blocks
            .get(pane_id)
            .is_some_and(|b| b.prompt_live());
        let top = self
            .mirror
            .blocks
            .get(pane_id)
            .and_then(|b| b.top())
            .cloned();
        let Some(top) = top else {
            bail!("refused {reply:?}: block has no status");
        };
        let ok = top.status() == crate::model::AgentStatus::Blocked
            && top.blocked_kind() == Some(crate::model::BlockedKind::Permission)
            && stamp.is_some()
            && top.updated_at.as_deref() == stamp;
        if ok && !live {
            bail!("refused {reply:?}: prompt owner is no longer the foreground process");
        }
        if !ok {
            bail!(
                "refused {reply:?}: status moved (now {} {:?} at {:?}, saw {:?})",
                top.state,
                top.kind,
                top.updated_at,
                stamp
            );
        }
        info!(block = %pane_id, ?reply, "respond to permission prompt");
        self.conn
            .call(
                &format!("{TERM}.write"),
                json!({"session_id": sid, "block_id": pane_id, "args": {"data": reply.keys()}}),
            )
            .await?;
        Ok(())
    }

    async fn poll_screen(&mut self, tx: &Updates) {
        let Some(block) = self.watch.clone() else {
            return;
        };
        let Some(sid) = self.mirror.locate(&block).map(|(s, _)| s.to_string()) else {
            return;
        };
        let text = match self.screen_tail(&sid, &block).await {
            Ok(t) => tail(&t, SCREEN_LINES),
            Err(e) => {
                debug!(%e, "screen poll");
                return;
            }
        };
        if self.last_screen.as_ref() == Some(&(block.clone(), text.clone())) {
            return;
        }
        self.last_screen = Some((block.clone(), text.clone()));
        let _ = tx.send(Update::Screen {
            pane_id: block,
            text,
        });
    }
}

/// Last `n` lines, ignoring trailing blank lines.
impl Driver {
    /// The end of a block's text output. `format{max_bytes}` returns the head
    /// plus a blob id when there's more; then read the last chunk by offset.
    /// Servers before revision 7 ignore `max_bytes` and return everything.
    async fn screen_tail(&self, sid: &str, block: &str) -> Result<String> {
        let v = self
            .conn
            .call_value(
                &format!("{TERM}.format"),
                json!({"session_id": sid, "block_id": block,
                       "args": {"format": "text", "trim": true,
                                "max_bytes": SCREEN_TAIL_BYTES}}),
            )
            .await?;
        let content = v["content"].as_str().unwrap_or_default();
        let (Some(id), Some(total)) = (v["id"].as_u64(), v["total"].as_u64()) else {
            return Ok(content.to_string());
        };
        let start = total.saturating_sub(SCREEN_TAIL_BYTES);
        // `max_bytes` counts JSON-escaped bytes (`\n` is two), so ask for 4x
        // headroom; text escapes at most ~2x. The loop is a fallback only: a
        // start mid-character comes back as U+FFFD, which skews `offset`.
        let mut offset = start;
        let mut data = String::new();
        for _ in 0..4 {
            let r = self
                .conn
                .call_value(
                    &format!("{TERM}.blob_read"),
                    json!({"session_id": sid, "block_id": block,
                           "args": {"id": id, "offset": offset,
                                    "max_bytes": SCREEN_TAIL_BYTES * 4}}),
                )
                .await?;
            let chunk = r["data"].as_str().unwrap_or_default();
            data.push_str(chunk);
            offset += chunk.len() as u64;
            if r["eof"].as_bool().unwrap_or(true) || chunk.is_empty() {
                break;
            }
        }
        Ok(skip_partial_line(&data, start > 0).to_string())
    }
}

/// A read that starts mid-output likely starts mid-line; drop that fragment.
fn skip_partial_line(s: &str, mid: bool) -> &str {
    match (mid, s.find('\n')) {
        (true, Some(i)) => &s[i + 1..],
        _ => s,
    }
}

fn tail(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    let end = lines
        .iter()
        .rposition(|l| !l.trim().is_empty())
        .map_or(0, |i| i + 1);
    lines[end.saturating_sub(n)..end].join("\n")
}

fn raise() {
    std::mem::drop(tokio::task::spawn_blocking(crate::host::raise_rex));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_first_line() {
        assert_eq!(
            skip_partial_line("ial line\nnext\nlast", true),
            "next\nlast"
        );
        assert_eq!(skip_partial_line("whole\nnext", false), "whole\nnext");
        assert_eq!(skip_partial_line("no newline", true), "no newline");
    }

    #[test]
    fn tail_skips_trailing_blank() {
        assert_eq!(tail("a\nb\nc\n\n  \n", 2), "b\nc");
        assert_eq!(tail("", 3), "");
        assert_eq!(tail("x", 3), "x");
    }
}
