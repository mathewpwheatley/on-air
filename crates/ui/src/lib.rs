//! On Air screens.
//!
//! Every screen draws onto any `DrawTarget<Color = Rgb888>`, so the same code
//! runs in the desktop simulator and on the panel.

#![cfg_attr(not(test), no_std)]

use embedded_graphics::{pixelcolor::Rgb888, prelude::*, primitives::Rectangle};

/// Panel width in pixels.
pub const WIDTH: u32 = 64;
/// Panel height in pixels.
pub const HEIGHT: u32 = 64;

/// Bring-up test pattern: eight vertical color bars over a black-to-white
/// ramp along the bottom 8 rows. Used in the simulator and for panel bring-up.
pub fn draw_test_pattern<D>(target: &mut D) -> Result<(), D::Error>
where
    D: DrawTarget<Color = Rgb888>,
{
    const BARS: [Rgb888; 8] = [
        Rgb888::WHITE,
        Rgb888::YELLOW,
        Rgb888::CYAN,
        Rgb888::GREEN,
        Rgb888::MAGENTA,
        Rgb888::RED,
        Rgb888::BLUE,
        Rgb888::BLACK,
    ];
    let bar_width = WIDTH / BARS.len() as u32;
    let ramp_top = HEIGHT - 8;

    for (i, color) in BARS.iter().enumerate() {
        let x = (i as u32 * bar_width) as i32;
        target.fill_solid(
            &Rectangle::new(Point::new(x, 0), Size::new(bar_width, ramp_top)),
            *color,
        )?;
    }

    for x in 0..WIDTH {
        let level = (x * 255 / (WIDTH - 1)) as u8;
        target.fill_solid(
            &Rectangle::new(Point::new(x as i32, ramp_top as i32), Size::new(1, 8)),
            Rgb888::new(level, level, level),
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::convert::Infallible;

    /// Minimal in-memory 64×64 target for tests.
    struct Buffer([[Rgb888; WIDTH as usize]; HEIGHT as usize]);

    impl Buffer {
        fn new() -> Self {
            Self([[Rgb888::BLACK; WIDTH as usize]; HEIGHT as usize])
        }
        fn at(&self, x: usize, y: usize) -> Rgb888 {
            self.0[y][x]
        }
    }

    impl OriginDimensions for Buffer {
        fn size(&self) -> Size {
            Size::new(WIDTH, HEIGHT)
        }
    }

    impl DrawTarget for Buffer {
        type Color = Rgb888;
        type Error = Infallible;

        fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
        where
            I: IntoIterator<Item = Pixel<Self::Color>>,
        {
            for Pixel(p, c) in pixels {
                if (0..WIDTH as i32).contains(&p.x) && (0..HEIGHT as i32).contains(&p.y) {
                    self.0[p.y as usize][p.x as usize] = c;
                }
            }
            Ok(())
        }
    }

    #[test]
    fn test_pattern_bars_and_ramp() {
        let mut buf = Buffer::new();
        draw_test_pattern(&mut buf).unwrap();

        assert_eq!(buf.at(0, 0), Rgb888::WHITE);
        assert_eq!(buf.at(40, 10), Rgb888::RED);
        assert_eq!(buf.at(63, 0), Rgb888::BLACK);
        assert_eq!(buf.at(0, 63), Rgb888::BLACK);
        assert_eq!(buf.at(63, 63), Rgb888::WHITE);
    }
}
