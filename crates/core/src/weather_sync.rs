//! The weather and tides loop, without the network: decides which
//! Open-Meteo request to send next, reads each reply, and reports what
//! changed. The host only sends requests and waits, like
//! [`crate::spotify_sync`].
//!
//! ```text
//! loop {
//!     match sync.next(now) {
//!         Next::Send(request) => {
//!             let outcome = send(&request);
//!             sync.handle(now, outcome, &mut emit);
//!         }
//!         // Sleep, waking early if the idle screen changes:
//!         // sync.set_idle_mode(mode)
//!         Next::Wait(ms) => {}
//!         Next::Idle => {} // nothing to fetch until the idle screen changes
//!     }
//! }
//! ```
//!
//! - The forecast is fetched every 15 min, only while the selected idle
//!   screen shows weather.
//! - Tide times are fetched every 6 h, only while the tide screen is
//!   selected.
//! - While the city's tide check is still unknown, it's retried whatever
//!   the screen, so the tide screen can join the Mode cycle once it's known.
//! - Errors back off, and the last good data stays on screen meanwhile.
//!
//! Start a new `WeatherSync` when the city or the units change.

use alloc::string::String;
use alloc::vec::Vec;

use crate::backoff::Backoff;
use crate::http::{Outcome, Request};
use crate::settings::IdleMode;
use crate::tide::{self, TideSupport, TideTurn};
use crate::weather::{self, Reading, Units};

/// What the host should do next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Next {
    /// Send this request, then pass the outcome to [`WeatherSync::handle`].
    Send(Request),
    /// Nothing to do for this many milliseconds, unless the idle screen
    /// changes.
    Wait(u64),
    /// Nothing to fetch for the selected idle screen. Wait until it changes.
    Idle,
}

/// What changed.
#[derive(Debug, Clone, PartialEq)]
pub enum Output {
    /// A new forecast. Note when it arrived, to mark it stale later.
    Weather(Reading),
    /// The upcoming highs and lows, in time order.
    Tides(Vec<TideTurn>),
    /// The city's tide check has an answer. Save it, and offer the tide
    /// screen only if it's available.
    TideSupport(TideSupport),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fetch {
    Forecast,
    Tides,
}

pub struct WeatherSync {
    forecast_url: String,
    tide_url: String,
    tides: TideSupport,
    idle_mode: IdleMode,
    in_flight: Option<Fetch>,
    forecast_due: u64,
    tides_due: u64,
    forecast_backoff: Backoff,
    tide_backoff: Backoff,
    random: u32,
}

impl WeatherSync {
    /// `tides` is the saved answer from the city's tide check. `seed` is any
    /// random number, used to spread out retries.
    pub fn new(
        lat: f32,
        lon: f32,
        units: Units,
        tides: TideSupport,
        idle_mode: IdleMode,
        seed: u32,
    ) -> Self {
        Self {
            forecast_url: weather::forecast_url(lat, lon, units),
            tide_url: tide::tide_url(lat, lon),
            tides,
            idle_mode,
            in_flight: None,
            forecast_due: 0,
            tides_due: 0,
            forecast_backoff: Backoff::weather(),
            tide_backoff: Backoff::weather(),
            random: seed | 1,
        }
    }

    /// The idle screen the owner picked, which decides what to fetch.
    pub fn set_idle_mode(&mut self, idle_mode: IdleMode) {
        self.idle_mode = idle_mode;
    }

    /// The next request to send, or how long to wait.
    pub fn next(&mut self, now: u64) -> Next {
        debug_assert!(self.in_flight.is_none(), "handle the last request first");
        let mut wait: Option<u64> = None;
        for (fetch, wanted, due) in [
            (Fetch::Forecast, self.wants_forecast(), self.forecast_due),
            (Fetch::Tides, self.wants_tides(), self.tides_due),
        ] {
            if !wanted {
                continue;
            }
            if now >= due {
                self.in_flight = Some(fetch);
                let url = match fetch {
                    Fetch::Forecast => &self.forecast_url,
                    Fetch::Tides => &self.tide_url,
                };
                return Next::Send(Request::get(url.clone()));
            }
            wait = Some(wait.map_or(due - now, |w| w.min(due - now)));
        }
        wait.map_or(Next::Idle, Next::Wait)
    }

