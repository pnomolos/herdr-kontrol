use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::backend::{self, Command, Reply, Update};
use crate::device::{
    Brightness, Button, HidEvent, IndexedColor, LedState, Maschine, Screen, ScreenCache, ScreenId,
};
use crate::model::SessionSnapshot;
use crate::surface::{draw_left, draw_right, AttentionModel, PAGE};

pub struct Options {
    pub backend: backend::Config,
}

pub async fn run(opts: Options) -> Result<()> {
    let device = Arc::new(Mutex::new(Maschine::open()?));
    let caps = crate::surface::MaschineCaps::mk3().with_exclusive_usb(true);
    info!(
        exclusive_usb = caps.exclusive_usb,
        cells = caps.grid.cells(),
        encoders = caps.encoders,
        interval_us = caps.update_interval_us,
        "maschine hid+bulk open"
    );
    let mut shown = ScreenCache::default();
    if let Err(e) = paint(&device, &AttentionModel::default(), &mut shown, true) {
        warn!(%e, "initial paint");
    }

    let (hid_tx, mut hid_rx) = mpsc::unbounded_channel::<HidEvent>();
    {
        let device = Arc::clone(&device);
        std::thread::Builder::new()
            .name("maschine-hid".into())
            .spawn(move || hid_thread(device, hid_tx))?;
    }

    info!(
        backend = ?opts.backend.choice,
        herdr = %opts.backend.herdr_socket.display(),
        rex = %opts.backend.rex_socket.display(),
        "kontrol running"
    );
    let (up_tx, mut up_rx) = mpsc::unbounded_channel::<Update>();
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<Command>();
    let driver = tokio::spawn(backend::run(opts.backend, up_tx, cmd_rx));

    let mut model = AttentionModel::default();
    let mut input = Input::default();
    let mut watched: Option<String> = None;
    let mut leds_dirty = true;
    let mut screens_dirty = true;
    let mut screens_ok = true;
    let mut screens_retry = Instant::now();
    let mut last_screen_blit = Instant::now()
        .checked_sub(Duration::from_secs(1))
        .unwrap_or_else(Instant::now);
    let min_blit = Duration::from_micros(caps.update_interval_us as u64);
    let mut pulse = tokio::time::interval(Duration::from_millis(400));

    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;

    let run_result: Result<()> = async {
        loop {
            tokio::select! {
                biased;
                _ = tokio::signal::ctrl_c() => {
                    info!("ctrl-c");
                    break;
                }
                _ = sigterm.recv() => {
                    info!("sigterm");
                    break;
                }
                ev = hid_rx.recv() => {
                    let Some(ev) = ev else { break };
                    if let Some(cmd) = handle_hid(&mut model, &mut input, ev) {
                        if cmd_tx.send(cmd).is_err() {
                            break;
                        }
                    }
                    leds_dirty = true;
                    screens_dirty = true;
                }
                up = up_rx.recv() => {
                    let Some(up) = up else { break };
                    apply_update(&mut model, up, &mut leds_dirty, &mut screens_dirty);
                }
                _ = pulse.tick() => {
                    model.pulse = !model.pulse;
                    leds_dirty = true;
                }
            }

            let want = model
                .caps
                .screen
                .then(|| model.selected_pane_id())
                .flatten();
            if want != watched {
                watched = want.clone();
                let _ = cmd_tx.send(Command::WatchScreen(want));
            }

            if leds_dirty || screens_dirty {
                let now = Instant::now();
                let blit = screens_dirty
                    && (screens_ok || now >= screens_retry)
                    && last_screen_blit.elapsed() >= min_blit;
                match paint(&device, &model, &mut shown, blit) {
                    Ok(()) => {
                        leds_dirty = false;
                        if blit {
                            screens_ok = true;
                            screens_dirty = false;
                            last_screen_blit = now;
                        }
                    }
                    Err(e) => {
                        warn!(%e, "paint");
                        leds_dirty = false;
                        if blit {
                            screens_ok = false;
                            screens_retry = Instant::now() + Duration::from_secs(3);
                            if let Ok(mut dev) = device.lock() {
                                dev.recover_screens();
                            }
                            shown.invalidate();
                        }
                    }
                }
            }
        }
        Ok(())
    }
    .await;

    driver.abort();
    if let Ok(dev) = device.lock() {
        let _ = dev.blank();
    }
    run_result
}

