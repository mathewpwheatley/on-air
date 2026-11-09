//! Snapshot tests: render each screen and compare it with a checked-in
//! 64×64 PNG in `tests/snapshots/`.
//!
//! Run with `UPDATE_SNAPSHOTS=1` to accept changes. Every run also writes
//! 8× previews to `target/snapshot-previews/` for viewing.

use std::path::PathBuf;
use std::sync::LazyLock;

use on_air_core::app::{Notice, Screen, View};
use on_air_core::settings::IdleMode;
use on_air_core::timezone::{self, DateTime};
use on_air_core::weather::{Icon, Reading, Units};
use on_air_ui::{Framebuffer, Scene, TideTimes, Track, WeatherInfo, draw};
use rstest::rstest;

/// 2026-11-08 13:30:00 UTC, a Sunday.
const UNIX_S: i64 = 1_794_144_600;

fn view(screen: Screen) -> View {
    View {
        screen,
        notice: None,
        notice_age_ms: 0,
        no_wifi: false,
    }
}

fn reading(icon: Icon) -> Reading {
    Reading {
        temp: 11,
        high: 15,
        low: 4,
        icon,
        utc_offset_s: 0,
    }
}

/// Local time `offset_s` seconds after the snapshot moment, in UTC.
fn at(offset_s: i64) -> Option<DateTime> {
    timezone::local_time(&timezone::TimeZone::UTC, UNIX_S + offset_s)
}

fn weather(temp: i16, high: i16, low: i16, icon: Icon, stale: bool) -> Option<WeatherInfo> {
    Some(WeatherInfo {
        reading: Reading {
            temp,
            high,
            low,
            icon,
            utc_offset_s: 0,
        },
        stale,
        units: Units::Fahrenheit,
    })
}

fn base(screen: Screen) -> Scene<'static> {
    Scene {
        view: view(screen),
        art: None,
        time: at(0),
        clock_24h: false,
        weather: Some(WeatherInfo {
            reading: reading(Icon::Cloud),
            stale: false,
            units: Units::Celsius,
        }),
        tides_available: false,
        tides: None,
        track: Some(Track {
            title: "Mr. Brightside",
            artist: "The Killers",
            album: Some("Hot Fuss"),
        }),
        setup_network: "OnAir-1A2B",
        now_ms: 0,
    }
}

/// A stand-in cover: a diagonal color gradient.
static ART: LazyLock<Vec<u8>> = LazyLock::new(|| {
    let mut art = Vec::with_capacity(64 * 64 * 3);
    for y in 0..64u32 {
        for x in 0..64u32 {
            art.extend([(x * 4) as u8, (y * 4) as u8, (255 - (x + y) * 2) as u8]);
        }
    }
    art
});

fn with_art(s: &mut Scene) {
    s.art = Some(ART.as_slice());
}

const LONG_TRACK: Track<'static> = Track {
    title: "Bohemian Rhapsody - Remastered 2011",
    artist: "Queen",
    album: Some("A Night at the Opera (2011 Remaster)"),
};

fn check(name: &str, scene: &Scene) {
    let mut fb = Framebuffer::new();
    draw(scene, &mut fb).unwrap();
    let pixels = fb.as_bytes().to_vec();

    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let preview_dir = root.join("../../target/snapshot-previews");
    std::fs::create_dir_all(&preview_dir).unwrap();
    let image = image::RgbImage::from_raw(64, 64, pixels.clone()).unwrap();
    image::imageops::resize(&image, 512, 512, image::imageops::FilterType::Nearest)
        .save(preview_dir.join(format!("{name}.png")))
        .unwrap();

    let path = root.join("tests/snapshots").join(format!("{name}.png"));
    if std::env::var_os("UPDATE_SNAPSHOTS").is_some() || !path.exists() {
        image.save(&path).unwrap();
        return;
    }
    let expected = image::open(&path).unwrap().to_rgb8().into_raw();
    assert!(
        expected == pixels,
        "{name} changed. Check target/snapshot-previews/{name}.png, then rerun with UPDATE_SNAPSHOTS=1 to accept."
    );
}

type Change = fn(&mut Scene);

