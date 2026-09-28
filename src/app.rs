use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::device::{
    blit_iface, Brightness, Button, HidEvent, IndexedColor, LedState, Maschine, Screen, ScreenId,
};
use crate::herdr::{default_socket, HerdrClient, HerdrEvent, SessionSnapshot};
use crate::surface::{draw_left, draw_right, AttentionModel, Focus, MaschineCaps, PAGE};

pub struct Options {
    pub socket: PathBuf,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            socket: default_socket(),
        }
    }
}

pub async fn run(opts: Options) -> Result<()> {
    let device = Arc::new(Mutex::new(Maschine::open()?));
    let caps = MaschineCaps::mk3().with_exclusive_usb(true);
    info!(
        exclusive_usb = caps.exclusive_usb,
        cells = caps.grid.cells(),
        encoders = caps.encoders,
        interval_us = caps.update_interval_us,
        "maschine hid+bulk open"
    );
    if let Err(e) = paint(&device, &AttentionModel::default(), true) {
        warn!(%e, "initial paint");
    }

    let (hid_tx, mut hid_rx) = mpsc::unbounded_channel::<HidEvent>();
    {
        let device = Arc::clone(&device);
        std::thread::Builder::new()
            .name("maschine-hid".into())
            .spawn(move || hid_thread(device, hid_tx))?;
    }

    let mut model = AttentionModel::default();
    let mut herdr: Option<HerdrClient> = None;
    let (ev_tx, mut ev_rx) = mpsc::unbounded_channel::<Result<HerdrEvent>>();
    let mut ev_task: Option<tokio::task::JoinHandle<()>> = None;
    let mut leds_dirty = true;
    let mut screens_dirty = true;
    let mut screens_ok = true;
    let mut screens_retry = Instant::now();
    let mut last_screen_blit = Instant::now()
        .checked_sub(Duration::from_secs(1))
        .unwrap_or_else(Instant::now);
    let min_blit = Duration::from_micros(caps.update_interval_us as u64);
    let mut pulse = tokio::time::interval(Duration::from_millis(400));
    let mut reconnect = tokio::time::interval(Duration::from_millis(750));
    reconnect.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // herdr does not always emit pane.agent_status_changed for done/idle; poll.
    let mut refresh = tokio::time::interval(Duration::from_secs(2));
    refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    info!(socket = %opts.socket.display(), "kontrol running");

    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut last_pane_ids: Vec<String> = Vec::new();

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
                if let Some(cmd) = handle_hid(&mut model, ev) {
                    let hint = match &cmd {
                        Cmd::Focus(f) => model.window_hint_for_pane(&f.occupant_id),
                        Cmd::FocusWorkspace { workspace_id, .. } => {
                            model.window_hint_for_workspace(workspace_id)
                        }
                    };
                    if let Some(h) = herdr.as_mut() {
                        if let Err(e) = dispatch(h, cmd).await {
                            warn!(%e, "herdr command failed");
                        }
                    }
                    std::mem::drop(tokio::task::spawn_blocking(move || {
                        crate::host::raise(hint.as_deref())
                    }));
                }
                leds_dirty = true;
                screens_dirty = true;
            }
            ev = ev_rx.recv() => {
                match ev {
                    Some(Ok(HerdrEvent::Refresh)) | Some(Ok(HerdrEvent::AgentDetected { .. })) => {
                        if let Some(h) = herdr.as_ref() {
                            match h.snapshot().await {
                                Ok(snap) => {
                                    let ids = snap_pane_ids(&snap);
                                    apply_visible(&mut model, snap, &mut leds_dirty, &mut screens_dirty);
                                    maybe_resubscribe(
                                        &opts.socket,
                                        ids,
                                        &mut last_pane_ids,
                                        &mut ev_task,
                                        ev_tx.clone(),
                                    )
                                    .await;
                                }
                                Err(e) => warn!(%e, "refresh snapshot"),
                            }
                        }
                    }
                    Some(Ok(ev)) => {
                        let sig = model.view_sig();
                        apply_herdr(&mut model, ev);
                        leds_dirty = true;
                        if model.view_sig() != sig {
                            screens_dirty = true;
                        }
                    }
                    Some(Err(e)) => {
                        warn!(%e, "herdr stream died");
                        if let Some(t) = ev_task.take() {
                            t.abort();
                        }
                        herdr = None;
                        model.connected = false;
                        leds_dirty = true;
                        screens_dirty = true;
                    }
                    None => break,
                }
            }
            _ = pulse.tick() => {
                model.pulse = !model.pulse;
                leds_dirty = true;
            }
            _ = refresh.tick(), if herdr.is_some() => {
                match herdr.as_ref().unwrap().snapshot().await {
                    Ok(snap) => {
                        let ids = snap_pane_ids(&snap);
                        apply_visible(&mut model, snap, &mut leds_dirty, &mut screens_dirty);
                        maybe_resubscribe(
                            &opts.socket,
                            ids,
                            &mut last_pane_ids,
                            &mut ev_task,
                            ev_tx.clone(),
                        )
                        .await;
                    }
                    Err(e) => {
                        warn!(%e, "periodic snapshot");
                        if let Some(t) = ev_task.take() {
                            t.abort();
                        }
                        herdr = None;
                        model.connected = false;
                        leds_dirty = true;
                        screens_dirty = true;
                    }
                }
            }
            _ = reconnect.tick(), if herdr.is_none() => {
                let rpc = HerdrClient::new(&opts.socket);
                match rpc.ping().await {
                    Ok(()) => {}
                    Err(_) => {
                        if model.connected {
                            model.connected = false;
                            leds_dirty = true;
                            screens_dirty = true;
                        }
                        continue;
                    }
                }
                // 0.9 does not replay retained events; subscribe before snapshot.
                if let Err(e) =
                    resubscribe(&opts.socket, &[], &mut ev_task, ev_tx.clone()).await
                {
                    warn!(%e, "subscribe");
                }
                last_pane_ids.clear();
                let pane_ids = match rpc.snapshot().await {
                    Ok(snap) => {
                        let ids = snap_pane_ids(&snap);
                        apply_visible(&mut model, snap, &mut leds_dirty, &mut screens_dirty);
                        ids
                    }
                    Err(e) => {
                        warn!(%e, "snapshot");
                        continue;
                    }
                };
                maybe_resubscribe(
                    &opts.socket,
                    pane_ids,
                    &mut last_pane_ids,
                    &mut ev_task,
                    ev_tx.clone(),
                )
                .await;
                info!("herdr connected");
                herdr = Some(rpc);
                leds_dirty = true;
                screens_dirty = true;
            }
        }

        if leds_dirty || screens_dirty {
            let now = Instant::now();
            let blit = screens_dirty
                && (screens_ok || now >= screens_retry)
                && last_screen_blit.elapsed() >= min_blit;
            match paint(&device, &model, blit) {
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
                    }
                }
            }
        }
    }
    Ok(())
    }.await;

    if let Ok(dev) = device.lock() {
        let _ = dev.blank();
    }
    run_result
}