fn apply_update(
    model: &mut AttentionModel,
    up: Update,
    leds_dirty: &mut bool,
    screens_dirty: &mut bool,
) {
    let sig = model.view_sig();
    match up {
        Update::Connected(caps) => {
            info!(backend = caps.name, "backend connected");
            model.caps = caps;
        }
        Update::Disconnected => {
            model.connected = false;
            model.screen = None;
        }
        Update::Snapshot(snap) => apply_snapshot(model, snap),
        Update::Status(p) => model.upsert_status(
            &p.pane_id,
            p.workspace_id,
            p.agent_status,
            p.title,
            p.display_agent,
            p.agent,
        ),
        Update::PaneClosed(id) => model.drop_pane(&id),
        Update::Screen { pane_id, text } => model.screen = Some((pane_id, text)),
    }
    *leds_dirty = true;
    if model.view_sig() != sig {
        *screens_dirty = true;
    }
}

fn apply_snapshot(model: &mut AttentionModel, snap: SessionSnapshot) {
    model.apply_snapshot(snap);
}

/// Held modifiers.
#[derive(Debug, Default)]
struct Input {
    shift: bool,
}

fn focus(model: &AttentionModel, pane_id: String) -> Command {
    Command::FocusPane {
        hint: model.window_hint_for_pane(&pane_id),
        pane_id,
    }
}

fn focus_pad(model: &mut AttentionModel, physical: u8) -> Option<Command> {
    let id = model.select_pad(physical)?.pane_id.clone();
    Some(focus(model, id))
}

/// Play/Stop answer the selected pane's permission prompt, if the backend can.
fn respond(model: &AttentionModel, reply: Reply) -> Option<Command> {
    if !model.caps.respond {
        return None;
    }
    let a = model.selected_agent().filter(|a| a.awaits_permission())?;
    Some(Command::Respond {
        pane_id: a.pane_id.clone(),
        stamp: a.status_stamp.clone(),
        reply,
    })
}

fn handle_hid(model: &mut AttentionModel, input: &mut Input, ev: HidEvent) -> Option<Command> {
    match ev {
        HidEvent::Pad {
            physical,
            pressed: true,
            ..
        } => focus_pad(model, physical),
        HidEvent::Encoder { delta } => {
            model.nudge_selection(delta);
            None
        }
        HidEvent::Button {
            button: Button::Shift,
            pressed,
        } => {
            input.shift = pressed;
            None
        }
        HidEvent::Button {
            button: Button::EncoderPress,
            pressed: true,
        } => {
            let id = model.selected_pane_id()?;
            Some(focus(model, id))
        }
        HidEvent::Button {
            button: Button::Play,
            pressed: true,
        } => respond(
            model,
            if input.shift {
                Reply::ApproveAlways
            } else {
                Reply::Approve
            },
        ),
        HidEvent::Button {
            button: Button::Stop,
            pressed: true,
        } => respond(model, Reply::Deny),
        HidEvent::Button {
            button: Button::ArrowLeft,
            pressed: true,
        } => {
            model.page_delta(-1);
            None
        }
        HidEvent::Button {
            button: Button::ArrowRight,
            pressed: true,
        } => {
            model.page_delta(1);
            None
        }
        HidEvent::Button {
            button: Button::Top(n),
            pressed: true,
        } => focus_pad(model, n),
        HidEvent::Button {
            button: Button::Group(g),
            pressed: true,
        } => {
            let workspace_id = model.group_workspace(g)?.workspace_id.clone();
            Some(Command::FocusWorkspace {
                pane_id: model.pane_id_in_workspace(&workspace_id),
                hint: model.window_hint_for_workspace(&workspace_id),
                workspace_id,
            })
        }
        _ => None,
    }
}

