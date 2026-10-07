use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use hidapi::{HidApi, HidDevice as RawHid};
use tracing::trace;

use super::buttons::{Button, BUTTON_TABLE};
use super::leds::HID_TO_PHYSICAL;
use super::{HID_USAGE_PAGE, PID_MK3, PID_PLUS, VID};

const REPORT_BUTTONS: u8 = 0x01;
const REPORT_PADS: u8 = 0x02;

/// Upper nibble of a pad tuple's `d1`, per NI's own decoder (via Encdr's MK3 notes).
const PAD_SWITCH_ON: u8 = 0x00;
const PAD_HIT_ON: u8 = 0x10;
const PAD_SWITCH_OFF: u8 = 0x20;
const PAD_HIT_OFF: u8 = 0x30;
const PAD_PRESSURE: u8 = 0x40;
/// 12-bit pressure hysteresis for the tags that carry settling noise.
const PAD_PRESS_AT: u16 = 32;
const PAD_RELEASE_AT: u16 = 16;
/// Low readings right after a strike are rebound, not a release.
const PAD_REBOUND: Duration = Duration::from_millis(30);

pub struct HidDevice {
    dev: RawHid,
    buttons: u64,
    encoder: Option<u8>,
    pads: PadDecoder,
}

#[derive(Default)]
struct PadDecoder {
    held: u16,
    struck: [Option<Instant>; 16],
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
            // Double-pumped: a release can show up only in the second set.
            REPORT_PADS if buf.len() >= 64 => {
                let now = Instant::now();
                for set in buf.chunks_exact(64).take(2) {
                    self.pads.decode_set(set, now, out);
                }
            }
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
}

impl PadDecoder {
    /// One 64-byte set: marker byte, then (pad, d1, d2) tuples until an all-zero one.
    fn decode_set(&mut self, buf: &[u8], now: Instant, out: &mut Vec<HidEvent>) {
        for t in buf[1..].chunks_exact(3) {
            let (p, d1, d2) = (t[0], t[1], t[2]);
            // Pad 0 can report d1 == 0 at low pressure; only all three end the list.
            if p == 0 && d1 == 0 && d2 == 0 {
                break;
            }
            if p >= 16 {
                continue;
            }
            let pressure = ((d1 as u16 & 0xf) << 8) | d2 as u16;
            let bit = 1u16 << p;
            let held = self.held & bit != 0;
            let pressed = match d1 & 0xf0 {
                PAD_HIT_ON => true,
                PAD_SWITCH_OFF | PAD_HIT_OFF => false,
                PAD_SWITCH_ON | PAD_PRESSURE => {
                    if held {
                        let rebound = self.struck[p as usize]
                            .is_some_and(|at| now.duration_since(at) < PAD_REBOUND);
                        pressure > PAD_RELEASE_AT || rebound
                    } else {
                        pressure >= PAD_PRESS_AT
                    }
                }
                tag => {
                    trace!(tag, pad = p, "unknown pad tag");
                    if held {
                        pressure > 48
                    } else {
                        pressure >= 64
                    }
                }
            };
            // A strike always counts, so a missed release cannot swallow the next tap.
            if pressed == held && d1 & 0xf0 != PAD_HIT_ON {
                continue;
            }
            if pressed {
                self.held |= bit;
                self.struck[p as usize] = Some(now);
            } else {
                self.held &= !bit;
            }
            out.push(HidEvent::Pad {
                physical: HID_TO_PHYSICAL[p as usize],
                pressure,
                pressed,
            });
        }
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
                    pads: PadDecoder::default(),
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

    fn set(tuples: &[(u8, u8, u8)]) -> [u8; 64] {
        let mut buf = [0u8; 64];
        buf[0] = REPORT_PADS;
        for (i, (p, d1, d2)) in tuples.iter().enumerate() {
            buf[1 + i * 3..4 + i * 3].copy_from_slice(&[*p, *d1, *d2]);
        }
        buf
    }

    fn edges(out: &[HidEvent]) -> Vec<(u8, bool)> {
        out.iter()
            .map(|e| match e {
                HidEvent::Pad {
                    physical, pressed, ..
                } => (*physical, *pressed),
                other => panic!("{other:?}"),
            })
            .collect()
    }

    #[test]
    fn pad_hit_and_explicit_off_ignore_pressure() {
        let mut d = PadDecoder::default();
        let mut out = Vec::new();
        let t = Instant::now();
        // Hit ON at pressure 1, then Hit OFF with residual pressure.
        d.decode_set(&set(&[(5, 0x10, 0x01)]), t, &mut out);
        d.decode_set(&set(&[(5, 0x10, 0x01)]), t, &mut out);
        d.decode_set(&set(&[(5, 0x3f, 0xff)]), t, &mut out);
        let pad = HID_TO_PHYSICAL[5];
        assert_eq!(edges(&out), vec![(pad, true), (pad, true), (pad, false)]);
    }

    #[test]
    fn pad_release_only_in_second_set() {
        // Capture from Encdr's notes: set A still held at 373, set B Hit OFF.
        let mut d = PadDecoder::default();
        let mut out = Vec::new();
        let t = Instant::now();
        d.decode_set(&set(&[(3, 0x41, 0x75)]), t, &mut out);
        d.decode_set(&set(&[(3, 0x30, 0x00)]), t, &mut out);
        let pad = HID_TO_PHYSICAL[3];
        assert_eq!(edges(&out), vec![(pad, true), (pad, false)]);
        assert!(matches!(out[0], HidEvent::Pad { pressure: 373, .. }));
    }

    #[test]
    fn pad_pressure_hysteresis_and_rebound() {
        let mut d = PadDecoder::default();
        let mut out = Vec::new();
        let t = Instant::now();
        d.decode_set(&set(&[(2, 0x40, 31)]), t, &mut out);
        assert!(out.is_empty());
        d.decode_set(&set(&[(2, 0x40, 32)]), t, &mut out);
        // Deadband holds; a dip inside the rebound window is not a release.
        d.decode_set(&set(&[(2, 0x40, 4)]), t + PAD_REBOUND / 2, &mut out);
        d.decode_set(&set(&[(2, 0x40, 20)]), t + PAD_REBOUND, &mut out);
        assert_eq!(out.len(), 1);
        d.decode_set(&set(&[(2, 0x40, 16)]), t + PAD_REBOUND, &mut out);
        let pad = HID_TO_PHYSICAL[2];
        assert_eq!(edges(&out), vec![(pad, true), (pad, false)]);
    }

    #[test]
    fn pad_zero_low_pressure_is_not_end_of_list() {
        let mut d = PadDecoder::default();
        let mut out = Vec::new();
        // Pad 0 with d1 == 0 (Switch ON, pressure 40), then pad 1.
        d.decode_set(
            &set(&[(0, 0x00, 40), (1, 0x10, 0x80)]),
            Instant::now(),
            &mut out,
        );
        assert_eq!(
            edges(&out),
            vec![(HID_TO_PHYSICAL[0], true), (HID_TO_PHYSICAL[1], true)]
        );
    }

    #[test]
    fn encoder_wrap() {
        assert_eq!(wrap16(1, 0), 1);
        assert_eq!(wrap16(0, 1), -1);
        assert_eq!(wrap16(0, 15), 1);
        assert_eq!(wrap16(15, 0), -1);
    }
}
