//! Open-Meteo forecast and geocoding.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use serde::{Deserialize, Serialize};

use crate::{ParseError, form_encode};

pub const FORECAST_URL: &str = "https://api.open-meteo.com/v1/forecast";
pub const GEOCODING_URL: &str = "https://geocoding-api.open-meteo.com/v1/search";
/// Refresh interval while a weather screen is selected.
pub const REFRESH_S: u64 = 15 * 60;
/// Older than this, the weather screens show a "stale" marker.
pub const STALE_AFTER_S: u64 = 2 * 60 * 60;

/// Temperature units (configurable). Variant order is part of the
/// stored settings format: only append.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Units {
    #[default]
    Celsius,
    Fahrenheit,
}

/// The small icon set the panel can draw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    Sun,
    Cloud,
    Fog,
    Rain,
    Snow,
    Storm,
}

/// Map a WMO weather code to an icon. Unknown codes show a cloud.
pub fn icon_for_wmo(code: u16) -> Icon {
    match code {
        0 | 1 => Icon::Sun,
        2 | 3 => Icon::Cloud,
        45 | 48 => Icon::Fog,
        51..=67 | 80..=82 => Icon::Rain,
        71..=77 | 85 | 86 => Icon::Snow,
        95..=99 => Icon::Storm,
        _ => Icon::Cloud,
    }
}

/// A forecast reading, in whole degrees of the requested units.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reading {
    pub temp: i16,
    pub high: i16,
    pub low: i16,
    pub icon: Icon,
    /// The location's current UTC offset, as reported by Open-Meteo.
    pub utc_offset_s: i32,
}

/// Forecast URL for one day of current conditions plus today's high and low.
pub fn forecast_url(lat: f32, lon: f32, units: Units) -> String {
    let mut url = format!(
        "{FORECAST_URL}?latitude={lat:.4}&longitude={lon:.4}\
         &current=temperature_2m,weather_code\
         &daily=temperature_2m_max,temperature_2m_min\
         &timezone=auto&forecast_days=1"
    );
    if units == Units::Fahrenheit {
        url.push_str("&temperature_unit=fahrenheit");
    }
    url
}

#[derive(Deserialize)]
struct RawForecast {
    utc_offset_seconds: i32,
    current: RawCurrent,
    daily: RawDaily,
}

#[derive(Deserialize)]
struct RawCurrent {
    temperature_2m: f32,
    weather_code: u16,
}

#[derive(Deserialize)]
struct RawDaily {
    temperature_2m_max: Vec<Option<f32>>,
    temperature_2m_min: Vec<Option<f32>>,
}

pub fn parse_forecast(body: &[u8]) -> Result<Reading, ParseError> {
    let raw: RawForecast = serde_json::from_slice(body).map_err(|_| ParseError)?;
    let first = |v: &[Option<f32>]| v.first().copied().flatten().ok_or(ParseError);
    Ok(Reading {
        temp: round(raw.current.temperature_2m),
        high: round(first(&raw.daily.temperature_2m_max)?),
        low: round(first(&raw.daily.temperature_2m_min)?),
        icon: icon_for_wmo(raw.current.weather_code),
        utc_offset_s: raw.utc_offset_seconds,
    })
}

fn round(v: f32) -> i16 {
    libm::roundf(v) as i16
}

/// Whether a reading this old should be marked stale.
pub fn is_stale(age_s: u64) -> bool {
    age_s > STALE_AFTER_S
}

/// A geocoding match, shown in the portal so the owner can pick the right
/// "Portland".
#[derive(Debug, Clone, PartialEq)]
pub struct Place {
    pub name: String,
    /// State or province.
    pub region: Option<String>,
    pub country: Option<String>,
    pub lat: f32,
    pub lon: f32,
    /// IANA name, e.g. `America/Los_Angeles`.
    pub timezone: Option<String>,
}

/// Geocoding URL for a city name typed in the portal.
pub fn geocoding_url(query: &str, count: u8) -> String {
    let count = format!("{count}");
    let params = form_encode(&[
        ("name", query.trim()),
        ("count", &count),
        ("language", "en"),
        ("format", "json"),
    ]);
    format!("{GEOCODING_URL}?{params}")
}

#[derive(Deserialize)]
struct RawGeocoding {
    #[serde(default)]
    results: Vec<RawPlace>,
}

#[derive(Deserialize)]
struct RawPlace {
    name: String,
    latitude: f32,
    longitude: f32,
    #[serde(default)]
    admin1: Option<String>,
    #[serde(default)]
    country: Option<String>,
    #[serde(default)]
    timezone: Option<String>,
}

