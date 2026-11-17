//! Stored settings, saved as one versioned blob.
//!
//! The format is `[SCHEMA_VERSION, postcard bytes...]`. Postcard encodes
//! fields and enum variants by position, so: only append fields and
//! variants, and bump `SCHEMA_VERSION` (with a migration) for anything else.

use alloc::string::String;
use alloc::vec::Vec;
use serde::{Deserialize, Serialize};

use crate::tide::TideSupport;
use crate::weather::Units;

pub const SCHEMA_VERSION: u8 = 1;

/// Idle time choices in the portal, in minutes.
pub const IDLE_AFTER_MIN_RANGE: core::ops::RangeInclusive<u8> = 1..=30;
pub const DEFAULT_IDLE_AFTER_MIN: u8 = 5;

/// The idle screens. The Mode-button order is set by [`IdleMode::next`].
/// Variant order is part of the stored settings format: only append.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum IdleMode {
    #[default]
    DigitalClock,
    AnalogClock,
    Weather,
    ClockWeather,
    Off,
    /// The most recent cover, kept on screen.
    LastSong,
    /// Weather with the next high and low tide. Only offered where the city
    /// has tide data.
    WeatherTides,
}

impl IdleMode {
    /// Every idle screen, in Mode-button order.
    const CYCLE: [IdleMode; 7] = [
        IdleMode::AnalogClock,
        IdleMode::DigitalClock,
        IdleMode::Weather,
        IdleMode::ClockWeather,
        IdleMode::WeatherTides,
        IdleMode::LastSong,
        IdleMode::Off,
    ];
    const CYCLE_WITHOUT_TIDES: [IdleMode; 6] = [
        IdleMode::AnalogClock,
        IdleMode::DigitalClock,
        IdleMode::Weather,
        IdleMode::ClockWeather,
        IdleMode::LastSong,
        IdleMode::Off,
    ];

    /// The idle screen with this name, such as `clock_weather`, for
    /// settings typed by hand. Case and `-` versus `_` don't matter.
    pub fn from_name(name: &str) -> Option<IdleMode> {
        let name = name.trim();
        let is = |wanted: &str| {
            name.len() == wanted.len()
                && name
                    .bytes()
                    .zip(wanted.bytes())
                    .all(|(a, b)| a.to_ascii_lowercase() == b || (a == b'-' && b == b'_'))
        };
        [
            ("digital_clock", IdleMode::DigitalClock),
            ("analog_clock", IdleMode::AnalogClock),
            ("weather", IdleMode::Weather),
            ("clock_weather", IdleMode::ClockWeather),
            ("weather_tides", IdleMode::WeatherTides),
            ("last_song", IdleMode::LastSong),
            ("off", IdleMode::Off),
        ]
        .into_iter()
        .find(|(wanted, _)| is(wanted))
        .map(|(_, mode)| mode)
    }

