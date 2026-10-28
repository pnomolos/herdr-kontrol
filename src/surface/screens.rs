use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::{PrimitiveStyle, Rectangle};

use super::attention::{AttentionModel, PAGE};
use super::text::Face;
use crate::device::{Screen, DISPLAY_H, DISPLAY_W};
use crate::model::{AgentInfo, AgentStatus, BlockedKind};

/// High-contrast panel chrome: black field, inverted header, 4 encoder captions.
const BG: Rgb565 = Rgb565::new(0, 0, 0);
const FG: Rgb565 = Rgb565::new(0x1f, 0x3f, 0x1f);
const DIM: Rgb565 = Rgb565::new(0x12, 0x24, 0x12);
const HEADER_BG: Rgb565 = Rgb565::new(0x1d, 0x3a, 0x1d);
const HEADER_FG: Rgb565 = Rgb565::new(0, 0, 0);
const RED: Rgb565 = Rgb565::new(0x1f, 0x08, 0x08);
const GREEN: Rgb565 = Rgb565::new(0x04, 0x3a, 0x0c);
const AMBER: Rgb565 = Rgb565::new(0x1f, 0x30, 0x00);
const ORANGE: Rgb565 = Rgb565::new(0x1f, 0x1c, 0x00);
const PURPLE: Rgb565 = Rgb565::new(0x14, 0x10, 0x1f);
const TRACK: Rgb565 = Rgb565::new(0x04, 0x08, 0x08);
const GREY: Rgb565 = Rgb565::new(0x08, 0x10, 0x10);
const SELECT_BG: Rgb565 = Rgb565::new(0x06, 0x0c, 0x10);

const HEADER_H: u32 = 28;
const FOOTER_H: u32 = 36;
const KNOB_W: u32 = DISPLAY_W / 4;
const MARGIN: u32 = 12;

const TITLE: Face = Face::semibold(20.0);
const HEADER: Face = Face::semibold(16.0);
const NOTICE: Face = Face::medium(16.0);
const BODY: Face = Face::medium(14.0);
const SMALL: Face = Face::medium(12.0);
const CAPTION: Face = Face::medium(11.0);

fn backend_name(model: &AttentionModel) -> String {
    match model.caps.name {
        "" => "HERDR / REX".into(),
        n => n.to_uppercase(),
    }
}

fn left_header(model: &AttentionModel) -> String {
    let name = backend_name(model);
    if !model.connected {
        return format!("{name}   NOT RUNNING");
    }
    let n = model.agents.len();
    if model.page_count() == 1 {
        format!("{name}   {n}")
    } else {
        format!("{name}   P{}/{}   {n}", model.page + 1, model.page_count())
    }
}

/// Status strip under the header; with progress it becomes a bar on a dark track.
fn status_strip(s: &mut Screen, agent: &AgentInfo) {
    let color = status_color(agent);
    match agent.progress {
        Some(p) => {
            fill(s, 0, HEADER_H, DISPLAY_W, 6, TRACK);
            fill(
                s,
                0,
                HEADER_H,
                DISPLAY_W * p.min(100) as u32 / 100,
                6,
                color,
            );
        }
        None => fill(s, 0, HEADER_H, DISPLAY_W, 6, color),
    }
}

fn place(agent: &AgentInfo) -> String {
    agent
        .place
        .clone()
        .unwrap_or_else(|| format!("{}  {}", agent.workspace_id, agent.tab_id))
}

fn hero_status(agent: &AgentInfo) -> String {
    let mut st = agent.status_label().to_uppercase();
    if let Some(p) = agent.progress {
        st.push_str(&format!("  {p}%"));
    }
    st
}

/// Screen-preview lines: CAPTION rows from `top` down to the footer.
fn screen_preview(s: &mut Screen, top: u32, text: &str) {
    let line_h = CAPTION.line_height() + 1;
    let bottom = DISPLAY_H - FOOTER_H - 2;
    let rows = (bottom.saturating_sub(top) / line_h) as usize;
    fill(s, 0, top - 4, DISPLAY_W, 1, GREY);
    let lines: Vec<&str> = text.lines().filter(|l| !is_rule(l)).collect();
    let start = lines.len().saturating_sub(rows);
    let mut y = top;
    for line in &lines[start..] {
        // Inter lacks the TUI cursor arrows.
        let line = line.replace(['❯', '›', '⏵', '▶'], ">");
        put(s, MARGIN, y, &line, CAPTION, DIM);
        y += line_h;
    }
}

