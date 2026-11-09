//! Fonts and text drawing, using pixel fonts from `u8g2-fonts`.

use embedded_graphics::{pixelcolor::Rgb888, prelude::*, primitives::Rectangle};
use u8g2_fonts::types::{FontColor, HorizontalAlignment, VerticalPosition};
use u8g2_fonts::{Error, FontRenderer, fonts};

pub use u8g2_fonts::types::HorizontalAlignment as Align;

/// Large digits for the clock and temperature.
pub const LARGE: FontRenderer =
    FontRenderer::new::<fonts::u8g2_font_logisoso20_tn>().with_ignore_unknown_chars(true);
/// Between large and medium, for a 12-hour clock that won't fit in large.
pub const MEDIUM_LARGE: FontRenderer =
    FontRenderer::new::<fonts::u8g2_font_logisoso18_tn>().with_ignore_unknown_chars(true);
/// Medium digits for the clock + weather screen.
pub const MEDIUM: FontRenderer =
    FontRenderer::new::<fonts::u8g2_font_logisoso16_tn>().with_ignore_unknown_chars(true);
/// Labels and the date.
pub const SMALL: FontRenderer =
    FontRenderer::new::<fonts::u8g2_font_5x7_tr>().with_ignore_unknown_chars(true);
/// Notices.
pub const TINY: FontRenderer =
    FontRenderer::new::<fonts::u8g2_font_4x6_tr>().with_ignore_unknown_chars(true);

pub const WHITE: Rgb888 = Rgb888::new(255, 255, 255);
pub const GREY: Rgb888 = Rgb888::new(140, 140, 150);
pub const DIM: Rgb888 = Rgb888::new(80, 80, 90);

/// Draw text with its vertical center on `at.y`. Returns the area drawn,
/// if any. Characters a font lacks are skipped.
pub fn draw<D>(
    font: &FontRenderer,
    content: impl u8g2_fonts::Content,
    at: Point,
    align: HorizontalAlignment,
    color: Rgb888,
    target: &mut D,
) -> Result<Option<Rectangle>, D::Error>
where
    D: DrawTarget<Color = Rgb888>,
{
    match font.render_aligned(
        content,
        at,
        VerticalPosition::Center,
        align,
        FontColor::Transparent(color),
        target,
    ) {
        Ok(area) => Ok(area),
        Err(Error::DisplayError(e)) => Err(e),
        // Can't happen with a transparent background and unknown characters ignored.
        Err(_) => Ok(None),
    }
}

/// Width in pixels that `content` would take up.
pub fn width(font: &FontRenderer, content: impl u8g2_fonts::Content) -> u32 {
    font.get_rendered_dimensions(content, Point::zero(), VerticalPosition::Center)
        .map(|d| d.advance.x.max(0) as u32)
        .unwrap_or(0)
}

/// A short formatted string on the stack, for text that has to be measured
/// before it's drawn. Longer text is cut off at 16 bytes.
pub struct ShortText {
    bytes: [u8; 16],
    len: usize,
}

impl ShortText {
    pub fn format(args: core::fmt::Arguments) -> Self {
        let mut text = Self {
            bytes: [0; 16],
            len: 0,
        };
        let _ = core::fmt::Write::write_fmt(&mut text, args);
        text
    }

    pub fn as_str(&self) -> &str {
        core::str::from_utf8(&self.bytes[..self.len]).unwrap_or("")
    }
}

impl core::fmt::Write for ShortText {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        for c in s.chars() {
            let mut buffer = [0; 4];
            let encoded = c.encode_utf8(&mut buffer).as_bytes();
            if self.len + encoded.len() > self.bytes.len() {
                return Err(core::fmt::Error);
            }
            self.bytes[self.len..self.len + encoded.len()].copy_from_slice(encoded);
            self.len += encoded.len();
        }
        Ok(())
    }
}
