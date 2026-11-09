//! Tide times from Open-Meteo for the weather city, or made-up ones.

use std::sync::mpsc::Sender;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use on_air_core::backoff::Backoff;
use on_air_core::tide::{self, TideKind, TideSupport, TideTurn};

use crate::messages::Update;
use crate::weather::{Generation, get};

/// A high every 12 h 25 min, with lows halfway between, starting an hour
/// from now.
pub fn fake(updates: Sender<Update>) {
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

/// Keep the tide times for a location coming, every few hours. Stops as
/// soon as it finds the location has no tide data.
pub fn run_at(lat: f32, lon: f32, updates: Sender<Update>, generation: Generation) {
    let client = crate::http_client::client();
    let mut backoff = Backoff::weather();
    let url = tide::tide_url(lat, lon);
    while generation.is_current() {
        let Some(body) = get(&client, &url) else {
            let delay = backoff.next_delay_ms(crate::random_u32());
            eprintln!("tides: request failed, retrying in {} s", delay / 1000);
            std::thread::sleep(Duration::from_millis(delay as u64));
            continue;
        };
        backoff.reset();
        let support = tide::support_from(&body);
        if !generation.is_current() || updates.send(Update::TideSupport(support)).is_err() {
            return;
        }
        match support {
            TideSupport::Available => {
                let turns = tide::parse_tides(&body).unwrap_or_default();
                if updates.send(Update::Tides(turns)).is_err() {
                    return;
                }
            }
            TideSupport::Unavailable => {
                println!("tides: no tide data for this city");
                return;
            }
            TideSupport::Unknown => eprintln!("tides: unreadable reply, trying again later"),
        }
        std::thread::sleep(Duration::from_secs(tide::REFRESH_S));
    }
}
