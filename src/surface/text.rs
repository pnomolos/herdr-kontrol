use std::borrow::Cow;
use std::sync::OnceLock;

use embedded_graphics::pixelcolor::Rgb565;
use fontdue::{Font, FontSettings};

use crate::device::Screen;

static MEDIUM: OnceLock<Font> = OnceLock::new();
static SEMIBOLD: OnceLock<Font> = OnceLock::new();

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Weight {
    Medium,
    SemiBold,
}

/// Inter at a pixel size. Greyscale AA only: the panel's subpixel order is unknown.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Face {
    pub weight: Weight,
    pub px: f32,
}

impl Face {
    pub const fn medium(px: f32) -> Self {
        Self {
            weight: Weight::Medium,
            px,
        }
    }

    pub const fn semibold(px: f32) -> Self {
        Self {
            weight: Weight::SemiBold,
            px,
        }
    }

    fn font(self) -> &'static Font {
        let (cell, data): (_, &[u8]) = match self.weight {
            Weight::Medium => (
                &MEDIUM,
                include_bytes!("../../assets/fonts/Inter-Medium.ttf"),
            ),
            Weight::SemiBold => (
                &SEMIBOLD,
                include_bytes!("../../assets/fonts/Inter-SemiBold.ttf"),
            ),
        };
        cell.get_or_init(|| {
            let settings = FontSettings {
                scale: 20.0,
                ..FontSettings::default()
            };
            Font::from_bytes(data, settings).expect("bundled font parses")
        })
    }

    /// (ascent, descent) in px; descent is negative.
    fn vertical(self) -> (f32, f32) {
        let m = self
            .font()
            .horizontal_line_metrics(self.px)
            .expect("bundled font has horizontal metrics");
        (m.ascent, m.descent)
    }

    pub fn line_height(self) -> u32 {
        let (ascent, descent) = self.vertical();
        (ascent - descent).ceil() as u32
    }

    pub fn width(self, text: &str) -> f32 {
        let font = self.font();
        text.chars()
            .map(|c| font.metrics(c, self.px).advance_width)
            .sum()
    }

    /// `text`, or its longest prefix plus an ellipsis, within `max_w` px.
    pub fn fit(self, text: &str, max_w: u32) -> Cow<'_, str> {
        let max_w = max_w as f32;
        if self.width(text) <= max_w {
            return Cow::Borrowed(text);
        }
        let font = self.font();
        let budget = max_w - font.metrics('…', self.px).advance_width;
        let mut w = 0.0;
        let mut end = 0;
        for (i, c) in text.char_indices() {
            w += font.metrics(c, self.px).advance_width;
            if w > budget {
                break;
            }
            end = i + c.len_utf8();
        }
        let mut out = text[..end].trim_end().to_string();
        out.push('…');
        Cow::Owned(out)
    }

    /// Draws one line with its line box starting at `top`.
    pub fn draw(self, s: &mut Screen, x: i32, top: i32, text: &str, color: Rgb565) {
        let font = self.font();
        let baseline = top + self.vertical().0.round() as i32;
        let mut pen = x as f32;
        for c in text.chars() {
            let (m, coverage) = font.rasterize(c, self.px);
            let gx = pen.round() as i32 + m.xmin;
            let gy = baseline - m.ymin - m.height as i32;
            for (i, a) in coverage.iter().enumerate() {
                if *a != 0 {
                    s.blend(
                        gx + (i % m.width) as i32,
                        gy + (i / m.width) as i32,
                        color,
                        *a,
                    );
                }
            }
            pen += m.advance_width;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use embedded_graphics::prelude::RgbColor;

    #[test]
    fn fit_keeps_short_and_ellipsizes_long() {
        let face = Face::medium(14.0);
        assert_eq!(face.fit("fix auth", 200), "fix auth");
        let long = "Worky architecture survey, leadership data";
        let cut = face.fit(long, 120);
        assert!(cut.ends_with('…'), "{cut}");
        assert!(!cut.ends_with(" …"), "{cut}");
        assert!(face.width(&cut) <= 120.0, "{cut}");
        assert!(long.starts_with(cut.trim_end_matches('…')), "{cut}");
        assert_eq!(face.fit(long, 0), "…");
    }

    #[test]
    fn draw_covers_only_the_line_box() {
        let face = Face::semibold(20.0);
        let mut s = Screen::new();
        face.draw(&mut s, 10, 40, "Agjpy", Rgb565::WHITE);
        let rgb = s.rgb888();
        let lit = |y: u32| (0..480).any(|x| rgb[((y * 480 + x) * 3) as usize] != 0);
        let rows: Vec<u32> = (0..272).filter(|y| lit(*y)).collect();
        assert!(!rows.is_empty());
        assert!(*rows.first().unwrap() >= 40);
        assert!(*rows.last().unwrap() < 40 + face.line_height());
        // Clipped, not wrapped or panicking, off the right and bottom edges.
        face.draw(&mut s, 470, 265, "clip", Rgb565::WHITE);
    }
}