/// Each case is one snapshot: its file name, the screen, and what to change
/// from the base scene (12-hour clock, 11°C and cloudy, a song playing).
#[rstest]
// Clocks.
#[case::digital_clock_12h("digital_clock_12h", Screen::Idle(IdleMode::DigitalClock), |_: &mut Scene| {})]
#[case::digital_clock_24h("digital_clock_24h", Screen::Idle(IdleMode::DigitalClock), |s: &mut Scene| s.clock_24h = true)]
#[case::digital_clock_unsynced("digital_clock_unsynced", Screen::Idle(IdleMode::DigitalClock), |s: &mut Scene| {
    s.clock_24h = true;
    s.time = None;
})]
// The widest 12-hour time, 12:58 PM, to check AM/PM still fits.
#[case::digital_clock_widest("digital_clock_widest", Screen::Idle(IdleMode::DigitalClock), |s: &mut Scene| s.time = at(-32 * 60))]
#[case::analog_clock("analog_clock", Screen::Idle(IdleMode::AnalogClock), |_: &mut Scene| {})]
#[case::analog_clock_unsynced("analog_clock_unsynced", Screen::Idle(IdleMode::AnalogClock), |s: &mut Scene| s.time = None)]
// Weather.
#[case::weather_cold_stale("weather_cold_stale", Screen::Idle(IdleMode::Weather), |s: &mut Scene| {
    s.weather = weather(-12, -3, -18, Icon::Snow, true);
})]
#[case::weather_none("weather_none", Screen::Idle(IdleMode::Weather), |s: &mut Scene| s.weather = None)]
#[case::weather_hot("weather_hot", Screen::Idle(IdleMode::Weather), |s: &mut Scene| {
    s.weather = weather(104, 109, 88, Icon::Sun, false);
})]
#[case::weather_cold("weather_cold", Screen::Idle(IdleMode::Weather), |s: &mut Scene| {
    s.weather = weather(-40, -31, -45, Icon::Sun, false);
})]
// Clock + weather.
#[case::clock_weather("clock_weather", Screen::Idle(IdleMode::ClockWeather), |_: &mut Scene| {})]
#[case::clock_weather_no_weather("clock_weather_no_weather", Screen::Idle(IdleMode::ClockWeather), |s: &mut Scene| s.weather = None)]
#[case::clock_weather_widest("clock_weather_widest", Screen::Idle(IdleMode::ClockWeather), |s: &mut Scene| s.time = at(-32 * 60))]
#[case::clock_weather_hot("clock_weather_hot", Screen::Idle(IdleMode::ClockWeather), |s: &mut Scene| {
    s.weather = weather(104, 109, 88, Icon::Sun, false);
})]
#[case::clock_weather_cold("clock_weather_cold", Screen::Idle(IdleMode::ClockWeather), |s: &mut Scene| {
    s.weather = weather(-40, -31, -45, Icon::Sun, false);
})]
// Weather + tides. High at 4:12 PM and low at 10:38 PM.
#[case::weather_tides("weather_tides", Screen::Idle(IdleMode::WeatherTides), |s: &mut Scene| {
    s.tides = Some(TideTimes { high: at(2 * 3600 + 42 * 60), low: at(9 * 3600 + 8 * 60) });
})]
#[case::weather_tides_24h("weather_tides_24h", Screen::Idle(IdleMode::WeatherTides), |s: &mut Scene| {
    s.clock_24h = true;
    s.tides = Some(TideTimes { high: at(2 * 3600 + 42 * 60), low: at(9 * 3600 + 8 * 60) });
})]
// Tides fetched but not back yet, and no weather either.
#[case::weather_tides_waiting("weather_tides_waiting", Screen::Idle(IdleMode::WeatherTides), |s: &mut Scene| s.weather = None)]
// Hot, stale weather with the widest times, 10:58 AM and 11:49 PM.
#[case::weather_tides_widest("weather_tides_widest", Screen::Idle(IdleMode::WeatherTides), |s: &mut Scene| {
    s.weather = weather(104, 110, 80, Icon::Sun, true);
    s.tides = Some(TideTimes { high: at(-2 * 3600 - 32 * 60), low: at(10 * 3600 + 19 * 60) });
})]
// Album art and what shows over it.
#[case::art("art", Screen::Art { dimmed: false }, with_art)]
#[case::art_paused("art_paused", Screen::Art { dimmed: true }, with_art)]
#[case::art_notice_pause("art_notice_pause", Screen::Art { dimmed: false }, |s: &mut Scene| {
    with_art(s);
    s.view.notice = Some(Notice::Pause);
})]
#[case::art_notice_premium("art_notice_premium", Screen::Art { dimmed: false }, |s: &mut Scene| {
    with_art(s);
    s.view.notice = Some(Notice::PremiumNeeded);
})]
#[case::art_no_wifi("art_no_wifi", Screen::Art { dimmed: false }, |s: &mut Scene| {
    with_art(s);
    s.view.no_wifi = true;
})]
#[case::last_song("last_song", Screen::Idle(IdleMode::LastSong), with_art)]
// Track info.
#[case::track_info("track_info", Screen::Art { dimmed: false }, |s: &mut Scene| {
    with_art(s);
    s.view.notice = Some(Notice::TrackInfo);
})]
#[case::track_info_long_start("track_info_long_start", Screen::Art { dimmed: false }, |s: &mut Scene| {
    with_art(s);
    s.view.notice = Some(Notice::TrackInfo);
    s.track = Some(LONG_TRACK);
})]
#[case::track_info_long_scrolled("track_info_long_scrolled", Screen::Art { dimmed: false }, |s: &mut Scene| {
    with_art(s);
    s.view.notice = Some(Notice::TrackInfo);
    s.view.notice_age_ms = 3_000;
    s.track = Some(LONG_TRACK);
})]
#[case::track_info_podcast("track_info_podcast", Screen::Art { dimmed: false }, |s: &mut Scene| {
    with_art(s);
    s.view.notice = Some(Notice::TrackInfo);
    s.track = Some(Track { title: "Episode 42", artist: "A Podcast", album: None });
})]
#[case::track_info_nothing("track_info_nothing", Screen::Idle(IdleMode::DigitalClock), |s: &mut Scene| {
    with_art(s);
    s.view.notice = Some(Notice::TrackInfo);
    s.track = None;
})]
// Status screens.
#[case::setup("setup", Screen::Setup, |_: &mut Scene| {})]
#[case::connecting("connecting", Screen::Connecting, |_: &mut Scene| {})]
#[case::sign_in_needed("sign_in_needed", Screen::SignInNeeded, |_: &mut Scene| {})]
// Without tide data there's one dot fewer.
#[case::mode_weather_without_tides("mode_weather_without_tides", Screen::Idle(IdleMode::Weather), |s: &mut Scene| {
    s.view.notice = Some(Notice::Mode(IdleMode::Weather));
})]
fn snapshot(#[case] name: &str, #[case] screen: Screen, #[case] change: Change) {
    let mut scene = base(screen);
    change(&mut scene);
    check(name, &scene);
}

