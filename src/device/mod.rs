mod buttons;
mod color;
mod display;
mod hid;
mod leds;

pub use buttons::Button;
pub use color::{Brightness, IndexedColor};
pub use display::{Rect, Screen, ScreenId, DISPLAY_H, DISPLAY_W};
pub use hid::HidEvent;
pub use leds::LedState;

use hid::{open_hid, HidDevice};

use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use nusb::transfer::{Bulk, Out};
use nusb::{DeviceInfo, MaybeFuture};
use tracing::{info, warn};

const VID: u16 = 0x17cc;
const PID_MK3: u16 = 0x1600;
const PID_PLUS: u16 = 0x1820;
const HID_USAGE_PAGE: u16 = 0xff01;
const SCREEN_INTERFACE: u8 = 5;
const SCREEN_EP_OUT: u8 = 0x04;
/// Partial blits only between full frames this far apart: the diff cannot see
/// what another process painted over IF#5.
const KEYFRAME: Duration = Duration::from_secs(2);

pub struct Maschine {
    hid: HidDevice,
    screen: Option<nusb::Interface>,
}

impl Maschine {
    pub fn open() -> Result<Self> {
        let hid = open_hid()?;
        let screen = open_screen_interface()?;
        Ok(Self {
            hid,
            screen: Some(screen),
        })
    }

    pub fn poll(&mut self) -> Result<Vec<HidEvent>> {
        self.hid.poll_mut()
    }

    pub fn flush_leds(&self, leds: &LedState) -> Result<()> {
        self.hid.write(&leds.report_80())?;
        self.hid.write(&leds.report_81())?;
        Ok(())
    }

    pub fn clone_screen(&self) -> Result<nusb::Interface> {
        self.screen.clone().context("IF#5 not claimed")
    }

    pub fn recover_screens(&mut self) {
        if let Some(s) = &self.screen {
            if let Err(e) = clear_screen_halt(s) {
                warn!(%e, "IF#5 clear_halt");
            }
            if let Err(e) = s.set_alt_setting(0).wait() {
                warn!(%e, "IF#5 recover alt 0");
            }
        }
        // Drop the live claim before USBInterfaceOpen of a new handle.
        self.screen = None;
        match open_screen_interface() {
            Ok(iface) => {
                info!("IF#5 re-claimed");
                self.screen = Some(iface);
            }
            Err(e) => warn!(%e, "IF#5 re-claim failed"),
        }
    }

    pub fn blit(&self, screen: ScreenId, frame: &Screen) -> Result<()> {
        blit_iface(&self.clone_screen()?, screen, frame)
    }

    pub fn blank(&self) -> Result<()> {
        let leds = LedState::default();
        self.flush_leds(&leds)?;
        let Ok(iface) = self.clone_screen() else {
            return Ok(());
        };
        let black = Screen::fill(embedded_graphics::pixelcolor::Rgb565::new(0, 0, 0));
        let left = blit_iface(&iface, ScreenId::Left, &black);
        let right = blit_iface(&iface, ScreenId::Right, &black);
        left.or(right)
    }
}

impl Drop for Maschine {
    fn drop(&mut self) {
        let _ = self.flush_leds(&LedState::default());
    }
}

/// What each screen last acked, so a repaint only sends the changed rect.
#[derive(Default)]
pub struct ScreenCache {
    /// Frame on the panel and when it last got a full blit.
    shown: [Option<(Screen, Instant)>; 2],
}

impl ScreenCache {
    pub fn blit(&mut self, iface: &nusb::Interface, screen: ScreenId, frame: Screen) -> Result<()> {
        let slot = &mut self.shown[screen as usize];
        let (rect, full_at) = match slot.as_ref() {
            Some((prev, at)) if at.elapsed() < KEYFRAME => match frame.diff(prev) {
                Some(rect) => (rect, *at),
                None => return Ok(()),
            },
            _ => (Rect::FULL, Instant::now()),
        };
        // Panel contents are unknown if the write fails part-way.
        *slot = None;
        write_bulk(iface, screen, frame.encode_rect(screen, rect))?;
        *slot = Some((frame, full_at));
        Ok(())
    }

    pub fn invalidate(&mut self) {
        self.shown = Default::default();
    }
}

pub fn blit_iface(iface: &nusb::Interface, screen: ScreenId, frame: &Screen) -> Result<()> {
    write_bulk(iface, screen, frame.encode(screen))
}

fn write_bulk(iface: &nusb::Interface, screen: ScreenId, packet: Vec<u8>) -> Result<()> {
    let want = packet.len();
    // Fails busy while an earlier timed-out URB is still draining.
    let mut ep = iface
        .endpoint::<Bulk, Out>(SCREEN_EP_OUT)
        .with_context(|| format!("open bulk EP for {screen:?}"))?;
    ep.submit(packet.into());
    match ep.wait_next_complete(Duration::from_millis(1500)) {
        Some(completion) => {
            if completion.actual_len != want {
                warn!(?screen, n = completion.actual_len, want, "bulk short write");
            }
            completion
                .status
                .map_err(|e| anyhow!("bulk write {screen:?}: {e}"))
        }
        None => {
            // Drain cancelled URBs before clear_halt.
            warn!(?screen, "bulk write timed out; abort pipe and drain");
            ep.cancel_all();
            while ep.pending() > 0 {
                if ep.wait_next_complete(Duration::from_millis(400)).is_none() {
                    break;
                }
            }
            if ep.pending() == 0 {
                if let Err(e) = ep.clear_halt().wait() {
                    warn!(%e, "clear_halt after timeout");
                }
            }
            Err(anyhow!("bulk write {screen:?} timed out"))
        }
    }
}

fn clear_screen_halt(iface: &nusb::Interface) -> Result<(), nusb::Error> {
    iface
        .endpoint::<Bulk, Out>(SCREEN_EP_OUT)?
        .clear_halt()
        .wait()
}

fn open_screen_interface() -> Result<nusb::Interface> {
    let info = list_maschine().context("no Maschine MK3/Plus on USB")?;
    let device = info.open().wait().context("open USB device for screens")?;
    let iface = device
        .claim_interface(SCREEN_INTERFACE)
        .wait()
        .context("claim IF#5 (screens). Is NIHostIntegrationAgent still running?")?;
    if let Err(e) = iface.set_alt_setting(0).wait() {
        warn!(%e, "IF#5 alt 0");
    }
    if let Err(e) = clear_screen_halt(&iface) {
        warn!(%e, "IF#5 open clear_halt");
    }
    Ok(iface)
}

fn list_maschine() -> Option<DeviceInfo> {
    nusb::list_devices()
        .wait()
        .ok()?
        .find(|d| d.vendor_id() == VID && (d.product_id() == PID_MK3 || d.product_id() == PID_PLUS))
}