pub fn draw_left(model: &AttentionModel) -> Screen {
    let mut s = Screen::fill(BG);
    inverted_header(&mut s, &left_header(model));

    if !model.connected {
        put(&mut s, 16, 80, "waiting for herdr or rex", NOTICE, DIM);
        knob_footer(
            &mut s,
            &[("PAGE", None), ("SEL", None), ("WS", None), ("FOCUS", None)],
        );
        return s;
    }

    match model.selected_agent() {
        None => {
            put(&mut s, 16, 80, "NO OCCUPANTS", TITLE, DIM);
        }
        Some(agent) if model.selected_screen().is_some() => {
            // Compact hero over the live screen tail.
            status_strip(&mut s, agent);
            put(&mut s, MARGIN, 40, &hero_title(agent), TITLE, FG);
            let respond = model.caps.respond && agent.awaits_permission();
            let meta = if respond {
                format!(
                    "{}   PLAY yes · SHIFT+PLAY always · STOP no",
                    hero_status(agent)
                )
            } else {
                format!("{}   {}", hero_status(agent), place(agent))
            };
            put(
                &mut s,
                MARGIN,
                66,
                &meta,
                SMALL,
                if respond { FG } else { DIM },
            );
            screen_preview(&mut s, 90, model.selected_screen().unwrap_or(""));
        }
        Some(agent) => {
            status_strip(&mut s, agent);
            put(&mut s, MARGIN, 44, &hero_title(agent), TITLE, FG);
            if let Some(sub) = hero_sub(agent) {
                put(&mut s, MARGIN, 72, &sub, BODY, FG);
            }
            put(&mut s, MARGIN, 100, &hero_status(agent), BODY, DIM);
            put(&mut s, MARGIN, 122, &place(agent), SMALL, DIM);
            let mut y = 150;
            for (k, v) in agent.tokens.iter().take(4) {
                put(&mut s, MARGIN, y, &format!("{k}  {v}"), SMALL, FG);
                y += 16;
            }
        }
    }
    let ws = model
        .selected_agent()
        .and_then(|a| {
            model
                .workspaces
                .iter()
                .position(|w| w.workspace_id == a.workspace_id)
        })
        .map(|i| i as i32 + 1)
        .unwrap_or(0);
    knob_footer(
        &mut s,
        &[
            ("PAGE", Some(model.page as i32 + 1)),
            ("SEL", Some(model.selected as i32 + 1)),
            ("WS", Some(ws)),
            (
                "FOCUS",
                Some(i32::from(
                    model.selected_agent().map(|a| a.focused).unwrap_or(false),
                )),
            ),
        ],
    );
    s
}

fn right_header(model: &AttentionModel) -> String {
    let (blocked, working, done, _) = model.status_counts();
    if !model.connected {
        "OFFLINE".into()
    } else if blocked > 0 {
        format!("UNREAD   {blocked} BLOCKED")
    } else if done > 0 {
        format!("UNREAD   {done}")
    } else if working > 0 {
        format!("WORKING  {working}")
    } else {
        format!("QUEUE    {}", model.agents.len())
    }
}

