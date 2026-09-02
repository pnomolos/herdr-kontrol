use super::occupancy::{Focus, Occupancy, OccupancySource, Occupant};
use crate::device::IndexedColor;
use crate::herdr::{AgentInfo, AgentStatus, PaneInfo, SessionSnapshot, WorkspaceInfo};

impl From<AgentStatus> for Occupancy {
    fn from(status: AgentStatus) -> Self {
        match status {
            AgentStatus::Blocked => Self::Blocked,
            AgentStatus::Working => Self::Working,
            AgentStatus::Done | AgentStatus::Idle => Self::Idle,
            AgentStatus::Unknown => Self::Unknown,
        }
    }
}

impl From<&AgentInfo> for Occupant {
    fn from(a: &AgentInfo) -> Self {
        Occupant {
            id: a.pane_id.clone(),
            occupancy: Occupancy::from(a.agent_status),
        }
    }
}

impl From<&AgentInfo> for Focus {
    fn from(a: &AgentInfo) -> Self {
        Self {
            occupant_id: a.pane_id.clone(),
        }
    }
}

fn pane_is_shell(pane: &PaneInfo) -> bool {
    let has_agent = pane
        .agent
        .as_deref()
        .map(|s| !s.is_empty())
        .unwrap_or(false);
    !has_agent && matches!(pane.agent_status, AgentStatus::Unknown | AgentStatus::Idle)
}

fn occupants(agents: Vec<AgentInfo>, panes: Vec<PaneInfo>) -> Vec<AgentInfo> {
    if agents.is_empty() {
        return panes.into_iter().map(PaneInfo::into_agent).collect();
    }
    let mut seen: std::collections::HashSet<String> =
        agents.iter().map(|a| a.pane_id.clone()).collect();
    let mut out = agents;
    for pane in panes {
        if pane_is_shell(&pane) {
            continue;
        }
        if seen.insert(pane.pane_id.clone()) {
            out.push(pane.into_agent());
        }
    }
    out
}

/// Primary glance bank. 6–8 agents is the working set; extras page via ← →.
/// Maps to physical pads 0..PAGE (MK3 bottom two rows).
pub const PAGE: usize = 8;

#[derive(Clone, Debug, Default)]
pub struct AttentionModel {
    pub workspaces: Vec<WorkspaceInfo>,
    pub agents: Vec<AgentInfo>,
    pub page: usize,
    pub selected: usize,
    pub connected: bool,
    pub pulse: bool,
}

impl AttentionModel {
    pub fn from_snapshot(snap: SessionSnapshot) -> Self {
        let mut m = Self {
            workspaces: snap.workspaces,
            agents: occupants(snap.agents, snap.panes),
            connected: true,
            ..Self::default()
        };
        m.sort_workspaces();
        m.sort_agents();
        m.retarget_attention();
        m.clamp();
        m
    }

    pub fn apply_snapshot(&mut self, snap: SessionSnapshot) {
        let sel_id = self.selected_pane_id();
        self.workspaces = snap.workspaces;
        self.agents = occupants(snap.agents, snap.panes);
        self.connected = true;
        self.sort_workspaces();
        self.sort_agents();
        if let Some(id) = sel_id {
            if let Some(i) = self.agents.iter().position(|a| a.pane_id == id) {
                self.selected = i;
            }
        }
        self.retarget_attention();
        self.clamp();
    }

    /// Steal selection for a blocked occupant unless the current one is already blocked.
    fn retarget_attention(&mut self) {
        let some_blocked = self
            .agents
            .iter()
            .any(|a| a.agent_status == AgentStatus::Blocked);
        if !some_blocked {
            return;
        }
        let selected_blocked = self
            .selected_agent()
            .is_some_and(|a| a.agent_status == AgentStatus::Blocked);
        if selected_blocked {
            return;
        }
        if let Some(i) = self
            .agents
            .iter()
            .position(|a| a.agent_status == AgentStatus::Blocked)
        {
            self.selected = i;
        }
    }

    pub fn sort_workspaces(&mut self) {
        self.workspaces.sort_by_key(|w| w.number);
    }

