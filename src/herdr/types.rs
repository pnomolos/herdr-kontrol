use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Rank is [`Self::attention_rank`], not derived `Ord` (variant order is not the API).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    Blocked,
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
            Self::Working => 1,
            Self::Done => 2,
            Self::Idle => 3,
            Self::Unknown => 4,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
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
    pub revision: u64,
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
        self.title
            .clone()
            .or_else(|| self.terminal_title_stripped.clone())
            .or_else(|| self.terminal_title.clone())
            .unwrap_or_default()
    }

    pub fn status_label(&self) -> String {
        let key = match self.agent_status {
            AgentStatus::Blocked => "blocked",
            AgentStatus::Working => "working",
            AgentStatus::Done => "done",
            AgentStatus::Idle => "idle",
            AgentStatus::Unknown => "unknown",
        };
        self.state_labels
            .get(key)
            .cloned()
            .unwrap_or_else(|| key.to_string())
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
            revision: self.revision,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
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

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum WireLine {
    Response {
        id: String,
        #[serde(default)]
        result: Option<serde_json::Value>,
        #[serde(default)]
        error: Option<WireError>,
    },
    Event {
        event: String,
        #[serde(default)]
        data: serde_json::Value,
    },
}

#[derive(Debug, Deserialize)]
pub struct WireError {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Deserialize)]
pub struct SnapshotResult {
    #[serde(rename = "type")]
    pub type_: String,
    pub snapshot: SessionSnapshot,
}

#[derive(Clone, Debug)]
pub enum HerdrEvent {
    Snapshot(SessionSnapshot),
    AgentStatus {
        pane_id: String,
        workspace_id: String,
        agent_status: AgentStatus,
        agent: Option<String>,
        title: Option<String>,
        display_agent: Option<String>,
    },
    AgentDetected {
        pane_id: String,
    },
    PaneClosed {
        pane_id: String,
    },
    Refresh,
    Other {
        kind: String,
    },
}

impl HerdrEvent {
    pub fn from_wire(kind: &str, data: serde_json::Value) -> Self {
        let pane = data.get("pane").unwrap_or(&data);
        match kind {
            "pane_agent_status_changed" | "pane.agent_status_changed" => HerdrEvent::AgentStatus {
                pane_id: str_field(pane, "pane_id"),
                workspace_id: str_field(pane, "workspace_id"),
                agent_status: serde_json::from_value(
                    pane.get("agent_status")
                        .cloned()
                        .unwrap_or(serde_json::Value::String("unknown".into())),
                )
                .unwrap_or(AgentStatus::Unknown),
                agent: opt_str(pane, "agent"),
                title: opt_str(pane, "title").or_else(|| opt_str(pane, "terminal_title_stripped")),
                display_agent: opt_str(pane, "display_agent"),
            },
            "pane_agent_detected" | "pane.agent_detected" => HerdrEvent::AgentDetected {
                pane_id: str_field(pane, "pane_id"),
            },
            "pane_closed" | "pane.closed" => HerdrEvent::PaneClosed {
                pane_id: str_field(pane, "pane_id"),
            },
            // pane.updated is partial; missing agent_status would clobber to Unknown — snapshot.
            "pane_created" | "pane.created" | "pane_exited" | "pane.exited" | "pane_moved"
            | "pane.moved" | "pane_updated" | "pane.updated" | "pane_focused" | "pane.focused"
            | "workspace_updated" | "workspace.updated" | "workspace_created"
            | "workspace.created" | "workspace_closed" | "workspace.closed"
            | "workspace_focused" | "workspace.focused" | "tab_focused" | "tab.focused"
            | "tab_created" | "tab.created" | "tab_closed" | "tab.closed" => HerdrEvent::Refresh,
            other => HerdrEvent::Other {
                kind: other.to_string(),
            },
        }
    }
}

fn str_field(v: &serde_json::Value, k: &str) -> String {
    v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string()
}

fn opt_str(v: &serde_json::Value, k: &str) -> Option<String> {
    v.get(k).and_then(|x| x.as_str()).map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_agent() {
        let j = r#"{
            "terminal_id":"t1","agent_status":"blocked","workspace_id":"w1",
            "tab_id":"w1:t1","pane_id":"w1:p1","focused":false,"revision":1,
            "display_agent":"claude","title":"fix auth"
        }"#;
        let a: AgentInfo = serde_json::from_str(j).unwrap();
        assert_eq!(a.agent_status, AgentStatus::Blocked);
        assert_eq!(a.label(), "claude");
        assert_eq!(a.headline(), "fix auth");
    }

    #[test]
    fn rank() {
        assert!(AgentStatus::Blocked.attention_rank() < AgentStatus::Working.attention_rank());
        assert!(AgentStatus::Working.attention_rank() < AgentStatus::Done.attention_rank());
        assert!(AgentStatus::Done.attention_rank() < AgentStatus::Idle.attention_rank());
        assert!(AgentStatus::Idle.attention_rank() < AgentStatus::Unknown.attention_rank());
    }

    #[test]
    fn status_changed_is_patch() {
        let ev = HerdrEvent::from_wire(
            "pane_agent_status_changed",
            serde_json::json!({
                "pane_id": "w1:p1",
                "workspace_id": "w1",
                "agent_status": "done",
                "title": "Repository overview and structure"
            }),
        );
        match ev {
            HerdrEvent::AgentStatus {
                pane_id,
                agent_status,
                title,
                ..
            } => {
                assert_eq!(pane_id, "w1:p1");
                assert_eq!(agent_status, AgentStatus::Done);
                assert_eq!(title.as_deref(), Some("Repository overview and structure"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn pane_updated_is_refresh() {
        let ev = HerdrEvent::from_wire(
            "pane_updated",
            serde_json::json!({"pane": {"pane_id": "w1:p1", "agent_status": "done"}}),
        );
        assert!(matches!(ev, HerdrEvent::Refresh));
    }

    #[test]
    fn tab_created_and_closed_are_refresh() {
        let created = HerdrEvent::from_wire(
            "tab.created",
            serde_json::json!({"tab_id": "w1:t2", "workspace_id": "w1"}),
        );
        let closed = HerdrEvent::from_wire("tab.closed", serde_json::json!({"tab_id": "w1:t2"}));
        assert!(matches!(created, HerdrEvent::Refresh));
        assert!(matches!(closed, HerdrEvent::Refresh));
    }

    #[test]
    fn workspace_focused_is_refresh() {
        let ev = HerdrEvent::from_wire(
            "workspace.focused",
            serde_json::json!({"workspace_id": "w1"}),
        );
        assert!(matches!(ev, HerdrEvent::Refresh));
    }

    #[test]
    fn event_envelope_not_stolen_by_response() {
        let line = r#"{"event":"pane_agent_status_changed","data":{"pane_id":"w1:p1","workspace_id":"w1","agent_status":"done"}}"#;
        match serde_json::from_str::<WireLine>(line).unwrap() {
            WireLine::Event { event, data } => {
                assert_eq!(event, "pane_agent_status_changed");
                assert_eq!(data["agent_status"], "done");
            }
            other => panic!("{other:?}"),
        }
    }
}