fn paint(
    device: &Mutex<Maschine>,
    model: &AttentionModel,
    shown: &mut ScreenCache,
    blit_screens: bool,
) -> Result<()> {
    let mut leds = LedState::default();
    leds.set_transport_connected(model.connected);
    leds.set_respond(
        model.connected
            && model.caps.respond
            && model
                .selected_agent()
                .is_some_and(|a| a.awaits_permission()),
    );
    leds.set_nav(model.page > 0, model.page + 1 < model.page_count());
    leds.set_encoder_compass(if model.connected {
        IndexedColor::WHITE
    } else {
        IndexedColor::DARK_GREY
    });
    let (blocked, working, done, _) = model.status_counts();
    leds.set_rec(model.connected && (blocked > 0 || done > 0));
    let page_start = model.page.saturating_mul(PAGE);
    let fill = model.strip_fill();
    leds.fill_strip(
        fill,
        if blocked > 0 || done > 0 {
            IndexedColor::PINK
        } else if working > 0 {
            IndexedColor::GREEN
        } else {
            IndexedColor::YELLOW
        },
    );
    for (i, slot) in model.pad_slots().iter().enumerate() {
        if let Some(agent) = slot {
            let selected = page_start + i == model.selected;
            leds.set_pad_physical(
                i as u8,
                AttentionModel::pad_color(agent, model.pulse, selected),
            );
            leds.set_top(
                i as u8,
                if selected {
                    Brightness::Bright
                } else {
                    Brightness::On
                },
            );
        }
    }
    for (i, ws) in model.workspaces.iter().take(8).enumerate() {
        leds.set_group(i as u8, AttentionModel::workspace_color(ws.agent_status));
    }

    let iface = {
        let dev = match device.lock() {
            Ok(d) => d,
            Err(_) => anyhow::bail!("maschine mutex poisoned"),
        };
        dev.flush_leds(&leds)?;
        if !blit_screens {
            return Ok(());
        }
        dev.clone_screen()?
    };
    shown.blit(&iface, ScreenId::Left, draw_left(model))?;
    shown.blit(&iface, ScreenId::Right, draw_right(model))?;
    Ok(())
}

fn hid_thread(device: Arc<Mutex<Maschine>>, tx: mpsc::UnboundedSender<HidEvent>) {
    loop {
        let events = {
            let mut dev = match device.lock() {
                Ok(d) => d,
                Err(_) => break,
            };
            match dev.poll() {
                Ok(ev) => ev,
                Err(e) => {
                    debug!(%e, "hid poll");
                    Vec::new()
                }
            }
        };
        for ev in events {
            if tx.send(ev).is_err() {
                return;
            }
        }
        std::thread::sleep(Duration::from_millis(4));
    }
}

/// Backend only, no device: print what the surface would show. Sends no commands
/// except screen watching (read-only), so it is safe against a live server.
pub async fn watch(opts: Options, screen: bool) -> Result<()> {
    let (up_tx, mut up_rx) = mpsc::unbounded_channel::<Update>();
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<Command>();
    let driver = tokio::spawn(backend::run(opts.backend, up_tx, cmd_rx));
    let mut model = AttentionModel::default();
    let mut last = String::new();
    let mut watched = None;
    loop {
        let up = tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            up = up_rx.recv() => match up { Some(u) => u, None => break },
        };
        let (mut l, mut s) = (false, false);
        apply_update(&mut model, up, &mut l, &mut s);
        let want = (screen && model.caps.screen)
            .then(|| model.selected_pane_id())
            .flatten();
        if want != watched {
            watched = want.clone();
            let _ = cmd_tx.send(Command::WatchScreen(want));
        }
        let text = render_text(&model);
        if text != last {
            println!("{text}");
            last = text;
        }
    }
    driver.abort();
    Ok(())
}

fn render_text(model: &AttentionModel) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    if !model.connected {
        return "-- disconnected".into();
    }
    let _ = writeln!(
        out,
        "-- {} · {} groups · {} pads",
        model.caps.name,
        model.workspaces.len(),
        model.agents.len()
    );
    for (i, w) in model.workspaces.iter().take(8).enumerate() {
        let _ = writeln!(
            out,
            "   group {} {:<20} {:?}",
            (b'A' + i as u8) as char,
            w.label,
            w.agent_status
        );
    }
    for (i, a) in model.agents.iter().enumerate() {
        let _ = writeln!(
            out,
            "{} {:>2} {:<8} {:<11} {:<14} {}{}",
            if i == model.selected { ">" } else { " " },
            i + 1,
            format!("{:?}", a.agent_status),
            a.blocked_kind.map(|k| k.key()).unwrap_or(""),
            a.label(),
            a.headline(),
            a.progress.map(|p| format!(" [{p}%]")).unwrap_or_default(),
        );
    }
    if let Some(t) = model.selected_screen() {
        let _ = writeln!(out, "   screen (last 5):");
        for l in t
            .lines()
            .rev()
            .take(5)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
        {
            let _ = writeln!(out, "   | {l}");
        }
    }
    out.trim_end().to_string()
}

