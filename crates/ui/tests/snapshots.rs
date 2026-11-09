//! Snapshot tests: render each screen and compare it with a checked-in
//! 64×64 PNG in `tests/snapshots/`.
//!
//! Run with `UPDATE_SNAPSHOTS=1` to accept changes. Every run also writes
//! 8× previews to `target/snapshot-previews/` for viewing.

use std::path::PathBuf;

use on_air_core::app::{Notice, Screen, View};
use on_air_core::settings::IdleMode;
use on_air_core::timezone;
use on_air_core::weather::{Icon, Reading, Units};
use on_air_ui::{Framebuffer, Scene, TideTimes, Track, WeatherInfo, draw};

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

fn base<'a>(screen: Screen) -> Scene<'a> {
    let utc = timezone::find("UTC").unwrap();
    Scene {
        view: view(screen),
        art: None,
        time: timezone::local_time(&utc, UNIX_S),
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
        setup_network: "OnAir-Setup-1A2B",
        now_ms: 0,
    }
}

/// A stand-in cover: a diagonal color gradient.
fn test_art() -> Vec<u8> {
    let mut art = Vec::with_capacity(64 * 64 * 3);
    for y in 0..64u32 {
        for x in 0..64u32 {
            art.extend([(x * 4) as u8, (y * 4) as u8, (255 - (x + y) * 2) as u8]);
        }
    }
    art
}

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

#[test]
fn digital_clock() {
    check(
        "digital_clock_12h",
        &base(Screen::Idle(IdleMode::DigitalClock)),
    );
    let mut s = base(Screen::Idle(IdleMode::DigitalClock));
    s.clock_24h = true;
    check("digital_clock_24h", &s);
    s.time = None;
    check("digital_clock_unsynced", &s);

    // The widest 12-hour time, to check AM/PM still fits.
    // 2026-11-08 12:58 PM UTC.
    let utc = timezone::find("UTC").unwrap();
    let mut s = base(Screen::Idle(IdleMode::DigitalClock));
    s.time = timezone::local_time(&utc, 1_794_142_680);
    check("digital_clock_widest", &s);
    s.view.screen = Screen::Idle(IdleMode::ClockWeather);
    check("clock_weather_widest", &s);
}

#[test]
fn analog_clock() {
    check("analog_clock", &base(Screen::Idle(IdleMode::AnalogClock)));
    let mut s = base(Screen::Idle(IdleMode::AnalogClock));
    s.time = None;
    check("analog_clock_unsynced", &s);
}

#[test]
fn weather() {
    for icon in [
        Icon::Sun,
        Icon::Cloud,
        Icon::Fog,
        Icon::Rain,
        Icon::Snow,
        Icon::Storm,
    ] {
        let mut s = base(Screen::Idle(IdleMode::Weather));
        s.weather = Some(WeatherInfo {
            reading: reading(icon),
            stale: false,
            units: Units::Celsius,
        });
        check(&format!("weather_{icon:?}").to_lowercase(), &s);
    }
    let mut s = base(Screen::Idle(IdleMode::Weather));
    s.weather = Some(WeatherInfo {
        reading: Reading {
            temp: -12,
            high: -3,
            low: -18,
            icon: Icon::Snow,
            utc_offset_s: 0,
        },
        stale: true,
        units: Units::Fahrenheit,
    });
    check("weather_cold_stale", &s);
    s.weather = None;
    check("weather_none", &s);
}

/// Three digits and below zero, on both weather screens.
#[test]
fn extreme_temperatures() {
    for (name, temp, high, low) in [("hot", 104, 109, 88), ("cold", -40, -31, -45)] {
        for (screen, mode) in [
            ("weather", IdleMode::Weather),
            ("clock_weather", IdleMode::ClockWeather),
        ] {
            let mut s = base(Screen::Idle(mode));
            s.weather = Some(WeatherInfo {
                reading: Reading {
                    temp,
                    high,
                    low,
                    icon: Icon::Sun,
                    utc_offset_s: 0,
                },
                stale: false,
                units: Units::Fahrenheit,
            });
            check(&format!("{screen}_{name}"), &s);
        }
    }
}

#[test]
fn clock_weather() {
    check("clock_weather", &base(Screen::Idle(IdleMode::ClockWeather)));
    let mut s = base(Screen::Idle(IdleMode::ClockWeather));
    s.weather = None;
    check("clock_weather_no_weather", &s);
}

