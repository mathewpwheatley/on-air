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

    #[test]
    fn round_trips() {
        let s = sample();
        let bytes = s.to_bytes();
        assert_eq!(bytes[0], SCHEMA_VERSION);
        assert_eq!(Settings::from_bytes(&bytes), Ok(s));
    }

    #[test]
    fn rejects_empty_unknown_and_corrupt() {
        assert_eq!(
            Settings::from_bytes(&[]),
            Err(SettingsError::UnknownVersion)
        );
        assert_eq!(
            Settings::from_bytes(&[99, 1, 2]),
            Err(SettingsError::UnknownVersion)
        );
        let mut bytes = sample().to_bytes();
        bytes.truncate(bytes.len() / 2);
        assert_eq!(Settings::from_bytes(&bytes), Err(SettingsError::Corrupt));
    }

    #[test]
    fn idle_time_defaults_to_five_minutes_and_stays_in_range() {
        assert_eq!(Settings::default().idle_after_ms(), 5 * 60_000);
        let mut s = sample();
        s.idle_after_min = 0;
        assert_eq!(s.idle_after_ms(), 60_000);
        s.idle_after_min = 99;
        assert_eq!(s.idle_after_ms(), 30 * 60_000);
    }

    #[test]
    fn completeness() {
        assert!(!Settings::default().has_wifi());
        assert!(sample().is_complete());
        let mut s = sample();
        s.spotify_refresh_token = None;
        assert!(s.has_wifi() && !s.is_complete());
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

    #[test]
    fn idle_mode_cycle_includes_tides_only_when_available() {
        assert_eq!(
            cycle_from(IdleMode::AnalogClock, true),
            [
                IdleMode::AnalogClock,
                IdleMode::DigitalClock,
                IdleMode::Weather,
                IdleMode::ClockWeather,
                IdleMode::WeatherTides,
                IdleMode::LastSong,
                IdleMode::Off
            ]
        );
        assert_eq!(
            cycle_from(IdleMode::AnalogClock, false),
            [
                IdleMode::AnalogClock,
                IdleMode::DigitalClock,
                IdleMode::Weather,
                IdleMode::ClockWeather,
                IdleMode::LastSong,
                IdleMode::Off
            ]
        );
        assert_eq!(
            IdleMode::cycle(true),
            cycle_from(IdleMode::AnalogClock, true)
        );
        assert_eq!(
            IdleMode::cycle(false),
            cycle_from(IdleMode::AnalogClock, false)
        );
    }

    #[test]
    fn tide_screen_falls_back_to_weather_without_tides() {
        assert_eq!(IdleMode::WeatherTides.shown(true), IdleMode::WeatherTides);
        assert_eq!(IdleMode::WeatherTides.shown(false), IdleMode::Weather);
        // Mode from the fallback carries on as if from weather.
        assert_eq!(IdleMode::WeatherTides.next(false), IdleMode::ClockWeather);
        assert_eq!(IdleMode::DigitalClock.shown(false), IdleMode::DigitalClock);
    }

    #[test]
    fn weather_and_tide_screens() {
        for m in [
            IdleMode::Weather,
            IdleMode::ClockWeather,
            IdleMode::WeatherTides,
        ] {
            assert!(m.shows_weather(), "{m:?}");
        }
        assert!(!IdleMode::DigitalClock.shows_weather() && !IdleMode::Off.shows_weather());
        assert!(IdleMode::WeatherTides.shows_tides() && !IdleMode::Weather.shows_tides());
    }

    #[test]
    fn tides_need_a_city_with_tide_data() {
        assert!(sample().has_tides());
        let mut s = sample();
        s.tides = TideSupport::Unknown;
        assert!(!s.has_tides());
        let mut s = sample();
        s.weather = None;
        assert!(!s.has_tides());
    }
}
