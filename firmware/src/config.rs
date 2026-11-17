//! The settings the board runs with. Once the setup portal has saved a
//! WiFi network, the saved settings are used as they are. Until then (in
//! development), the saved settings are filled in from `.env` at build time,
//! so a board can run without going through setup.

use on_air_core::settings::{IdleMode, Settings};
use on_air_core::weather::Units;

const WIFI_SSID: Option<&str> = option_env!("ON_AIR_WIFI_SSID");
const WIFI_PASSWORD: Option<&str> = option_env!("ON_AIR_WIFI_PASSWORD");
const SPOTIFY_CLIENT_ID: Option<&str> = option_env!("SPOTIFY_CLIENT_ID");
/// A refresh token from `.env` or the simulator's sign-in, tried after a
/// saved one.
pub const SPOTIFY_REFRESH_TOKEN: Option<&str> = option_env!("ON_AIR_SPOTIFY_REFRESH_TOKEN");
const TIMEZONE: Option<&str> = option_env!("ON_AIR_TIMEZONE");
const CLOCK_24H: Option<&str> = option_env!("ON_AIR_CLOCK_24H");
/// The weather city from `.env`. The weather task looks it up and saves its
/// position.
pub const CITY: Option<&str> = option_env!("ON_AIR_CITY");
const FAHRENHEIT: Option<&str> = option_env!("ON_AIR_FAHRENHEIT");
const IDLE_SCREEN: Option<&str> = option_env!("ON_AIR_IDLE_SCREEN");
/// The HTTPS relay page for Spotify sign-in (see `tools/spotify-relay`).
/// Required: `build.rs` stops the build without it.
pub const SPOTIFY_RELAY: &str = env!("ON_AIR_SPOTIFY_RELAY");

fn set(value: Option<&'static str>) -> Option<&'static str> {
    value.map(str::trim).filter(|v| !v.is_empty())
}

fn yes(value: Option<&str>) -> bool {
    matches!(value.map(str::trim), Some("true" | "1"))
}

/// The settings to run with, and where they came from.
#[derive(Debug, Clone)]
pub struct Effective {
    pub settings: Settings,
    /// The setup portal saved them, so `.env` isn't used.
    pub from_portal: bool,
}

impl Effective {
    /// The city to look up because the settings have none: `.env`'s, in
    /// development.
    pub fn city_to_look_up(&self) -> Option<&'static str> {
        if self.settings.weather.is_some() || self.from_portal {
            return None;
        }
        set(CITY)
    }
}

/// The settings to run with, from what's saved.
pub fn effective(saved: Settings) -> Effective {
    // The portal always saves a network, so that marks its settings.
    if saved.has_wifi() {
        return Effective {
            settings: saved,
            from_portal: true,
        };
    }
    let mut settings = saved;
    if let Some(ssid) = set(WIFI_SSID) {
        settings.wifi_ssid = ssid.into();
        settings.wifi_pass = WIFI_PASSWORD.unwrap_or("").into();
    }
    if let Some(client_id) = set(SPOTIFY_CLIENT_ID)
        && settings.spotify_client_id != client_id
    {
        // A saved token belongs to the client ID it was issued for.
        settings.spotify_client_id = client_id.into();
        settings.spotify_refresh_token = None;
    }
    if let Some(timezone) = set(TIMEZONE) {
        settings.timezone = timezone.into();
    }
    settings.clock_24h = yes(CLOCK_24H);
    settings.units = if yes(FAHRENHEIT) {
        Units::Fahrenheit
    } else {
        Units::Celsius
    };
    if let Some(mode) = set(IDLE_SCREEN).and_then(IdleMode::from_name) {
        settings.idle_mode = mode;
    }
    // The weather task looks up `.env`'s city when it isn't the saved one.
    if settings
        .weather
        .as_ref()
        .is_some_and(|w| Some(w.city.as_str()) != set(CITY))
    {
        settings.weather = None;
    }
    Effective {
        settings,
        from_portal: false,
    }
}
