//! Idle screens: analog clock, digital clock, weather, clock + weather,
//! weather + tides, last song, none.

use embedded_graphics::{
    pixelcolor::Rgb888,
    prelude::*,
    primitives::{Circle, Line, PrimitiveStyle},
};
use on_air_core::settings::IdleMode;
use on_air_core::timezone::DateTime;
use on_air_core::weather::Units;
use u8g2_fonts::FontRenderer;

use crate::icons::{self, Icon};
use crate::screens::art;
use crate::text::{
    self, Align, DIM, GREY, LARGE, MEDIUM, MEDIUM_LARGE, SMALL, ShortText, TINY, WHITE,
};
use crate::{Scene, TideTimes, WeatherInfo};

const WEEKDAYS: [&str; 7] = ["MON", "TUE", "WED", "THU", "FRI", "SAT", "SUN"];
const MONTHS: [&str; 12] = [
    "JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC",
];
const ACCENT: Rgb888 = Rgb888::new(255, 130, 0);

pub fn draw<D>(mode: IdleMode, scene: &Scene, target: &mut D) -> Result<(), D::Error>
where
    D: DrawTarget<Color = Rgb888>,
{
    match mode {
        IdleMode::DigitalClock => digital_clock(scene.time, scene.clock_24h, target),
        IdleMode::AnalogClock => analog_clock(scene.time, target),
        IdleMode::Weather => weather(scene.weather, target),
        IdleMode::ClockWeather => clock_weather(scene.time, scene.clock_24h, scene.weather, target),
        IdleMode::WeatherTides => {
            weather_tides(scene.weather, scene.tides, scene.clock_24h, target)
        }
        // Dimmed like a paused cover, since nothing is playing.
        // Blank until something has played since power-up.
        IdleMode::LastSong => match scene.art {
            Some(cover) => art::draw(Some(cover), true, target),
            None => Ok(()),
        },
        IdleMode::Off => Ok(()),
    }
}

/// Hour as shown, and "AM"/"PM" in 12-hour mode.
fn hour_and_suffix(time: &DateTime, clock_24h: bool) -> (i8, Option<&'static str>) {
    let h = time.hour();
    if clock_24h {
        (h, None)
    } else {
        (if h % 12 == 0 { 12 } else { h % 12 }, Some(am_pm(time)))
    }
}

fn am_pm(time: &DateTime) -> &'static str {
    if time.hour() < 12 { "AM" } else { "PM" }
}

const SUFFIX_GAP: i32 = 2;

