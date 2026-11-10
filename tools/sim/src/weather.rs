//! Weather and tides from Open-Meteo for a real city, or fixed fake ones.
//! The core's `WeatherSync` decides what to fetch; this thread only sends
//! its requests and waits.

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use on_air_core::backoff::Backoff;
use on_air_core::settings::IdleMode;
use on_air_core::tide::{TideKind, TideSupport, TideTurn};
use on_air_core::weather::{self, Icon, Reading, Units};
use on_air_core::weather_sync::{Next, Output, WeatherSync};

use crate::messages::Update;

/// A fixed reading, and a high tide every 12 h 25 min with lows halfway
/// between, starting an hour from now.
pub fn fake(updates: Sender<Update>) {
    let _ = updates.send(Update::Weather(Reading {
        temp: 11,
        high: 15,
        low: 6,
        icon: Icon::Rain,
        utc_offset_s: 0,
    }));

    const HALF_CYCLE_S: i64 = (12 * 60 + 25) * 30;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let turns = (0..12)
        .map(|i| TideTurn {
            kind: if i % 2 == 0 {
                TideKind::High
            } else {
                TideKind::Low
            },
            at_s: now + 3600 + i * HALF_CYCLE_S,
        })
        .collect();
    let _ = updates.send(Update::TideSupport(TideSupport::Available));
    let _ = updates.send(Update::Tides(turns));
}

/// Look up `city`, then keep its weather and tides coming. Send the idle
/// screen to the returned channel whenever it changes; drop it to stop.
pub fn start_city(
    city: String,
    units: Units,
    idle_mode: IdleMode,
    updates: Sender<Update>,
) -> Sender<IdleMode> {
    let (controls, control_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let Some((lat, lon)) = look_up(&city, &updates) else {
            return;
        };
        let place = Place {
            lat,
            lon,
            units,
            tides: TideSupport::Unknown,
        };
        run(place, idle_mode, updates, control_rx);
    });
    controls
}

/// Keep the weather and tides for a known location coming. `tides` is the
/// saved answer from the city's tide check.
pub fn start_at(
    lat: f32,
    lon: f32,
    units: Units,
    tides: TideSupport,
    idle_mode: IdleMode,
    updates: Sender<Update>,
) -> Sender<IdleMode> {
    let (controls, control_rx) = mpsc::channel();
    let place = Place {
        lat,
        lon,
        units,
        tides,
    };
    std::thread::spawn(move || run(place, idle_mode, updates, control_rx));
    controls
}

struct Place {
    lat: f32,
    lon: f32,
    units: Units,
    tides: TideSupport,
}

/// The city's position, retrying until the lookup gets through. Also sends
/// its timezone.
fn look_up(city: &str, updates: &Sender<Update>) -> Option<(f32, f32)> {
    let client = crate::http_client::client();
    let mut backoff = Backoff::weather();
    let mut random = crate::random_u32() | 1;
    loop {
        let found = get(&client, &weather::geocoding_url(city, 1))
            .and_then(|b| weather::parse_geocoding(&b).ok());
        match found {
            Some(places) => {
                let Some(place) = places.into_iter().next() else {
                    eprintln!("weather: no city called {city:?}");
                    return None;
                };
                println!(
                    "weather: {}, {} ({:.3}, {:.3})",
                    place.name,
                    place.country.as_deref().unwrap_or("?"),
                    place.lat,
                    place.lon
                );
                if let Some(timezone) = place.timezone {
                    let _ = updates.send(Update::Timezone(timezone));
                }
                return Some((place.lat, place.lon));
            }
            None => {
                let delay = backoff.next_delay_from(&mut random);
                eprintln!(
                    "weather: city lookup failed, retrying in {} s",
                    delay / 1000
                );
                std::thread::sleep(Duration::from_millis(delay as u64));
            }
        }
    }
}

fn run(place: Place, idle_mode: IdleMode, updates: Sender<Update>, controls: Receiver<IdleMode>) {
    let client = crate::http_client::client();
    let start = Instant::now();
    let now = || start.elapsed().as_millis() as u64;
    let mut sync = WeatherSync::new(
        place.lat,
        place.lon,
        place.units,
        place.tides,
        idle_mode,
        crate::random_u32(),
    );
    loop {
        let wait = match sync.next(now()) {
            Next::Send(request) => {
                let outcome = crate::http_client::send(&client, &request);
                let mut outputs = Vec::new();
                sync.handle(now(), outcome, &mut |o| outputs.push(o));
                // Replaced while the request was out: drop what it found.
                if !take_controls(&controls, &mut sync) {
                    return;
                }
                for output in outputs {
                    let update = match output {
                        Output::Weather(reading) => Update::Weather(reading),
                        Output::Tides(turns) => Update::Tides(turns),
                        Output::TideSupport(support) => {
                            println!("tides: {support:?} for this city");
                            Update::TideSupport(support)
                        }
                    };
                    if updates.send(update).is_err() {
                        return;
                    }
                }
                continue;
            }
            Next::Wait(ms) => Duration::from_millis(ms),
            // Nothing to fetch until the idle screen changes.
            Next::Idle => Duration::MAX,
        };
        match controls.recv_timeout(wait) {
            Ok(mode) => sync.set_idle_mode(mode),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// Apply any idle screen changes. False once the window has replaced or
/// dropped this thread.
fn take_controls(controls: &Receiver<IdleMode>, sync: &mut WeatherSync) -> bool {
    loop {
        match controls.try_recv() {
            Ok(mode) => sync.set_idle_mode(mode),
            Err(TryRecvError::Empty) => return true,
            Err(TryRecvError::Disconnected) => return false,
        }
    }
}

/// GET `url`, returning the body of a 200 reply.
fn get(client: &reqwest::blocking::Client, url: &str) -> Option<Vec<u8>> {
    match crate::http_client::send(client, &on_air_core::http::Request::get(url)) {
        Ok(response) if response.status == 200 => Some(response.body),
        _ => None,
    }
}
