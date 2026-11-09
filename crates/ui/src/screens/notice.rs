//! Short-lived icons over the current screen: button feedback and errors.

use embedded_graphics::{
    pixelcolor::Rgb888,
    prelude::*,
    primitives::{CornerRadii, PrimitiveStyleBuilder, Rectangle, RoundedRectangle},
};
use on_air_core::app::Notice;
use on_air_core::settings::IdleMode;

use crate::icons::{self, Icon};
use crate::text::{self, Align, DIM, GREY, SMALL, TINY, WHITE};

const PANEL: Rgb888 = Rgb888::new(0, 0, 0);
const BORDER: Rgb888 = Rgb888::new(70, 70, 80);

fn backdrop<D>(area: Rectangle, radius: u32, target: &mut D) -> Result<(), D::Error>
where
    D: DrawTarget<Color = Rgb888>,
{
    RoundedRectangle::new(area, CornerRadii::new(Size::new(radius, radius)))
        .into_styled(
            PrimitiveStyleBuilder::new()
                .fill_color(PANEL)
                .stroke_color(BORDER)
                .stroke_width(1)
                .build(),
        )
        .draw(target)
}

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
        backdrop(
            Rectangle::new(Point::new(20, 20), Size::new(24, 24)),
            6,
            target,
        )?;
        return icons::draw(icon, Point::new(23, 23), 2, target);
    }

    let (first, second) = match notice {
        Notice::PremiumNeeded => ("NEEDS", "PREMIUM"),
        Notice::NoDevice => ("NO ACTIVE", "DEVICE"),
        _ => ("SPOTIFY", "ERROR"),
    };
    backdrop(
        Rectangle::new(Point::new(6, 21), Size::new(52, 22)),
        3,
        target,
    )?;
    text::draw(
        &TINY,
        first,
        Point::new(32, 28),
        Align::Center,
        GREY,
        target,
    )?;
    text::draw(
        &TINY,
        second,
        Point::new(32, 36),
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
