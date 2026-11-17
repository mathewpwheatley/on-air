//! On-Air screens.
//!
//! Every screen draws onto any `DrawTarget<Color = Rgb888>`, so the same code
//! runs in the desktop simulator and on the panel. [`draw`] renders a whole
//! [`Scene`]; the firmware and simulator build the scene from the app state.

#![cfg_attr(not(test), no_std)]

mod dimmed;
mod framebuffer;
mod icons;
mod screens;
mod text;

use embedded_graphics::{pixelcolor::Rgb888, prelude::*, primitives::Rectangle};
use on_air_core::app::{Notice, Screen, View};
use on_air_core::timezone::DateTime;
use on_air_core::weather::{Reading, Units};

pub use framebuffer::Framebuffer;

/// Panel width in pixels.
pub const WIDTH: u32 = 64;
/// Panel height in pixels.
pub const HEIGHT: u32 = 64;

/// Everything needed to draw one frame.
#[derive(Debug, Clone, Copy)]
pub struct Scene<'a> {
    pub view: View,
    /// The current cover, 64×64 RGB888.
    pub art: Option<&'a [u8]>,
    /// Local time. `None` until the clock has synced, which shows `--:--`.
    pub time: Option<DateTime>,
    pub clock_24h: bool,
    pub weather: Option<WeatherInfo>,
    /// Whether the city has tide data, which adds the tide screen to the
    /// Mode cycle.
    pub tides_available: bool,
    /// The next high and low tide, in local time.
    pub tides: Option<TideTimes>,
    /// What's playing, for the track info screen.
    pub track: Option<Track<'a>>,
    /// Setup network name, shown on the setup screen.
    pub setup_network: &'a str,
    /// The board's Spotify sign-in page on the home network, shown as a QR
    /// code while sign-in is needed.
    pub sign_in_url: Option<&'a str>,
    /// Any monotonic clock, for scrolling text and blinking.
    pub now_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Track<'a> {
    pub title: &'a str,
    pub artist: &'a str,
    pub album: Option<&'a str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WeatherInfo {
    pub reading: Reading,
    /// Too old to trust: drawn dimmed with a marker.
    pub stale: bool,
    /// The units the reading is in, shown after the temperature.
    pub units: Units,
}

/// The next high and low tide, in local time. Either is `None` when the
/// forecast doesn't reach that far.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TideTimes {
    pub high: Option<DateTime>,
    pub low: Option<DateTime>,
}

/// Draw a whole scene, replacing everything on the target.
pub fn draw<D>(scene: &Scene, target: &mut D) -> Result<(), D::Error>
where
    D: DrawTarget<Color = Rgb888>,
{
    // Button and error notices fade the screen behind them.
    let fade = scene
        .view
        .notice
        .is_some_and(|n| !matches!(n, Notice::Mode(_) | Notice::TrackInfo));
    if fade {
        draw_screen(
            scene,
            &mut dimmed::Dimmed::new(target, NOTICE_BACKGROUND_LEVEL),
        )?;
    } else {
        draw_screen(scene, target)?;
    }
    if scene.view.no_wifi {
        // In the top-right corner, outlined in black so it reads on bright
        // covers, with room for the outline at the edges.
        let width = icons::Icon::NoWifi.size().width as i32;
        let at = Point::new(WIDTH as i32 - width - 1, 1);
        icons::draw_outlined(icons::Icon::NoWifi, at, 1, target)?;
    }
    if scene.view.update_needed {
        // In the top-left corner, opposite the WiFi badge.
        icons::draw_outlined(icons::Icon::UpdateNeeded, Point::new(1, 1), 1, target)?;
    }
    match scene.view.notice {
        Some(Notice::TrackInfo) => screens::track_info::draw(scene, target)?,
        Some(notice) => screens::notice::draw(notice, scene.tides_available, target)?,
        None => {}
    }
    Ok(())
}

/// Brightness of the screen behind a button or error notice, out of 255.
const NOTICE_BACKGROUND_LEVEL: u8 = 90;