/// The first font, largest first, in which the widest time this format can
/// show fits across the panel. Chosen per format rather than per minute, so
/// the clock doesn't change size at 10:00.
fn clock_font<'a>(fonts: &'a [&'a FontRenderer], clock_24h: bool) -> &'a FontRenderer {
    let suffix = if clock_24h {
        0
    } else {
        text::width(&TINY, "M") as i32 + SUFFIX_GAP
    };
    fonts
        .iter()
        .find(|font| suffix + text::width(font, "88:88") as i32 <= 64)
        .unwrap_or(&fonts[fonts.len() - 1])
}

/// The time in the largest of `fonts` that fits, with AM/PM stacked letter
/// over letter to its right, the pair centered on `center`. Shows `--:--`
/// before the clock syncs.
fn time_with_suffix<D>(
    fonts: &[&FontRenderer],
    time: Option<DateTime>,
    clock_24h: bool,
    center: Point,
    target: &mut D,
) -> Result<(), D::Error>
where
    D: DrawTarget<Color = Rgb888>,
{
    let font = clock_font(fonts, clock_24h);
    let Some(time) = time else {
        text::draw(font, "--:--", center, Align::Center, GREY, target)?;
        return Ok(());
    };
    let (hour, suffix) = hour_and_suffix(&time, clock_24h);
    let digits = if clock_24h {
        ShortText::format(format_args!("{hour:02}:{:02}", time.minute()))
    } else {
        ShortText::format(format_args!("{hour}:{:02}", time.minute()))
    };
    let digits_width = text::width(font, digits.as_str()) as i32;

    let Some(suffix) = suffix else {
        text::draw(font, digits.as_str(), center, Align::Center, WHITE, target)?;
        return Ok(());
    };
    let letter_width = text::width(&TINY, "M") as i32;
    let left = center.x - (digits_width + SUFFIX_GAP + letter_width) / 2;
    text::draw(
        font,
        digits.as_str(),
        Point::new(left, center.y),
        Align::Left,
        WHITE,
        target,
    )?;
    let suffix_x = left + digits_width + SUFFIX_GAP;
    let (first, second) = suffix.split_at(1);
    text::draw(
        &TINY,
        first,
        Point::new(suffix_x, center.y - 4),
        Align::Left,
        GREY,
        target,
    )?;
    text::draw(
        &TINY,
        second,
        Point::new(suffix_x, center.y + 3),
        Align::Left,
        GREY,
        target,
    )?;
    Ok(())
}

fn digital_clock<D>(time: Option<DateTime>, clock_24h: bool, target: &mut D) -> Result<(), D::Error>
where
    D: DrawTarget<Color = Rgb888>,
{
    time_with_suffix(
        &[&LARGE, &MEDIUM_LARGE, &MEDIUM],
        time,
        clock_24h,
        Point::new(32, 26),
        target,
    )?;
    date(time, 47, target)
}

/// The date, like `SUN NOV 8`, centered at `y`. Nothing before the clock
/// syncs.
fn date<D>(time: Option<DateTime>, y: i32, target: &mut D) -> Result<(), D::Error>
where
    D: DrawTarget<Color = Rgb888>,
{
    let Some(time) = time else {
        return Ok(());
    };
    let weekday = WEEKDAYS[time.weekday().to_monday_zero_offset() as usize];
    let month = MONTHS[(time.month() - 1) as usize];
    text::draw(
        &SMALL,
        format_args!("{weekday} {month} {}", time.day()),
        Point::new(32, y),
        Align::Center,
        GREY,
        target,
    )?;
    Ok(())
}

fn analog_clock<D>(time: Option<DateTime>, target: &mut D) -> Result<(), D::Error>
where
    D: DrawTarget<Color = Rgb888>,
{
    const CENTER: (f32, f32) = (31.5, 31.5);
    let point = |angle: f32, radius: f32| {
        Point::new(
            libm::roundf(CENTER.0 + libm::sinf(angle) * radius) as i32,
            libm::roundf(CENTER.1 - libm::cosf(angle) * radius) as i32,
        )
    };
    let tau = core::f32::consts::TAU;

    for i in 0..12 {
        let angle = i as f32 / 12.0 * tau;
        let (inner, color) = if i % 3 == 0 {
            (25.0, WHITE)
        } else {
            (28.0, GREY)
        };
        Line::new(point(angle, inner), point(angle, 30.0))
            .into_styled(PrimitiveStyle::with_stroke(color, 1))
            .draw(target)?;
    }

    if let Some(time) = time {
        let minutes = time.minute() as f32 + time.second() as f32 / 60.0;
        let hours = (time.hour() % 12) as f32 + minutes / 60.0;
        let center = point(0.0, 0.0);
        Line::new(center, point(hours / 12.0 * tau, 14.0))
            .into_styled(PrimitiveStyle::with_stroke(WHITE, 3))
            .draw(target)?;
        Line::new(center, point(minutes / 60.0 * tau, 22.0))
            .into_styled(PrimitiveStyle::with_stroke(WHITE, 1))
            .draw(target)?;
        Circle::with_center(center, 4)
            .into_styled(PrimitiveStyle::with_fill(ACCENT))
            .draw(target)?;
        // An analog face can't tell morning from evening, so always show it.
        text::draw(
            &TINY,
            am_pm(&time),
            Point::new(63, 60),
            Align::Right,
            GREY,
            target,
        )?;
    }
    Ok(())
}

fn unit_letter(units: Units) -> &'static str {
    match units {
        Units::Celsius => "C",
        Units::Fahrenheit => "F",
    }
}

/// How wide `temperature` draws, degree sign and unit included.
fn temperature_width(font: &FontRenderer, temp: i16, units: Units, ring: u32) -> i32 {
    let number = ShortText::format(format_args!("{temp}"));
    text::width(font, number.as_str()) as i32
        + 1
        + ring as i32
        + 1
        + text::width(&SMALL, unit_letter(units)) as i32
}

/// A temperature like `11°F`: the number in `font`, a small ring for the
/// degree sign, and the unit letter beside it at the top. `x` is the center,
/// or the left edge with `Align::Left`.
#[allow(clippy::too_many_arguments)]
fn temperature<D>(
    font: &FontRenderer,
    temp: i16,
    units: Units,
    ring: u32,
    x: i32,
    y: i32,
    align: Align,
    color: Rgb888,
    target: &mut D,
) -> Result<(), D::Error>
where
    D: DrawTarget<Color = Rgb888>,
{
    let number = ShortText::format(format_args!("{temp}"));
    let left = match align {
        Align::Left => x,
        _ => x - temperature_width(font, temp, units, ring) / 2,
    };
    let Some(area) = text::draw(
        font,
        number.as_str(),
        Point::new(left, y),
        Align::Left,
        color,
        target,
    )?
    else {
        return Ok(());
    };
    let top = area.top_left.y;
    let ring_x = area.top_left.x + area.size.width as i32 + 1;
    Circle::new(Point::new(ring_x, top), ring)
        .into_styled(PrimitiveStyle::with_stroke(color, 1))
        .draw(target)?;
    text::draw(
        &SMALL,
        unit_letter(units),
        Point::new(ring_x + ring as i32 + 1, top + 3),
        Align::Left,
        color,
        target,
    )?;
    Ok(())
}

fn no_weather<D>(top: i32, target: &mut D) -> Result<(), D::Error>
where
    D: DrawTarget<Color = Rgb888>,
{
    icons::draw_tinted(Icon::Cloud, Point::new(24, top), 1, Some(DIM), target)?;
    text::draw(
        &SMALL,
        "NO WEATHER",
        Point::new(32, top + 22),
        Align::Center,
        GREY,
        target,
    )?;
    Ok(())
}

