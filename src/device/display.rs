use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::Rectangle;

pub const DISPLAY_W: u32 = 480;
pub const DISPLAY_H: u32 = 272;
const NUM_PX: usize = (DISPLAY_W * DISPLAY_H) as usize;

/// Blit packet matches Ctlra: 16 B header + 4 B cmd + RGB565 pixels + 8 B footer.
const HEADER: usize = 16;
const CMD: usize = 4;
const FOOTER: usize = 8;

/// Partial blits must start and end on these pixel boundaries.
const ALIGN_X: usize = 4;
const ALIGN_Y: usize = 2;

/// The panel takes RGB565 big-endian; `probe` paints labelled R/G/B bars to check.
fn wire(p: u16) -> [u8; 2] {
    p.to_be_bytes()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScreenId {
    Left = 0,
    Right = 1,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
}

impl Rect {
    pub const FULL: Rect = Rect {
        x: 0,
        y: 0,
        w: DISPLAY_W as u16,
        h: DISPLAY_H as u16,
    };
}

#[derive(Clone)]
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

    /// Aligned bounding box of the pixels that differ from `prev`.
    pub fn diff(&self, prev: &Screen) -> Option<Rect> {
        let w = DISPLAY_W as usize;
        let mut cols = (w, 0);
        let mut rows: Option<(usize, usize)> = None;
        let lines = self.pixels.chunks_exact(w).zip(prev.pixels.chunks_exact(w));
        for (y, (a, b)) in lines.enumerate() {
            let Some(first) = a.iter().zip(b).position(|(p, q)| p != q) else {
                continue;
            };
            let last = a.iter().zip(b).rposition(|(p, q)| p != q).unwrap_or(first);
            cols = (cols.0.min(first), cols.1.max(last));
            rows = Some((rows.map_or(y, |r| r.0), y));
        }
        let (y0, y1) = rows?;
        // 480 and 272 are multiples of the alignment, so rounding out stays in bounds.
        let x0 = cols.0 / ALIGN_X * ALIGN_X;
        let x1 = (cols.1 / ALIGN_X + 1) * ALIGN_X;
        let y0 = y0 / ALIGN_Y * ALIGN_Y;
        let y1 = (y1 / ALIGN_Y + 1) * ALIGN_Y;
        Some(Rect {
            x: x0 as u16,
            y: y0 as u16,
            w: (x1 - x0) as u16,
            h: (y1 - y0) as u16,
        })
    }

    /// Alpha-blends `color` over one pixel; off-screen coordinates are ignored.
    pub fn blend(&mut self, x: i32, y: i32, color: Rgb565, alpha: u8) {
        if x < 0 || y < 0 || x >= DISPLAY_W as i32 || y >= DISPLAY_H as i32 {
            return;
        }
        let px = &mut self.pixels[(y as u32 * DISPLAY_W + x as u32) as usize];
        let (dst, src) = (*px as u32, pack_rgb565(color) as u32);
        // 0..=256 so full coverage is exact.
        let a = alpha as u32 + (alpha as u32 >> 7);
        let mix = |shift: u32, mask: u32| {
            let (d, s) = ((dst >> shift) & mask, (src >> shift) & mask);
            ((d * (256 - a) + s * a) >> 8) << shift
        };
        *px = (mix(11, 0x1f) | mix(5, 0x3f) | mix(0, 0x1f)) as u16;
    }

    #[cfg(test)]
    pub(crate) fn rgb888(&self) -> Vec<u8> {
        self.pixels
            .iter()
            .flat_map(|p| {
                let (r, g, b) = (p >> 11, (p >> 5) & 0x3f, p & 0x1f);
                [
                    (r << 3 | r >> 2) as u8,
                    (g << 2 | g >> 4) as u8,
                    (b << 3 | b >> 2) as u8,
                ]
            })
            .collect()
    }

    pub fn encode(&self, id: ScreenId) -> Vec<u8> {
        self.encode_rect(id, Rect::FULL)
    }

    pub fn encode_rect(&self, id: ScreenId, r: Rect) -> Vec<u8> {
        let (x, y, w, h) = (r.x as usize, r.y as usize, r.w as usize, r.h as usize);
        let mut buf = Vec::with_capacity(HEADER + CMD + w * h * 2 + FOOTER);
        buf.extend_from_slice(&[0x84, 0x00, id as u8, 0x60, 0, 0, 0, 0]);
        for v in [r.x, r.y, r.w, r.h] {
            buf.extend_from_slice(&v.to_be_bytes());
        }
        // cmd 0x00, then a 24-bit count of pixel pairs (full frame: 0x00FF00).
        buf.extend_from_slice(&((w * h / 2) as u32).to_be_bytes());
        for row in self.pixels.chunks_exact(DISPLAY_W as usize).skip(y).take(h) {
            for p in &row[x..x + w] {
                buf.extend_from_slice(&wire(*p));
            }
        }
        buf.extend_from_slice(&[0x03, 0, 0, 0, 0x40, 0, 0, 0]);
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
        assert_eq!(p.len(), HEADER + CMD + NUM_PX * 2 + FOOTER);
        assert_eq!(&p[0..4], &[0x84, 0x00, 0x00, 0x60]);
        assert_eq!(&p[12..16], &[0x01, 0xe0, 0x01, 0x10]);
        assert_eq!(&p[16..20], &[0x00, 0x00, 0xff, 0x00]);
        assert_eq!(p[20], 0xf8); // red 0xf800 BE -> f8 00
        assert_eq!(p[21], 0x00);
        let f = 20 + NUM_PX * 2;
        assert_eq!(p[f], 0x03);
        assert_eq!(p[f + 4], 0x40);
        let r = s.encode(ScreenId::Right);
        assert_eq!(r[2], 1);
    }

    #[test]
    fn blend_endpoints_are_exact() {
        let mut s = Screen::fill(Rgb565::new(0x1f, 0, 0));
        s.blend(0, 0, Rgb565::new(0, 0x3f, 0), 0);
        s.blend(1, 0, Rgb565::new(0, 0x3f, 0), 255);
        s.blend(-1, 0, Rgb565::WHITE, 255);
        s.blend(0, DISPLAY_H as i32, Rgb565::WHITE, 255);
        assert_eq!(&s.pixels[..3], &[0xf800, 0x07e0, 0xf800]);
    }

    #[test]
    fn pair_count() {
        assert_eq!(NUM_PX / 2, 0xff00);
    }

    #[test]
    fn diff_is_aligned_bounding_box() {
        let prev = Screen::new();
        assert_eq!(prev.diff(&prev.clone()), None);
        let mut next = prev.clone();
        next.pixels[3 * DISPLAY_W as usize + 5] = 1;
        next.pixels[8 * DISPLAY_W as usize + 13] = 1;
        assert_eq!(
            next.diff(&prev),
            Some(Rect {
                x: 4,
                y: 2,
                w: 12,
                h: 8
            })
        );
        let mut corner = prev.clone();
        corner.pixels[NUM_PX - 1] = 1;
        assert_eq!(
            corner.diff(&prev),
            Some(Rect {
                x: 476,
                y: 270,
                w: 4,
                h: 2
            })
        );
        assert_eq!(Screen::fill(Rgb565::WHITE).diff(&prev), Some(Rect::FULL));
    }

    #[test]
    fn partial_packet_layout() {
        let mut s = Screen::new();
        s.pixels[2 * DISPLAY_W as usize + 4] = 0xf800;
        let r = Rect {
            x: 4,
            y: 2,
            w: 0x104,
            h: 2,
        };
        let p = s.encode_rect(ScreenId::Right, r);
        assert_eq!(p.len(), HEADER + CMD + 0x104 * 2 * 2 + FOOTER);
        assert_eq!(&p[0..8], &[0x84, 0x00, 0x01, 0x60, 0, 0, 0, 0]);
        assert_eq!(&p[8..16], &[0x00, 0x04, 0x00, 0x02, 0x01, 0x04, 0x00, 0x02]);
        assert_eq!(&p[16..20], &[0x00, 0x00, 0x01, 0x04]);
        assert_eq!(&p[20..22], &[0xf8, 0x00]);
        assert_eq!(&p[p.len() - 8..], &[0x03, 0, 0, 0, 0x40, 0, 0, 0]);
    }
}
