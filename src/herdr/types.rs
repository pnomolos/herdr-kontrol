use serde::Deserialize;

use crate::model::{AgentStatus, SessionSnapshot};

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
            "pane_created"
            | "pane.created"
            | "pane_exited"
            | "pane.exited"
            | "pane_moved"
            | "pane.moved"
            | "pane_updated"
            | "pane.updated"
            | "pane_focused"
            | "pane.focused"
            | "workspace_updated"
            | "workspace.updated"
            | "workspace_created"
            | "workspace.created"
            | "workspace_closed"
            | "workspace.closed"
            | "workspace_focused"
            | "workspace.focused"
            | "tab_focused"
            | "tab.focused"
            | "tab_created"
            | "tab.created"
            | "tab_closed"
            | "tab.closed"
            | "tab_moved"
            | "tab.moved"
            | "tab_renamed"
            | "tab.renamed"
            | "workspace_reordered"
            | "workspace.reordered"
            | "workspace_renamed"
            | "workspace.renamed"
            | "workspace_moved"
            | "workspace.moved"
            | "workspace_metadata_updated"
            | "workspace.metadata_updated"
            | "layout_updated"
            | "layout.updated"
            | "worktree_created"
            | "worktree.created"
            | "worktree_opened"
            | "worktree.opened"
            | "worktree_removed"
            | "worktree.removed" => HerdrEvent::Refresh,
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
    use crate::model::AgentInfo;

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
        let ev = HerdrEvent::from_wire("layout.updated", serde_json::json!({}));
        assert!(matches!(ev, HerdrEvent::Refresh));
        let ev = HerdrEvent::from_wire("workspace.reordered", serde_json::json!({}));
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