pub fn draw_right(model: &AttentionModel) -> Screen {
    let mut s = Screen::fill(BG);
    inverted_header(&mut s, &right_header(model));

    if !model.connected {
        put(&mut s, 16, 80, "start herdr or Rex", NOTICE, DIM);
        knob_footer(
            &mut s,
            &[("--", None), ("--", None), ("--", None), ("--", None)],
        );
        return s;
    }

    let start = model.page.saturating_mul(PAGE);
    let on_page = model.agents.len().saturating_sub(start).min(PAGE);
    let layout = queue_layout(on_page);
    let cells = layout.cells();
    let inset = layout.row_h.saturating_sub(layout.face.line_height()) / 2;
    let mut y = HEADER_H + 4;
    for (i, agent) in model.agents.iter().enumerate().skip(start).take(PAGE) {
        if y + layout.row_h + FOOTER_H > DISPLAY_H {
            break;
        }
        let selected = i == model.selected;
        if selected {
            fill(&mut s, 0, y, DISPLAY_W, layout.row_h, SELECT_BG);
        }
        let color = if selected { FG } else { status_color(agent) };
        let row = queue_row(agent, selected, i - start);
        let top = y + inset;
        let number = row.number.to_string();
        let text = [
            row.mark,
            &number,
            row.status,
            &row.name,
            row.activity.as_deref().unwrap_or(""),
        ];
        for ((x, w), text) in cells.into_iter().zip(text) {
            put_within(&mut s, x, top, w, text, layout.face, color);
        }
        y += layout.row_h;
    }
    knob_footer(
        &mut s,
        &[("--", None), ("--", None), ("--", None), ("--", None)],
    );
    s
}

struct QueueLayout {
    face: Face,
    row_h: u32,
}

impl QueueLayout {
    /// (x, width) of the mark, number, status, name and activity cells. Sized
    /// in ems so the three row sizes share one grid.
    fn cells(&self) -> [(u32, u32); 5] {
        let em = |n: f32| (n * self.face.px).round() as u32;
        let gap = em(0.4);
        let mark = 8;
        let number = mark + em(0.8);
        let status = number + em(0.8) + gap;
        let name = status + em(2.7) + gap;
        let activity = name + em(5.1) + gap;
        [
            (mark, number - mark),
            (number, status - number - gap),
            (status, name - status - gap),
            (name, activity - name - gap),
            (activity, DISPLAY_W.saturating_sub(activity + 8)),
        ]
    }
}

/// 1–4: 20px / 44px rows; 5–6: 16 / 30; 7–8: 14 / 22. Fits 272 − header − footer.
fn queue_layout(on_page: usize) -> QueueLayout {
    if on_page <= 4 {
        QueueLayout {
            face: Face::medium(20.0),
            row_h: 44,
        }
    } else if on_page <= 6 {
        QueueLayout {
            face: Face::medium(16.0),
            row_h: 30,
        }
    } else {
        QueueLayout {
            face: Face::medium(14.0),
            row_h: 22,
        }
    }
}

fn queue_activity(agent: &AgentInfo) -> Option<String> {
    if agent.agent_status == AgentStatus::Working {
        return None;
    }
    let t = scrub_title(&agent.headline());
    if t.is_empty() || is_shell_osc(&t) {
        return None;
    }
    let name = agent.label();
    if t == name || t == agent.pane_id || t == agent.short_id() {
        return None;
    }
    Some(t)
}

#[derive(Debug)]
struct QueueRow {
    mark: &'static str,
    number: usize,
    status: &'static str,
    name: String,
    activity: Option<String>,
}

fn queue_row(agent: &AgentInfo, selected: bool, physical: usize) -> QueueRow {
    let mark = if selected {
        ">"
    } else if agent.focused {
        "*"
    } else {
        ""
    };
    let status = match agent.agent_status {
        AgentStatus::Blocked => match agent.blocked_kind {
            Some(BlockedKind::Question) => "ASK",
            Some(BlockedKind::Auth) => "PWD",
            _ => "BLK",
        },
        AgentStatus::Error => "ERR",
        AgentStatus::Working => "WRK",
        AgentStatus::Done => "DON",
        AgentStatus::Idle => "IDL",
        AgentStatus::Unknown => "",
    };
    let name = {
        let label = agent.label();
        if is_shell_osc(&label) {
            agent.short_id()
        } else {
            label
        }
    };
    QueueRow {
        mark,
        number: physical + 1,
        status,
        name,
        activity: queue_activity(agent),
    }
}

fn inverted_header(s: &mut Screen, text: &str) {
    fill(s, 0, 0, DISPLAY_W, HEADER_H, HEADER_BG);
    let top = HEADER_H.saturating_sub(HEADER.line_height()) / 2;
    put(s, 10, top, text, HEADER, HEADER_FG);
}