fn draw_screen<D>(scene: &Scene, target: &mut D) -> Result<(), D::Error>
where
    D: DrawTarget<Color = Rgb888>,
{
    target.clear(Rgb888::BLACK)?;
    match scene.view.screen {
        Screen::Art { dimmed } => screens::art::draw(scene.art, dimmed, target),
        Screen::Idle(mode) => screens::idle::draw(mode, scene, target),
        Screen::Setup => screens::status::setup(scene.setup_network, target),
        Screen::Connecting => screens::status::connecting(scene.now_ms, target),
        Screen::SignInNeeded => screens::status::sign_in_needed(scene.sign_in_url, target),
    }
}

/// A test pattern for checking a panel's colors and gamma: eight vertical
/// color bars over a black-to-white ramp along the bottom 8 rows.
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

/// A test pattern for checking a panel's wiring: a white one-pixel border, a red block in the
/// top-left corner, green top-right, blue bottom-left and white
/// bottom-right, and a grey line across the middle. A mirrored, flipped or
/// half-missing image shows at a glance; without the E address line, the
/// bottom half repeats the top.
pub fn draw_addressing_pattern<D>(target: &mut D) -> Result<(), D::Error>
where
    D: DrawTarget<Color = Rgb888>,
{
    const BLOCK: u32 = 8;
    target.clear(Rgb888::BLACK)?;
    let edges = [
        Rectangle::new(Point::zero(), Size::new(WIDTH, 1)),
        Rectangle::new(Point::new(0, HEIGHT as i32 - 1), Size::new(WIDTH, 1)),
        Rectangle::new(Point::zero(), Size::new(1, HEIGHT)),
        Rectangle::new(Point::new(WIDTH as i32 - 1, 0), Size::new(1, HEIGHT)),
    ];
    for edge in edges {
        target.fill_solid(&edge, Rgb888::WHITE)?;
    }
    let far_x = (WIDTH - BLOCK) as i32;
    let far_y = (HEIGHT - BLOCK) as i32;
    let corners = [
        (Point::new(0, 0), Rgb888::RED),
        (Point::new(far_x, 0), Rgb888::GREEN),
        (Point::new(0, far_y), Rgb888::BLUE),
        (Point::new(far_x, far_y), Rgb888::WHITE),
    ];
    for (top_left, color) in corners {
        target.fill_solid(&Rectangle::new(top_left, Size::new(BLOCK, BLOCK)), color)?;
    }
    target.fill_solid(
        &Rectangle::new(Point::new(0, HEIGHT as i32 / 2), Size::new(WIDTH, 1)),
        Rgb888::new(128, 128, 128),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addressing_pattern_marks_each_corner() {
        let mut fb = Framebuffer::new();
        draw_addressing_pattern(&mut fb).unwrap();

        assert_eq!(fb.pixel(2, 2), Rgb888::RED);
        assert_eq!(fb.pixel(61, 2), Rgb888::GREEN);
        assert_eq!(fb.pixel(2, 61), Rgb888::BLUE);
        assert_eq!(fb.pixel(61, 61), Rgb888::WHITE);
        assert_eq!(fb.pixel(32, 0), Rgb888::WHITE);
        assert_eq!(fb.pixel(32, 32), Rgb888::new(128, 128, 128));
        assert_eq!(fb.pixel(32, 16), Rgb888::BLACK);
    }

    #[test]
    fn test_pattern_bars_and_ramp() {
        let mut fb = Framebuffer::new();
        draw_test_pattern(&mut fb).unwrap();

        assert_eq!(fb.pixel(0, 0), Rgb888::WHITE);
        assert_eq!(fb.pixel(40, 10), Rgb888::RED);
        assert_eq!(fb.pixel(63, 0), Rgb888::BLACK);
        assert_eq!(fb.pixel(0, 63), Rgb888::BLACK);
        assert_eq!(fb.pixel(63, 63), Rgb888::WHITE);
    }
}