async fn resubscribe(
    socket: &std::path::Path,
    pane_ids: &[String],
    ev_task: &mut Option<tokio::task::JoinHandle<()>>,
    ev_tx: mpsc::UnboundedSender<Result<HerdrEvent>>,
) -> Result<()> {
    let mut sub = HerdrClient::new(socket);
    sub.subscribe(pane_ids).await?;
    if let Some(t) = ev_task.take() {
        t.abort();
    }
    *ev_task = Some(tokio::spawn(async move {
        loop {
            match sub.next_event().await {
                Ok(ev) => {
                    if ev_tx.send(Ok(ev)).is_err() {
                        break;
                    }
                }
                Err(e) => {
                    let _ = ev_tx.send(Err(e));
                    break;
                }
            }
        }
    }));
    Ok(())
}

fn snap_pane_ids(snap: &SessionSnapshot) -> Vec<String> {
    snap.panes.iter().map(|p| p.pane_id.clone()).collect()
}

async fn maybe_resubscribe(
    socket: &std::path::Path,
    ids: Vec<String>,
    last_pane_ids: &mut Vec<String>,
    ev_task: &mut Option<tokio::task::JoinHandle<()>>,
    ev_tx: mpsc::UnboundedSender<Result<HerdrEvent>>,
) {
    if same_pane_ids(&ids, last_pane_ids) {
        return;
    }
    if let Err(e) = resubscribe(socket, &ids, ev_task, ev_tx).await {
        warn!(%e, "resubscribe");
        return;
    }
    *last_pane_ids = ids;
}

