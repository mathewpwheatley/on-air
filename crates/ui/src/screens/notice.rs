//! Short-lived notices over the current screen: button feedback, errors and
//! the idle mode name. Button and error notices sit on the faded screen.

use embedded_graphics::{pixelcolor::Rgb888, prelude::*, primitives::Rectangle};
use on_air_core::app::Notice;
use on_air_core::settings::IdleMode;

use crate::icons::{self, Icon};
use crate::text::{self, Align, DIM, GREY, SMALL, WHITE};

pub fn draw<D>(notice: Notice, target: &mut D) -> Result<(), D::Error>
where
    D: DrawTarget<Color = Rgb888>,
{
    match notice {
        Notice::Mode(mode) => return mode_label(mode, target),
        // Drawn by `track_info`, which needs the scene.
        Notice::TrackInfo => return Ok(()),
        _ => {}
    }
    let icon = match notice {
        Notice::Play => Some(Icon::Play),
        Notice::Pause => Some(Icon::Pause),
        Notice::Next => Some(Icon::Next),
        Notice::Previous => Some(Icon::Previous),
        _ => None,
    };
    if let Some(icon) = icon {
        return icons::draw_outlined(icon, Point::new(23, 23), 2, target);
    }

    let (first, second) = match notice {
        Notice::PremiumNeeded => ("NEEDS", "PREMIUM"),
        Notice::NoDevice => ("NO ACTIVE", "DEVICE"),
        _ => ("SPOTIFY", "ERROR"),
    };
    text::draw(
        &SMALL,
        first,
        Point::new(32, 27),
        Align::Center,
        GREY,
        target,
    )?;
    text::draw(
        &SMALL,
        second,
        Point::new(32, 37),
        Align::Center,
        WHITE,
        target,
    )?;
    Ok(())
}

/// Every idle mode in Mode-button order, for the position dots.
const MODES: [IdleMode; 5] = [
    IdleMode::AnalogClock,
    IdleMode::DigitalClock,
    IdleMode::Weather,
    IdleMode::ClockWeather,
    IdleMode::Off,
];

/// Full screen: "IDLE MODE", the mode's name and a row of dots showing
/// where it sits in the cycle.
fn mode_label<D>(mode: IdleMode, target: &mut D) -> Result<(), D::Error>
where
    D: DrawTarget<Color = Rgb888>,
{
    target.clear(Rgb888::BLACK)?;

    text::draw(
        &SMALL,
        "IDLE MODE",
        Point::new(32, 6),
        Align::Center,
        DIM,
        target,
    )?;

    let (first, second) = match mode {
        IdleMode::DigitalClock => ("DIGITAL", Some("CLOCK")),
        IdleMode::AnalogClock => ("ANALOG", Some("CLOCK")),
        IdleMode::Weather => ("WEATHER", None),
        IdleMode::ClockWeather => ("CLOCK &", Some("WEATHER")),
        IdleMode::Off => ("NONE", None),
    };
    match second {
        Some(second) => {
            text::draw(
                &SMALL,
                first,
                Point::new(32, 26),
                Align::Center,
                WHITE,
                target,
            )?;
            text::draw(
                &SMALL,
                second,
                Point::new(32, 36),
                Align::Center,
                WHITE,
                target,
            )?;
        }
        None => {
            text::draw(
                &SMALL,
                first,
                Point::new(32, 31),
                Align::Center,
                WHITE,
                target,
            )?;
        }
    }

    // Five 3×3 dots, 4 px apart, centered.
    let left = (64 - (5 * 3 + 4 * 4)) / 2;
    for (i, m) in MODES.iter().enumerate() {
        let color = if *m == mode { WHITE } else { DIM };
        let at = Point::new(left + i as i32 * 7, 52);
        target.fill_solid(&Rectangle::new(at, Size::new(3, 3)), color)?;
    }
    Ok(())
}