    /// The idle screens on offer, in Mode-button order. The tide screen is
    /// left out where there's no tide data.
    pub fn cycle(tides: bool) -> &'static [IdleMode] {
        if tides {
            &Self::CYCLE
        } else {
            &Self::CYCLE_WITHOUT_TIDES
        }
    }

    /// The screen to show for this choice: plain weather in place of the
    /// tide screen when there's no tide data.
    pub fn shown(self, tides: bool) -> Self {
        if self == IdleMode::WeatherTides && !tides {
            IdleMode::Weather
        } else {
            self
        }
    }

    /// The screen after this one when Mode is tapped.
    pub fn next(self, tides: bool) -> Self {
        let cycle = Self::cycle(tides);
        let current = self.shown(tides);
        let i = cycle.iter().position(|&m| m == current).unwrap_or(0);
        cycle[(i + 1) % cycle.len()]
    }

    /// Whether this screen needs weather data, so fetching can stop
    /// otherwise.
    pub fn shows_weather(self) -> bool {
        matches!(
            self,
            IdleMode::Weather | IdleMode::ClockWeather | IdleMode::WeatherTides
        )
    }

    /// Whether this screen needs tide times.
    pub fn shows_tides(self) -> bool {
        self == IdleMode::WeatherTides
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WeatherLocation {
    /// As typed in the portal, for showing back to the owner.
    pub city: String,
    pub lat: f32,
    pub lon: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    pub wifi_ssid: String,
    pub wifi_pass: String,
    pub spotify_client_id: String,
    /// Rewritten whenever Spotify rotates it.
    pub spotify_refresh_token: Option<String>,
    pub idle_mode: IdleMode,
    /// IANA timezone name for the clock, e.g. `America/Los_Angeles`.
    pub timezone: String,
    pub clock_24h: bool,
    pub weather: Option<WeatherLocation>,
    pub units: Units,
    /// Minutes with nothing playing before the idle screen shows.
    pub idle_after_min: u8,
    /// Whether the weather city has tide data, checked when it's saved.
    pub tides: TideSupport,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            wifi_ssid: String::new(),
            wifi_pass: String::new(),
            spotify_client_id: String::new(),
            spotify_refresh_token: None,
            idle_mode: IdleMode::default(),
            timezone: String::from("UTC"),
            clock_24h: false,
            weather: None,
            units: Units::default(),
            idle_after_min: DEFAULT_IDLE_AFTER_MIN,
            tides: TideSupport::Unknown,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsError {
    /// Nothing stored, or a blob from an unknown future version.
    UnknownVersion,
    /// The blob is damaged.
    Corrupt,
}

impl Settings {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(128);
        out.push(SCHEMA_VERSION);
        // Serializing plain strings, options and enums can't fail.
        out.extend(postcard::to_allocvec(self).expect("settings serialize"));
        out
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, SettingsError> {
        match bytes.split_first() {
            Some((&SCHEMA_VERSION, rest)) => {
                postcard::from_bytes(rest).map_err(|_| SettingsError::Corrupt)
            }
            _ => Err(SettingsError::UnknownVersion),
        }
    }

    /// The idle time in milliseconds, kept within the allowed range.
    pub fn idle_after_ms(&self) -> u64 {
        let range = IDLE_AFTER_MIN_RANGE;
        self.idle_after_min.clamp(*range.start(), *range.end()) as u64 * 60_000
    }

    /// Whether to offer the tide screen.
    pub fn has_tides(&self) -> bool {
        self.weather.is_some() && self.tides == TideSupport::Available
    }

    /// Whether there's enough to join WiFi.
    pub fn has_wifi(&self) -> bool {
        !self.wifi_ssid.is_empty()
    }

    /// Whether there's enough to show album art without the portal.
    pub fn is_complete(&self) -> bool {
        self.has_wifi()
            && !self.spotify_client_id.is_empty()
            && self.spotify_refresh_token.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::{fixture, rstest};

    #[rstest]
    #[case::snake("clock_weather", Some(IdleMode::ClockWeather))]
    #[case::kebab_and_caps("Weather-Tides", Some(IdleMode::WeatherTides))]
    #[case::padded(" off ", Some(IdleMode::Off))]
    #[case::unknown("radar", None)]
    #[case::empty("", None)]
    fn idle_mode_names(#[case] name: &str, #[case] expected: Option<IdleMode>) {
        assert_eq!(IdleMode::from_name(name), expected);
    }

    /// Every field set to something other than its default.
    #[fixture]
    fn sample() -> Settings {
        Settings {
            wifi_ssid: "Home".into(),
            wifi_pass: "hunter2".into(),
            spotify_client_id: "cid".into(),
            spotify_refresh_token: Some("rt".into()),
            idle_mode: IdleMode::Weather,
            timezone: "America/Los_Angeles".into(),
            clock_24h: true,
            weather: Some(WeatherLocation {
                city: "Portland".into(),
                lat: 45.52,
                lon: -122.68,
            }),
            units: Units::Fahrenheit,
            idle_after_min: 12,
            tides: TideSupport::Available,
        }
    }

    #[rstest]
    #[case::defaults(Settings::default())]
    #[case::every_field_set(sample())]
    fn round_trips(#[case] settings: Settings) {
        let bytes = settings.to_bytes();
        assert_eq!(bytes[0], SCHEMA_VERSION);
        assert_eq!(Settings::from_bytes(&bytes), Ok(settings));
    }

    #[rstest]
    #[case::empty(Vec::new(), SettingsError::UnknownVersion)]
    #[case::future_version(vec![99, 1, 2], SettingsError::UnknownVersion)]
    #[case::truncated(
        { let mut b = sample().to_bytes(); b.truncate(b.len() / 2); b },
        SettingsError::Corrupt,
    )]
    #[case::version_only(vec![SCHEMA_VERSION], SettingsError::Corrupt)]
    fn rejects_bad_blobs(#[case] bytes: Vec<u8>, #[case] error: SettingsError) {
        assert_eq!(Settings::from_bytes(&bytes), Err(error));
    }

    #[test]
    fn idle_time_defaults_to_five_minutes() {
        assert_eq!(Settings::default().idle_after_ms(), 5 * 60_000);
    }

    #[rstest]
    #[case::below_range(0, 1)]
    #[case::shortest(1, 1)]
    #[case::longest(30, 30)]
    #[case::above_range(99, 30)]
    fn idle_time_stays_in_range(mut sample: Settings, #[case] minutes: u8, #[case] kept: u64) {
        sample.idle_after_min = minutes;
        assert_eq!(sample.idle_after_ms(), kept * 60_000);
    }

    #[rstest]
    #[case::everything(|_: &mut Settings| {}, true, true)]
    #[case::no_wifi(|s: &mut Settings| s.wifi_ssid.clear(), false, false)]
    #[case::no_client_id(|s: &mut Settings| s.spotify_client_id.clear(), true, false)]
    #[case::not_signed_in(|s: &mut Settings| s.spotify_refresh_token = None, true, false)]
    fn completeness(
        mut sample: Settings,
        #[case] change: fn(&mut Settings),
        #[case] has_wifi: bool,
        #[case] complete: bool,
    ) {
        change(&mut sample);
        assert_eq!(sample.has_wifi(), has_wifi);
        assert_eq!(sample.is_complete(), complete);
    }

    #[rstest]
    #[case::available(|_: &mut Settings| {}, true)]
    #[case::not_checked_yet(|s: &mut Settings| s.tides = TideSupport::Unknown, false)]
    #[case::no_tide_data(|s: &mut Settings| s.tides = TideSupport::Unavailable, false)]
    #[case::no_city(|s: &mut Settings| s.weather = None, false)]
    fn tides_need_a_city_with_tide_data(
        mut sample: Settings,
        #[case] change: fn(&mut Settings),
        #[case] tides: bool,
    ) {
        change(&mut sample);
        assert_eq!(sample.has_tides(), tides);
    }

    fn cycle_from(start: IdleMode, tides: bool) -> Vec<IdleMode> {
        let mut m = start;
        let mut seen = Vec::new();
        loop {
            seen.push(m);
            m = m.next(tides);
            if m == start {
                return seen;
            }
        }
    }

    #[rstest]
    #[case::with_tides(true, &[
        IdleMode::AnalogClock,
        IdleMode::DigitalClock,
        IdleMode::Weather,
        IdleMode::ClockWeather,
        IdleMode::WeatherTides,
        IdleMode::LastSong,
        IdleMode::Off,
    ])]
    #[case::without_tides(false, &[
        IdleMode::AnalogClock,
        IdleMode::DigitalClock,
        IdleMode::Weather,
        IdleMode::ClockWeather,
        IdleMode::LastSong,
        IdleMode::Off,
    ])]
    fn mode_cycle(#[case] tides: bool, #[case] expected: &[IdleMode]) {
        assert_eq!(cycle_from(IdleMode::AnalogClock, tides), expected);
        assert_eq!(IdleMode::cycle(tides), expected);
    }

    #[rstest]
    #[case::tides_shown_with_data(IdleMode::WeatherTides, true, IdleMode::WeatherTides)]
    #[case::tides_fall_back_to_weather(IdleMode::WeatherTides, false, IdleMode::Weather)]
    #[case::others_unchanged(IdleMode::DigitalClock, false, IdleMode::DigitalClock)]
    fn shown_screen(#[case] mode: IdleMode, #[case] tides: bool, #[case] shown: IdleMode) {
        assert_eq!(mode.shown(tides), shown);
    }

    #[test]
    fn mode_from_the_weather_fallback_carries_on_as_if_from_weather() {
        assert_eq!(IdleMode::WeatherTides.next(false), IdleMode::ClockWeather);
    }

    #[rstest]
    #[case::analog_clock(IdleMode::AnalogClock, false, false)]
    #[case::digital_clock(IdleMode::DigitalClock, false, false)]
    #[case::weather(IdleMode::Weather, true, false)]
    #[case::clock_weather(IdleMode::ClockWeather, true, false)]
    #[case::weather_tides(IdleMode::WeatherTides, true, true)]
    #[case::last_song(IdleMode::LastSong, false, false)]
    #[case::off(IdleMode::Off, false, false)]
    fn what_each_screen_needs(#[case] mode: IdleMode, #[case] weather: bool, #[case] tides: bool) {
        assert_eq!(mode.shows_weather(), weather);
        assert_eq!(mode.shows_tides(), tides);
    }
}