    /// Read the outcome of the request [`WeatherSync::next`] returned.
    pub fn handle(&mut self, now: u64, outcome: Outcome, emit: &mut impl FnMut(Output)) {
        let Some(fetch) = self.in_flight.take() else {
            return;
        };
        let body = match outcome {
            Ok(response) if response.status == 200 => Some(response.body),
            _ => None,
        };
        match fetch {
            Fetch::Forecast => match body.and_then(|b| weather::parse_forecast(&b).ok()) {
                Some(reading) => {
                    self.forecast_backoff.reset();
                    self.forecast_due = now + weather::REFRESH_S * 1000;
                    emit(Output::Weather(reading));
                }
                None => {
                    let delay = self.forecast_backoff.next_delay_from(&mut self.random);
                    self.forecast_due = now + delay as u64;
                }
            },
            Fetch::Tides => {
                let support = body
                    .as_deref()
                    .map_or(TideSupport::Unknown, tide::support_from);
                if support != TideSupport::Unknown && support != self.tides {
                    self.tides = support;
                    emit(Output::TideSupport(support));
                }
                if support == TideSupport::Unknown {
                    let delay = self.tide_backoff.next_delay_from(&mut self.random);
                    self.tides_due = now + delay as u64;
                    return;
                }
                self.tide_backoff.reset();
                self.tides_due = now + tide::REFRESH_S * 1000;
                if support == TideSupport::Available {
                    let turns = body
                        .and_then(|b| tide::parse_tides(&b).ok())
                        .unwrap_or_default();
                    emit(Output::Tides(turns));
                }
            }
        }
    }

    fn wants_forecast(&self) -> bool {
        self.idle_mode.shows_weather()
    }