    pub fn sort_agents(&mut self) {
        self.agents.sort_by(|a, b| {
            a.agent_status
                .attention_rank()
                .cmp(&b.agent_status.attention_rank())
                .then(b.state_change_seq.cmp(&a.state_change_seq))
                .then(a.pane_id.cmp(&b.pane_id))
        });
    }

    pub fn page_count(&self) -> usize {
        self.agents.len().div_ceil(PAGE).max(1)
    }

    pub fn clamp(&mut self) {
        let pages = self.page_count();
        if self.page >= pages {
            self.page = pages - 1;
        }
        if self.agents.is_empty() {
            self.selected = 0;
            return;
        }
        if self.selected >= self.agents.len() {
            self.selected = self.agents.len() - 1;
        }
        let start = self.page.saturating_mul(PAGE);
        let end = (start + PAGE).min(self.agents.len());
        if self.selected < start || self.selected >= end {
            self.page = self.selected / PAGE;
        }
    }

    pub fn selected_pane_id(&self) -> Option<String> {
        self.agents.get(self.selected).map(|a| a.pane_id.clone())
    }

    pub fn selected_agent(&self) -> Option<&AgentInfo> {
        self.agents.get(self.selected)
    }

    /// LCD-visible state only; skip unselected OSC chatter so spinner ticks don't 522KB-blit.
    pub fn view_sig(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.connected.hash(&mut h);
        self.page.hash(&mut h);
        self.selected.hash(&mut h);
        self.agents.len().hash(&mut h);
        for a in &self.agents {
            a.pane_id.hash(&mut h);
            a.agent_status.hash(&mut h);
            a.name.hash(&mut h);
            a.display_agent.hash(&mut h);
            a.agent.hash(&mut h);
            a.focused.hash(&mut h);
            // Working titles spin; queue paints the rest.
            if a.agent_status != AgentStatus::Working {
                a.terminal_title_stripped.hash(&mut h);
                a.title.hash(&mut h);
                a.tokens.get("summary").hash(&mut h);
            }
        }
        if let Some(a) = self.selected_agent() {
            a.terminal_title_stripped.hash(&mut h);
            a.title.hash(&mut h);
            a.tokens.get("summary").hash(&mut h);
            a.state_labels.hash(&mut h);
        }
        for w in self.workspaces.iter().take(8) {
            w.workspace_id.hash(&mut h);
            w.agent_status.hash(&mut h);
        }
        h.finish()
    }

    pub fn pad_slots(&self) -> [Option<&AgentInfo>; PAGE] {
        let mut slots = [None; PAGE];
        let start = self.page.saturating_mul(PAGE);
        for (i, slot) in slots.iter_mut().enumerate() {
            *slot = self.agents.get(start + i);
        }
        slots
    }

    pub fn select_pad(&mut self, physical: u8) -> Option<&AgentInfo> {
        if (physical as usize) >= PAGE {
            return None;
        }
        let idx = self.page.saturating_mul(PAGE) + physical as usize;
        if idx < self.agents.len() {
            self.selected = idx;
            self.agents.get(self.selected)
        } else {
            None
        }
    }

    pub fn nudge_selection(&mut self, delta: i8) {
        if self.agents.is_empty() {
            return;
        }
        let n = self.agents.len() as i32;
        let next = (self.selected as i32 + delta as i32).rem_euclid(n) as usize;
        self.selected = next;
        self.page = self.selected / PAGE;
    }

    pub fn page_delta(&mut self, delta: i8) {
        let pages = self.page_count() as i32;
        self.page = (self.page as i32 + delta as i32).rem_euclid(pages) as usize;
        self.selected = self
            .page
            .saturating_mul(PAGE)
            .min(self.agents.len().saturating_sub(1));
    }

    pub fn group_workspace(&self, group: u8) -> Option<&WorkspaceInfo> {
        self.workspaces.get(group as usize)
    }

    pub fn drop_pane(&mut self, pane_id: &str) {
        let sel = self.selected_pane_id();
        self.agents.retain(|a| a.pane_id != pane_id);
        self.restore_selection(sel);
        self.retarget_attention();
        self.clamp();
    }

