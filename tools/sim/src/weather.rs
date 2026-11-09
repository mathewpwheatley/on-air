//! Weather from Open-Meteo for a real city, or a fixed fake reading.

use std::sync::mpsc::Sender;
use std::time::Duration;

use on_air_core::backoff::Backoff;
use on_air_core::weather::{self, Icon, Reading, Units};

use crate::messages::Update;

pub fn fake(updates: Sender<Update>) {
    let _ = updates.send(Update::Weather(Reading {
        temp: 11,
        high: 15,
        low: 6,
        icon: Icon::Rain,
        utc_offset_s: 0,
    }));
}

pub fn run(city: String, units: Units, updates: Sender<Update>) {
    let client = reqwest::blocking::Client::new();
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
    if let Some(timezone) = place.timezone {
        let _ = updates.send(Update::Timezone(timezone));
    }

    let url = weather::forecast_url(place.lat, place.lon, units);
    loop {
        match get(&client, &url).and_then(|b| weather::parse_forecast(&b).ok()) {
            Some(reading) => {
                backoff.reset();
                if updates.send(Update::Weather(reading)).is_err() {
                    return;
                }
                std::thread::sleep(Duration::from_secs(weather::REFRESH_S));
            }
            None => sleep_backoff(&mut backoff),
        }
    }
}

fn get(client: &reqwest::blocking::Client, url: &str) -> Option<Vec<u8>> {
    let response = client.get(url).send().ok()?;
    if !response.status().is_success() {
        return None;
    }
    response.bytes().ok().map(|b| b.to_vec())
}

fn sleep_backoff(backoff: &mut Backoff) {
    let delay = backoff.next_delay_ms(crate::random_u32());
    eprintln!("weather: request failed, retrying in {} s", delay / 1000);
    std::thread::sleep(Duration::from_millis(delay as u64));
}
