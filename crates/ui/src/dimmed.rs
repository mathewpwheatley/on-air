//! A draw target wrapper that darkens everything drawn through it, so a
//! screen can be drawn faded behind a notice.

use embedded_graphics::{pixelcolor::Rgb888, prelude::*, primitives::Rectangle};

pub struct Dimmed<'a, D> {
    target: &'a mut D,
    /// Brightness out of 255.
    level: u16,
}

impl<'a, D> Dimmed<'a, D> {
    pub fn new(target: &'a mut D, level: u8) -> Self {
        Self {
            target,
            level: level as u16,
        }
    }
}

fn scale(color: Rgb888, level: u16) -> Rgb888 {
    let s = |v: u8| ((v as u16 * level + 127) / 255) as u8;
    Rgb888::new(s(color.r()), s(color.g()), s(color.b()))
}

impl<D: Dimensions> Dimensions for Dimmed<'_, D> {
    fn bounding_box(&self) -> Rectangle {
        self.target.bounding_box()
    }
}

impl<D> DrawTarget for Dimmed<'_, D>
where
    D: DrawTarget<Color = Rgb888>,
{
    type Color = Rgb888;
    type Error = D::Error;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        let level = self.level;
        self.target.draw_iter(
            pixels
                .into_iter()
                .map(|Pixel(p, c)| Pixel(p, scale(c, level))),
        )
    }

    fn fill_solid(&mut self, area: &Rectangle, color: Self::Color) -> Result<(), Self::Error> {
        self.target.fill_solid(area, scale(color, self.level))
    }
}