fn knob_footer(s: &mut Screen, knobs: &[(&str, Option<i32>); 4]) {
    let y = DISPLAY_H - FOOTER_H;
    fill(s, 0, y, DISPLAY_W, 1, GREY);
    for (i, (label, value)) in knobs.iter().enumerate() {
        let x = i as u32 * KNOB_W + 8;
        put_within(s, x, y + 5, KNOB_W - 16, label, CAPTION, DIM);
        if let Some(v) = value {
            put_within(s, x, y + 19, KNOB_W - 16, &v.to_string(), SMALL, FG);
        }
    }
}

/// TUI chrome: a line of only box-drawing characters and spaces.
fn is_rule(line: &str) -> bool {
    let t = line.trim();
    !t.is_empty()
        && t.chars()
            .all(|c| ('\u{2500}'..='\u{257f}').contains(&c) || c == ' ')
}

fn is_shell_osc(s: &str) -> bool {
    s.contains('@') && (s.contains(":~") || s.contains(":/"))
}

fn is_noise_fragment(s: &str) -> bool {
    let t = s
        .trim()
        .trim_matches(|c: char| matches!(c, '.' | '…' | '-' | '*'))
        .trim()
        .to_ascii_lowercase();
    t.is_empty() || is_spinner_token(&t) || is_agent_token(&t)
}

fn is_spinner_token(t: &str) -> bool {
    matches!(
        t,
        "waiting for response" | "thinking" | "running" | "working" | "idle" | "done" | "blocked"
    ) || t.starts_with("waiting for")
}

fn is_agent_token(t: &str) -> bool {
    let base = t.trim_end_matches(|c: char| c.is_ascii_digit());
    matches!(
        base,
        "grok"
            | "claude"
            | "codex"
            | "gemini"
            | "copilot"
            | "cursor"
            | "amp"
            | "opencode"
            | "pi"
            | "omp"
            | "devin"
            | "kimi"
            | "hermes"
            | "qoder"
            | "qwen"
            | "droid"
            | "kilo"
            | "mastra"
            | "antigravity"
            | "kiro"
            | "maki"
            | "cline"
            | "aider"
            | "goose"
    )
}

fn scrub_title(s: &str) -> String {
    let cleaned = s.replace('✳', "");
    // OSC is `status - activity - title - agent`; first non-noise fragment.
    let best = cleaned
        .split(" - ")
        .map(str::trim)
        .find(|p| !p.is_empty() && !is_shell_osc(p) && !is_noise_fragment(p))
        .unwrap_or("")
        .trim_start_matches(['-', '*', '•', ' '])
        .trim()
        .to_string();
    best
}

fn hero_title(agent: &AgentInfo) -> String {
    let h = scrub_title(&agent.headline());
    if !h.is_empty() && !is_shell_osc(&h) && h != agent.pane_id {
        return h;
    }
    let label = agent.label();
    if !is_shell_osc(&label) && label != agent.pane_id {
        return label;
    }
    agent.short_id()
}

fn hero_sub(agent: &AgentInfo) -> Option<String> {
    let title = hero_title(agent);
    let label = agent.label();
    // A user-named pane still says what runs in it.
    let id = match agent.agent.as_deref() {
        Some(app) if !app.is_empty() && !label.starts_with(app) => {
            format!("{app}  {}", agent.short_id())
        }
        _ => agent.short_id(),
    };
    if label != title && label != agent.pane_id && !is_shell_osc(&label) {
        return Some(format!("{label}  {id}"));
    }
    if id != title {
        return Some(id);
    }
    agent.foreground_cwd.clone().or_else(|| agent.cwd.clone())
}

fn status_color(agent: &AgentInfo) -> Rgb565 {
    match agent.agent_status {
        AgentStatus::Blocked => match agent.blocked_kind {
            Some(BlockedKind::Question) => PURPLE,
            Some(BlockedKind::Auth) => ORANGE,
            _ => RED,
        },
        AgentStatus::Error => RED,
        AgentStatus::Working => GREEN,
        AgentStatus::Done => AMBER,
        AgentStatus::Idle | AgentStatus::Unknown => DIM,
    }
}