    pub fn status_counts(&self) -> (usize, usize, usize, usize) {
        let mut blocked = 0;
        let mut working = 0;
        let mut done = 0;
        let mut idle = 0;
        for a in &self.agents {
            match a.agent_status {
                AgentStatus::Blocked => blocked += 1,
                AgentStatus::Working => working += 1,
                AgentStatus::Done => done += 1,
                AgentStatus::Idle | AgentStatus::Unknown => idle += 1,
            }
        }
        (blocked, working, done, idle)
    }

    pub fn pad_color(status: AgentStatus, pulse: bool, selected: bool) -> IndexedColor {
        // Occupancy hue; Done is host unread (Idle occupancy, yellow pad).
        // Selected is same hue at max brightness (white silicone eats WHITE/GREY/SKY).
        let base = match Occupancy::from(status) {
            Occupancy::Blocked => {
                if pulse {
                    IndexedColor::PINK
                } else {
                    IndexedColor::MAGENTA
                }
            }
            Occupancy::Working => IndexedColor::GREEN,
            Occupancy::Idle if status == AgentStatus::Done => IndexedColor::YELLOW,
            Occupancy::Idle => IndexedColor::BLUE,
            Occupancy::Unknown => IndexedColor::DIM_BLUE,
        };
        if selected {
            IndexedColor(base.0 | 0x03)
        } else {
            base
        }
    }

    pub fn upsert_status(
        &mut self,
        pane_id: &str,
        workspace_id: String,
        status: AgentStatus,
        title: Option<String>,
        display_agent: Option<String>,
        agent: Option<String>,
    ) {
        if let Some(a) = self.agents.iter_mut().find(|a| a.pane_id == pane_id) {
            a.agent_status = status;
            if let Some(t) = title {
                a.title = Some(t);
            }
            if let Some(d) = display_agent {
                a.display_agent = Some(d);
            }
            if let Some(ag) = agent {
                a.agent = Some(ag);
            }
            a.state_change_seq = a.state_change_seq.saturating_add(1);
        } else if !pane_id.is_empty() {
            self.agents.push(crate::herdr::AgentInfo {
                terminal_id: pane_id.to_string(),
                name: None,
                agent,
                title,
                display_agent,
                terminal_title: None,
                terminal_title_stripped: None,
                agent_status: status,
                state_labels: Default::default(),
                tokens: Default::default(),
                workspace_id,
                tab_id: String::new(),
                pane_id: pane_id.to_string(),
                focused: false,
                state_change_seq: 1,
                cwd: None,
                foreground_cwd: None,
                revision: 0,
            });
        }
        let sel = self.selected_pane_id();
        self.sort_agents();
        self.restore_selection(sel);
        self.retarget_attention();
        self.clamp();
    }

    fn restore_selection(&mut self, pane_id: Option<String>) {
        if let Some(id) = pane_id {
            if let Some(i) = self.agents.iter().position(|a| a.pane_id == id) {
                self.selected = i;
            }
        }
    }

    /// Touch-strip segments (0..=25). Idle/unknown do not fill; saturates at [PAGE].
    pub fn strip_fill(&self) -> usize {
        let (blocked, working, done, _) = self.status_counts();
        let n = (blocked + working + done).min(PAGE);
        (n * 25) / PAGE
    }

    pub fn workspace_color(status: AgentStatus) -> IndexedColor {
        match Occupancy::from(status) {
            Occupancy::Blocked => IndexedColor::PINK,
            Occupancy::Working => IndexedColor::GREEN,
            Occupancy::Idle if status == AgentStatus::Done => IndexedColor::YELLOW,
            Occupancy::Idle | Occupancy::Unknown => IndexedColor::WHITE,
        }
    }
}

impl OccupancySource for AttentionModel {
    fn occupants(&self) -> Vec<Occupant> {
        self.agents.iter().map(Occupant::from).collect()
    }

    fn focus(&self, id: &str) -> Option<Focus> {
        self.agents
            .iter()
            .find(|a| a.pane_id == id)
            .map(Focus::from)
    }

