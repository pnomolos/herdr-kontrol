use anyhow::{anyhow, Context, Result};
use hidapi::{HidApi, HidDevice as RawHid};
use tracing::trace;

use super::buttons::{Button, BUTTON_TABLE};
use super::leds::HID_TO_PHYSICAL;
use super::{HID_USAGE_PAGE, PID_MK3, PID_PLUS, VID};

const REPORT_BUTTONS: u8 = 0x01;
const REPORT_PADS: u8 = 0x02;

pub struct HidDevice {
    dev: RawHid,
    buttons: u64,
    encoder: Option<u8>,
    pads: u16,
}

#[derive(Clone, Debug)]
pub enum HidEvent {
    Button {
        button: Button,
        pressed: bool,
    },
    Pad {
        physical: u8,
        pressure: u16,
        pressed: bool,
    },
    Encoder {
        delta: i8,
    },
}

impl HidDevice {
    pub fn poll_mut(&mut self) -> Result<Vec<HidEvent>> {
        let mut out = Vec::new();
        let mut buf = [0u8; 128];
        loop {
            match self.dev.read_timeout(&mut buf, 5) {
                Ok(0) => break,
                Ok(n) => self.decode(&buf[..n], &mut out),
                Err(e) => {
                    let msg = e.to_string();
                    if msg.contains("timed out") || msg.contains("Timeout") {
                        break;
                    }
                    return Err(e.into());
                }
            }
        }
        Ok(out)
    }

    pub fn write(&self, data: &[u8]) -> Result<()> {
        let n = self.dev.write(data)?;
        if n != data.len() {
            anyhow::bail!("HID short write {n}/{}", data.len());
        }
        Ok(())
    }

    fn decode(&mut self, buf: &[u8], out: &mut Vec<HidEvent>) {
        if buf.is_empty() {
            return;
        }
        match buf[0] {
            REPORT_BUTTONS if buf.len() >= 42 => self.decode_buttons(buf, out),
            REPORT_PADS if buf.len() >= 128 => {
                self.decode_pads(&buf[..64], out);
                self.decode_pads(&buf[64..128], out);
            }
            REPORT_PADS if buf.len() >= 64 => self.decode_pads(buf, out),
            other => trace!(report = other, n = buf.len(), "unhandled HID"),
        }
    }

    fn decode_buttons(&mut self, buf: &[u8], out: &mut Vec<HidEvent>) {
        let mut bits: u64 = 0;
        for (i, (off, mask, _)) in BUTTON_TABLE.iter().enumerate() {
            if (*off as usize) < buf.len() && buf[*off as usize] & mask != 0 {
                bits |= 1u64 << i;
            }
        }
        let changed = bits ^ self.buttons;
        if changed != 0 {
            for (i, (_, _, button)) in BUTTON_TABLE.iter().enumerate() {
                if changed & (1u64 << i) != 0 {
                    out.push(HidEvent::Button {
                        button: *button,
                        pressed: bits & (1u64 << i) != 0,
                    });
                }
            }
            self.buttons = bits;
        }

        let enc = buf[11] & 0x0f;
        match self.encoder {
            None => self.encoder = Some(enc),
            Some(prev) if prev != enc => {
                let delta = wrap16(enc, prev);
                if delta != 0 {
                    out.push(HidEvent::Encoder { delta });
                }
                self.encoder = Some(enc);
            }
            Some(_) => {}
        }
    }

    fn decode_pads(&mut self, buf: &[u8], out: &mut Vec<HidEvent>) {
        // Ctlra: 16 slots of (pad_index, d1, d2) at buf[1]; pressure=((d1 & 0xf) << 8) | d2; ends at p==0 && d1==0.
        let mut hit = self.pads;
        let mut pressures = [0u16; 16];
        for i in 0..16 {
            let o = 1 + i * 3;
            if o + 2 >= buf.len() {
                break;
            }
            let p = buf[o];
            let d1 = buf[o + 1];
            let d2 = buf[o + 2];
            if p == 0 && d1 == 0 {
                break;
            }
            if p >= 16 {
                continue;
            }
            let pressure = ((d1 as u16 & 0xf) << 8) | d2 as u16;
            pressures[p as usize] = pressure;
            if pressure > 128 {
                hit |= 1 << p;
            } else {
                hit &= !(1 << p);
            }
        }
        let changed = hit ^ self.pads;
        for i in 0..16u8 {
            if changed & (1 << i) == 0 {
                continue;
            }
            let pressed = hit & (1 << i) != 0;
            let physical = HID_TO_PHYSICAL[i as usize];
            out.push(HidEvent::Pad {
                physical,
                pressure: pressures[i as usize],
                pressed,
            });
        }
        self.pads = hit;
    }
}

fn wrap16(now: u8, prev: u8) -> i8 {
    let d = (now as i8).wrapping_sub(prev as i8) & 0x0f;
    if d >= 8 {
        d - 16
    } else {
        d
    }
}

pub fn open_hid() -> Result<HidDevice> {
    let api = HidApi::new().context("hidapi init")?;
    let info = api
        .device_list()
        .find(|d| {
            d.vendor_id() == VID
                && (d.product_id() == PID_MK3 || d.product_id() == PID_PLUS)
                && d.usage_page() == HID_USAGE_PAGE
        })
        .ok_or_else(|| anyhow!("Maschine HID (usage page 0xFF01) not found"))?
        .clone();
    let path = info.path().to_owned();
    let mut last = None;
    for attempt in 0..8 {
        match api.open_path(&path) {
            Ok(raw) => {
                raw.set_blocking_mode(false).ok();
                return Ok(HidDevice {
                    dev: raw,
                    buttons: 0,
                    encoder: None,
                    pads: 0,
                });
            }
            Err(e) => {
                last = Some(e);
                std::thread::sleep(std::time::Duration::from_millis(150 + attempt * 50));
            }
        }
    }
    Err(last.unwrap()).with_context(|| format!("open HID {}", path.to_string_lossy()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoder_wrap() {
        assert_eq!(wrap16(1, 0), 1);
        assert_eq!(wrap16(0, 1), -1);
        assert_eq!(wrap16(0, 15), 1);
        assert_eq!(wrap16(15, 0), -1);
    }
}
