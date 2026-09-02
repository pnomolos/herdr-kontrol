use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::Rectangle;

pub const DISPLAY_W: u32 = 480;
pub const DISPLAY_H: u32 = 272;
const NUM_PX: usize = (DISPLAY_W * DISPLAY_H) as usize;

/// Full-frame blit matches Ctlra: 16 B header + 4 B cmd + RGB565 LE pixels + 8 B footer.
const HEADER: usize = 16;
const CMD: usize = 4;
const FOOTER: usize = 8;
const PACKET: usize = HEADER + CMD + NUM_PX * 2 + FOOTER;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScreenId {
    Left = 0,
    Right = 1,
}

pub struct Screen {
    pixels: Vec<u16>,
}

impl Screen {
    pub fn new() -> Self {
        Self {
            pixels: vec![0; NUM_PX],
        }
    }

    pub fn fill(color: Rgb565) -> Self {
        Self {
            pixels: vec![pack_rgb565(color); NUM_PX],
        }
    }

    pub fn encode(&self, id: ScreenId) -> Vec<u8> {
        let mut buf = vec![0u8; PACKET];
        buf[0] = 0x84;
        buf[1] = 0x00;
        buf[2] = id as u8;
        buf[3] = 0x60;
        // x=0 y=0 already zero
        buf[12] = 0x01;
        buf[13] = 0xe0; // 480
        buf[14] = 0x01;
        buf[15] = 0x10; // 272
                        // cmd 0x00, count of pixel-pairs = 0x00FF00 = 65280 = NUM_PX/2
        buf[16] = 0x00;
        buf[17] = 0x00;
        buf[18] = 0xff;
        buf[19] = 0x00;
        let px = &mut buf[20..20 + NUM_PX * 2];
        for (i, p) in self.pixels.iter().enumerate() {
            let o = i * 2;
            px[o] = (*p & 0xff) as u8;
            px[o + 1] = (*p >> 8) as u8;
        }
        let f = 20 + NUM_PX * 2;
        buf[f] = 0x03;
        buf[f + 4] = 0x40;
        buf
    }
}

impl Default for Screen {
    fn default() -> Self {
        Self::new()
    }
}

fn pack_rgb565(c: Rgb565) -> u16 {
    let r = c.r() as u16;
    let g = c.g() as u16;
    let b = c.b() as u16;
    (r << 11) | (g << 5) | b
}

impl OriginDimensions for Screen {
    fn size(&self) -> Size {
        Size::new(DISPLAY_W, DISPLAY_H)
    }
}

impl DrawTarget for Screen {
    type Color = Rgb565;
    type Error = core::convert::Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        for Pixel(coord, color) in pixels {
            if coord.x < 0 || coord.y < 0 {
                continue;
            }
            let x = coord.x as u32;
            let y = coord.y as u32;
            if x < DISPLAY_W && y < DISPLAY_H {
                let i = (y * DISPLAY_W + x) as usize;
                self.pixels[i] = pack_rgb565(color);
            }
        }
        Ok(())
    }

    fn fill_solid(&mut self, area: &Rectangle, color: Self::Color) -> Result<(), Self::Error> {
        let v = pack_rgb565(color);
        let clipped = area.intersection(&self.bounding_box());
        if clipped.size.width == 0 || clipped.size.height == 0 {
            return Ok(());
        }
        let x0 = clipped.top_left.x as u32;
        let y0 = clipped.top_left.y as u32;
        let x1 = x0 + clipped.size.width;
        let y1 = y0 + clipped.size.height;
        for y in y0..y1 {
            let row = (y * DISPLAY_W) as usize;
            for x in x0..x1 {
                self.pixels[row + x as usize] = v;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packet_layout() {
        let s = Screen::fill(Rgb565::new(0x1f, 0, 0));
        let p = s.encode(ScreenId::Left);
        assert_eq!(p.len(), PACKET);
        assert_eq!(&p[0..4], &[0x84, 0x00, 0x00, 0x60]);
        assert_eq!(&p[12..16], &[0x01, 0xe0, 0x01, 0x10]);
        assert_eq!(&p[16..20], &[0x00, 0x00, 0xff, 0x00]);
        assert_eq!(p[20], 0x00); // red 0xf800 LE -> 00 f8
        assert_eq!(p[21], 0xf8);
        let f = 20 + NUM_PX * 2;
        assert_eq!(p[f], 0x03);
        assert_eq!(p[f + 4], 0x40);
        let r = s.encode(ScreenId::Right);
        assert_eq!(r[2], 1);
    }

    #[test]
    fn pair_count() {
        assert_eq!(NUM_PX / 2, 0xff00);
    }
}
