//! Weather and tides from Open-Meteo, with `core::weather_sync::WeatherSync`
//! deciding what to fetch for the selected idle screen. The city comes from
//! the setup portal, or in development from `.env` (see `config`); a city
//! from `.env` is looked up once and its position and tide check saved.

use alloc::vec::Vec;
use embassy_futures::select::{Either, select};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::signal::Signal;
use embassy_time::Instant;
use log::{info, warn};
use on_air_core::backoff::Backoff;
use on_air_core::http::Request;
use on_air_core::settings::{IdleMode, WeatherLocation};
use on_air_core::tide::{TideSupport, TideTurn};
use on_air_core::weather::{self, Reading};
use on_air_core::weather_sync::{Next, Output, WeatherSync};

use crate::config::Effective;
use crate::https::HttpsClient;
use crate::sntp_task;
use crate::storage::SettingsStore;
use crate::watchdog::{self, Watch};

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
    settings: &'static Effective,
    seed: u32,
) -> ! {
    let watch = watchdog::watch("weather", watchdog::NETWORK_TASK_MS);
    run(watch, client, store, settings, seed).await;
    watch.idle().await
}

async fn run(
    watch: Watch,
    client: &'static HttpsClient,
    store: Option<&'static SettingsStore>,
    settings: &'static Effective,
    seed: u32,
) {
    let looked_up = settings.city_to_look_up();
    if settings.settings.weather.is_none() && looked_up.is_none() {
        info!("weather: no city set, so the weather screens stay empty");
        return;
    }
    // Certificates can't be checked until the clock is set.
    while sntp_task::unix_seconds().is_none() {
        watch.sleep(500).await;
    }

    let (location, tides) = match (&settings.settings.weather, looked_up) {
        (Some(location), _) => (location.clone(), settings.settings.tides),
        (None, Some(city)) => {
            let location = look_up(watch, client, city, seed).await;
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
        (None, None) => return,
    };
    let city = location.city.as_str();
    UPDATES.send(WeatherUpdate::TideSupport(tides)).await;

    // `watch.idle()` never ends, so this waits for the display's first
    // idle screen while checking in.
    let Either::Second(mut idle_mode) = select(watch.idle(), IDLE_MODE.wait()).await;
    let units = settings.settings.units;
    let mut sync = WeatherSync::new(location.lat, location.lon, units, tides, idle_mode, seed);
    loop {
        match sync.next(now()) {
            Next::Send(request) => {
                let outcome = client.send(&request).await;
                watch.check_in();
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
                if let Either::Second(mode) = select(watch.sleep(ms), IDLE_MODE.wait()).await {
                    idle_mode = mode;
                    sync.set_idle_mode(idle_mode);
                }
            }
            Next::Idle => {
                let Either::Second(mode) = select(watch.idle(), IDLE_MODE.wait()).await;
                idle_mode = mode;
                sync.set_idle_mode(idle_mode);
            }
        }
    }
}

/// The city's position, retrying with backoff until the lookup gets through.
async fn look_up(watch: Watch, client: &HttpsClient, city: &str, seed: u32) -> WeatherLocation {
    let mut backoff = Backoff::weather();
    let mut random = seed | 1;
    loop {
        let found = client
            .send(&Request::get(weather::geocoding_url(city, 1)))
            .await
            .inspect(|_| watch.check_in())
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
        watch.sleep(delay_ms as u64).await;
    }
}