    fn wants_tides(&self) -> bool {
        match self.tides {
            // Keep checking until there's an answer, whatever the screen.
            TideSupport::Unknown => true,
            TideSupport::Available => self.idle_mode.shows_tides(),
            TideSupport::Unavailable => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::{NetworkError, Response};
    use rstest::rstest;

    const FORECAST: &[u8] = include_bytes!("../tests/fixtures/openmeteo_forecast_c.json");
    const COAST: &[u8] = include_bytes!("../tests/fixtures/openmeteo_marine_san_francisco.json");
    const INLAND: &[u8] = include_bytes!("../tests/fixtures/openmeteo_marine_denver.json");
    const MINUTE: u64 = 60_000;
    const HOUR: u64 = 60 * MINUTE;

    fn sync(tides: TideSupport, idle_mode: IdleMode) -> WeatherSync {
        WeatherSync::new(37.77, -122.42, Units::Celsius, tides, idle_mode, 42)
    }

    fn ok(body: &[u8]) -> Outcome {
        Ok(Response::new(200, body))
    }

    /// Expect a request to `url_start`, answer it, and return the outputs.
    fn fetch(sync: &mut WeatherSync, now: u64, url_start: &str, outcome: Outcome) -> Vec<Output> {
        match sync.next(now) {
            Next::Send(request) => assert!(request.url.starts_with(url_start), "{}", request.url),
            other => panic!("expected a request to {url_start}, got {other:?}"),
        }
        let mut out = Vec::new();
        sync.handle(now, outcome, &mut |o| out.push(o));
        out
    }

    #[rstest]
    #[case::clock(IdleMode::DigitalClock)]
    #[case::analog_clock(IdleMode::AnalogClock)]
    #[case::last_song(IdleMode::LastSong)]
    #[case::off(IdleMode::Off)]
    fn nothing_fetched_without_a_weather_screen(#[case] idle_mode: IdleMode) {
        assert_eq!(sync(TideSupport::Available, idle_mode).next(0), Next::Idle);
    }

    #[rstest]
    fn forecast_every_15_minutes_on_weather_screens(
        #[values(IdleMode::Weather, IdleMode::ClockWeather)] idle_mode: IdleMode,
    ) {
        let mut s = sync(TideSupport::Unavailable, idle_mode);
        let out = fetch(&mut s, 0, weather::FORECAST_URL, ok(FORECAST));
        assert!(matches!(out.as_slice(), [Output::Weather(r)] if r.temp == 11));
        assert_eq!(s.next(0), Next::Wait(15 * MINUTE));
        fetch(&mut s, 15 * MINUTE, weather::FORECAST_URL, ok(FORECAST));
    }

    #[test]
    fn tide_screen_fetches_both_on_their_own_schedules() {
        let mut s = sync(TideSupport::Available, IdleMode::WeatherTides);
        fetch(&mut s, 0, weather::FORECAST_URL, ok(FORECAST));
        let out = fetch(&mut s, 0, tide::MARINE_URL, ok(COAST));
        assert!(matches!(out.as_slice(), [Output::Tides(turns)] if turns.len() == 11));
        // The forecast comes round first; tides wait 6 h.
        assert_eq!(s.next(0), Next::Wait(15 * MINUTE));
        fetch(&mut s, 15 * MINUTE, weather::FORECAST_URL, ok(FORECAST));
        assert_eq!(s.next(15 * MINUTE), Next::Wait(15 * MINUTE));
        fetch(&mut s, 6 * HOUR, weather::FORECAST_URL, ok(FORECAST));
        fetch(&mut s, 6 * HOUR, tide::MARINE_URL, ok(COAST));
    }

    #[test]
    fn tides_not_fetched_for_other_weather_screens() {
        let mut s = sync(TideSupport::Available, IdleMode::Weather);
        fetch(&mut s, 0, weather::FORECAST_URL, ok(FORECAST));
        assert_eq!(s.next(0), Next::Wait(15 * MINUTE));
    }

    #[test]
    fn switching_to_a_weather_screen_fetches_right_away() {
        let mut s = sync(TideSupport::Unavailable, IdleMode::DigitalClock);
        assert_eq!(s.next(0), Next::Idle);
        s.set_idle_mode(IdleMode::Weather);
        fetch(&mut s, 5 * HOUR, weather::FORECAST_URL, ok(FORECAST));
    }

    #[rstest]
    #[case::coast(COAST, TideSupport::Available)]
    #[case::inland(INLAND, TideSupport::Unavailable)]
    fn unknown_tide_check_runs_on_any_screen(#[case] body: &[u8], #[case] answer: TideSupport) {
        let mut s = sync(TideSupport::Unknown, IdleMode::DigitalClock);
        let out = fetch(&mut s, 0, tide::MARINE_URL, ok(body));
        assert_eq!(out[0], Output::TideSupport(answer));
        // Answered: off the tide screen, nothing more to fetch.
        assert_eq!(s.next(0), Next::Idle);
    }

    #[test]
    fn inland_city_never_fetches_tides_again() {
        let mut s = sync(TideSupport::Unknown, IdleMode::WeatherTides);
        fetch(&mut s, 0, weather::FORECAST_URL, ok(FORECAST));
        let out = fetch(&mut s, 0, tide::MARINE_URL, ok(INLAND));
        assert_eq!(out, [Output::TideSupport(TideSupport::Unavailable)]);
        assert_eq!(s.next(0), Next::Wait(15 * MINUTE));
    }

    #[test]
    fn known_answer_isnt_reported_again() {
        let mut s = sync(TideSupport::Available, IdleMode::WeatherTides);
        fetch(&mut s, 0, weather::FORECAST_URL, ok(FORECAST));
        let out = fetch(&mut s, 0, tide::MARINE_URL, ok(COAST));
        assert!(matches!(out.as_slice(), [Output::Tides(_)]));
    }

    #[rstest]
    #[case::network_error(Err(NetworkError))]
    #[case::server_error(Ok(Response::new(503, &b""[..])))]
    #[case::unreadable(ok(b"not json"))]
    fn forecast_errors_back_off_and_report_nothing(#[case] outcome: Outcome) {
        let mut s = sync(TideSupport::Unavailable, IdleMode::Weather);
        assert!(fetch(&mut s, 0, weather::FORECAST_URL, outcome.clone()).is_empty());
        let Next::Wait(first) = s.next(0) else {
            panic!()
        };
        assert!((15_000..=30_000).contains(&first), "{first}");
        // The next failure waits longer.
        fetch(&mut s, first, weather::FORECAST_URL, outcome);
        let Next::Wait(second) = s.next(first) else {
            panic!()
        };
        assert!((30_000..=60_000).contains(&second), "{second}");
        // A success goes back to the normal schedule.
        fetch(&mut s, first + second, weather::FORECAST_URL, ok(FORECAST));
        assert_eq!(s.next(first + second), Next::Wait(15 * MINUTE));
    }

    #[rstest]
    #[case::network_error(Err(NetworkError))]
    #[case::server_error(Ok(Response::new(500, &b""[..])))]
    #[case::unreadable(ok(b"not json"))]
    fn failed_tide_check_stays_unknown_and_retries(#[case] outcome: Outcome) {
        let mut s = sync(TideSupport::Unknown, IdleMode::DigitalClock);
        assert!(fetch(&mut s, 0, tide::MARINE_URL, outcome).is_empty());
        let Next::Wait(retry) = s.next(0) else {
            panic!()
        };
        assert!(retry <= 30_000, "{retry}");
        let out = fetch(&mut s, retry, tide::MARINE_URL, ok(COAST));
        assert_eq!(out[0], Output::TideSupport(TideSupport::Available));
    }

    #[test]
    fn errors_keep_backing_off_up_to_15_minutes() {
        let mut s = sync(TideSupport::Unavailable, IdleMode::Weather);
        let mut now = 0;
        for _ in 0..20 {
            fetch(&mut s, now, weather::FORECAST_URL, Err(NetworkError));
            let Next::Wait(wait) = s.next(now) else {
                panic!()
            };
            assert!(wait <= 15 * MINUTE, "{wait}");
            now += wait;
        }
    }
}
