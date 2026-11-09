//! Tide times from Open-Meteo's marine forecast.
//!
//! Open-Meteo gives an hourly sea level height, not tide times, so the highs
//! and lows are found as the peaks and dips of that curve. Inland places, and
//! some sheltered water the model doesn't cover, get no heights at all.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use serde::{Deserialize, Serialize};

use crate::ParseError;

pub const MARINE_URL: &str = "https://marine-api.open-meteo.com/v1/marine";
/// Tides are predictable days ahead, so they're fetched far less often than
/// the weather.
pub const REFRESH_S: u64 = 6 * 60 * 60;
/// Days of heights to ask for, so there's always a next high and low.
const FORECAST_DAYS: u8 = 3;

/// Whether the saved location has tide data. Checked once when the city is
/// saved. Variant order is part of the stored settings format: only append.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TideSupport {
    /// Not checked yet, or the check couldn't reach Open-Meteo. Checked
    /// again later.
    #[default]
    Unknown,
    Available,
    /// Open-Meteo has no tide data here.
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TideKind {
    High,
    Low,
}

/// One high or low tide.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TideTurn {
    pub kind: TideKind,
    /// Unix time in seconds.
    pub at_s: i64,
}

/// The next high and low after some moment, as Unix times in seconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NextTides {
    pub high: Option<i64>,
    pub low: Option<i64>,
}

/// Marine forecast URL for the hourly sea level height. Times come back as
/// Unix seconds, so no timezone handling is needed. `cell_selection=sea`
/// uses the nearest sea grid cell, which helps for places right on the coast.
pub fn tide_url(lat: f32, lon: f32) -> String {
    format!(
        "{MARINE_URL}?latitude={lat:.4}&longitude={lon:.4}\
         &hourly=sea_level_height_msl&timeformat=unixtime\
         &forecast_days={FORECAST_DAYS}&cell_selection=sea"
    )
}

#[derive(Deserialize)]
struct RawMarine {
    hourly: RawHourly,
}

#[derive(Deserialize)]
struct RawHourly {
    time: Vec<i64>,
    sea_level_height_msl: Vec<Option<f32>>,
}

/// The highs and lows in a marine forecast, in time order. Empty when the
/// location has no tide data.
pub fn parse_tides(body: &[u8]) -> Result<Vec<TideTurn>, ParseError> {
    let raw: RawMarine = serde_json::from_slice(body).map_err(|_| ParseError)?;
    let RawHourly {
        time,
        sea_level_height_msl: heights,
    } = raw.hourly;
    if time.len() != heights.len() {
        return Err(ParseError);
    }

    let mut turns = Vec::new();
    for i in 1..heights.len().saturating_sub(1) {
        let (Some(before), Some(here), Some(after)) = (heights[i - 1], heights[i], heights[i + 1])
        else {
            continue;
        };
        // `>=` on one side only, so a flat top of two equal hours counts once.
        let kind = if here > before && here >= after {
            TideKind::High
        } else if here < before && here <= after {
            TideKind::Low
        } else {
            continue;
        };
        // Fit a parabola through the three hours to place the turn between
        // them, rather than on the hour.
        let curve = before - 2.0 * here + after;
        let offset_h = if curve == 0.0 {
            0.0
        } else {
            (0.5 * (before - after) / curve).clamp(-0.5, 0.5)
        };
        let step_s = time[i + 1] - time[i];
        turns.push(TideTurn {
            kind,
            at_s: time[i] + libm::roundf(offset_h * step_s as f32) as i64,
        });
    }
    Ok(turns)
}

/// What a tide check found. A reply that can't be read counts as unknown,
/// so it's checked again rather than hiding tides for good.
pub fn support_from(body: &[u8]) -> TideSupport {
    match parse_tides(body) {
        Ok(turns) if has_high_and_low(&turns) => TideSupport::Available,
        Ok(_) => TideSupport::Unavailable,
        Err(_) => TideSupport::Unknown,
    }
}

fn has_high_and_low(turns: &[TideTurn]) -> bool {
    turns.iter().any(|t| t.kind == TideKind::High) && turns.iter().any(|t| t.kind == TideKind::Low)
}