fn same_pane_ids(a: &[String], b: &[String]) -> bool {
    a.len() == b.len()
        && a.iter().collect::<std::collections::HashSet<_>>()
            == b.iter().collect::<std::collections::HashSet<_>>()
}

fn apply_visible(
    model: &mut AttentionModel,
    snap: SessionSnapshot,
    leds_dirty: &mut bool,
    screens_dirty: &mut bool,
) {
    let sig = model.view_sig();
    model.apply_snapshot(snap);
    *leds_dirty = true;
    if model.view_sig() != sig {
        *screens_dirty = true;
    }
}

#[derive(Debug)]
enum Cmd {
    Focus(Focus),
    FocusWorkspace {
        workspace_id: String,
        pane_id: Option<String>,
    },
}

fn focus_pad(model: &mut AttentionModel, physical: u8) -> Option<Cmd> {
    model.select_pad(physical).map(Focus::from).map(Cmd::Focus)
}

fn handle_hid(model: &mut AttentionModel, ev: HidEvent) -> Option<Cmd> {
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
            button: Button::EncoderPress,
            pressed: true,
        } => model.selected_agent().map(Focus::from).map(Cmd::Focus),
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
        } => model.group_workspace(g).map(|w| {
            let workspace_id = w.workspace_id.clone();
            Cmd::FocusWorkspace {
                pane_id: model.pane_id_in_workspace(&workspace_id),
                workspace_id,
            }
        }),
        _ => None,
    }
}

async fn dispatch(herdr: &mut HerdrClient, cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Focus(f) => {
            // 0.9.0: only pane.focus projects the attached TUI. agent.focus
            // updates the server record and is a no-op on the viewport.
            info!(pane = %f.occupant_id, "focus pane");
            herdr.focus_pane(&f.occupant_id).await
        }
        Cmd::FocusWorkspace {
            workspace_id,
            pane_id,
        } => {
            info!(ws = %workspace_id, "focus workspace");
            herdr.focus_workspace(&workspace_id).await?;
            if let Some(pane_id) = pane_id {
                herdr.focus_pane(&pane_id).await?;
            }
            Ok(())
        }
    }
}

fn apply_herdr(model: &mut AttentionModel, ev: HerdrEvent) {
    match ev {
        HerdrEvent::Snapshot(s) => model.apply_snapshot(s),
        HerdrEvent::AgentStatus {
            pane_id,
            workspace_id,
            agent_status,
            title,
            display_agent,
            agent,
        } => model.upsert_status(
            &pane_id,
            workspace_id,
            agent_status,
            title,
            display_agent,
            agent,
        ),
        HerdrEvent::PaneClosed { pane_id } => model.drop_pane(&pane_id),
        HerdrEvent::Refresh | HerdrEvent::AgentDetected { .. } => {}
        HerdrEvent::Other { kind } => debug!(kind, "unhandled herdr event"),
    }
}

