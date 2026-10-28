//! Runtime edge: one driver task owns the connection to herdr or Rex.
//!
//! The app sends [`Command`]s and paints from [`Update`]s; it never sees a
//! socket. A driver returns when its connection dies; [`run`] then re-detects.

use std::path::PathBuf;
use std::time::Duration;

use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tracing::{debug, info, warn};

use crate::model::{AgentStatus, SessionSnapshot};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, clap::ValueEnum)]
pub enum Choice {
    /// Rex if its socket answers, else herdr.
    #[default]
    Auto,
    Herdr,
    Rex,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub choice: Choice,
    pub herdr_socket: PathBuf,
    pub rex_socket: PathBuf,
}

/// What the connected backend can do beyond glance+focus.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Caps {
    pub name: &'static str,
    /// Approve/deny a permission prompt by typing into the pane.
    pub respond: bool,
    /// Stream the selected pane's screen text.
    pub screen: bool,
}

#[derive(Clone, Debug)]
pub enum Update {
    Connected(Caps),
    Disconnected,
    Snapshot(SessionSnapshot),
    Status(StatusPatch),
    PaneClosed(String),
    /// Plain-text screen of the watched pane.
    Screen {
        pane_id: String,
        text: String,
    },
}

#[derive(Clone, Debug)]
pub struct StatusPatch {
    pub pane_id: String,
    pub workspace_id: String,
    pub agent_status: AgentStatus,
    pub title: Option<String>,
    pub display_agent: Option<String>,
    pub agent: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Reply {
    /// Enter: accept the highlighted (first) option.
    Approve,
    /// `2`: Claude Code's "yes, don't ask again".
    ApproveAlways,
    /// Esc.
    Deny,
}

impl Reply {
    pub fn keys(self) -> &'static str {
        match self {
            Self::Approve => "\r",
            Self::ApproveAlways => "2",
            Self::Deny => "\x1b",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Command {
    FocusPane {
        pane_id: String,
        hint: Option<String>,
    },
    FocusWorkspace {
        workspace_id: String,
        pane_id: Option<String>,
        hint: Option<String>,
    },
    /// Only sent for a pane showing a permission prompt. `stamp` is the
    /// status `updated_at` the user saw; the driver refuses if it moved.
    Respond {
        pane_id: String,
        stamp: Option<String>,
        reply: Reply,
    },
    /// Pane whose screen text to stream, or none.
    WatchScreen(Option<String>),
}

pub type Updates = UnboundedSender<Update>;
pub type Commands = UnboundedReceiver<Command>;

pub async fn run(cfg: Config, tx: Updates, mut rx: Commands) {
    let mut announced_wait = false;
    loop {
        if tx.is_closed() {
            return;
        }
        let tried = match cfg.choice {
            Choice::Rex => try_rex(&cfg, &tx, &mut rx).await,
            Choice::Herdr => try_herdr(&cfg, &tx, &mut rx).await,
            Choice::Auto => {
                try_rex(&cfg, &tx, &mut rx).await || try_herdr(&cfg, &tx, &mut rx).await
            }
        };
        if tried {
            announced_wait = false;
            let _ = tx.send(Update::Disconnected);
        } else if !announced_wait {
            info!(choice = ?cfg.choice, "no backend reachable; waiting");
            announced_wait = true;
        }
        // Drop commands aimed at a dead backend.
        while let Ok(cmd) = rx.try_recv() {
            debug!(?cmd, "dropped: no backend");
        }
        tokio::time::sleep(Duration::from_millis(750)).await;
    }
}

/// True if it connected (and has since dropped).
async fn try_rex(cfg: &Config, tx: &Updates, rx: &mut Commands) -> bool {
    match crate::rex::run(&cfg.rex_socket, tx, rx).await {
        Ok(()) => true,
        Err(crate::rex::RunError::Connect(e)) => {
            debug!(%e, "rex not reachable");
            false
        }
        Err(crate::rex::RunError::Lost(e)) => {
            warn!(%e, "rex connection lost");
            true
        }
    }
}

async fn try_herdr(cfg: &Config, tx: &Updates, rx: &mut Commands) -> bool {
    match crate::herdr::driver::run(&cfg.herdr_socket, tx, rx).await {
        Ok(()) => true,
        Err(crate::herdr::driver::RunError::Connect(e)) => {
            debug!(%e, "herdr not reachable");
            false
        }
        Err(crate::herdr::driver::RunError::Lost(e)) => {
            warn!(%e, "herdr connection lost");
            true
        }
    }
}
