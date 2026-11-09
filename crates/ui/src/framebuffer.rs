//! A 64×64 RGB888 frame in memory, laid out like `on_air_core::image`
//! frames: row-major, 3 bytes per pixel.

use core::convert::Infallible;
use embedded_graphics::{pixelcolor::Rgb888, prelude::*};
use on_air_core::image::FRAME_LEN;

use crate::{HEIGHT, WIDTH};

#[derive(Clone)]
pub struct Framebuffer {
    bytes: [u8; FRAME_LEN],
}

impl Default for Framebuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl Framebuffer {
    pub const fn new() -> Self {
        Self {
            bytes: [0; FRAME_LEN],
        }
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn as_bytes_mut(&mut self) -> &mut [u8] {
        &mut self.bytes
    }

    pub fn pixel(&self, x: u32, y: u32) -> Rgb888 {
        let i = ((y * WIDTH + x) * 3) as usize;
        Rgb888::new(self.bytes[i], self.bytes[i + 1], self.bytes[i + 2])
    }
}

impl OriginDimensions for Framebuffer {
    fn size(&self) -> Size {
        Size::new(WIDTH, HEIGHT)
    }
}

impl DrawTarget for Framebuffer {
    type Color = Rgb888;
    type Error = Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        for Pixel(p, color) in pixels {
            if (0..WIDTH as i32).contains(&p.x) && (0..HEIGHT as i32).contains(&p.y) {
                let i = ((p.y as u32 * WIDTH + p.x as u32) * 3) as usize;
                self.bytes[i] = color.r();
                self.bytes[i + 1] = color.g();
                self.bytes[i + 2] = color.b();
            }
        }
        Ok(())
    }
}
