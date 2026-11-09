//! Icons, stored as BMP files in `assets/icons/`. Black pixels are
//! transparent.

use embedded_graphics::{pixelcolor::Rgb888, prelude::*, primitives::Rectangle};
use on_air_core::weather;
use tinybmp::Bmp;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    Sun,
    Cloud,
    Fog,
    Rain,
    Snow,
    Storm,
    Play,
    Pause,
    Next,
    Previous,
    Wifi,
    NoWifi,
    Music,
    Stale,
}

impl From<weather::Icon> for Icon {
    fn from(icon: weather::Icon) -> Self {
        match icon {
            weather::Icon::Sun => Icon::Sun,
            weather::Icon::Cloud => Icon::Cloud,
            weather::Icon::Fog => Icon::Fog,
            weather::Icon::Rain => Icon::Rain,
            weather::Icon::Snow => Icon::Snow,
            weather::Icon::Storm => Icon::Storm,
        }
    }
}

impl Icon {
    fn bytes(self) -> &'static [u8] {
        match self {
            Icon::Sun => include_bytes!("../assets/icons/sun.bmp"),
            Icon::Cloud => include_bytes!("../assets/icons/cloud.bmp"),
            Icon::Fog => include_bytes!("../assets/icons/fog.bmp"),
            Icon::Rain => include_bytes!("../assets/icons/rain.bmp"),
            Icon::Snow => include_bytes!("../assets/icons/snow.bmp"),
            Icon::Storm => include_bytes!("../assets/icons/storm.bmp"),
            Icon::Play => include_bytes!("../assets/icons/play.bmp"),
            Icon::Pause => include_bytes!("../assets/icons/pause.bmp"),
            Icon::Next => include_bytes!("../assets/icons/next.bmp"),
            Icon::Previous => include_bytes!("../assets/icons/previous.bmp"),
            Icon::Wifi => include_bytes!("../assets/icons/wifi.bmp"),
            Icon::NoWifi => include_bytes!("../assets/icons/no_wifi.bmp"),
            Icon::Music => include_bytes!("../assets/icons/music.bmp"),
            Icon::Stale => include_bytes!("../assets/icons/stale.bmp"),
        }
    }

    fn bmp(self) -> Bmp<'static, Rgb888> {
        // The files are checked in and covered by tests, so this can't fail.
        Bmp::from_slice(self.bytes()).expect("valid icon BMP")
    }

    pub fn size(self) -> Size {
        self.bmp().size()
    }
}

/// Draw an icon with its top-left corner at `at`, each pixel `scale`×`scale`.
pub fn draw<D>(icon: Icon, at: Point, scale: u32, target: &mut D) -> Result<(), D::Error>
where
    D: DrawTarget<Color = Rgb888>,
{
    draw_tinted(icon, at, scale, None, target)
}

/// Like [`draw`], but every visible pixel is drawn in `tint` when given.
pub fn draw_tinted<D>(
    icon: Icon,
    at: Point,
    scale: u32,
    tint: Option<Rgb888>,
    target: &mut D,
) -> Result<(), D::Error>
where
    D: DrawTarget<Color = Rgb888>,
{
    for Pixel(p, color) in icon.bmp().pixels() {
        if color == Rgb888::BLACK {
            continue;
        }
        let color = tint.unwrap_or(color);
        let origin = at + Point::new(p.x * scale as i32, p.y * scale as i32);
        target.fill_solid(&Rectangle::new(origin, Size::new(scale, scale)), color)?;
    }
    Ok(())
}

/// Draw an icon centered horizontally on `center_x`, top edge at `top`.
pub fn draw_centered<D>(
    icon: Icon,
    center_x: i32,
    top: i32,
    scale: u32,
    target: &mut D,
) -> Result<(), D::Error>
where
    D: DrawTarget<Color = Rgb888>,
{
    let width = (icon.size().width * scale) as i32;
    draw(icon, Point::new(center_x - width / 2, top), scale, target)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_file_parses() {
        for icon in [
            Icon::Sun,
            Icon::Cloud,
            Icon::Fog,
            Icon::Rain,
            Icon::Snow,
            Icon::Storm,
            Icon::Play,
            Icon::Pause,
            Icon::Next,
            Icon::Previous,
            Icon::Wifi,
            Icon::NoWifi,
            Icon::Music,
            Icon::Stale,
        ] {
            assert!(icon.size().width > 0, "{icon:?}");
        }
    }
}