fn fill(s: &mut Screen, x: u32, y: u32, w: u32, h: u32, c: Rgb565) {
    let _ = Rectangle::new(Point::new(x as i32, y as i32), Size::new(w, h))
        .into_styled(PrimitiveStyle::with_fill(c))
        .draw(s);
}

/// One line with its box top at `y`, ellipsized at the right margin.
fn put(s: &mut Screen, x: u32, y: u32, text: &str, face: Face, color: Rgb565) {
    put_within(
        s,
        x,
        y,
        DISPLAY_W.saturating_sub(x + MARGIN),
        text,
        face,
        color,
    );
}

fn put_within(s: &mut Screen, x: u32, y: u32, w: u32, text: &str, face: Face, color: Rgb565) {
    if text.is_empty() {
        return;
    }
    face.draw(s, x as i32, y as i32, &face.fit(text, w), color);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::AgentInfo;

    fn agent(title: &str, name: &str, pane: &str, status: AgentStatus) -> AgentInfo {
        AgentInfo {
            terminal_id: pane.into(),
            name: Some(name.into()),
            agent: Some(name.trim_end_matches(|c: char| c.is_ascii_digit()).into()),
            title: None,
            display_agent: None,
            terminal_title: Some(title.into()),
            terminal_title_stripped: Some(title.into()),
            agent_status: status,
            state_labels: Default::default(),
            tokens: Default::default(),
            workspace_id: "w3".into(),
            tab_id: "w3:t1".into(),
            pane_id: pane.into(),
            focused: false,
            state_change_seq: 1,
            cwd: None,
            foreground_cwd: None,
            interactive_ready: false,
            launch_pending: false,
            screen_detection_skipped: false,
            revision: 1,
            ..Default::default()
        }
    }

    #[test]
    fn shell_osc() {
        assert!(is_shell_osc("user@host:~/Projects/worky"));
        assert!(!is_shell_osc("fix auth middleware"));
    }

    #[test]
    fn hero_strips_hostname_agent_suffix_and_ellipsis() {
        let a = agent(
            "Worky architecture survey, leadership da… - grok",
            "grok1",
            "w3:p1",
            AgentStatus::Idle,
        );
        let title = hero_title(&a);
        assert!(!title.contains('@'), "{title}");
        assert!(!title.ends_with("grok"), "{title}");
        assert!(title.contains("Worky architecture survey"), "{title}");
    }

    #[test]
    fn hero_skips_waiting_spinner_prefix() {
        let a = agent(
            "- Waiting for response… - Worky architecture survey, leadership da… - grok",
            "grok1",
            "w3:p1",
            AgentStatus::Working,
        );
        let title = hero_title(&a);
        assert!(title.starts_with("Worky architecture survey"), "{title}");
        assert!(!title.to_ascii_lowercase().contains("waiting"), "{title}");
    }

    #[test]
    fn hero_keeps_short_snake_case_title() {
        let a = agent("fix_auth - grok", "grok1", "w3:p1", AgentStatus::Idle);
        assert_eq!(hero_title(&a), "fix_auth");
        let a = agent("survey - grok1", "grok1", "w3:p1", AgentStatus::Idle);
        assert_eq!(hero_title(&a), "survey");
    }

    #[test]
    fn hero_prefers_current_activity_over_stale_session_title() {
        let a = agent(
            "- Run related leadership flag tests… - Worky architecture survey, leadership da… - grok",
            "grok1",
            "w3:p1",
            AgentStatus::Working,
        );
        let title = hero_title(&a);
        assert!(
            title.starts_with("Run related leadership flag tests"),
            "{title}"
        );
        assert!(!title.contains("Worky architecture survey"), "{title}");
    }

    #[test]
    fn hero_skips_shell_osc() {
        let a = agent(
            "user@host:~/Projects/worky",
            "grok1",
            "w3:p1",
            AgentStatus::Idle,
        );
        assert_eq!(hero_title(&a), "grok1");
    }

    #[test]
    fn queue_row_is_name_first() {
        let a = agent("fix auth - grok", "grok1", "w3:p1", AgentStatus::Blocked);
        let row = queue_row(&a, true, 0);
        assert_eq!((row.mark, row.number, row.status), (">", 1, "BLK"));
        assert_eq!(row.name, "grok1");
        assert_eq!(row.activity.as_deref(), Some("fix auth"));
        let row = queue_row(&a, false, 7);
        assert_eq!((row.mark, row.number), ("", 8));
    }

    #[test]
    fn queue_row_skips_working_spinner_title() {
        let a = agent(
            "- Waiting for response… - Worky architecture survey - grok",
            "grok1",
            "w3:p1",
            AgentStatus::Working,
        );
        let row = queue_row(&a, false, 1);
        assert_eq!((row.number, row.status), (2, "WRK"));
        assert_eq!(row.name, "grok1");
        assert_eq!(row.activity, None);
    }

    #[test]
    fn queue_row_marks_herdr_focus() {
        let mut a = agent("survey - grok", "grok1", "w3:p1", AgentStatus::Idle);
        a.focused = true;
        assert_eq!(queue_row(&a, false, 2).mark, "*");
        assert_eq!(queue_row(&a, true, 2).mark, ">");
    }

    #[test]
    fn queue_cells_fit_every_row_size() {
        for on_page in [1, 5, 8] {
            let layout = queue_layout(on_page);
            let face = layout.face;
            let [mark, number, status, name, activity] = layout.cells();
            for m in [">", "*"] {
                assert_eq!(face.fit(m, mark.1), m);
            }
            for n in 1..=PAGE {
                assert_eq!(face.fit(&n.to_string(), number.1), n.to_string());
            }
            for st in ["BLK", "WRK", "DON", "IDL"] {
                assert_eq!(face.fit(st, status.1), st);
            }
            assert_eq!(face.fit("claude12", name.1), "claude12");
            assert!(mark.0 + mark.1 <= number.0 && number.0 + number.1 < status.0);
            assert!(status.0 + status.1 < name.0 && name.0 + name.1 < activity.0);
            // Activity keeps at least half the panel.
            assert!(activity.0 <= DISPLAY_W / 2 && activity.0 + activity.1 <= DISPLAY_W);
            assert!(face.line_height() <= layout.row_h);
        }
    }

    /// `cargo test preview -- --ignored` writes both panels to target/preview/*.ppm.
    #[test]
    #[ignore]
    fn preview() {
        let names = [
            "claude1", "grok1", "codex1", "gemini1", "amp1", "claude2", "pi1", "kiro1",
        ];
        let titles = [
            "✳ Fix pad focus for herdr 0.9 independent client views - claude",
            "- Waiting for response… - Worky architecture survey, leadership da… - grok",
            "Réécrire le décodeur – étape 2 - codex",
            "user@host:~/Projects/worky",
            "partial blit header layout - amp",
            "Keep the event socket until the replacement subscribe acks - claude",
            "survey - pi",
            "nusb 0.2 port - kiro",
        ];
        let statuses = [
            AgentStatus::Blocked,
            AgentStatus::Working,
            AgentStatus::Done,
            AgentStatus::Idle,
        ];
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/preview");
        std::fs::create_dir_all(&dir).unwrap();
        for n in [0, 3, 6, 8] {
            let mut m = AttentionModel {
                connected: true,
                agents: (0..n)
                    .map(|i| {
                        let mut a =
                            agent(titles[i], names[i], &format!("w1:p{i}"), statuses[i % 4]);
                        a.focused = i == 2;
                        a
                    })
                    .collect(),
                ..Default::default()
            };
            m.sort_agents();
            m.clamp();
            for (side, screen) in [("left", draw_left(&m)), ("right", draw_right(&m))] {
                write_ppm(&dir.join(format!("{side}-{n}.ppm")), &screen);
            }
        }

        // Rex: permission prompt with progress and a live screen tail.
        let mut a = agent(
            "",
            "claude-code",
            "block:01a17eec-b5f3-7115-b8bd-4f0d8c7f34d6",
            AgentStatus::Blocked,
        );
        a.blocked_kind = Some(BlockedKind::Permission);
        a.msg = Some("Allow Bash: cargo test --release?".into());
        a.progress = Some(60);
        a.place = Some("silent-thicket / Window 3b0e3fa4".into());
        let mut q = agent(
            "",
            "codex",
            "block:01a17eec-0000-7115-b8bd-4f0d8c7f0001",
            AgentStatus::Blocked,
        );
        q.blocked_kind = Some(BlockedKind::Question);
        q.msg = Some("Which branch should I target?".into());
        let shell = agent(
            "user@host:~",
            "zsh",
            "block:01a17eec-0000-7115-b8bd-4f0d8c7f0002",
            AgentStatus::Unknown,
        );
        let mut m = AttentionModel {
            connected: true,
            agents: vec![a.clone(), q, shell],
            caps: crate::backend::Caps {
                name: "rex",
                respond: true,
                screen: true,
            },
            ..Default::default()
        };
        m.screen = Some((
            a.pane_id.clone(),
            "  Running tests…\n────────────────\n Bash command\n   cargo test --release\n   Run the test suite\n Do you want to proceed?\n ❯ 1. Yes\n   2. Yes, and don't ask again for cargo test\n   3. No, and tell Claude what to do differently (esc)\n────────────────".into(),
        ));
        write_ppm(&dir.join("left-rex.ppm"), &draw_left(&m));
        write_ppm(&dir.join("right-rex.ppm"), &draw_right(&m));
        m.screen = None;
        write_ppm(&dir.join("left-rex-noscreen.ppm"), &draw_left(&m));
    }

    fn write_ppm(path: &std::path::Path, screen: &Screen) {
        let mut ppm = format!("P6 {DISPLAY_W} {DISPLAY_H} 255\n").into_bytes();
        ppm.extend(screen.rgb888());
        std::fs::write(path, ppm).unwrap();
    }

    #[test]
    fn rules_are_chrome() {
        assert!(is_rule("  ──────── "));
        assert!(is_rule("╭─────╮"));
        assert!(!is_rule("│ text │"));
        assert!(!is_rule(""));
    }

    #[test]
    fn queue_layout_grows_for_small_working_set() {
        assert_eq!(queue_layout(1).row_h, 44);
        assert_eq!(queue_layout(4).row_h, 44);
        assert_eq!(queue_layout(6).row_h, 30);
        assert_eq!(queue_layout(8).row_h, 22);
        assert!(queue_layout(1).face.px > queue_layout(8).face.px);
        let avail = DISPLAY_H - HEADER_H - 4 - FOOTER_H;
        assert!(4 * queue_layout(4).row_h <= avail);
        assert!(6 * queue_layout(6).row_h <= avail);
        assert!(8 * queue_layout(8).row_h <= avail);
    }

    #[test]
    fn left_header_hides_trivial_page() {
        let mut m = AttentionModel {
            connected: true,
            agents: (0..8)
                .map(|i| {
                    agent(
                        "t",
                        &format!("a{i}"),
                        &format!("w1:p{i}"),
                        AgentStatus::Idle,
                    )
                })
                .collect(),
            caps: crate::backend::Caps {
                name: "herdr",
                ..Default::default()
            },
            ..Default::default()
        };
        m.sort_agents();
        assert_eq!(left_header(&m), "HERDR   8");
        m.agents
            .push(agent("t", "overflow", "w1:p9", AgentStatus::Idle));
        m.sort_agents();
        m.clamp();
        assert_eq!(left_header(&m), "HERDR   P1/2   9");
    }

    #[test]
    fn unread_beats_working() {
        let mut m = AttentionModel {
            connected: true,
            agents: vec![
                agent("repo overview", "claude1", "w1:p1", AgentStatus::Done),
                agent("survey", "grok1", "w3:p1", AgentStatus::Working),
            ],
            ..Default::default()
        };
        m.sort_agents();
        assert_eq!(right_header(&m), "UNREAD   1");
        m.agents[0].agent_status = AgentStatus::Working;
        m.agents[1].agent_status = AgentStatus::Working;
        assert_eq!(right_header(&m), "WORKING  2");
    }
}