#[rstest]
fn weather_icons(
    #[values(Icon::Sun, Icon::Cloud, Icon::Fog, Icon::Rain, Icon::Snow, Icon::Storm)] icon: Icon,
) {
    let mut s = base(Screen::Idle(IdleMode::Weather));
    s.weather = Some(WeatherInfo {
        reading: reading(icon),
        stale: false,
        units: Units::Celsius,
    });
    check(&format!("weather_{icon:?}").to_lowercase(), &s);
}

/// The name and position dots shown after a Mode tap, with tide data so
/// every screen is in the cycle.
#[rstest]
fn mode_labels(
    #[values(
        IdleMode::AnalogClock,
        IdleMode::DigitalClock,
        IdleMode::Weather,
        IdleMode::ClockWeather,
        IdleMode::WeatherTides,
        IdleMode::LastSong,
        IdleMode::Off
    )]
    mode: IdleMode,
) {
    let mut s = base(Screen::Idle(mode));
    s.tides_available = true;
    s.view.notice = Some(Notice::Mode(mode));
    check(&format!("mode_{mode:?}").to_lowercase(), &s);
}

/// Screens that should leave every LED off.
#[rstest]
#[case::off(IdleMode::Off)]
// Nothing has played since power-up, so there's no cover to show.
#[case::last_song_without_cover(IdleMode::LastSong)]
fn black_screens(#[case] mode: IdleMode) {
    let mut fb = Framebuffer::new();
    draw(&base(Screen::Idle(mode)), &mut fb).unwrap();
    assert!(fb.as_bytes().iter().all(|&b| b == 0));
}

/// Every checked-in snapshot is still drawn by a test, so renamed or
/// removed screens don't leave stale files behind.
#[test]
fn no_leftover_snapshots() {
    let source = include_str!("snapshots.rs");
    // Names built with `format!` in the tests above.
    let icons = [
        Icon::Sun,
        Icon::Cloud,
        Icon::Fog,
        Icon::Rain,
        Icon::Snow,
        Icon::Storm,
    ];
    let generated: Vec<String> = icons
        .iter()
        .map(|icon| format!("weather_{icon:?}"))
        .chain(
            IdleMode::cycle(true)
                .iter()
                .map(|mode| format!("mode_{mode:?}")),
        )
        .map(|name| name.to_lowercase())
        .collect();

    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots");
    let leftover: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter(|file| {
            let stem = file.trim_end_matches(".png");
            !source.contains(&format!("\"{stem}\"")) && !generated.iter().any(|g| g == stem)
        })
        .collect();
    assert!(leftover.is_empty(), "no test draws {leftover:?}");
}