/// The first high and the first low after `now_s`.
pub fn next_tides(turns: &[TideTurn], now_s: i64) -> NextTides {
    let next = |kind| {
        turns
            .iter()
            .find(|t| t.kind == kind && t.at_s > now_s)
            .map(|t| t.at_s)
    };
    NextTides {
        high: next(TideKind::High),
        low: next(TideKind::Low),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    const SAN_FRANCISCO: &[u8] =
        include_bytes!("../tests/fixtures/openmeteo_marine_san_francisco.json");
    const DENVER: &[u8] = include_bytes!("../tests/fixtures/openmeteo_marine_denver.json");

    fn high(at_s: i64) -> TideTurn {
        TideTurn {
            kind: TideKind::High,
            at_s,
        }
    }

    fn low(at_s: i64) -> TideTurn {
        TideTurn {
            kind: TideKind::Low,
            at_s,
        }
    }

    #[test]
    fn tide_url_asks_for_hourly_heights_at_sea() {
        assert_eq!(
            tide_url(37.77493, -122.41942),
            "https://marine-api.open-meteo.com/v1/marine?latitude=37.7749&longitude=-122.4194\
             &hourly=sea_level_height_msl&timeformat=unixtime&forecast_days=3&cell_selection=sea"
        );
    }

    #[test]
    fn finds_highs_and_lows_in_recorded_forecast() {
        let turns = parse_tides(SAN_FRANCISCO).unwrap();
        assert_eq!(turns.len(), 11);
        // Highs and lows alternate.
        for pair in turns.windows(2) {
            assert_ne!(pair[0].kind, pair[1].kind);
            assert!(pair[0].at_s < pair[1].at_s);
        }
        // 2026-11-09 09:18 PST, between the 09:00 and 10:00 readings.
        assert_eq!(turns[2], high(1_794_244_725));
    }

    /// Hourly heights, starting at time 0.
    #[rstest]
    #[case::peak_on_the_hour(&[Some(0.0), Some(1.0), Some(0.0)], &[high(3600)])]
    #[case::dip_on_the_hour(&[Some(1.0), Some(0.0), Some(1.0)], &[low(3600)])]
    #[case::peak_between_hours(&[Some(0.0), Some(1.0), Some(0.5)], &[high(3600 + 600)])]
    #[case::flat_top_counts_once(
        &[Some(0.0), Some(1.0), Some(1.0), Some(0.0), Some(-1.0)],
        &[high(5400)],
    )]
    #[case::rising_only(&[Some(0.0), Some(1.0), Some(2.0)], &[])]
    #[case::gap_hides_the_turn(&[Some(0.0), None, Some(1.0), Some(0.0)], &[])]
    #[case::too_short(&[Some(0.0), Some(1.0)], &[])]
    #[case::all_missing(&[None, None, None], &[])]
    fn turns_in_heights(#[case] heights: &[Option<f32>], #[case] expected: &[TideTurn]) {
        let time: Vec<i64> = (0..heights.len() as i64).map(|h| h * 3600).collect();
        let body = serde_json::json!({
            "hourly": { "time": time, "sea_level_height_msl": heights }
        });
        let body = serde_json::to_vec(&body).unwrap();
        assert_eq!(parse_tides(&body), Ok(expected.to_vec()));
    }

    #[rstest]
    #[case::not_json(b"not json")]
    #[case::mismatched_lengths(br#"{"hourly":{"time":[0,3600],"sea_level_height_msl":[1.0]}}"#)]
    #[case::no_hourly(br#"{"latitude":1.0}"#)]
    fn bad_replies_are_errors(#[case] body: &[u8]) {
        assert_eq!(parse_tides(body), Err(ParseError));
    }

    #[rstest]
    #[case::coast(SAN_FRANCISCO, TideSupport::Available)]
    #[case::inland(DENVER, TideSupport::Unavailable)]
    #[case::only_a_high(
        br#"{"hourly":{"time":[0,3600,7200],"sea_level_height_msl":[0.0,1.0,0.0]}}"#,
        TideSupport::Unavailable
    )]
    #[case::unreadable(b"not json", TideSupport::Unknown)]
    fn support(#[case] body: &[u8], #[case] expected: TideSupport) {
        assert_eq!(support_from(body), expected);
    }

    // The recorded forecast's first turns, in PST: 22:34 high, 03:15 low,
    // 09:18 high, 16:30 low.
    #[rstest]
    #[case::before_the_forecast(0, Some(1_794_206_040), Some(1_794_222_900))]
    #[case::morning(1_794_240_000, Some(1_794_244_725), Some(1_794_270_600))]
    #[case::exactly_at_a_high_takes_the_next(
        1_794_244_725,
        Some(1_794_295_286),
        Some(1_794_270_600)
    )]
    #[case::past_the_forecast(i64::MAX, None, None)]
    fn next_high_and_low(#[case] now_s: i64, #[case] high: Option<i64>, #[case] low: Option<i64>) {
        let turns = parse_tides(SAN_FRANCISCO).unwrap();
        assert_eq!(next_tides(&turns, now_s), NextTides { high, low });
    }
}
