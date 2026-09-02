use super::color::{Brightness, IndexedColor};

/// HID pad index 0 is Maschine pad 13 (top-left). Physical 0 is pad 1 (bottom-left).
pub const HID_TO_PHYSICAL: [u8; 16] = [12, 13, 14, 15, 8, 9, 10, 11, 4, 5, 6, 7, 0, 1, 2, 3];
pub const PHYSICAL_TO_HID: [u8; 16] = [12, 13, 14, 15, 8, 9, 10, 11, 4, 5, 6, 7, 0, 1, 2, 3];

/// 0x80 is 1 + 62 bytes. 0x81 is 1 + 62 (strip + pads + pad).
const LIGHTS: usize = 62;

#[derive(Clone, Debug)]
pub struct LedState {
    lights: [u8; LIGHTS],
    pads: [u8; LIGHTS],
}

impl Default for LedState {
    fn default() -> Self {
        Self {
            lights: [0; LIGHTS],
            pads: [0; LIGHTS],
        }
    }
}

impl LedState {
    pub fn set_button(&mut self, id: u8, brightness: Brightness) {
        if (id as usize) < LIGHTS {
            match id {
                5 | 29..=36 | 58..=61 => {}
                _ => self.lights[id as usize] = brightness.byte(),
            }
        }
    }

    pub fn set_group(&mut self, group: u8, color: IndexedColor) {
        if group < 8 {
            self.lights[29 + group as usize] = color.0;
        }
    }

    pub fn set_encoder_compass(&mut self, color: IndexedColor) {
        let v = color.0;
        self.lights[58] = v;
        self.lights[59] = v;
        self.lights[60] = v;
        self.lights[61] = v;
    }

    pub fn set_pad_physical(&mut self, physical: u8, color: IndexedColor) {
        if physical < 16 {
            let hid = PHYSICAL_TO_HID[physical as usize] as usize;
            self.pads[25 + hid] = color.0;
        }
    }

    pub fn set_strip(&mut self, i: usize, color: IndexedColor) {
        if i < 25 {
            self.pads[i] = color.0;
        }
    }

    pub fn fill_strip(&mut self, filled: usize, color: IndexedColor) {
        let n = filled.min(25);
        for i in 0..25 {
            self.pads[i] = if i < n { color.0 } else { 0 };
        }
    }

    pub fn set_top(&mut self, i: u8, brightness: Brightness) {
        if i < 8 {
            self.set_button(12 + i, brightness);
        }
    }

    pub fn set_rec(&mut self, on: bool) {
        self.set_button(
            42,
            if on {
                Brightness::Bright
            } else {
                Brightness::Off
            },
        );
    }

    pub fn set_nav(&mut self, left: bool, right: bool) {
        self.set_button(
            6,
            if left {
                Brightness::On
            } else {
                Brightness::Off
            },
        );
        self.set_button(
            7,
            if right {
                Brightness::On
            } else {
                Brightness::Off
            },
        );
    }

    pub fn set_transport_connected(&mut self, connected: bool) {
        self.set_button(
            41,
            if connected {
                Brightness::On
            } else {
                Brightness::Off
            },
        );
        self.set_button(
            43,
            if connected {
                Brightness::Off
            } else {
                Brightness::Dim
            },
        );
    }

    pub fn report_80(&self) -> [u8; LIGHTS + 1] {
        let mut buf = [0u8; LIGHTS + 1];
        buf[0] = 0x80;
        buf[1..].copy_from_slice(&self.lights);
        buf
    }

    pub fn report_81(&self) -> [u8; LIGHTS + 1] {
        let mut buf = [0u8; LIGHTS + 1];
        buf[0] = 0x81;
        buf[1..].copy_from_slice(&self.pads);
        buf
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hid_physical_inverse() {
        for hid in 0..16u8 {
            let p = HID_TO_PHYSICAL[hid as usize];
            assert_eq!(PHYSICAL_TO_HID[p as usize], hid);
        }
    }

    #[test]
    fn report_ids() {
        let s = LedState::default();
        assert_eq!(s.report_80()[0], 0x80);
        assert_eq!(s.report_81()[0], 0x81);
        assert_eq!(s.report_80().len(), 63);
    }

    #[test]
    fn pad_slot() {
        let mut s = LedState::default();
        s.set_pad_physical(0, IndexedColor::RED);
        // physical 0 (pad 1) -> hid 12 -> byte 25+12 = 37 of payload, index 38 of report
        assert_eq!(s.report_81()[1 + 25 + 12], IndexedColor::RED.0);
        s.set_pad_physical(12, IndexedColor::GREEN);
        assert_eq!(s.report_81()[1 + 25], IndexedColor::GREEN.0);
    }
}