fn weather<D>(weather: Option<WeatherInfo>, target: &mut D) -> Result<(), D::Error>
where
    D: DrawTarget<Color = Rgb888>,
{
    let Some(info) = weather else {
        return no_weather(18, target);
    };
    let r = info.reading;
    let (color, tint) = if info.stale {
        (GREY, Some(DIM))
    } else {
        (WHITE, None)
    };

    icons::draw_tinted(r.icon.into(), Point::new(16, 0), 2, tint, target)?;
    temperature(
        &LARGE,
        r.temp,
        info.units,
        4,
        32,
        46,
        Align::Center,
        color,
        target,
    )?;
    text::draw(
        &SMALL,
        format_args!("H{} L{}", r.high, r.low),
        Point::new(32, 60),
        Align::Center,
        GREY,
        target,
    )?;
    if info.stale {
        icons::draw(Icon::Stale, Point::new(1, 1), 1, target)?;
    }
    Ok(())
}

fn clock_weather<D>(
    time: Option<DateTime>,
    clock_24h: bool,
    weather: Option<WeatherInfo>,
    target: &mut D,
) -> Result<(), D::Error>
where
    D: DrawTarget<Color = Rgb888>,
{
    time_with_suffix(&[&MEDIUM], time, clock_24h, Point::new(32, 16), target)?;
    date(time, 31, target)?;
    weather_row(weather, 42, target)
}

/// The weather icon and temperature, centered together as one group, with
/// the icon's top edge at `top`.
fn weather_row<D>(weather: Option<WeatherInfo>, top: i32, target: &mut D) -> Result<(), D::Error>
where
    D: DrawTarget<Color = Rgb888>,
{
    let middle = top + 8;
    let Some(info) = weather else {
        text::draw(
            &SMALL,
            "NO WEATHER",
            Point::new(32, middle),
            Align::Center,
            GREY,
            target,
        )?;
        return Ok(());
    };
    let r = info.reading;
    let (color, tint) = if info.stale {
        (GREY, Some(DIM))
    } else {
        (WHITE, None)
    };
    // Center the icon and temperature together as one group.
    const GAP: i32 = 4;
    let icon = Icon::from(r.icon);
    let icon_width = icon.size().width as i32;
    let group_width = icon_width + GAP + temperature_width(&MEDIUM, r.temp, info.units, 3);
    let left = 32 - group_width / 2;
    icons::draw_tinted(icon, Point::new(left, top), 1, tint, target)?;
    temperature(
        &MEDIUM,
        r.temp,
        info.units,
        3,
        left + icon_width + GAP,
        middle,
        Align::Left,
        color,
        target,
    )?;
    if info.stale {
        icons::draw(Icon::Stale, Point::new(1, (top - 5).max(0)), 1, target)?;
    }
    Ok(())
}

/// Weather on top, then the next high and low tide, each after a wave icon
/// with an up or down arrow.
fn weather_tides<D>(
    weather: Option<WeatherInfo>,
    tides: Option<TideTimes>,
    clock_24h: bool,
    target: &mut D,
) -> Result<(), D::Error>
where
    D: DrawTarget<Color = Rgb888>,
{
    weather_row(weather, 8, target)?;

    const GAP: i32 = 4;
    let rows = [
        (Icon::TideHigh, tides.and_then(|t| t.high), 40),
        (Icon::TideLow, tides.and_then(|t| t.low), 52),
    ];
    let label = |time: Option<DateTime>| match time {
        Some(time) => tide_time(&time, clock_24h),
        None => ShortText::format(format_args!("--:--")),
    };
    // Line both rows up on the left, centered on the wider of the two.
    let icon_width = Icon::TideHigh.size().width as i32;
    let widest = rows
        .iter()
        .map(|&(_, time, _)| text::width(&SMALL, label(time).as_str()) as i32)
        .max()
        .unwrap_or(0);
    let left = 32 - (icon_width + GAP + widest) / 2;
    for (icon, time, middle) in rows {
        let height = icon.size().height as i32;
        icons::draw(icon, Point::new(left, middle - height / 2), 1, target)?;
        let color = if time.is_some() { WHITE } else { GREY };
        text::draw(
            &SMALL,
            label(time).as_str(),
            Point::new(left + icon_width + GAP, middle),
            Align::Left,
            color,
            target,
        )?;
    }
    Ok(())
}

/// A tide time like `4:12 PM`, or `16:12` on a 24-hour clock.
fn tide_time(time: &DateTime, clock_24h: bool) -> ShortText {
    let (hour, suffix) = hour_and_suffix(time, clock_24h);
    match suffix {
        Some(suffix) => ShortText::format(format_args!("{hour}:{:02} {suffix}", time.minute())),
        None => ShortText::format(format_args!("{hour:02}:{:02}", time.minute())),
    }
}
