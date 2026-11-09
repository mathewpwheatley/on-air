//! Album art, full screen.

use embedded_graphics::{pixelcolor::Rgb888, prelude::*, primitives::Rectangle};

use crate::{HEIGHT, WIDTH};

/// Brightness of the art while paused, out of 255 (about 40%).
pub const PAUSED_LEVEL: u16 = 102;

pub fn draw<D>(art: Option<&[u8]>, dimmed: bool, target: &mut D) -> Result<(), D::Error>
where
    D: DrawTarget<Color = Rgb888>,
{
    let Some(art) = art else {
        return Ok(());
    };
    let level = if dimmed { PAUSED_LEVEL } else { 255 };
    let colors = art.chunks_exact(3).map(|p| {
        let s = |v: u8| ((v as u16 * level + 127) / 255) as u8;
        Rgb888::new(s(p[0]), s(p[1]), s(p[2]))
    });
    target.fill_contiguous(
        &Rectangle::new(Point::zero(), Size::new(WIDTH, HEIGHT)),
        colors,
    )
}
