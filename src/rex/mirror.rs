//! Local copy of Rex server state, folded into the backend-neutral [`SessionSnapshot`].
//!
//! Ordering: each fetch returns the response `seq` (the last event the result
//! already reflects). Per block and per facet we keep the highest seq applied,
//! and skip events at or below it, so an old queued event never clobbers a
//! newer fetch.

use std::collections::{BTreeMap, HashMap};

use serde::Deserialize;
use serde_json::Value;

use crate::model::{
    AgentInfo, AgentStatus, BlockedKind, PaneInfo, SessionSnapshot, TabInfo, WorkspaceInfo,
};

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct Record {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub progress: Option<u16>,
    #[serde(default)]
    pub app: Option<String>,
    #[serde(default)]
    pub effective_app: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub msg: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
    /// The process that sent the report.
    #[serde(default)]
    pub owner: Option<Owner>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct Owner {
    pub pid: u32,
    #[serde(default)]
    pub name: String,
}

impl Record {
    pub fn status(&self) -> AgentStatus {
        match self.state.as_str() {
            "blocked" => AgentStatus::Blocked,
            "error" => AgentStatus::Error,
            "working" => AgentStatus::Working,
            "done" => AgentStatus::Done,
            // Spec: treat unknown states as idle.
            _ => AgentStatus::Idle,
        }
    }

    pub fn blocked_kind(&self) -> Option<BlockedKind> {
        match self.kind.as_deref()? {
            "permission" => Some(BlockedKind::Permission),
            "question" => Some(BlockedKind::Question),
            "auth" => Some(BlockedKind::Auth),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct Proc {
    #[serde(default)]
    pub pid: Option<u32>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub argv0: Option<String>,
    #[serde(default)]
    pub script: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
}

impl Proc {
    /// Spec: label with the first of script, argv0, name.
    pub fn label(&self) -> Option<String> {
        let pick = self
            .script
            .as_deref()
            .or(self.argv0.as_deref())
            .or(self.name.as_deref())?;
        let base = pick
            .rsplit('/')
            .next()
            .unwrap_or(pick)
            .trim_start_matches('-');
        (!base.is_empty()).then(|| base.to_string())
    }
}

#[derive(Clone, Debug, Default)]
pub struct Window {
    pub id: String,
    pub label: String,
    pub focused_block_id: Option<String>,
    pub blocks: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct Session {
    pub id: String,
    pub label: String,
    pub revision: u64,
    pub active_window_id: Option<String>,
    pub windows: Vec<Window>,
}

#[derive(Clone, Debug, Default)]
pub struct Block {
    pub session_id: String,
    pub window_id: String,
    pub label: Option<String>,
    pub records: BTreeMap<String, Record>,
    pub title: Option<String>,
    pub process: Option<Proc>,
    pub status_seq: u64,
    pub title_seq: u64,
    pub proc_seq: u64,
    /// `done`/`error` records the user has looked at, by record id -> `updated_at`.
    /// Rex keeps them until replaced ("the terminal decides when to stop showing
    /// them"), so we hide them locally once the block gains focus.
    pub seen: HashMap<String, Option<String>>,
}

impl Block {
    fn is_seen(&self, id: &str, r: &Record) -> bool {
        matches!(r.status(), AgentStatus::Done | AgentStatus::Error)
            && self.seen.get(id).is_some_and(|at| *at == r.updated_at)
    }

    /// Mark every current `done`/`error` record as seen. Returns true if any changed.
    pub fn mark_seen(&mut self) -> bool {
        let fresh: HashMap<String, Option<String>> = self
            .records
            .iter()
            .filter(|(_, r)| matches!(r.status(), AgentStatus::Done | AgentStatus::Error))
            .map(|(id, r)| (id.clone(), r.updated_at.clone()))
            .collect();
        let changed = fresh.iter().any(|(id, at)| self.seen.get(id) != Some(at));
        self.seen = fresh;
        changed
    }

    /// Newest record of any state, seen or not: keeps app/title after a done is seen.
    pub fn newest(&self) -> Option<&Record> {
        self.records
            .values()
            .max_by(|a, b| a.updated_at.cmp(&b.updated_at))
    }

    /// Most urgent unseen record; ties go to the newest report.
    pub fn top(&self) -> Option<&Record> {
        self.records
            .iter()
            .filter(|(id, r)| !self.is_seen(id, r))
            .map(|(_, r)| r)
            .min_by(|a, b| {
                a.status()
                    .attention_rank()
                    .cmp(&b.status().attention_rank())
                    .then_with(|| b.updated_at.cmp(&a.updated_at))
            })
    }

    /// Seen-only records read as idle: the program reported, and the user has looked.
    pub fn status(&self) -> AgentStatus {
        match self.top() {
            Some(r) => r.status(),
            None if self.records.is_empty() => AgentStatus::Unknown,
            None => AgentStatus::Idle,
        }
    }

    /// Top record is a permission prompt whose reporter is still the foreground
    /// process. Compare pid and name: pids are reused.
    pub fn prompt_live(&self) -> bool {
        let (Some(top), Some(fg)) = (self.top(), self.process.as_ref()) else {
            return false;
        };
        let Some(owner) = top.owner.as_ref() else {
            return false;
        };
        top.status() == AgentStatus::Blocked
            && top.blocked_kind() == Some(BlockedKind::Permission)
            && fg.pid == Some(owner.pid)
            && fg.name.as_deref() == Some(owner.name.as_str())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Facet {
    Status,
    Title,
    Process,
}

#[derive(Debug, Default)]
pub struct Mirror {
    pub sessions: Vec<Session>,
    pub blocks: HashMap<String, Block>,
}

impl Mirror {
    pub fn session(&self, id: &str) -> Option<&Session> {
        self.sessions.iter().find(|s| s.id == id)
    }

    pub fn add_session(&mut self, id: &str, label: &str) {
        if self.session(id).is_none() {
            self.sessions.push(Session {
                id: id.into(),
                label: label.into(),
                ..Session::default()
            });
        }
    }

    pub fn remove_session(&mut self, id: &str) {
        self.sessions.retain(|s| s.id != id);
        self.blocks.retain(|_, b| b.session_id != id);
    }

    /// `before` is the session now directly after `id`; none means last.
    pub fn move_session(&mut self, id: &str, before: Option<&str>) {
        let Some(i) = self.sessions.iter().position(|s| s.id == id) else {
            return;
        };
        let s = self.sessions.remove(i);
        let at = before
            .and_then(|b| self.sessions.iter().position(|s| s.id == b))
            .unwrap_or(self.sessions.len());
        self.sessions.insert(at, s);
    }

    pub fn set_window_label(&mut self, session_id: &str, window_id: &str, label: &str) {
        if let Some(w) = self
            .sessions
            .iter_mut()
            .filter(|s| s.id == session_id)
            .flat_map(|s| s.windows.iter_mut())
            .find(|w| w.id == window_id)
        {
            w.label = label.into();
        }
    }

    pub fn set_session_label(&mut self, id: &str, label: &str) {
        if let Some(s) = self.sessions.iter_mut().find(|s| s.id == id) {
            s.label = label.into();
        }
    }

    pub fn session_revision(&self, id: &str) -> Option<u64> {
        self.session(id).map(|s| s.revision)
    }

    /// Apply a `session.view` result. Returns blocks that are new and need their facets loaded.
    pub fn apply_view(&mut self, session_id: &str, view: &Value) -> Vec<String> {
        let revision = view["revision"].as_u64().unwrap_or(0);
        let Some(sess) = self.sessions.iter_mut().find(|s| s.id == session_id) else {
            return Vec::new();
        };
        if revision < sess.revision {
            return Vec::new();
        }
        let was = focused_block(sess);
        sess.revision = revision;
        if let Some(l) = view["label"].as_str() {
            sess.label = l.into();
        }
        sess.active_window_id = view["active_window_id"].as_str().map(str::to_string);
        let mut placed: Vec<(String, String, Option<String>)> = Vec::new();
        sess.windows = view["windows"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .map(|w| {
                let id = w["window_id"].as_str().unwrap_or_default().to_string();
                let mut blocks = Vec::new();
                for layer in w["layers"]
                    .as_array()
                    .map(Vec::as_slice)
                    .unwrap_or_default()
                {
                    for b in layer["blocks"]
                        .as_array()
                        .map(Vec::as_slice)
                        .unwrap_or_default()
                    {
                        let Some(bid) = b["block_id"].as_str() else {
                            continue;
                        };
                        if b["creator_name"]
                            .as_str()
                            .is_some_and(|c| c != "com.superlogical.terminal")
                        {
                            continue;
                        }
                        blocks.push(bid.to_string());
                        placed.push((
                            bid.to_string(),
                            id.clone(),
                            b["label"].as_str().map(str::to_string),
                        ));
                    }
                }
                Window {
                    label: w["label"].as_str().unwrap_or_default().to_string(),
                    focused_block_id: w["focused_block_id"].as_str().map(str::to_string),
                    id,
                    blocks,
                }
            })
            .collect();
        let now = focused_block(sess);

        let mut fresh = Vec::new();
        let keep: std::collections::HashSet<&str> =
            placed.iter().map(|(b, _, _)| b.as_str()).collect();
        self.blocks
            .retain(|id, b| b.session_id != session_id || keep.contains(id.as_str()));
        for (bid, wid, label) in placed {
            match self.blocks.get_mut(&bid) {
                Some(b) => {
                    b.session_id = session_id.into();
                    b.window_id = wid;
                    b.label = label;
                }
                None => {
                    self.blocks.insert(
                        bid.clone(),
                        Block {
                            session_id: session_id.into(),
                            window_id: wid,
                            label,
                            ..Block::default()
                        },
                    );
                    fresh.push(bid);
                }
            }
        }
        // Focus moved onto a block (window switch, pane switch): the user is looking at it.
        if let Some(now) = now.filter(|n| was.as_ref() != Some(n)) {
            self.mark_seen(&now);
        }
        fresh
    }

    /// Returns true if a visible status changed.
    pub fn mark_seen(&mut self, block_id: &str) -> bool {
        self.blocks.get_mut(block_id).is_some_and(Block::mark_seen)
    }

    pub fn remove_block(&mut self, id: &str) -> bool {
        self.blocks.remove(id).is_some()
    }

    fn admit(&mut self, block_id: &str, facet: Facet, seq: u64) -> Option<&mut Block> {
        let b = self.blocks.get_mut(block_id)?;
        let last = match facet {
            Facet::Status => &mut b.status_seq,
            Facet::Title => &mut b.title_seq,
            Facet::Process => &mut b.proc_seq,
        };
        // An event is applied once; a fetch result (seq S) supersedes events <= S.
        if seq != 0 && seq <= *last {
            return None;
        }
        *last = (*last).max(seq);
        Some(b)
    }

    /// `program_status` result. `seq` is the response seq.
    pub fn load_status(&mut self, block_id: &str, seq: u64, result: &Value) -> bool {
        let records: Vec<Record> =
            serde_json::from_value(result["records"].clone()).unwrap_or_default();
        let Some(b) = self.blocks.get_mut(block_id) else {
            return false;
        };
        if seq < b.status_seq {
            return false;
        }
        b.status_seq = seq;
        b.records = records
            .into_iter()
            .map(|r| (r.id.clone().unwrap_or_default(), r))
            .collect();
        true
    }

    pub fn load_title(&mut self, block_id: &str, seq: u64, result: &Value) -> bool {
        let Some(b) = self.blocks.get_mut(block_id) else {
            return false;
        };
        if seq < b.title_seq {
            return false;
        }
        b.title_seq = seq;
        b.title = result["title"]
            .as_str()
            .filter(|t| !t.is_empty())
            .map(str::to_string);
        true
    }

    pub fn load_process(&mut self, block_id: &str, seq: u64, result: &Value) -> bool {
        let Some(b) = self.blocks.get_mut(block_id) else {
            return false;
        };
        if seq < b.proc_seq {
            return false;
        }
        b.proc_seq = seq;
        b.process = serde_json::from_value(result["foreground"].clone()).ok();
        true
    }

    /// A terminal `block_event`. Returns true if visible state changed.
    pub fn apply_block_event(&mut self, seq: u64, block_id: &str, name: &str, p: &Value) -> bool {
        match name {
            "program_status_changed" => {
                let Ok(rec) = serde_json::from_value::<Record>(p["record"].clone()) else {
                    return false;
                };
                let id = p["id"].as_str().unwrap_or_default().to_string();
                let Some(b) = self.admit(block_id, Facet::Status, seq) else {
                    return false;
                };
                b.records.insert(id, rec);
                true
            }
            "program_status_removed" => {
                let Some(b) = self.admit(block_id, Facet::Status, seq) else {
                    return false;
                };
                for id in p["ids"].as_array().map(Vec::as_slice).unwrap_or_default() {
                    if let Some(id) = id.as_str() {
                        b.records.remove(id);
                    }
                }
                true
            }
            "child_exited" => {
                let Some(b) = self.admit(block_id, Facet::Status, seq) else {
                    return false;
                };
                b.records.clear();
                true
            }
            "title_changed" => {
                let Some(b) = self.admit(block_id, Facet::Title, seq) else {
                    return false;
                };
                b.title = p["title"]
                    .as_str()
                    .filter(|t| !t.is_empty())
                    .map(str::to_string);
                true
            }
            "process_changed" => {
                let Some(b) = self.admit(block_id, Facet::Process, seq) else {
                    return false;
                };
                b.process = serde_json::from_value(p["foreground"].clone()).ok();
                true
            }
            _ => false,
        }
    }

    pub fn locate(&self, block_id: &str) -> Option<(&str, &str)> {
        self.blocks
            .get(block_id)
            .map(|b| (b.session_id.as_str(), b.window_id.as_str()))
    }

    pub fn snapshot(&self) -> SessionSnapshot {
        let mut snap = SessionSnapshot {
            version: "rex".into(),
            protocol: super::conn::API_REVISION as u32,
            ..SessionSnapshot::default()
        };
        let mut order = 0u64;
        for (si, s) in self.sessions.iter().enumerate() {
            let mut ws_status = AgentStatus::Unknown;
            let mut pane_count = 0;
            for (wi, w) in s.windows.iter().enumerate() {
                let active = s.active_window_id.as_deref() == Some(w.id.as_str());
                let mut tab_status = AgentStatus::Unknown;
                for bid in &w.blocks {
                    let Some(b) = self.blocks.get(bid) else {
                        continue;
                    };
                    pane_count += 1;
                    let status = b.status();
                    tab_status = worst(tab_status, status);
                    let focused = active && w.focused_block_id.as_deref() == Some(bid.as_str());
                    let mut agent = agent_info(bid, b, s, w, focused);
                    agent.order = Some(order);
                    order += 1;
                    snap.panes.push(PaneInfo {
                        pane_id: bid.clone(),
                        workspace_id: s.id.clone(),
                        tab_id: w.id.clone(),
                        terminal_id: bid.clone(),
                        agent_status: status,
                        focused,
                        agent: agent.agent.clone(),
                        title: agent.title.clone(),
                        display_agent: agent.display_agent.clone(),
                        terminal_title: b.title.clone(),
                        terminal_title_stripped: b.title.clone(),
                        cwd: agent.cwd.clone(),
                        foreground_cwd: agent.cwd.clone(),
                        revision: 0,
                        label: b.label.clone(),
                    });
                    snap.agents.push(agent);
                }
                ws_status = worst(ws_status, tab_status);
                snap.tabs.push(TabInfo {
                    tab_id: w.id.clone(),
                    workspace_id: s.id.clone(),
                    label: Some(w.label.clone()),
                    number: wi as u64 + 1,
                    focused: active,
                    agent_status: tab_status,
                });
            }
            snap.workspaces.push(WorkspaceInfo {
                workspace_id: s.id.clone(),
                number: si as u64 + 1,
                label: s.label.clone(),
                // Rex has no event for which session the app shows.
                focused: false,
                pane_count,
                tab_count: s.windows.len() as u64,
                active_tab_id: s.active_window_id.clone().unwrap_or_default(),
                agent_status: ws_status,
            });
        }
        snap
    }
}

/// The block that receives typing: the active window's focused block.
fn focused_block(s: &Session) -> Option<String> {
    let active = s.active_window_id.as_deref()?;
    s.windows
        .iter()
        .find(|w| w.id == active)?
        .focused_block_id
        .clone()
}

fn worst(a: AgentStatus, b: AgentStatus) -> AgentStatus {
    if b.attention_rank() < a.attention_rank() {
        b
    } else {
        a
    }
}

fn agent_info(id: &str, b: &Block, s: &Session, w: &Window, focused: bool) -> AgentInfo {
    let top = b.top();
    let shown = top.or_else(|| b.newest());
    let proc_label = b.process.as_ref().and_then(Proc::label);
    let app = shown
        .and_then(|r| r.effective_app.clone().or_else(|| r.app.clone()))
        .or_else(|| proc_label.clone());
    // A name the user gave (`rex block rename` / `rex window rename`) beats the program.
    let name = custom_block_label(b).or_else(|| custom_window_label(w));
    AgentInfo {
        terminal_id: id.into(),
        name: name.clone(),
        agent: app.clone(),
        display_agent: name.or(app),
        title: shown
            .and_then(|r| r.title.clone())
            .or_else(|| b.title.clone()),
        terminal_title: b.title.clone(),
        terminal_title_stripped: b.title.clone(),
        agent_status: b.status(),
        workspace_id: s.id.clone(),
        tab_id: w.id.clone(),
        pane_id: id.into(),
        focused,
        cwd: b.process.as_ref().and_then(|p| p.cwd.clone()),
        foreground_cwd: b.process.as_ref().and_then(|p| p.cwd.clone()),
        blocked_kind: top.and_then(Record::blocked_kind),
        progress: top.and_then(|r| r.progress).map(|p| p.min(100) as u8),
        msg: top.and_then(|r| r.msg.clone()),
        status_stamp: top.and_then(|r| r.updated_at.clone()),
        place: Some(format!("{} / {}", s.label, w.label)),
        prompt_live: b.prompt_live(),
        ..AgentInfo::default()
    }
}

fn custom_block_label(b: &Block) -> Option<String> {
    b.label.clone().filter(|l| !l.is_empty() && l != "shell")
}

/// Rex names an unnamed window `Window <last 8 of its id>`.
fn custom_window_label(w: &Window) -> Option<String> {
    let tail = &w.id[w.id.len().saturating_sub(8)..];
    Some(w.label.clone()).filter(|l| !l.is_empty() && *l != format!("Window {tail}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn view(rev: u64, blocks: &[&str]) -> Value {
        json!({
            "revision": rev,
            "label": "work",
            "active_window_id": "window:00000000-0000-7000-8000-0000000000a1",
            "windows": [{
                "window_id": "window:00000000-0000-7000-8000-0000000000a1",
                "label": "Window 000000a1",
                "focused_block_id": blocks.first(),
                "layers": [{"blocks": blocks.iter().map(|b| json!({
                    "block_id": b, "label": "shell", "creator_name": "com.superlogical.terminal"
                })).collect::<Vec<_>>()}]
            }]
        })
    }

    fn mirror() -> Mirror {
        let mut m = Mirror::default();
        m.add_session("session:1", "work");
        let fresh = m.apply_view("session:1", &view(5, &["block:a", "block:b"]));
        assert_eq!(fresh, vec!["block:a", "block:b"]);
        m
    }

    fn status_ev(state: &str, kind: Option<&str>, at: &str) -> Value {
        json!({"id": "", "record": {"state": state, "kind": kind, "app": "claude-code",
            "effective_app": "claude-code", "msg": "Edit main.rs?", "updated_at": at,
            "owner": {"pid": 42, "name": "2.1.312"}}})
    }

    fn fg(m: &mut Mirror, block: &str, seq: u64, pid: u32, name: &str) {
        m.apply_block_event(
            seq,
            block,
            "process_changed",
            &json!({"foreground": {"pid": pid, "name": name}}),
        );
    }

    #[test]
    fn prompt_live_needs_owner_in_foreground() {
        let mut m = mirror();
        m.apply_block_event(
            1,
            "block:a",
            "program_status_changed",
            &status_ev("blocked", Some("permission"), "t1"),
        );
        assert!(!m.blocks["block:a"].prompt_live(), "no foreground known");
        fg(&mut m, "block:a", 2, 42, "2.1.312");
        assert!(m.blocks["block:a"].prompt_live());
        assert!(m.snapshot().agents[0].awaits_permission());
        // Agent exits uncleanly: record stays, shell is foreground again.
        fg(&mut m, "block:a", 3, 7, "zsh");
        assert!(!m.blocks["block:a"].prompt_live());
        let snap = m.snapshot();
        assert_eq!(
            snap.agents[0].agent_status,
            AgentStatus::Blocked,
            "still shown"
        );
        assert!(!snap.agents[0].awaits_permission(), "but not answerable");
        // Reused pid, different program.
        fg(&mut m, "block:a", 4, 42, "vim");
        assert!(!m.blocks["block:a"].prompt_live());
    }

    #[test]
    fn pads_follow_window_and_layout_order() {
        let mut m = Mirror::default();
        m.add_session("session:1", "work");
        let two_windows = |order: [(&str, &[&str]); 2]| {
            json!({"revision": 0, "windows": order.iter().map(|(w, bs)| json!({
                "window_id": w, "label": "x",
                "layers": [{"blocks": bs.iter().map(|b| json!({"block_id": b})).collect::<Vec<_>>()}]
            })).collect::<Vec<_>>()})
        };
        m.apply_view(
            "session:1",
            &two_windows([
                ("window:1", &["block:z", "block:a"]),
                ("window:2", &["block:m"]),
            ]),
        );
        let ids = |m: &Mirror| {
            m.snapshot()
                .agents
                .into_iter()
                .map(|a| (a.pane_id, a.order.unwrap()))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            ids(&m),
            [
                ("block:z".into(), 0),
                ("block:a".into(), 1),
                ("block:m".into(), 2)
            ]
        );
        // rex window move: window 2 first.
        let mut v = two_windows([
            ("window:2", &["block:m"]),
            ("window:1", &["block:z", "block:a"]),
        ]);
        v["revision"] = json!(1);
        m.apply_view("session:1", &v);
        assert_eq!(
            ids(&m),
            [
                ("block:m".into(), 0),
                ("block:z".into(), 1),
                ("block:a".into(), 2)
            ]
        );
    }

    #[test]
    fn renamed_window_names_the_pad() {
        let mut m = Mirror::default();
        m.add_session("session:1", "work");
        let view = |label: &str| {
            json!({"revision": 1, "windows": [{
            "window_id": "window:01a159ff-72ac-7c8e-8a8f-d459a514f7bb", "label": label,
            "layers": [{"blocks": [{"block_id": "block:a", "label": "shell"}]}]}]})
        };
        m.apply_view("session:1", &view("Window a514f7bb"));
        m.load_process(
            "block:a",
            1,
            &json!({"foreground": {"name": "zsh", "argv0": "-zsh"}}),
        );
        assert_eq!(
            m.snapshot().agents[0].label(),
            "zsh",
            "default window name ignored"
        );
        m.set_window_label(
            "session:1",
            "window:01a159ff-72ac-7c8e-8a8f-d459a514f7bb",
            "auth-fix",
        );
        let a = &m.snapshot().agents[0];
        assert_eq!(a.label(), "auth-fix");
        assert_eq!(a.agent.as_deref(), Some("zsh"));
    }

    #[test]
    fn view_diffs_blocks() {
        let mut m = mirror();
        assert!(
            m.apply_view("session:1", &view(4, &[])).is_empty(),
            "stale view ignored"
        );
        assert_eq!(m.blocks.len(), 2);
        let fresh = m.apply_view("session:1", &view(6, &["block:b", "block:c"]));
        assert_eq!(fresh, vec!["block:c"]);
        assert!(!m.blocks.contains_key("block:a"));
    }

    #[test]
    fn status_event_maps_to_agent() {
        let mut m = mirror();
        assert!(m.apply_block_event(
            7,
            "block:a",
            "program_status_changed",
            &status_ev("blocked", Some("permission"), "2026-01-01T00:00:01Z")
        ));
        let snap = m.snapshot();
        let a = snap.agents.iter().find(|a| a.pane_id == "block:a").unwrap();
        assert_eq!(a.agent_status, AgentStatus::Blocked);
        assert_eq!(a.blocked_kind, Some(BlockedKind::Permission));
        assert!(!a.awaits_permission(), "foreground unknown");
        assert_eq!(a.label(), "claude-code");
        assert_eq!(a.headline(), "Edit main.rs?");
        assert_eq!(a.status_stamp.as_deref(), Some("2026-01-01T00:00:01Z"));
        assert!(a.focused);
        assert_eq!(a.place.as_deref(), Some("work / Window 000000a1"));
        assert_eq!(snap.workspaces[0].agent_status, AgentStatus::Blocked);
        let b = snap.agents.iter().find(|a| a.pane_id == "block:b").unwrap();
        assert_eq!(b.agent_status, AgentStatus::Unknown);
    }

    #[test]
    fn stale_event_does_not_clobber_fetch() {
        let mut m = mirror();
        // Fetch at seq 10 says done; a queued event from seq 8 says working.
        m.load_status(
            "block:a",
            10,
            &json!({"records": [{"state": "done", "updated_at": "t2"}]}),
        );
        assert!(!m.apply_block_event(
            8,
            "block:a",
            "program_status_changed",
            &status_ev("working", None, "t1")
        ));
        assert_eq!(m.blocks["block:a"].status(), AgentStatus::Done);
        assert!(m.apply_block_event(
            11,
            "block:a",
            "program_status_changed",
            &status_ev("working", None, "t3")
        ));
        assert_eq!(m.blocks["block:a"].status(), AgentStatus::Working);
        // Older fetch after a newer event is dropped.
        assert!(!m.load_status("block:a", 9, &json!({"records": []})));
    }

    #[test]
    fn worst_record_wins_and_removal() {
        let mut m = mirror();
        m.apply_block_event(1, "block:a", "program_status_changed", &json!({"id": "build", "record": {"id": "build", "state": "working", "updated_at": "t1"}}));
        m.apply_block_event(2, "block:a", "program_status_changed", &json!({"id": "deploy", "record": {"id": "deploy", "state": "error", "updated_at": "t0"}}));
        assert_eq!(m.blocks["block:a"].status(), AgentStatus::Error);
        m.apply_block_event(
            3,
            "block:a",
            "program_status_removed",
            &json!({"ids": ["deploy"], "reason": "clear"}),
        );
        assert_eq!(m.blocks["block:a"].status(), AgentStatus::Working);
        m.apply_block_event(4, "block:a", "child_exited", &json!({"exit_code": 0}));
        assert_eq!(m.blocks["block:a"].status(), AgentStatus::Unknown);
    }

    #[test]
    fn statusless_block_labels_from_process() {
        let mut m = mirror();
        m.load_process(
            "block:b",
            1,
            &json!({"foreground": {"name": "2.1.311", "argv0": "claude", "cwd": "/src"}}),
        );
        m.apply_block_event(2, "block:b", "title_changed", &json!({"title": "fix auth"}));
        let snap = m.snapshot();
        let b = snap.agents.iter().find(|a| a.pane_id == "block:b").unwrap();
        assert_eq!(b.label(), "claude");
        assert_eq!(b.headline(), "fix auth");
        assert_eq!(b.cwd.as_deref(), Some("/src"));
        let login = Proc {
            argv0: Some("-zsh".into()),
            ..Proc::default()
        };
        assert_eq!(login.label().as_deref(), Some("zsh"));
    }

    #[test]
    fn session_order() {
        let mut m = Mirror::default();
        for id in ["a", "b", "c"] {
            m.add_session(id, id);
        }
        m.move_session("c", Some("a"));
        let ids: Vec<_> = m.sessions.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["c", "a", "b"]);
        m.move_session("c", None);
        let ids: Vec<_> = m.sessions.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["a", "b", "c"]);
        m.remove_session("a");
        assert_eq!(m.snapshot().workspaces[0].number, 1);
    }

    #[test]
    fn done_clears_once_focused() {
        let mut m = mirror();
        let status = |m: &Mirror, b: &str| {
            let snap = m.snapshot();
            snap.agents
                .iter()
                .find(|a| a.pane_id == b)
                .unwrap()
                .agent_status
        };
        m.apply_block_event(
            1,
            "block:b",
            "program_status_changed",
            &status_ev("done", None, "t1"),
        );
        assert_eq!(status(&m, "block:b"), AgentStatus::Done);

        // Focus moves a -> b: seen. App survives from the hidden record.
        m.apply_view("session:1", &view(6, &["block:b", "block:a"]));
        assert_eq!(status(&m, "block:b"), AgentStatus::Idle);
        let snap = m.snapshot();
        let b = snap.agents.iter().find(|a| a.pane_id == "block:b").unwrap();
        assert_eq!(b.agent.as_deref(), Some("claude-code"));

        // A new done (new updated_at) shows again while focus doesn't move.
        m.apply_block_event(
            2,
            "block:b",
            "program_status_changed",
            &status_ev("done", None, "t2"),
        );
        assert_eq!(status(&m, "block:b"), AgentStatus::Done);
        // A view refresh without a focus change doesn't hide it.
        m.apply_view("session:1", &view(7, &["block:b", "block:a"]));
        assert_eq!(status(&m, "block:b"), AgentStatus::Done);
        // Explicit focus from the surface does.
        assert!(m.mark_seen("block:b"));
        assert!(!m.mark_seen("block:b"));
        assert_eq!(status(&m, "block:b"), AgentStatus::Idle);

        // Blocked is never hidden by focus.
        m.apply_block_event(
            3,
            "block:b",
            "program_status_changed",
            &status_ev("blocked", Some("permission"), "t3"),
        );
        m.mark_seen("block:b");
        assert_eq!(status(&m, "block:b"), AgentStatus::Blocked);
    }
}
