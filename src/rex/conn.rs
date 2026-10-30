//! `rex.control.v1`: one JSON object per WebSocket text frame, over HTTP on a unix socket.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::UnixStream;
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, trace};

/// Shapes this client is written for. 7 adds `format{max_bytes}` + `blob_read`.
pub const API_REVISION: u64 = 7;
/// Oldest revision we can work with; the server picks the newest both speak.
pub const MIN_REVISION: u64 = 4;
const PROTOCOL: &str = "rex.control.v1";
const CALL_TIMEOUT: Duration = Duration::from_secs(10);
const PING_EVERY: Duration = Duration::from_secs(30);

/// A server-side error response. Branch on `code`.
#[derive(Clone, Debug)]
pub struct RexError {
    pub code: String,
    pub message: String,
}

impl std::fmt::Display for RexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.message, self.code)
    }
}

impl std::error::Error for RexError {}

pub fn error_code(e: &anyhow::Error) -> Option<&str> {
    e.downcast_ref::<RexError>().map(|r| r.code.as_str())
}

#[derive(Debug)]
pub struct Event {
    pub seq: u64,
    pub kind: String,
    pub payload: Value,
}

#[derive(Debug)]
pub enum Inbound {
    Event(Event),
    Closed(String),
}

type Reply = Result<(u64, Value), RexError>;
type Pending = Arc<Mutex<HashMap<String, oneshot::Sender<Reply>>>>;

pub struct Conn {
    out: mpsc::UnboundedSender<Message>,
    pending: Pending,
    next: AtomicU64,
}

impl Conn {
    /// Returns the response's `seq` (last event already reflected in the result) and payload.
    pub async fn call(&self, method: &str, payload: Value) -> Result<(u64, Value)> {
        let id = format!("k{}", self.next.fetch_add(1, Ordering::Relaxed));
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .map_err(|_| anyhow!("pending poisoned"))?
            .insert(id.clone(), tx);
        let req = json!({"type": "request", "id": id, "method": method, "payload": payload});
        trace!(%req, "rex tx");
        if self.out.send(Message::Text(req.to_string())).is_err() {
            self.forget(&id);
            bail!("rex connection closed");
        }
        match tokio::time::timeout(CALL_TIMEOUT, rx).await {
            Ok(Ok(Ok(r))) => Ok(r),
            Ok(Ok(Err(e))) => Err(anyhow::Error::new(e).context(method.to_string())),
            Ok(Err(_)) => bail!("rex closed during {method}"),
            Err(_) => {
                self.forget(&id);
                bail!("rex {method} timed out")
            }
        }
    }

    pub async fn call_value(&self, method: &str, payload: Value) -> Result<Value> {
        self.call(method, payload).await.map(|(_, v)| v)
    }

    fn forget(&self, id: &str) {
        if let Ok(mut p) = self.pending.lock() {
            p.remove(id);
        }
    }
}

