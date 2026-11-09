//! Song title, artist and album over the dimmed cover.

use embedded_graphics::{pixelcolor::Rgb888, prelude::*};
use on_air_core::app::Screen;

use crate::Scene;
use crate::screens::art;
use crate::text::{self, Align, GREY, TRACK, WHITE};

/// Cover brightness behind the text, out of 255.
const BACKGROUND_LEVEL: u16 = 45;
const ARTIST: Rgb888 = Rgb888::new(200, 200, 210);
/// Long lines sit still this long before scrolling.
const SCROLL_DELAY_MS: u64 = 800;
const SCROLL_PIXELS_PER_SECOND: u64 = 30;
/// Room for text across the panel, keeping a 1 px margin each side.
const LINE_WIDTH: i32 = 62;

pub fn draw<D>(scene: &Scene, target: &mut D) -> Result<(), D::Error>
where
    D: DrawTarget<Color = Rgb888>,
{
    target.clear(Rgb888::BLACK)?;
    if let (Some(cover), Screen::Art { .. }) = (scene.art, scene.view.screen) {
        art::draw_at_level(cover, BACKGROUND_LEVEL, target)?;
    }

    let Some(track) = scene.track else {
        text::draw(
            &TRACK,
            "NOTHING",
            Point::new(32, 27),
            Align::Center,
            GREY,
            target,
        )?;
        text::draw(
            &TRACK,
            "PLAYING",
            Point::new(32, 37),
            Align::Center,
            GREY,
            target,
        )?;
        return Ok(());
    };

    let age = scene.view.notice_age_ms;
    match track.album {
        Some(album) => {
            line(track.title, 18, WHITE, age, target)?;
            line(track.artist, 32, ARTIST, age, target)?;
            line(album, 46, GREY, age, target)?;
        }
        None => {
            line(track.title, 24, WHITE, age, target)?;
            line(track.artist, 40, ARTIST, age, target)?;
        }
    }
    Ok(())
}

/// One line of text, centered if it fits. A longer line starts at the left
/// edge, waits, then scrolls left until its end is showing.
fn line<D>(
    content: &str,
    y: i32,
    color: Rgb888,
    age_ms: u64,
    target: &mut D,
) -> Result<(), D::Error>
where
    D: DrawTarget<Color = Rgb888>,
{
    let width = text::width(&TRACK, content) as i32;
    if width <= LINE_WIDTH {
        text::draw(
            &TRACK,
            content,
            Point::new(32, y),
            Align::Center,
            color,
            target,
        )?;
        return Ok(());
    }
    let scrolled =
        (age_ms.saturating_sub(SCROLL_DELAY_MS) * SCROLL_PIXELS_PER_SECOND / 1000) as i32;
    let offset = scrolled.min(width - LINE_WIDTH);
    text::draw(
        &TRACK,
        content,
        Point::new(1 - offset, y),
        Align::Left,
        color,
        target,
    )?;
    Ok(())
}
