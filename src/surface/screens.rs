use embedded_graphics::mono_font::{MonoFont, MonoTextStyle};
use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::{PrimitiveStyle, Rectangle};
use embedded_graphics::text::Text;
use profont::{
    PROFONT_10_POINT, PROFONT_12_POINT, PROFONT_14_POINT, PROFONT_18_POINT, PROFONT_9_POINT,
};

use super::attention::{AttentionModel, PAGE};
use crate::device::{Screen, DISPLAY_H, DISPLAY_W};
use crate::herdr::{AgentInfo, AgentStatus};

/// High-contrast panel chrome: black field, inverted header, 4 encoder captions.
const BG: Rgb565 = Rgb565::new(0, 0, 0);
const FG: Rgb565 = Rgb565::new(0x1f, 0x3f, 0x1f);
const DIM: Rgb565 = Rgb565::new(0x12, 0x24, 0x12);
const HEADER_BG: Rgb565 = Rgb565::new(0x1d, 0x3a, 0x1d);
const HEADER_FG: Rgb565 = Rgb565::new(0, 0, 0);
const RED: Rgb565 = Rgb565::new(0x1f, 0x08, 0x08);
const GREEN: Rgb565 = Rgb565::new(0x04, 0x3a, 0x0c);
const AMBER: Rgb565 = Rgb565::new(0x1f, 0x30, 0x00);
const GREY: Rgb565 = Rgb565::new(0x08, 0x10, 0x10);
const SELECT_BG: Rgb565 = Rgb565::new(0x06, 0x0c, 0x10);

const HEADER_H: u32 = 28;
const FOOTER_H: u32 = 36;
const KNOB_W: u32 = DISPLAY_W / 4;

fn left_header(model: &AttentionModel) -> String {
    if !model.connected {
        return "HERDR   NOT RUNNING".into();
    }
    let n = model.agents.len();
    if model.page_count() == 1 {
        format!("HERDR   {n}")
    } else {
        format!("HERDR   P{}/{}   {n}", model.page + 1, model.page_count())
    }
}

