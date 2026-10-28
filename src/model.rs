//! Backend-neutral session model. herdr deserializes into it directly; Rex builds it.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Rank is [`Self::attention_rank`], not derived `Ord` (variant order is not the API).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    Blocked,
    /// Rex only (OSC 7501 `state=error`).
    Error,
    Working,
    Done,
    Idle,
    #[default]
    #[serde(other)]
    Unknown,
}

impl AgentStatus {
    pub fn attention_rank(self) -> u8 {
        match self {
            Self::Blocked => 0,
            Self::Error => 1,
            Self::Working => 2,
            Self::Done => 3,
            Self::Idle => 4,
            Self::Unknown => 5,
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Self::Blocked => "blocked",
            Self::Error => "error",
            Self::Working => "working",
            Self::Done => "done",
            Self::Idle => "idle",
            Self::Unknown => "unknown",
        }
    }
}

/// What a blocked program waits on (OSC 7501 `kind`). Rex only.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockedKind {
    Permission,
    Question,
    Auth,
}

impl BlockedKind {
    pub fn key(self) -> &'static str {
        match self {
            Self::Permission => "permission",
            Self::Question => "question",
            Self::Auth => "auth",
        }
    }
}

pub fn short_id(id: &str) -> String {
    match id.split_once(':') {
        Some((_, rest)) if rest.len() > 12 && rest.contains('-') => {
            rest[rest.len() - 8..].to_string()
        }
        _ => id.to_string(),
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct AgentInfo {
    pub terminal_id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub display_agent: Option<String>,
    #[serde(default)]
    pub terminal_title: Option<String>,
    #[serde(default)]
    pub terminal_title_stripped: Option<String>,
    pub agent_status: AgentStatus,
    #[serde(default)]
    pub state_labels: BTreeMap<String, String>,
    #[serde(default)]
    pub tokens: BTreeMap<String, String>,
    pub workspace_id: String,
    pub tab_id: String,
    pub pane_id: String,
    pub focused: bool,
    #[serde(default)]
    pub state_change_seq: u64,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub foreground_cwd: Option<String>,
    #[serde(default)]
    pub interactive_ready: bool,
    #[serde(default)]
    pub launch_pending: bool,
    #[serde(default)]
    pub screen_detection_skipped: bool,
    pub revision: u64,
    // Rex program-status extras; herdr leaves them empty.
    #[serde(default)]
    pub blocked_kind: Option<BlockedKind>,
    #[serde(default)]
    pub progress: Option<u8>,
    #[serde(default)]
    pub msg: Option<String>,
    /// `updated_at` of the record that set `agent_status`. Approvals must match it.
    #[serde(default)]
    pub status_stamp: Option<String>,
    /// Human place, e.g. "session / window". Screens prefer it over raw ids.
    #[serde(default)]
    pub place: Option<String>,
    /// Backend-supplied pad order (Rex: window order, then layout order). None: pane id order.
    #[serde(default)]
    pub order: Option<u64>,
    /// The process that raised the prompt is still in the foreground, so typed keys reach it.
    #[serde(default)]
    pub prompt_live: bool,
}

impl AgentInfo {
    pub fn label(&self) -> String {
        self.display_agent
            .clone()
            .or_else(|| self.name.clone())
            .or_else(|| self.agent.clone())
            .unwrap_or_else(|| self.pane_id.clone())
    }

    pub fn headline(&self) -> String {
        if let Some(t) = self.tokens.get("summary") {
            return t.clone();
        }
        if let Some(m) = self.msg.as_ref().filter(|m| !m.is_empty()) {
            return m.clone();
        }
        self.title
            .clone()
            .or_else(|| self.terminal_title_stripped.clone())
            .or_else(|| self.terminal_title.clone())
            .unwrap_or_default()
    }

    pub fn status_label(&self) -> String {
        let key = self.agent_status.key();
        let base = self
            .state_labels
            .get(key)
            .cloned()
            .unwrap_or_else(|| key.to_string());
        match (self.agent_status, self.blocked_kind) {
            (AgentStatus::Blocked, Some(k)) => format!("{base} ({})", k.key()),
            _ => base,
        }
    }

    /// Approve/deny only targets a live permission prompt: a stale record whose owner
    /// has exited or left the foreground would have the keys land in whatever runs now.
    pub fn awaits_permission(&self) -> bool {
        self.agent_status == AgentStatus::Blocked
            && self.blocked_kind == Some(BlockedKind::Permission)
            && self.prompt_live
    }

    /// Rex ids are `block:<uuidv7>`; show the tail.
    pub fn short_id(&self) -> String {
        short_id(&self.pane_id)
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct WorkspaceInfo {
    pub workspace_id: String,
    pub number: u64,
    pub label: String,
    pub focused: bool,
    #[serde(default)]
    pub pane_count: u64,
    #[serde(default)]
    pub tab_count: u64,
    #[serde(default)]
    pub active_tab_id: String,
    #[serde(default)]
    pub agent_status: AgentStatus,
}

#[derive(Clone, Debug, Deserialize)]
pub struct TabInfo {
    pub tab_id: String,
    pub workspace_id: String,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub number: u64,
    #[serde(default)]
    pub focused: bool,
    #[serde(default)]
    pub agent_status: AgentStatus,
}

#[derive(Clone, Debug, Deserialize)]
pub struct PaneInfo {
    pub pane_id: String,
    pub workspace_id: String,
    pub tab_id: String,
    #[serde(default)]
    pub terminal_id: String,
    #[serde(default)]
    pub agent_status: AgentStatus,
    #[serde(default)]
    pub focused: bool,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub display_agent: Option<String>,
    #[serde(default)]
    pub terminal_title: Option<String>,
    #[serde(default)]
    pub terminal_title_stripped: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub foreground_cwd: Option<String>,
    #[serde(default)]
    pub revision: u64,
    #[serde(default)]
    pub label: Option<String>,
}

impl PaneInfo {
    pub fn into_agent(self) -> AgentInfo {
        AgentInfo {
            terminal_id: if self.terminal_id.is_empty() {
                self.pane_id.clone()
            } else {
                self.terminal_id
            },
            name: self.label.clone(),
            agent: self.agent,
            title: self.title,
            display_agent: self.display_agent.or(self.label),
            terminal_title: self.terminal_title,
            terminal_title_stripped: self.terminal_title_stripped,
            agent_status: self.agent_status,
            state_labels: Default::default(),
            tokens: Default::default(),
            workspace_id: self.workspace_id,
            tab_id: self.tab_id,
            pane_id: self.pane_id,
            focused: self.focused,
            state_change_seq: self.revision,
            cwd: self.cwd,
            foreground_cwd: self.foreground_cwd,
            interactive_ready: false,
            launch_pending: false,
            screen_detection_skipped: false,
            revision: self.revision,
            ..Default::default()
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct SessionSnapshot {
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub protocol: u32,
    #[serde(default)]
    pub focused_workspace_id: Option<String>,
    #[serde(default)]
    pub focused_tab_id: Option<String>,
    #[serde(default)]
    pub focused_pane_id: Option<String>,
    #[serde(default)]
    pub workspaces: Vec<WorkspaceInfo>,
    #[serde(default)]
    pub tabs: Vec<TabInfo>,
    #[serde(default)]
    pub panes: Vec<PaneInfo>,
    #[serde(default)]
    pub agents: Vec<AgentInfo>,
}
