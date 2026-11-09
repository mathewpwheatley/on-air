//! Stored settings, saved as one versioned blob.
//!
//! The format is `[SCHEMA_VERSION, postcard bytes...]`. Postcard encodes
//! fields and enum variants by position, so: only append fields and
//! variants, and bump `SCHEMA_VERSION` (with a migration) for anything else.

use alloc::string::String;
use alloc::vec::Vec;
use serde::{Deserialize, Serialize};

use crate::weather::Units;

pub const SCHEMA_VERSION: u8 = 1;

/// The idle screens, in Mode-button order.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum IdleMode {
    #[default]
    DigitalClock,
    AnalogClock,
    Weather,
    ClockWeather,
    Off,
}

impl IdleMode {
    /// The screen after this one when Mode is tapped.
    pub fn next(self) -> Self {
        match self {
            IdleMode::DigitalClock => IdleMode::AnalogClock,
            IdleMode::AnalogClock => IdleMode::Weather,
            IdleMode::Weather => IdleMode::ClockWeather,
            IdleMode::ClockWeather => IdleMode::Off,
            IdleMode::Off => IdleMode::DigitalClock,
        }
    }

    /// Whether this screen needs weather data, so fetching can stop
    /// otherwise.
    pub fn shows_weather(self) -> bool {
        matches!(self, IdleMode::Weather | IdleMode::ClockWeather)
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
    fn completeness() {
        assert!(!Settings::default().has_wifi());
        assert!(sample().is_complete());
        let mut s = sample();
        s.spotify_refresh_token = None;
        assert!(s.has_wifi() && !s.is_complete());
    }

    #[test]
    fn idle_mode_cycles_through_all_five() {
        let mut m = IdleMode::DigitalClock;
        let mut seen = Vec::new();
        for _ in 0..5 {
            seen.push(m);
            m = m.next();
        }
        assert_eq!(m, IdleMode::DigitalClock);
        assert_eq!(
            seen,
            [
                IdleMode::DigitalClock,
                IdleMode::AnalogClock,
                IdleMode::Weather,
                IdleMode::ClockWeather,
                IdleMode::Off
            ]
        );
        assert!(IdleMode::Weather.shows_weather() && IdleMode::ClockWeather.shows_weather());
        assert!(!IdleMode::DigitalClock.shows_weather() && !IdleMode::Off.shows_weather());
    }
}