pub fn draw_left(model: &AttentionModel) -> Screen {
    let mut s = Screen::fill(BG);
    inverted_header(&mut s, &left_header(model));

    if !model.connected {
        put(
            &mut s,
            16,
            80,
            "waiting for herdr.sock",
            &PROFONT_14_POINT,
            DIM,
        );
        knob_footer(
            &mut s,
            &[("PAGE", None), ("SEL", None), ("WS", None), ("FOCUS", None)],
        );
        return s;
    }

    match model.selected_agent() {
        None => {
            put(&mut s, 16, 80, "NO OCCUPANTS", &PROFONT_18_POINT, DIM);
        }
        Some(agent) => {
            fill(
                &mut s,
                0,
                HEADER_H,
                DISPLAY_W,
                6,
                status_color(agent.agent_status),
            );
            put(
                &mut s,
                12,
                44,
                &truncate(&hero_title(agent), 36),
                &PROFONT_18_POINT,
                FG,
            );
            if let Some(sub) = hero_sub(agent) {
                put(&mut s, 12, 72, &truncate(&sub, 42), &PROFONT_12_POINT, FG);
            }
            put(
                &mut s,
                12,
                100,
                &agent.status_label().to_uppercase(),
                &PROFONT_12_POINT,
                DIM,
            );
            put(
                &mut s,
                12,
                122,
                &format!("{}  {}", agent.workspace_id, agent.tab_id),
                &PROFONT_10_POINT,
                DIM,
            );
            let mut y = 150;
            for (k, v) in agent.tokens.iter().take(4) {
                put(
                    &mut s,
                    12,
                    y,
                    &truncate(&format!("{k}  {v}"), 48),
                    &PROFONT_10_POINT,
                    FG,
                );
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
        put(&mut s, 16, 80, "start herdr", &PROFONT_14_POINT, DIM);
        knob_footer(
            &mut s,
            &[("--", None), ("--", None), ("--", None), ("--", None)],
        );
        return s;
    }

    let start = model.page.saturating_mul(PAGE);
    let on_page = model.agents.len().saturating_sub(start).min(PAGE);
    let layout = queue_layout(on_page);
    let mut y = HEADER_H + 4;
    for (i, agent) in model.agents.iter().enumerate().skip(start).take(PAGE) {
        if y + layout.row_h + FOOTER_H > DISPLAY_H {
            break;
        }
        let selected = i == model.selected;
        if selected {
            fill(
                &mut s,
                0,
                y.saturating_sub(1),
                DISPLAY_W,
                layout.row_h,
                SELECT_BG,
            );
        }
        let color = if selected {
            FG
        } else {
            status_color(agent.agent_status)
        };
        put(
            &mut s,
            8,
            y,
            &truncate(&queue_row(agent, selected, i - start), layout.max_chars),
            layout.font,
            color,
        );
        y += layout.row_h;
    }
    knob_footer(
        &mut s,
        &[("--", None), ("--", None), ("--", None), ("--", None)],
    );
    s
}

struct QueueLayout {
    font: &'static MonoFont<'static>,
    row_h: u32,
    max_chars: usize,
}

/// 1–4: ProFont 18 / 44px; 5–6: 14 / 30px; 7–8: 12 / 22px. Fits 272 − header − footer.
fn queue_layout(on_page: usize) -> QueueLayout {
    if on_page <= 4 {
        QueueLayout {
            font: &PROFONT_18_POINT,
            row_h: 44,
            max_chars: 42,
        }
    } else if on_page <= 6 {
        QueueLayout {
            font: &PROFONT_14_POINT,
            row_h: 30,
            max_chars: 52,
        }
    } else {
        QueueLayout {
            font: &PROFONT_12_POINT,
            row_h: 22,
            max_chars: 64,
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
    if t == name || t == agent.pane_id {
        return None;
    }
    Some(t)
}

fn queue_row(agent: &AgentInfo, selected: bool, physical: usize) -> String {
    let mark = if selected {
        ">"
    } else if agent.focused {
        "*"
    } else {
        " "
    };
    let n = physical + 1;
    let st = match agent.agent_status {
        AgentStatus::Blocked => "BLK",
        AgentStatus::Working => "WRK",
        AgentStatus::Done => "DON",
        AgentStatus::Idle => "IDL",
        AgentStatus::Unknown => "   ",
    };
    let name = {
        let label = agent.label();
        if is_shell_osc(&label) {
            agent.pane_id.clone()
        } else {
            label
        }
    };
    let extra = queue_activity(agent)
        .map(|t| format!("  {}", truncate(&t, 24)))
        .unwrap_or_default();
    if st.trim().is_empty() {
        format!("{mark}{n}     {}{}", truncate(&name, 12), extra)
    } else {
        format!("{mark}{n} {:3}  {}{}", st, truncate(&name, 12), extra)
    }
}

fn inverted_header(s: &mut Screen, text: &str) {
    fill(s, 0, 0, DISPLAY_W, HEADER_H, HEADER_BG);
    put(s, 10, 6, text, &PROFONT_14_POINT, HEADER_FG);
}

fn knob_footer(s: &mut Screen, knobs: &[(&str, Option<i32>); 4]) {
    let y = DISPLAY_H - FOOTER_H;
    fill(s, 0, y, DISPLAY_W, 1, GREY);
    for (i, (label, value)) in knobs.iter().enumerate() {
        let x = i as u32 * KNOB_W;
        put(
            s,
            x + 8,
            y + 8,
            &format!("{:<8}", label),
            &PROFONT_9_POINT,
            DIM,
        );
        if let Some(v) = value {
            put(s, x + 8, y + 20, &format!("{v}"), &PROFONT_10_POINT, FG);
        }
    }
}

fn is_shell_osc(s: &str) -> bool {
    s.contains('@') && (s.contains(":~") || s.contains(":/"))
}

fn ascii_clean(s: &str) -> String {
    s.replace('…', "...")
        .replace(['–', '—'], "-")
        .replace('✳', "")
}

fn is_noise_fragment(s: &str) -> bool {
    let t = s
        .trim()
        .trim_matches(|c: char| matches!(c, '.' | '-' | '*'))
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
    let cleaned = ascii_clean(s);
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
    if !is_shell_osc(&label) {
        return label;
    }
    agent.pane_id.clone()
}

fn hero_sub(agent: &AgentInfo) -> Option<String> {
    let title = hero_title(agent);
    let label = agent.label();
    if label != title && !is_shell_osc(&label) {
        return Some(format!("{}  {}", label, agent.pane_id));
    }
    if agent.pane_id != title {
        return Some(agent.pane_id.clone());
    }
    agent.foreground_cwd.clone().or_else(|| agent.cwd.clone())
}

fn status_color(status: AgentStatus) -> Rgb565 {
    match status {
        AgentStatus::Blocked => RED,
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

fn put(s: &mut Screen, x: u32, y: u32, text: &str, font: &MonoFont<'_>, color: Rgb565) {
    let style = MonoTextStyle::new(font, color);
    let _ = Text::new(
        text,
        Point::new(x as i32, y as i32 + font.character_size.height as i32),
        style,
    )
    .draw(s);
}

fn truncate(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_string();
    }
    if max <= 3 {
        return s.chars().take(max).collect();
    }
    let mut out: String = s.chars().take(max - 3).collect();
    out.push_str("...");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::herdr::AgentInfo;

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
        assert!(!title.contains('…'), "{title}");
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
    fn truncate_is_ascii() {
        let t = truncate("Worky architecture survey, leadership data", 24);
        assert!(t.is_ascii(), "{t}");
        assert!(t.ends_with("..."), "{t}");
        assert_eq!(t.chars().count(), 24);
    }

    #[test]
    fn queue_row_is_name_first() {
        let a = agent("fix auth - grok", "grok1", "w3:p1", AgentStatus::Blocked);
        let row = queue_row(&a, true, 0);
        assert!(row.starts_with(">1 BLK  grok1"), "{row}");
        assert!(row.contains("fix auth"), "{row}");
        assert!(!row.contains("w3:p1"), "{row}");
        assert!(queue_row(&a, false, 7).starts_with(" 8 BLK  grok1"));
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
        assert!(row.contains("WRK  grok1"), "{row}");
        assert!(row.starts_with(" 2 WRK"), "{row}");
        assert!(!row.to_ascii_lowercase().contains("waiting"), "{row}");
        assert!(!row.contains("Worky"), "{row}");
    }

    #[test]
    fn queue_row_marks_herdr_focus() {
        let mut a = agent("survey - grok", "grok1", "w3:p1", AgentStatus::Idle);
        a.focused = true;
        assert!(
            queue_row(&a, false, 2).starts_with("*3 IDL  grok1"),
            "{}",
            queue_row(&a, false, 2)
        );
        assert!(queue_row(&a, true, 2).starts_with(">3 IDL  grok1"));
    }

    #[test]
    fn queue_layout_grows_for_small_working_set() {
        assert_eq!(queue_layout(1).row_h, 44);
        assert_eq!(queue_layout(4).row_h, 44);
        assert_eq!(queue_layout(6).row_h, 30);
        assert_eq!(queue_layout(8).row_h, 22);
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