#[test]
fn off_is_black() {
    let mut fb = Framebuffer::new();
    draw(&base(Screen::Idle(IdleMode::Off)), &mut fb).unwrap();
    assert!(fb.as_bytes().iter().all(|&b| b == 0));
}

#[test]
fn art() {
    let art = test_art();
    let mut s = base(Screen::Art { dimmed: false });
    s.art = Some(&art);
    check("art", &s);
    s.view.screen = Screen::Art { dimmed: true };
    check("art_paused", &s);
    s.view.screen = Screen::Art { dimmed: false };
    s.view.notice = Some(Notice::Pause);
    check("art_notice_pause", &s);
    s.view.notice = Some(Notice::PremiumNeeded);
    check("art_notice_premium", &s);
    s.view.notice = None;
    s.view.no_wifi = true;
    check("art_no_wifi", &s);
}

#[test]
fn last_song() {
    let art = test_art();
    let mut s = base(Screen::Idle(IdleMode::LastSong));
    s.art = Some(&art);
    check("last_song", &s);
}

#[test]
fn last_song_without_cover_is_black() {
    // Nothing has played since power-up.
    let mut fb = Framebuffer::new();
    draw(&base(Screen::Idle(IdleMode::LastSong)), &mut fb).unwrap();
    assert!(fb.as_bytes().iter().all(|&b| b == 0));
}

#[test]
fn mode_labels() {
    for &mode in IdleMode::cycle(true) {
        let mut s = base(Screen::Idle(mode));
        s.tides_available = true;
        s.view.notice = Some(Notice::Mode(mode));
        check(&format!("mode_{mode:?}").to_lowercase(), &s);
    }
    // Without tide data there's one dot fewer.
    let mut s = base(Screen::Idle(IdleMode::Weather));
    s.view.notice = Some(Notice::Mode(IdleMode::Weather));
    check("mode_weather_without_tides", &s);
}

#[test]
fn weather_tides() {
    let utc = timezone::find("UTC").unwrap();
    // 4:12 PM and 10:38 PM on the snapshot day.
    let tides = TideTimes {
        high: timezone::local_time(&utc, UNIX_S + 2 * 3600 + 42 * 60),
        low: timezone::local_time(&utc, UNIX_S + 9 * 3600 + 8 * 60),
    };
    let mut s = base(Screen::Idle(IdleMode::WeatherTides));
    s.tides_available = true;
    s.tides = Some(tides);
    check("weather_tides", &s);
    s.clock_24h = true;
    check("weather_tides_24h", &s);

    // Fetched but not back yet, and no weather either.
    let mut s = base(Screen::Idle(IdleMode::WeatherTides));
    s.tides_available = true;
    s.weather = None;
    check("weather_tides_waiting", &s);

    // Hot, stale weather with the widest times.
    let mut s = base(Screen::Idle(IdleMode::WeatherTides));
    s.tides_available = true;
    s.weather = Some(WeatherInfo {
        reading: Reading {
            temp: 104,
            high: 110,
            low: 80,
            icon: Icon::Sun,
            utc_offset_s: 0,
        },
        stale: true,
        units: Units::Fahrenheit,
    });
    // 10:58 AM and 11:49 PM.
    s.tides = Some(TideTimes {
        high: timezone::local_time(&utc, UNIX_S - 2 * 3600 - 32 * 60),
        low: timezone::local_time(&utc, UNIX_S + 10 * 3600 + 19 * 60),
    });
    check("weather_tides_widest", &s);
}

#[test]
fn track_info() {
    let art = test_art();
    let mut s = base(Screen::Art { dimmed: false });
    s.art = Some(&art);
    s.view.notice = Some(Notice::TrackInfo);
    check("track_info", &s);

    s.track = Some(Track {
        title: "Bohemian Rhapsody - Remastered 2011",
        artist: "Queen",
        album: Some("A Night at the Opera (2011 Remaster)"),
    });
    check("track_info_long_start", &s);
    s.view.notice_age_ms = 3_000;
    check("track_info_long_scrolled", &s);

    s.track = Some(Track {
        title: "Episode 42",
        artist: "A Podcast",
        album: None,
    });
    s.view.notice_age_ms = 0;
    check("track_info_podcast", &s);

    s.track = None;
    s.view.screen = Screen::Idle(IdleMode::DigitalClock);
    check("track_info_nothing", &s);
}

#[test]
fn status_screens() {
    check("setup", &base(Screen::Setup));
    check("connecting", &base(Screen::Connecting));
    check("sign_in_needed", &base(Screen::SignInNeeded));
}