    fn glance(&self, page: usize, cells: usize) -> Vec<Occupant> {
        if cells == 0 {
            return Vec::new();
        }
        let start = page.saturating_mul(PAGE);
        self.agents
            .iter()
            .skip(start)
            .take(PAGE.min(cells))
            .map(Occupant::from)
            .collect()
    }

    fn visible(&self, cells: usize) -> Vec<Occupant> {
        self.glance(self.page, cells)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(id: &str, status: AgentStatus, seq: u64) -> AgentInfo {
        AgentInfo {
            terminal_id: id.into(),
            name: Some(id.into()),
            agent: Some("claude".into()),
            title: None,
            display_agent: None,
            terminal_title: None,
            terminal_title_stripped: None,
            agent_status: status,
            state_labels: Default::default(),
            tokens: Default::default(),
            workspace_id: "w1".into(),
            tab_id: "w1:t1".into(),
            pane_id: id.into(),
            focused: false,
            state_change_seq: seq,
            cwd: None,
            foreground_cwd: None,
            revision: 1,
        }
    }

    #[test]
    fn attention_order() {
        let mut snap = empty_snap();
        snap.agents = vec![
            agent("idle", AgentStatus::Idle, 1),
            agent("block", AgentStatus::Blocked, 1),
            agent("work", AgentStatus::Working, 9),
            agent("done", AgentStatus::Done, 3),
        ];
        let m = AttentionModel::from_snapshot(snap);
        let ids: Vec<_> = m.agents.iter().map(|a| a.pane_id.as_str()).collect();
        assert_eq!(ids, ["block", "work", "done", "idle"]);
    }

    #[test]
    fn page_size_is_working_set() {
        assert_eq!(PAGE, 8);
        assert_eq!(crate::surface::MaschineCaps::mk3().grid.cells(), 16);
    }

    #[test]
    fn paging() {
        let agents: Vec<_> = (0..20)
            .map(|i| agent(&format!("a{i}"), AgentStatus::Idle, i))
            .collect();
        let mut m = AttentionModel {
            agents,
            connected: true,
            ..Default::default()
        };
        m.sort_agents();
        assert_eq!(m.page_count(), 3);
        m.page_delta(1);
        assert_eq!(m.page, 1);
        assert!(m.select_pad(0).is_some());
        assert!(m.select_pad(PAGE as u8).is_none());
    }

    #[test]
    fn pad_slots_are_the_working_set() {
        let agents: Vec<_> = (0..20)
            .map(|i| agent(&format!("a{i:02}"), AgentStatus::Idle, 1))
            .collect();
        let mut m = AttentionModel {
            agents,
            connected: true,
            page: 1,
            ..Default::default()
        };
        m.sort_agents();
        let slots = m.pad_slots();
        assert_eq!(slots.len(), PAGE);
        assert_eq!(slots[0].unwrap().pane_id, "a08");
        assert_eq!(slots[7].unwrap().pane_id, "a15");
        assert!(slots.iter().all(|s| s.is_some()));
    }

    #[test]
    fn eight_agents_fit_one_page() {
        let agents: Vec<_> = (0..8)
            .map(|i| agent(&format!("a{i}"), AgentStatus::Idle, i))
            .collect();
        let mut m = AttentionModel {
            agents,
            connected: true,
            ..Default::default()
        };
        m.sort_agents();
        assert_eq!(m.page_count(), 1);
        m.agents.push(agent("overflow", AgentStatus::Idle, 99));
        m.sort_agents();
        m.clamp();
        assert_eq!(m.page_count(), 2);
    }

    #[test]
    fn strip_fill_is_attention_not_headcount() {
        let mut m = AttentionModel {
            agents: (0..8)
                .map(|i| agent(&format!("a{i}"), AgentStatus::Idle, 1))
                .collect(),
            connected: true,
            ..Default::default()
        };
        assert_eq!(m.strip_fill(), 0);
        m.agents[0].agent_status = AgentStatus::Working;
        m.agents[1].agent_status = AgentStatus::Blocked;
        m.agents[2].agent_status = AgentStatus::Done;
        assert_eq!(
            crate::surface::Occupancy::from(AgentStatus::Done),
            crate::surface::Occupancy::Idle
        );
        assert_eq!(m.strip_fill(), 9);
        for a in &mut m.agents {
            a.agent_status = AgentStatus::Working;
        }
        assert_eq!(m.strip_fill(), 25);
        m.agents.push(agent("overflow", AgentStatus::Working, 1));
        assert_eq!(m.strip_fill(), 25);
    }

    fn pane(id: &str, status: AgentStatus, agent: Option<&str>) -> PaneInfo {
        PaneInfo {
            pane_id: id.into(),
            workspace_id: "w1".into(),
            tab_id: "w1:t1".into(),
            terminal_id: id.into(),
            agent_status: status,
            focused: false,
            agent: agent.map(|s| s.into()),
            title: None,
            display_agent: None,
            terminal_title: None,
            terminal_title_stripped: None,
            cwd: None,
            foreground_cwd: None,
            revision: 1,
            label: None,
        }
    }

    #[test]
    fn shells_dropped_when_agents_exist() {
        let snap = SessionSnapshot {
            agents: vec![agent("w1:p1", AgentStatus::Done, 1)],
            panes: vec![
                pane("w1:p1", AgentStatus::Done, Some("claude")),
                pane("w1:p2", AgentStatus::Unknown, None),
                pane("w2:p1", AgentStatus::Unknown, None),
                pane("w3:p1", AgentStatus::Working, Some("grok")),
            ],
            ..serde_json::from_str(
                r#"{"version":"","protocol":0,"workspaces":[],"tabs":[],"panes":[],"agents":[]}"#,
            )
            .unwrap()
        };
        let m = AttentionModel::from_snapshot(snap);
        let ids: Vec<_> = m.agents.iter().map(|a| a.pane_id.as_str()).collect();
        assert_eq!(ids, ["w3:p1", "w1:p1"]);
    }

    #[test]
    fn pad_color_status_hue_survives_selection() {
        use crate::device::IndexedColor;
        assert_eq!(
            AttentionModel::pad_color(AgentStatus::Idle, false, false),
            IndexedColor::BLUE
        );
        assert_eq!(
            AttentionModel::pad_color(AgentStatus::Done, false, false),
            IndexedColor::YELLOW
        );
        let sel_done = AttentionModel::pad_color(AgentStatus::Done, false, true);
        assert_ne!(sel_done, IndexedColor::WHITE);
        assert_eq!(sel_done.0 & !0x03, IndexedColor::YELLOW.0 & !0x03);
        assert_eq!(sel_done.0 & 0x03, 0x03);
        let sel_work = AttentionModel::pad_color(AgentStatus::Working, false, true);
        assert_eq!(sel_work.0 & !0x03, IndexedColor::GREEN.0 & !0x03);
    }

    #[test]
    fn occupancy_source_collapses_done() {
        assert_eq!(Occupancy::from(AgentStatus::Done), Occupancy::Idle);
        assert_eq!(Occupancy::from(AgentStatus::Idle), Occupancy::Idle);
        assert_eq!(Occupancy::from(AgentStatus::Blocked), Occupancy::Blocked);
        assert_eq!(Occupancy::from(AgentStatus::Working), Occupancy::Working);
        assert_eq!(Occupancy::from(AgentStatus::Unknown), Occupancy::Unknown);
        assert!(
            Occupancy::from(AgentStatus::Done).attention_rank()
                > Occupancy::Working.attention_rank()
        );
        let mut model = AttentionModel {
            agents: vec![
                agent("done", AgentStatus::Done, 1),
                agent("block", AgentStatus::Blocked, 1),
                agent("work", AgentStatus::Working, 1),
                agent("idle", AgentStatus::Idle, 1),
            ],
            connected: true,
            ..AttentionModel::default()
        };
        model.sort_agents();
        let occ = OccupancySource::occupants(&model);
        assert_eq!(
            occ.iter()
                .map(|o| (o.id.as_str(), o.occupancy))
                .collect::<Vec<_>>(),
            [
                ("block", Occupancy::Blocked),
                ("work", Occupancy::Working),
                ("done", Occupancy::Idle),
                ("idle", Occupancy::Idle),
            ]
        );
        assert_eq!(
            model.focus("work"),
            Some(Focus {
                occupant_id: "work".into()
            })
        );
        assert_eq!(model.focus("missing"), None);
        assert_eq!(
            AttentionModel::pad_color(AgentStatus::Done, false, false),
            IndexedColor::YELLOW
        );
        assert_eq!(
            AttentionModel::pad_color(AgentStatus::Idle, false, false),
            IndexedColor::BLUE
        );
        assert_eq!(
            AttentionModel::workspace_color(AgentStatus::Done),
            IndexedColor::YELLOW
        );
        assert_eq!(
            AttentionModel::workspace_color(AgentStatus::Idle),
            IndexedColor::WHITE
        );
    }

    fn empty_snap() -> SessionSnapshot {
        serde_json::from_str(
            r#"{"version":"","protocol":0,"workspaces":[],"tabs":[],"panes":[],"agents":[]}"#,
        )
        .unwrap()
    }

    #[test]
    fn blocked_steals_selection() {
        let mut snap = empty_snap();
        snap.agents = vec![agent("done", AgentStatus::Done, 1)];
        let mut m = AttentionModel::from_snapshot(snap);
        assert_eq!(m.selected_pane_id().as_deref(), Some("done"));

        let mut snap = empty_snap();
        snap.agents = vec![
            agent("done", AgentStatus::Done, 1),
            agent("block", AgentStatus::Blocked, 2),
        ];
        m.apply_snapshot(snap);
        assert_eq!(m.selected_pane_id().as_deref(), Some("block"));
        assert_eq!(m.agents[0].pane_id, "block");
    }

    #[test]
    fn view_sig_ignores_unselected_title_chatter() {
        let mut snap = empty_snap();
        snap.agents = vec![
            agent("block", AgentStatus::Blocked, 2),
            agent("work", AgentStatus::Working, 1),
        ];
        let mut m = AttentionModel::from_snapshot(snap);
        let before = m.view_sig();
        m.agents[1].terminal_title_stripped = Some("spinner tick 2".into());
        m.agents[1].title = Some("spinner tick 2".into());
        assert_eq!(m.view_sig(), before);
        m.agents[0].terminal_title_stripped = Some("permission dialog".into());
        assert_ne!(m.view_sig(), before);
    }

    #[test]
    fn view_sig_paints_unselected_stable_titles() {
        let mut snap = empty_snap();
        snap.agents = vec![
            agent("block", AgentStatus::Blocked, 2),
            agent("done", AgentStatus::Done, 1),
        ];
        let mut m = AttentionModel::from_snapshot(snap);
        let before = m.view_sig();
        m.agents[1].title = Some("repo overview".into());
        assert_ne!(m.view_sig(), before);
    }

    #[test]
    fn upsert_keeps_selected_pane_across_reorder() {
        let mut snap = empty_snap();
        snap.agents = vec![
            agent("work", AgentStatus::Working, 9),
            agent("idle", AgentStatus::Idle, 1),
        ];
        let mut m = AttentionModel::from_snapshot(snap);
        assert_eq!(m.selected_pane_id().as_deref(), Some("work"));
        m.upsert_status("idle", "w1".into(), AgentStatus::Done, None, None, None);
        assert_eq!(m.selected_pane_id().as_deref(), Some("work"));
        assert_eq!(
            m.agents
                .iter()
                .map(|a| a.pane_id.as_str())
                .collect::<Vec<_>>(),
            ["work", "idle"]
        );
    }

    #[test]
    fn drop_pane_keeps_other_selected() {
        let mut snap = empty_snap();
        snap.agents = vec![
            agent("a", AgentStatus::Idle, 1),
            agent("b", AgentStatus::Idle, 2),
        ];
        let mut m = AttentionModel::from_snapshot(snap);
        m.selected = 1;
        m.drop_pane("a");
        assert_eq!(m.selected_pane_id().as_deref(), Some("b"));
        assert_eq!(m.agents.len(), 1);
    }
}
