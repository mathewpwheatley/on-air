//! Weather and tides from Open-Meteo, with `core::weather_sync::WeatherSync`
//! deciding what to fetch for the selected idle screen. The city comes from
//! `.env` at build time (`ON_AIR_CITY`, `ON_AIR_FAHRENHEIT`) until the setup
//! portal saves one; its position and tide check are saved, so it's looked
//! up only once.

use alloc::vec::Vec;
use embassy_futures::select::{Either, select};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::signal::Signal;
use embassy_time::{Instant, Timer};
use log::{info, warn};
use on_air_core::backoff::Backoff;
use on_air_core::http::Request;
use on_air_core::settings::{IdleMode, WeatherLocation};
use on_air_core::tide::{TideSupport, TideTurn};
use on_air_core::weather::{self, Reading, Units};
use on_air_core::weather_sync::{Next, Output, WeatherSync};

use crate::https::HttpsClient;
use crate::sntp_task;
use crate::storage::SettingsStore;

const CITY: Option<&str> = option_env!("ON_AIR_CITY");
const FAHRENHEIT: Option<&str> = option_env!("ON_AIR_FAHRENHEIT");

/// Temperatures in Fahrenheit or Celsius, from `.env`.
pub fn units() -> Units {
    if matches!(FAHRENHEIT, Some("true" | "1")) {
        Units::Fahrenheit
    } else {
        Units::Celsius
    }
}

/// What the display needs to know about the weather.
pub enum WeatherUpdate {
    Weather(Reading),
    /// The upcoming highs and lows, in time order.
    Tides(Vec<TideTurn>),
    /// Whether the city has tide data, which adds the tide screen.
    TideSupport(TideSupport),
}

/// Weather updates, for the display task.
pub static UPDATES: Channel<CriticalSectionRawMutex, WeatherUpdate, 4> = Channel::new();
/// The selected idle screen, from the display, which decides what to fetch.
pub static IDLE_MODE: Signal<CriticalSectionRawMutex, IdleMode> = Signal::new();

fn now() -> u64 {
    Instant::now().as_millis()
}

#[embassy_executor::task]
pub async fn weather_task(
    client: &'static HttpsClient,
    store: Option<&'static SettingsStore>,
    seed: u32,
) {
    let Some(city) = CITY.map(str::trim).filter(|c| !c.is_empty()) else {
        info!("weather: no ON_AIR_CITY in .env, so the weather screens stay empty");
        return;
    };
    // Certificates can't be checked until the clock is set.
    while sntp_task::unix_seconds().is_none() {
        Timer::after_millis(500).await;
    }

    let saved = match store {
        Some(store) => Some(store.get().await),
        None => None,
    };
    let (location, tides) = match saved {
        Some(settings) if settings.weather.as_ref().is_some_and(|w| w.city == city) => {
            (settings.weather.unwrap(), settings.tides)
        }
        _ => {
            let location = look_up(client, city, seed).await;
            if let Some(store) = store {
                let saved = location.clone();
                store
                    .update(|settings| {
                        settings.weather = Some(saved);
                        settings.tides = TideSupport::Unknown;
                    })
                    .await;
            }
            (location, TideSupport::Unknown)
        }
    };
    UPDATES.send(WeatherUpdate::TideSupport(tides)).await;

    let mut idle_mode = IDLE_MODE.wait().await;
    let mut sync = WeatherSync::new(location.lat, location.lon, units(), tides, idle_mode, seed);
    loop {
        match sync.next(now()) {
            Next::Send(request) => {
                let outcome = client.send(&request).await;
                let mut outputs = Vec::new();
                sync.handle(now(), outcome, &mut |output| outputs.push(output));
                for output in outputs {
                    let update = match output {
                        Output::Weather(reading) => {
                            info!(
                                "weather: {}° (high {}°, low {}°), {:?}",
                                reading.temp, reading.high, reading.low, reading.icon
                            );
                            WeatherUpdate::Weather(reading)
                        }
                        Output::Tides(turns) => {
                            info!("weather: {} tide turns", turns.len());
                            WeatherUpdate::Tides(turns)
                        }
                        Output::TideSupport(support) => {
                            info!("weather: {city} tide data: {support:?}");
                            if let Some(store) = store {
                                store.update(|settings| settings.tides = support).await;
                            }
                            WeatherUpdate::TideSupport(support)
                        }
                    };
                    UPDATES.send(update).await;
                }
            }
            Next::Wait(ms) => {
                if let Either::Second(mode) =
                    select(Timer::after_millis(ms), IDLE_MODE.wait()).await
                {
                    idle_mode = mode;
                    sync.set_idle_mode(idle_mode);
                }
            }
            Next::Idle => {
                idle_mode = IDLE_MODE.wait().await;
                sync.set_idle_mode(idle_mode);
            }
        }
    }
}

/// The city's position, retrying with backoff until the lookup gets through.
async fn look_up(client: &HttpsClient, city: &str, seed: u32) -> WeatherLocation {
    let mut backoff = Backoff::weather();
    let mut random = seed | 1;
    loop {
        let found = client
            .send(&Request::get(weather::geocoding_url(city, 1)))
            .await
            .ok()
            .filter(|response| response.status == 200)
            .and_then(|response| weather::parse_geocoding(&response.body).ok());
        match found.map(|places| places.into_iter().next()) {
            Some(Some(place)) => {
                info!(
                    "weather: {city} is {}, {} ({:.3}, {:.3})",
                    place.name,
                    place.country.as_deref().unwrap_or("?"),
                    place.lat,
                    place.lon
                );
                return WeatherLocation {
                    city: city.into(),
                    lat: place.lat,
                    lon: place.lon,
                };
            }
            Some(None) => warn!("weather: Open-Meteo knows no city called {city:?}"),
            None => {}
        }
        let delay_ms = backoff.next_delay_from(&mut random);
        warn!(
            "weather: city lookup failed, retrying in {} s",
            delay_ms / 1000
        );
        Timer::after_millis(delay_ms as u64).await;
    }
}
