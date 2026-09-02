/// NI indexed pad/group colour, packed `(hue << 2) | (bright & 0x3)` from ktemkin's MK3 HID notes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IndexedColor(pub u8);

impl IndexedColor {
    pub const OFF: Self = Self(0);
    pub const WHITE: Self = Self(78);
    pub const GREY: Self = Self(77);
    pub const DARK_GREY: Self = Self(76);
    pub const RED: Self = Self(6);
    pub const DIM_RED: Self = Self(5);
    pub const ORANGE: Self = Self(10);
    pub const AMBER: Self = Self(14);
    pub const YELLOW: Self = Self(22);
    pub const GREEN: Self = Self(30);
    pub const DIM_GREEN: Self = Self(29);
    pub const SKY: Self = Self(38);
    pub const BLUE: Self = Self(42);
    pub const DIM_BLUE: Self = Self(45);
    pub const PURPLE: Self = Self(50);
    pub const MAGENTA: Self = Self(58);
    pub const PINK: Self = Self(62);
    pub const LIME: Self = Self(34);
    pub const TURQUOISE: Self = Self(31);
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Brightness {
    Off = 0,
    Dim = 4,
    On = 6,
    Bright = 7,
}

impl Brightness {
    pub fn byte(self) -> u8 {
        self as u8
    }
}
