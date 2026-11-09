//! Weather from Open-Meteo for a real city, or a fixed fake reading.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::time::Duration;

use on_air_core::backoff::Backoff;
use on_air_core::weather::{self, Icon, Reading, Units};

use crate::messages::Update;

/// Lets a newer weather thread replace an older one: each thread keeps its
/// number and stops once the shared counter moves on.
#[derive(Clone)]
pub struct Generation {
    counter: Arc<AtomicU64>,
    mine: u64,
}

impl Generation {
    pub fn first() -> (Self, Arc<AtomicU64>) {
        let counter = Arc::new(AtomicU64::new(0));
        (
            Self {
                counter: counter.clone(),
                mine: 0,
            },
            counter,
        )
    }

    /// Stop the current thread and return a handle for the next one.
    pub fn next(counter: &Arc<AtomicU64>) -> Self {
        let mine = counter.fetch_add(1, Ordering::SeqCst) + 1;
        Self {
            counter: counter.clone(),
            mine,
        }
    }

    pub fn is_current(&self) -> bool {
        self.counter.load(Ordering::SeqCst) == self.mine
    }
}

pub fn fake(updates: Sender<Update>) {
    let _ = updates.send(Update::Weather(Reading {
        temp: 11,
        high: 15,
        low: 6,
        icon: Icon::Rain,
        utc_offset_s: 0,
    }));
}

/// Look up `city`, then keep its weather coming.
pub fn run(city: String, units: Units, updates: Sender<Update>, generation: Generation) {
    let client = crate::http_client::client();
    let mut backoff = Backoff::weather();

    let place = loop {
        match get(&client, &weather::geocoding_url(&city, 1))
            .and_then(|b| weather::parse_geocoding(&b).ok())
        {
            Some(places) => match places.into_iter().next() {
                Some(place) => break place,
                None => {
                    eprintln!("weather: no city called {city:?}");
                    return;
                }
            },
            None => sleep_backoff(&mut backoff),
        }
    };
    println!(
        "weather: {}, {} ({:.3}, {:.3})",
        place.name,
        place.country.as_deref().unwrap_or("?"),
        place.lat,
        place.lon
    );
    if let Some(timezone) = place.timezone
        && generation.is_current()
    {
        let _ = updates.send(Update::Timezone(timezone));
    }
    let (tx, tide_generation) = (updates.clone(), generation.clone());
    std::thread::spawn(move || crate::tides::run_at(place.lat, place.lon, tx, tide_generation));
    run_at(place.lat, place.lon, units, updates, generation);
}

/// Keep the weather for a known location coming, every 15 minutes.
pub fn run_at(lat: f32, lon: f32, units: Units, updates: Sender<Update>, generation: Generation) {
    let client = crate::http_client::client();
    let mut backoff = Backoff::weather();
    let url = weather::forecast_url(lat, lon, units);
    while generation.is_current() {
        match get(&client, &url).and_then(|b| weather::parse_forecast(&b).ok()) {
            Some(reading) => {
                backoff.reset();
                if !generation.is_current() || updates.send(Update::Weather(reading)).is_err() {
                    return;
                }
                std::thread::sleep(Duration::from_secs(weather::REFRESH_S));
            }
            None => sleep_backoff(&mut backoff),
        }
    }
}

/// GET `url`, returning the body of a 200 reply.
pub fn get(client: &reqwest::blocking::Client, url: &str) -> Option<Vec<u8>> {
    match crate::http_client::send(client, &on_air_core::http::Request::get(url)) {
        Ok(response) if response.status == 200 => Some(response.body),
        _ => None,
    }
}

fn sleep_backoff(backoff: &mut Backoff) {
    let delay = backoff.next_delay_ms(crate::random_u32());
    eprintln!("weather: request failed, retrying in {} s", delay / 1000);
    std::thread::sleep(Duration::from_millis(delay as u64));
}
