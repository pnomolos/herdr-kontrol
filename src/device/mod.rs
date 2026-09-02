mod buttons;
mod color;
mod display;
mod hid;
mod leds;

pub use buttons::Button;
pub use color::{Brightness, IndexedColor};
pub use display::{Screen, ScreenId, DISPLAY_H, DISPLAY_W};
pub use hid::HidEvent;
pub use leds::LedState;

use hid::{open_hid, HidDevice};

use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use nusb::DeviceInfo;
use tracing::{info, warn};

const VID: u16 = 0x17cc;
const PID_MK3: u16 = 0x1600;
const PID_PLUS: u16 = 0x1820;
const HID_USAGE_PAGE: u16 = 0xff01;
const SCREEN_INTERFACE: u8 = 5;
const SCREEN_EP_OUT: u8 = 0x04;

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
            if let Err(e) = s.clear_halt(SCREEN_EP_OUT) {
                warn!(%e, "IF#5 clear_halt");
            }
            if let Err(e) = s.set_alt_setting(0) {
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

fn wait_bulk(
    q: &mut nusb::transfer::Queue<Vec<u8>>,
    timeout: Duration,
) -> Option<nusb::transfer::Completion<nusb::transfer::ResponseBuffer>> {
    pollster::block_on(async {
        let write = async { Some(q.next_complete().await) };
        let to = async {
            async_io::Timer::after(timeout).await;
            None
        };
        futures_lite::future::or(write, to).await
    })
}

pub fn blit_iface(iface: &nusb::Interface, screen: ScreenId, frame: &Screen) -> Result<()> {
    // Drain cancelled URBs before clear_halt; do not drop Queue with pending.
    let packet = frame.encode(screen);
    let want = packet.len();
    let mut q = iface.bulk_out_queue(SCREEN_EP_OUT);
    q.submit(packet);
    match wait_bulk(&mut q, Duration::from_millis(1500)) {
        Some(completion) => {
            let n = completion.data.actual_length();
            if n != want {
                warn!(?screen, n, want, "bulk short write");
            }
            completion
                .into_result()
                .map(|_| ())
                .map_err(|e| anyhow!("bulk write {screen:?}: {e}"))
        }
        None => {
            warn!(?screen, "bulk write timed out; abort pipe and drain");
            q.cancel_all();
            while q.pending() > 0 {
                if wait_bulk(&mut q, Duration::from_millis(400)).is_none() {
                    break;
                }
            }
            if q.pending() == 0 {
                if let Err(e) = q.clear_halt() {
                    warn!(%e, "clear_halt after timeout");
                }
            }
            Err(anyhow!("bulk write {screen:?} timed out"))
        }
    }
}

fn open_screen_interface() -> Result<nusb::Interface> {
    let info = list_maschine().context("no Maschine MK3/Plus on USB")?;
    let device = info.open().context("open USB device for screens")?;
    let iface = device
        .claim_interface(SCREEN_INTERFACE)
        .context("claim IF#5 (screens). Is NIHostIntegrationAgent still running?")?;
    if let Err(e) = iface.set_alt_setting(0) {
        warn!(%e, "IF#5 alt 0");
    }
    if let Err(e) = iface.clear_halt(SCREEN_EP_OUT) {
        warn!(%e, "IF#5 open clear_halt");
    }
    Ok(iface)
}

fn list_maschine() -> Option<DeviceInfo> {
    nusb::list_devices()
        .ok()?
        .find(|d| d.vendor_id() == VID && (d.product_id() == PID_MK3 || d.product_id() == PID_PLUS))
}