pub async fn connect(socket: &Path) -> Result<(Conn, mpsc::UnboundedReceiver<Inbound>)> {
    let stream = UnixStream::connect(socket)
        .await
        .with_context(|| format!("connect {}", socket.display()))?;
    let mut req = "ws://localhost/api/control".into_client_request()?;
    req.headers_mut()
        .insert("Sec-WebSocket-Protocol", HeaderValue::from_static(PROTOCOL));
    let (ws, _resp) = tokio::time::timeout(
        Duration::from_secs(3),
        tokio_tungstenite::client_async(req, stream),
    )
    .await
    .map_err(|_| anyhow!("rex websocket handshake timed out"))?
    .context("rex websocket handshake")?;
    let (mut sink, mut stream) = ws.split();

    let hello = json!({
        "type": "hello",
        "api_revision": API_REVISION,
        "api_min_revision": MIN_REVISION,
        "client": {
            "kind": "other",
            "platform": "macos",
            "name": "herdr-kontrol",
            "version": env!("CARGO_PKG_VERSION"),
        },
    });
    sink.send(Message::Text(hello.to_string())).await?;

    let welcome = loop {
        let msg = tokio::time::timeout(Duration::from_secs(5), stream.next())
            .await
            .map_err(|_| anyhow!("no rex welcome"))?
            .ok_or_else(|| anyhow!("rex closed before welcome"))??;
        match msg {
            Message::Text(t) => break serde_json::from_str::<Value>(&t)?,
            Message::Close(f) => bail!("rex refused: {f:?}"),
            _ => continue,
        }
    };
    if welcome["type"] != "welcome" || welcome["protocol"] != PROTOCOL {
        bail!("unexpected rex welcome: {welcome}");
    }
    let rev = welcome["api_revision"].as_u64().unwrap_or(0);
    if rev < MIN_REVISION {
        bail!("rex server speaks api_revision {rev}; need {MIN_REVISION}");
    }
    debug!(
        client = welcome["client"]["id"].as_str().unwrap_or(""),
        rev, "rex welcome"
    );

    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Message>();
    let (in_tx, in_rx) = mpsc::unbounded_channel::<Inbound>();
    let pending: Pending = Arc::default();

    tokio::spawn(async move {
        let mut ping = tokio::time::interval(PING_EVERY);
        ping.tick().await;
        loop {
            let msg = tokio::select! {
                m = out_rx.recv() => match m { Some(m) => m, None => break },
                _ = ping.tick() => Message::Ping(Vec::new()),
            };
            if sink.send(msg).await.is_err() {
                break;
            }
        }
        let _ = sink.close().await;
    });

    {
        let pending = Arc::clone(&pending);
        tokio::spawn(async move {
            let reason = loop {
                let msg = match stream.next().await {
                    Some(Ok(m)) => m,
                    Some(Err(e)) => break e.to_string(),
                    None => break "eof".to_string(),
                };
                let text = match msg {
                    Message::Text(t) => t,
                    Message::Close(f) => break format!("closed: {f:?}"),
                    _ => continue,
                };
                let v: Value = match serde_json::from_str(&text) {
                    Ok(v) => v,
                    Err(e) => {
                        debug!(%e, "skip unparsed rex frame");
                        continue;
                    }
                };
                trace!(%v, "rex rx");
                match v["type"].as_str() {
                    Some("response") => route(&pending, v),
                    Some("event") => {
                        let Some(ev) = parse_event(v) else { continue };
                        if in_tx.send(Inbound::Event(ev)).is_err() {
                            break "receiver gone".into();
                        }
                    }
                    _ => {}
                }
            };
            if let Ok(mut p) = pending.lock() {
                p.clear();
            }
            let _ = in_tx.send(Inbound::Closed(reason));
        });
    }

    Ok((
        Conn {
            out: out_tx,
            pending,
            next: AtomicU64::new(1),
        },
        in_rx,
    ))
}

fn route(pending: &Pending, mut v: Value) {
    let Some(id) = v["id"].as_str().map(str::to_string) else {
        return;
    };
    let Some(tx) = pending.lock().ok().and_then(|mut p| p.remove(&id)) else {
        return;
    };
    let seq = v["seq"].as_u64().unwrap_or(0);
    let reply = if v["error"].is_object() {
        Err(RexError {
            code: v["error"]["code"]
                .as_str()
                .unwrap_or("internal")
                .to_string(),
            message: v["error"]["message"].as_str().unwrap_or("").to_string(),
        })
    } else {
        Ok((seq, v["payload"].take()))
    };
    let _ = tx.send(reply);
}

fn parse_event(mut v: Value) -> Option<Event> {
    let seq = v["seq"].as_u64()?;
    let mut inner = v["payload"].take();
    let kind = inner["type"].as_str()?.to_string();
    Some(Event {
        seq,
        kind,
        payload: inner["payload"].take(),
    })
}

/// `REX_SERVER` is `unix:///path%20with%20escapes`; fall back to the macOS default.
pub fn default_socket() -> std::path::PathBuf {
    if let Ok(s) = std::env::var("REX_SERVER") {
        if let Some(p) = parse_server_url(&s) {
            return p;
        }
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    std::path::PathBuf::from(home).join("Library/Application Support/rex/server.sock")
}

pub fn parse_server_url(s: &str) -> Option<std::path::PathBuf> {
    let path = s
        .strip_prefix("unix://")
        .or_else(|| s.starts_with('/').then_some(s))?;
    Some(std::path::PathBuf::from(percent_decode(path)))
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(n) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(n);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_url() {
        assert_eq!(
            parse_server_url("unix:///Users/me/Library/Application%20Support/rex/server.sock"),
            Some("/Users/me/Library/Application Support/rex/server.sock".into())
        );
        assert_eq!(parse_server_url("/tmp/x.sock"), Some("/tmp/x.sock".into()));
        assert_eq!(parse_server_url("https://host"), None);
        assert_eq!(percent_decode("a%2"), "a%2");
        assert_eq!(percent_decode("a%41"), "aA");
    }

    #[test]
    fn event_envelope() {
        let v = json!({"type":"event","seq":3,"payload":{"type":"session_event","payload":{"session_id":"s","event":{"type":"block_closed","payload":{"block_id":"b"}}}}});
        let ev = parse_event(v).unwrap();
        assert_eq!(ev.seq, 3);
        assert_eq!(ev.kind, "session_event");
        assert_eq!(ev.payload["event"]["payload"]["block_id"], "b");
    }
}