fn paint(device: &Mutex<Maschine>, model: &AttentionModel, blit_screens: bool) -> Result<()> {
    let mut leds = LedState::default();
    leds.set_transport_connected(model.connected);
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
                AttentionModel::pad_color(agent.agent_status, model.pulse, selected),
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
    let left = draw_left(model);
    let right = draw_right(model);
    blit_iface(&iface, ScreenId::Left, &left)?;
    blit_iface(&iface, ScreenId::Right, &right)?;
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

pub async fn probe() -> Result<()> {
    use crate::device::IndexedColor;
    use embedded_graphics::mono_font::MonoTextStyle;
    use embedded_graphics::pixelcolor::Rgb565;
    use embedded_graphics::prelude::*;
    use embedded_graphics::primitives::{PrimitiveStyle, Rectangle};
    use embedded_graphics::text::Text;
    use profont::PROFONT_14_POINT;

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
    let style = MonoTextStyle::new(&PROFONT_14_POINT, Rgb565::WHITE);
    let _ = Text::new("herdr-kontrol", Point::new(20, 40), style).draw(&mut left);
    let _ = Text::new("MK3 HID + bulk OK", Point::new(20, 64), style).draw(&mut left);
    let _ = Rectangle::new(Point::new(20, 90), Size::new(440, 140))
        .into_styled(PrimitiveStyle::with_fill(Rgb565::new(0x1f, 0x00, 0x00)))
        .draw(&mut left);
    let _ = Text::new("right screen", Point::new(20, 40), style).draw(&mut right);
    let _ = Rectangle::new(Point::new(20, 90), Size::new(440, 140))
        .into_styled(PrimitiveStyle::with_fill(Rgb565::new(0x00, 0x3f, 0x00)))
        .draw(&mut right);
    dev.blit(ScreenId::Left, &left)?;
    dev.blit(ScreenId::Right, &right)?;
    println!("pads rainbow, screens painted. mash controls for 12s (ctrl-c to stop).");

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
    use crate::herdr::{AgentInfo, AgentStatus};

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
        }
    }

    #[test]
    fn pane_ids_ignore_order() {
        assert!(same_pane_ids(
            &["b".into(), "a".into()],
            &["a".into(), "b".into()]
        ));
        assert!(!same_pane_ids(&["a".into()], &["a".into(), "b".into()]));
        assert!(same_pane_ids(&[], &[]));
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
            HidEvent::Pad {
                physical: 1,
                pressure: 200,
                pressed: true,
            },
        ) {
            Some(Cmd::Focus(f)) => assert_eq!(f.occupant_id, "work"),
            other => panic!("{other:?}"),
        }
        assert_eq!(model.selected_pane_id().as_deref(), Some("work"));
        match handle_hid(
            &mut model,
            HidEvent::Button {
                button: Button::Top(0),
                pressed: true,
            },
        ) {
            Some(Cmd::Focus(f)) => assert_eq!(f.occupant_id, "block"),
            other => panic!("{other:?}"),
        }
        assert!(handle_hid(
            &mut model,
            HidEvent::Pad {
                physical: PAGE as u8,
                pressure: 200,
                pressed: true,
            },
        )
        .is_none());
        // Group still emits workspace+pane so 0.9 TUI can follow.
        model.workspaces = vec![crate::herdr::WorkspaceInfo {
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
            HidEvent::Button {
                button: Button::Group(0),
                pressed: true,
            },
        ) {
            Some(Cmd::FocusWorkspace {
                workspace_id,
                pane_id,
            }) => {
                assert_eq!(workspace_id, "w1");
                assert_eq!(pane_id.as_deref(), Some("block"));
            }
            other => panic!("{other:?}"),
        }
        match handle_hid(
            &mut model,
            HidEvent::Button {
                button: Button::EncoderPress,
                pressed: true,
            },
        ) {
            Some(Cmd::Focus(f)) => assert_eq!(f.occupant_id, "block"),
            other => panic!("{other:?}"),
        }
    }
}