/// Matches, best first. Empty when nothing matched.
pub fn parse_geocoding(body: &[u8]) -> Result<Vec<Place>, ParseError> {
    let raw: RawGeocoding = serde_json::from_slice(body).map_err(|_| ParseError)?;
    Ok(raw
        .results
        .into_iter()
        .map(|p| Place {
            name: p.name,
            region: p.admin1,
            country: p.country,
            lat: p.latitude,
            lon: p.longitude,
            timezone: p.timezone,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wmo_codes_map_to_icons() {
        let cases = [
            (0, Icon::Sun),
            (1, Icon::Sun),
            (2, Icon::Cloud),
            (3, Icon::Cloud),
            (45, Icon::Fog),
            (48, Icon::Fog),
            (51, Icon::Rain),
            (61, Icon::Rain),
            (67, Icon::Rain),
            (81, Icon::Rain),
            (71, Icon::Snow),
            (77, Icon::Snow),
            (86, Icon::Snow),
            (95, Icon::Storm),
            (99, Icon::Storm),
            (42, Icon::Cloud),
        ];
        for (code, icon) in cases {
            assert_eq!(icon_for_wmo(code), icon, "code {code}");
        }
    }

    #[test]
    fn forecast_urls() {
        let c = forecast_url(45.52345, -122.67621, Units::Celsius);
        assert_eq!(
            c,
            "https://api.open-meteo.com/v1/forecast?latitude=45.5234&longitude=-122.6762\
             &current=temperature_2m,weather_code&daily=temperature_2m_max,temperature_2m_min\
             &timezone=auto&forecast_days=1"
        );
        let f = forecast_url(45.5, -122.7, Units::Fahrenheit);
        assert!(f.ends_with("&temperature_unit=fahrenheit"));
    }

    #[test]
    fn parses_recorded_forecasts() {
        let c = parse_forecast(include_bytes!(
            "../tests/fixtures/openmeteo_forecast_c.json"
        ))
        .unwrap();
        assert_eq!(
            c,
            Reading {
                temp: 11,
                high: 15,
                low: 10,
                icon: Icon::Cloud,
                utc_offset_s: -28_800
            }
        );
        let f = parse_forecast(include_bytes!(
            "../tests/fixtures/openmeteo_forecast_f.json"
        ))
        .unwrap();
        assert_eq!((f.temp, f.high, f.low), (51, 59, 51));
    }

    #[test]
    fn forecast_missing_daily_value_is_an_error() {
        let body = br#"{"utc_offset_seconds":0,"current":{"temperature_2m":1.0,"weather_code":0},
                        "daily":{"temperature_2m_max":[null],"temperature_2m_min":[1.0]}}"#;
        assert_eq!(parse_forecast(body), Err(ParseError));
        assert_eq!(parse_forecast(b"not json"), Err(ParseError));
    }

    #[test]
    fn rounds_negative_temperatures() {
        let body = br#"{"utc_offset_seconds":0,"current":{"temperature_2m":-3.5,"weather_code":71},
                        "daily":{"temperature_2m_max":[-0.4],"temperature_2m_min":[-10.6]}}"#;
        let r = parse_forecast(body).unwrap();
        assert_eq!((r.temp, r.high, r.low, r.icon), (-4, 0, -11, Icon::Snow));
    }

    #[test]
    fn staleness() {
        assert!(!is_stale(0));
        assert!(!is_stale(STALE_AFTER_S));
        assert!(is_stale(STALE_AFTER_S + 1));
    }

    #[test]
    fn geocoding_url_encodes_city() {
        assert_eq!(
            geocoding_url(" San José ", 3),
            "https://geocoding-api.open-meteo.com/v1/search?name=San+Jos%C3%A9&count=3&language=en&format=json"
        );
    }

    #[test]
    fn parses_recorded_geocoding() {
        let places = parse_geocoding(include_bytes!(
            "../tests/fixtures/openmeteo_geocode_portland.json"
        ))
        .unwrap();
        assert!(!places.is_empty());
        let first = &places[0];
        assert_eq!(first.name, "Portland");
        assert_eq!(first.timezone.as_deref(), Some("America/Los_Angeles"));
        assert!((first.lat - 45.52345).abs() < 1e-4);
        assert!((first.lon + 122.67621).abs() < 1e-4);

        let none = parse_geocoding(include_bytes!(
            "../tests/fixtures/openmeteo_geocode_none.json"
        ))
        .unwrap();
        assert!(none.is_empty());
    }
}