pub async fn probe() -> Result<()> {
    use crate::device::IndexedColor;
    use crate::surface::Face;
    use embedded_graphics::pixelcolor::Rgb565;
    use embedded_graphics::prelude::*;
    use embedded_graphics::primitives::{PrimitiveStyle, Rectangle};

    let mut dev = Maschine::open()?;
    let mut leds = LedState::default();
    let rainbow = [
        IndexedColor::RED,
        IndexedColor::ORANGE,
        IndexedColor::YELLOW,
        IndexedColor::GREEN,
        IndexedColor::SKY,
        IndexedColor::BLUE,
        IndexedColor::PURPLE,
        IndexedColor::PINK,
        IndexedColor::WHITE,
        IndexedColor::AMBER,
        IndexedColor::MAGENTA,
        IndexedColor::LIME,
        IndexedColor::TURQUOISE,
        IndexedColor::GREY,
        IndexedColor::DIM_BLUE,
        IndexedColor::DIM_GREEN,
    ];
    for i in 0..16u8 {
        leds.set_pad_physical(i, rainbow[i as usize]);
        leds.set_group(i.min(7), rainbow[i as usize]);
    }
    leds.set_transport_connected(true);
    leds.set_encoder_compass(IndexedColor::WHITE);
    for i in 0..25 {
        leds.set_strip(i, rainbow[i % 16]);
    }
    dev.flush_leds(&leds)?;

    let mut left = Screen::fill(Rgb565::new(0x04, 0x08, 0x10));
    let mut right = Screen::fill(Rgb565::new(0x10, 0x04, 0x04));
    let face = Face::medium(16.0);
    face.draw(&mut left, 20, 24, "herdr-kontrol", Rgb565::WHITE);
    face.draw(&mut left, 20, 48, "MK3 HID + bulk OK", Rgb565::WHITE);
    // Pixel byte-order check: each bar must show the colour it is labelled with.
    let bars = [
        ("RED", Rgb565::new(0x1f, 0x00, 0x00)),
        ("GREEN", Rgb565::new(0x00, 0x3f, 0x00)),
        ("BLUE", Rgb565::new(0x00, 0x00, 0x1f)),
    ];
    for (i, (label, color)) in bars.into_iter().enumerate() {
        let x = 20 + i as i32 * 150;
        let _ = Rectangle::new(Point::new(x, 90), Size::new(140, 140))
            .into_styled(PrimitiveStyle::with_fill(color))
            .draw(&mut left);
        face.draw(&mut left, x + 8, 236, label, Rgb565::WHITE);
    }
    face.draw(&mut right, 20, 24, "right screen", Rgb565::WHITE);
    let _ = Rectangle::new(Point::new(20, 90), Size::new(440, 140))
        .into_styled(PrimitiveStyle::with_fill(Rgb565::new(0x00, 0x3f, 0x00)))
        .draw(&mut right);
    let iface = dev.clone_screen()?;
    let mut shown = ScreenCache::default();
    shown.blit(&iface, ScreenId::Left, left)?;
    shown.blit(&iface, ScreenId::Right, right.clone())?;
    // Partial-blit check: only this box goes over the wire, at an unaligned origin.
    let _ = Rectangle::new(Point::new(201, 121), Size::new(78, 78))
        .into_styled(PrimitiveStyle::with_fill(Rgb565::WHITE))
        .draw(&mut right);
    shown.blit(&iface, ScreenId::Right, right)?;
    println!("pads rainbow, screens painted. mash controls for 12s (ctrl-c to stop).");
    println!("left: bars must match their RED/GREEN/BLUE labels.");
    println!("right: a white square centred in an otherwise intact green box.");

    let start = std::time::Instant::now();
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            _ = tokio::time::sleep(Duration::from_millis(8)) => {
                for ev in dev.poll()? {
                    println!("{ev:?}");
                }
                if start.elapsed() >= Duration::from_secs(12) {
                    break;
                }
            }
        }
    }
    let _ = dev.blank();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AgentInfo, AgentStatus};

    fn agent(id: &str, status: AgentStatus) -> AgentInfo {
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
    fn hid_pad_and_top_emit_occupancy_focus() {
        let mut model = AttentionModel {
            agents: vec![
                agent("block", AgentStatus::Blocked),
                agent("work", AgentStatus::Working),
            ],
            connected: true,
            ..AttentionModel::default()
        };
        model.sort_agents();
        match handle_hid(
            &mut model,
            &mut Input::default(),
            HidEvent::Pad {
                physical: 1,
                pressure: 200,
                pressed: true,
            },
        ) {
            Some(Command::FocusPane { pane_id, .. }) => assert_eq!(pane_id, "work"),
            other => panic!("{other:?}"),
        }
        assert_eq!(model.selected_pane_id().as_deref(), Some("work"));
        match handle_hid(
            &mut model,
            &mut Input::default(),
            HidEvent::Button {
                button: Button::Top(0),
                pressed: true,
            },
        ) {
            Some(Command::FocusPane { pane_id, .. }) => assert_eq!(pane_id, "block"),
            other => panic!("{other:?}"),
        }
        assert!(handle_hid(
            &mut model,
            &mut Input::default(),
            HidEvent::Pad {
                physical: PAGE as u8,
                pressure: 200,
                pressed: true,
            },
        )
        .is_none());
        // Group still emits workspace+pane so 0.9 TUI can follow.
        model.workspaces = vec![crate::model::WorkspaceInfo {
            workspace_id: "w1".into(),
            number: 1,
            label: "worky".into(),
            focused: true,
            pane_count: 2,
            tab_count: 1,
            active_tab_id: "w1:t1".into(),
            agent_status: AgentStatus::Idle,
        }];
        match handle_hid(
            &mut model,
            &mut Input::default(),
            HidEvent::Button {
                button: Button::Group(0),
                pressed: true,
            },
        ) {
            Some(Command::FocusWorkspace {
                workspace_id,
                pane_id,
                ..
            }) => {
                assert_eq!(workspace_id, "w1");
                assert_eq!(pane_id.as_deref(), Some("block"));
            }
            other => panic!("{other:?}"),
        }
        match handle_hid(
            &mut model,
            &mut Input::default(),
            HidEvent::Button {
                button: Button::EncoderPress,
                pressed: true,
            },
        ) {
            Some(Command::FocusPane { pane_id, .. }) => assert_eq!(pane_id, "block"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn play_stop_answer_only_permission_prompts() {
        use crate::backend::Caps;
        use crate::model::BlockedKind;
        let mut prompt = agent("ask", AgentStatus::Blocked);
        prompt.blocked_kind = Some(BlockedKind::Permission);
        prompt.status_stamp = Some("t1".into());
        prompt.prompt_live = true;
        let mut question = agent("q", AgentStatus::Blocked);
        question.blocked_kind = Some(BlockedKind::Question);
        let mut model = AttentionModel {
            agents: vec![prompt, question],
            connected: true,
            ..AttentionModel::default()
        };
        let play = |shift| HidEvent::Button {
            button: Button::Play,
            pressed: shift,
        };
        // herdr: no respond capability.
        assert!(handle_hid(&mut model, &mut Input::default(), play(true)).is_none());

        model.caps = Caps {
            name: "rex",
            respond: true,
            screen: true,
        };
        let mut input = Input::default();
        match handle_hid(&mut model, &mut input, play(true)) {
            Some(Command::Respond {
                pane_id,
                stamp,
                reply,
            }) => {
                assert_eq!(pane_id, "ask");
                assert_eq!(stamp.as_deref(), Some("t1"));
                assert_eq!(reply, Reply::Approve);
            }
            other => panic!("{other:?}"),
        }
        handle_hid(
            &mut model,
            &mut input,
            HidEvent::Button {
                button: Button::Shift,
                pressed: true,
            },
        );
        assert!(matches!(
            handle_hid(&mut model, &mut input, play(true)),
            Some(Command::Respond {
                reply: Reply::ApproveAlways,
                ..
            })
        ));
        assert!(matches!(
            handle_hid(
                &mut model,
                &mut input,
                HidEvent::Button {
                    button: Button::Stop,
                    pressed: true
                }
            ),
            Some(Command::Respond {
                reply: Reply::Deny,
                ..
            })
        ));
        // A question prompt is not answered with keys.
        model.selected = 1;
        assert!(handle_hid(&mut model, &mut input, play(true)).is_none());
    }
}
