//! The setup portal: the page the owner's phone opens on the setup network,
//! and the logic behind it.
//!
//! The HTTP server differs between the device (`picoserve`) and the
//! simulator (`tiny_http`), so each host routes requests itself. Everything
//! else lives here: the page, form checking, the Spotify paste-back sign-in
//! and the JSON replies.
//!
//! Routes, the same on every host:
//!
//! | Request | Reply |
//! |---|---|
//! | `GET /` | [`PAGE`] |
//! | `GET` a captive-portal probe ([`is_captive_probe`]) | `302` to `/` |
//! | `GET /api/status` | [`Status`] |
//! | `GET /api/networks` | [`networks_json`] |
//! | `GET /api/places?q=<city>` | [`places_json`] |
//! | `POST /api/wifi` (form) | [`parse_wifi`], then join the network |
//! | `POST /api/spotify/start` (form) | [`SignIn::new`], reply `{"ok":true,"authorize_url":…}` |
//! | `POST /api/spotify/finish` (form) | [`SignIn::code_from`], exchange the code, save the refresh token |
//! | `POST /api/display` (form) | [`parse_display`], save |
//! | `POST /api/done` | save everything and restart |

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use serde::Serialize;

use crate::auth::{self, CallbackError, Pkce};
use crate::settings::{Settings, WeatherLocation};
use crate::timezone;
use crate::weather::{Place, Units};

/// The whole portal page: HTML with inline CSS and JavaScript.
pub const PAGE: &str = include_str!("../assets/portal.html");

/// Paths phones and laptops request to check for a captive portal. Answering
/// them with a redirect makes the phone open the portal page by itself.
pub fn is_captive_probe(path: &str) -> bool {
    let path = path.split('?').next().unwrap_or("");
    matches!(
        path,
        "/generate_204"
            | "/gen_204"
            | "/hotspot-detect.html"
            | "/library/test/success.html"
            | "/connecttest.txt"
            | "/ncsi.txt"
            | "/success.txt"
            | "/canonical.html"
            | "/redirect"
    )
}

/// Form fields that failed, keyed by field name, with a message for the
/// owner.
pub type FieldErrors = BTreeMap<&'static str, &'static str>;

fn form_fields(body: &str) -> BTreeMap<String, String> {
    form_urlencoded::parse(body.as_bytes())
        .map(|(k, v)| (k.into_owned(), v.trim().to_string()))
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WifiForm {
    pub ssid: String,
    /// Empty for an open network.
    pub password: String,
}

/// Read and check the WiFi form: fields `ssid` and `password`.
pub fn parse_wifi(body: &str) -> Result<WifiForm, FieldErrors> {
    let fields = form_fields(body);
    let ssid = fields.get("ssid").cloned().unwrap_or_default();
    // Passwords may legitimately start or end with spaces, so read it raw.
    let password = form_urlencoded::parse(body.as_bytes())
        .find(|(k, _)| k == "password")
        .map(|(_, v)| v.into_owned())
        .unwrap_or_default();

    let mut errors = FieldErrors::new();
    if ssid.is_empty() {
        errors.insert("ssid", "Pick or type a network name.");
    } else if ssid.len() > 32 {
        errors.insert("ssid", "Network names are at most 32 characters.");
    }
    let hex_key = password.len() == 64 && password.bytes().all(|b| b.is_ascii_hexdigit());
    if !password.is_empty() && !hex_key && !(8..=63).contains(&password.len()) {
        errors.insert("password", "WiFi passwords are 8 to 63 characters.");
    }
    if errors.is_empty() {
        Ok(WifiForm { ssid, password })
    } else {
        Err(errors)
    }
}

/// Check a Spotify Client ID: 32 hex characters. Returns it in lowercase.
pub fn parse_client_id(value: &str) -> Result<String, FieldErrors> {
    let id = value.trim().to_ascii_lowercase();
    if id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(id)
    } else {
        let mut errors = FieldErrors::new();
        errors.insert(
            "client_id",
            "A Client ID is 32 letters and numbers. Copy it from your Spotify app's settings.",
        );
        Err(errors)
    }
}

/// A Spotify sign-in in progress: the PKCE verifier and `state` stay on the
/// device between "Connect Spotify" and the pasted callback URL.
#[derive(Debug, Clone)]
pub struct SignIn {
    pub client_id: String,
    pkce: Pkce,
    state: String,
}

impl SignIn {
    /// Start a sign-in. `random` and `state_random` come from the hardware
    /// random number generator.
    pub fn new(
        client_id: &str,
        random: &[u8; 32],
        state_random: &[u8; 16],
    ) -> Result<Self, FieldErrors> {
        Ok(Self {
            client_id: parse_client_id(client_id)?,
            pkce: Pkce::from_random(random),
            state: auth::state_from_random(state_random),
        })
    }

    /// The page the **Connect Spotify** link opens.
    pub fn authorize_url(&self) -> String {
        auth::authorize_url(&self.client_id, &self.pkce.challenge, &self.state)
    }

    /// Read the authorization code from the URL the owner pasted.
    pub fn code_from(&self, pasted: &str) -> Result<String, CallbackError> {
        auth::parse_callback(pasted, &self.state)
    }

    /// Form body for exchanging the code for tokens.
    pub fn exchange_body(&self, code: &str) -> String {
        auth::code_exchange_body(&self.client_id, code, &self.pkce.verifier)
    }
}

/// A message for the owner when the pasted URL is wrong.
pub fn callback_message(error: &CallbackError) -> &'static str {
    match error {
        CallbackError::NotACallback => {
            "That doesn't look like the right address. Copy the whole address of the page Spotify sent you to (it starts with http://127.0.0.1:8888/callback)."
        }
        CallbackError::Denied(_) => {
            "Spotify sign-in was cancelled. Tap Connect Spotify to try again."
        }
        CallbackError::StateMismatch => {
            "That address is from an older sign-in. Tap Connect Spotify again and paste the new address."
        }
    }
}

/// The city, which sets the clock's timezone and the weather location.
#[derive(Debug, Clone, PartialEq)]
pub enum PlaceChoice {
    /// Leave the saved city as it is.
    Keep,
    /// A city picked from the search, with its IANA timezone.
    Set(WeatherLocation, String),
}

/// The display settings form, checked.
#[derive(Debug, Clone, PartialEq)]
pub struct DisplayForm {
    pub place: PlaceChoice,
    pub units: Units,
    pub clock_24h: bool,
}

impl DisplayForm {
    pub fn apply(&self, settings: &mut Settings) {
        match &self.place {
            PlaceChoice::Keep => {}
            PlaceChoice::Set(location, zone) => {
                settings.weather = Some(location.clone());
                settings.timezone = zone.clone();
            }
        }
        settings.units = self.units;
        settings.clock_24h = self.clock_24h;
    }
}

/// Read and check the display form. Fields: `place` (`keep`, or `set` with
/// `city`, `lat`, `lon` and `timezone` from the search), `units` (`c` or
/// `f`) and `clock` (`12` or `24`).
pub fn parse_display(body: &str) -> Result<DisplayForm, FieldErrors> {
    let fields = form_fields(body);
    let get = |k: &str| fields.get(k).map(String::as_str).unwrap_or("");
    let mut errors = FieldErrors::new();

    let place = match get("place") {
        "keep" | "" => PlaceChoice::Keep,
        _ => {
            let lat = get("lat")
                .parse::<f32>()
                .ok()
                .filter(|v| (-90.0..=90.0).contains(v));
            let lon = get("lon")
                .parse::<f32>()
                .ok()
                .filter(|v| (-180.0..=180.0).contains(v));
            let zone = get("timezone");
            match (get("city"), lat, lon, timezone::find(zone)) {
                (city, Some(lat), Some(lon), Some(_)) if !city.is_empty() => PlaceChoice::Set(
                    WeatherLocation {
                        city: city.to_string(),
                        lat,
                        lon,
                    },
                    zone.to_string(),
                ),
                _ => {
                    errors.insert("place", "Search for your city and pick it from the list.");
                    PlaceChoice::Keep
                }
            }
        }
    };

    let units = match get("units") {
        "f" => Units::Fahrenheit,
        "c" | "" => Units::Celsius,
        _ => {
            errors.insert("units", "Pick °C or °F.");
            Units::Celsius
        }
    };
    let clock_24h = match get("clock") {
        "24" => true,
        "12" | "" => false,
        _ => {
            errors.insert("clock", "Pick a 12-hour or 24-hour clock.");
            false
        }
    };

    if errors.is_empty() {
        Ok(DisplayForm {
            place,
            units,
            clock_24h,
        })
    } else {
        Err(errors)
    }
}

/// Where joining the home WiFi has got to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WifiState {
    NotSet,
    Connecting,
    Connected,
    Failed,
}

/// `GET /api/status`: progress through setup, and saved values to fill the
/// form with. Never includes the WiFi password or Spotify tokens.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Status {
    pub network_name: String,
    pub wifi: WifiState,
    pub wifi_ssid: String,
    pub spotify_client_id: String,
    pub spotify_signed_in: bool,
    pub city: String,
    pub units: &'static str,
    pub clock: &'static str,
}

impl Status {
    pub fn new(settings: &Settings, network_name: &str, wifi: WifiState) -> Self {
        Self {
            network_name: network_name.to_string(),
            wifi,
            wifi_ssid: settings.wifi_ssid.clone(),
            spotify_client_id: settings.spotify_client_id.clone(),
            spotify_signed_in: settings.spotify_refresh_token.is_some(),
            city: settings
                .weather
                .as_ref()
                .map(|w| w.city.clone())
                .unwrap_or_default(),
            units: match settings.units {
                Units::Celsius => "c",
                Units::Fahrenheit => "f",
            },
            clock: if settings.clock_24h { "24" } else { "12" },
        }
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

/// A network from a WiFi scan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Network {
    pub ssid: String,
    /// Signal strength, 0–100.
    pub signal: u8,
    pub secure: bool,
}

/// `GET /api/networks`: strongest first, one entry per name, hidden networks
/// left out.
pub fn networks_json(networks: &[Network]) -> String {
    let mut list: Vec<&Network> = networks.iter().filter(|n| !n.ssid.is_empty()).collect();
    list.sort_by(|a, b| b.signal.cmp(&a.signal));
    let mut seen: Vec<&str> = Vec::new();
    list.retain(|n| {
        let new = !seen.contains(&n.ssid.as_str());
        seen.push(&n.ssid);
        new
    });
    serde_json::to_string(&list).unwrap_or_default()
}

/// Convert a WiFi signal level in dBm to 0–100.
pub fn signal_from_dbm(dbm: i8) -> u8 {
    ((dbm as i16 + 100).clamp(0, 50) * 2) as u8
}

#[derive(Serialize)]
struct PlaceEntry<'a> {
    /// e.g. "Portland, Oregon, United States"
    label: String,
    city: &'a str,
    lat: f32,
    lon: f32,
    timezone: &'a str,
}

/// `GET /api/places`: geocoding matches the page can offer. Places without
/// a timezone are left out, since the clock needs one.
pub fn places_json(places: &[Place]) -> String {
    let entries: Vec<PlaceEntry> = places
        .iter()
        .filter_map(|p| {
            let zone = p.timezone.as_deref()?;
            let mut label = p.name.clone();
            for part in [&p.region, &p.country].into_iter().flatten() {
                label.push_str(", ");
                label.push_str(part);
            }
            Some(PlaceEntry {
                label,
                city: &p.name,
                lat: p.lat,
                lon: p.lon,
                timezone: zone,
            })
        })
        .collect();
    serde_json::to_string(&entries).unwrap_or_default()
}

/// Why setup can't finish yet, if anything is missing.
pub fn missing_before_done(settings: &Settings) -> Option<&'static str> {
    if !settings.has_wifi() {
        Some("Connect to your WiFi first (step 1).")
    } else if settings.spotify_client_id.is_empty() || settings.spotify_refresh_token.is_none() {
        Some("Sign in to Spotify first (step 2).")
    } else if settings.weather.is_none() {
        Some("Pick your city first (step 3).")
    } else {
        None
    }
}

/// `{"ok":true}`.
pub fn ok_json() -> String {
    String::from(r#"{"ok":true}"#)
}

/// `{"ok":false,"errors":{field: message}}`, for a form with bad fields.
pub fn errors_json(errors: &FieldErrors) -> String {
    #[derive(Serialize)]
    struct Reply<'a> {
        ok: bool,
        errors: &'a FieldErrors,
    }
    serde_json::to_string(&Reply { ok: false, errors }).unwrap_or_default()
}

/// `{"ok":false,"message":…}`, for anything else that went wrong.
pub fn message_json(message: &str) -> String {
    #[derive(Serialize)]
    struct Reply<'a> {
        ok: bool,
        message: &'a str,
    }
    serde_json::to_string(&Reply { ok: false, message }).unwrap_or_default()
}

/// `{"ok":true,"authorize_url":…}`, the reply to starting a sign-in.
pub fn authorize_json(url: &str) -> String {
    #[derive(Serialize)]
    struct Reply<'a> {
        ok: bool,
        authorize_url: &'a str,
    }
    serde_json::to_string(&Reply {
        ok: true,
        authorize_url: url,
    })
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captive_probes_are_recognised() {
        for path in [
            "/generate_204",
            "/hotspot-detect.html",
            "/connecttest.txt",
            "/ncsi.txt",
            "/generate_204?x=1",
        ] {
            assert!(is_captive_probe(path), "{path}");
        }
        for path in ["/", "/api/status", "/favicon.ico"] {
            assert!(!is_captive_probe(path), "{path}");
        }
    }

    #[test]
    fn wifi_form() {
        assert_eq!(
            parse_wifi("ssid=Home+WiFi&password=hunter22"),
            Ok(WifiForm {
                ssid: "Home WiFi".into(),
                password: "hunter22".into()
            })
        );
        // Open network, and a password with spaces kept as typed.
        assert_eq!(parse_wifi("ssid=Cafe&password=").unwrap().password, "");
        assert_eq!(
            parse_wifi("ssid=Home&password=+spaces+ok+")
                .unwrap()
                .password,
            " spaces ok "
        );
        // 64 hex characters is a raw key.
        let key = "a".repeat(64);
        assert!(parse_wifi(&alloc::format!("ssid=Home&password={key}")).is_ok());

        let errors = parse_wifi("ssid=&password=short").unwrap_err();
        assert!(errors.contains_key("ssid") && errors.contains_key("password"));
        let long_name = "x".repeat(33);
        assert!(
            parse_wifi(&alloc::format!("ssid={long_name}"))
                .unwrap_err()
                .contains_key("ssid")
        );
    }

    #[test]
    fn client_id() {
        let id = "0123456789ABCDEF0123456789abcdef";
        assert_eq!(
            parse_client_id(&alloc::format!(" {id} ")).unwrap(),
            id.to_ascii_lowercase()
        );
        assert!(parse_client_id("too-short").is_err());
        assert!(parse_client_id(&"g".repeat(32)).is_err());
    }

    #[test]
    fn sign_in_round_trip() {
        let sign_in = SignIn::new("0123456789abcdef0123456789abcdef", &[7; 32], &[9; 16]).unwrap();
        let url = sign_in.authorize_url();
        assert!(url.contains("client_id=0123456789abcdef0123456789abcdef"));

        // The callback carries back the same state the URL sent out.
        let state = url.split("state=").nth(1).unwrap();
        let pasted = alloc::format!("http://127.0.0.1:8888/callback?code=abc&state={state}");
        let code = sign_in.code_from(&pasted).unwrap();
        assert_eq!(code, "abc");
        assert!(sign_in.exchange_body(&code).contains("code_verifier="));

        let wrong = sign_in
            .code_from("http://127.0.0.1:8888/callback?code=abc&state=other")
            .unwrap_err();
        assert!(callback_message(&wrong).contains("older sign-in"));
        assert!(SignIn::new("nope", &[0; 32], &[0; 16]).is_err());
    }

    #[test]
    fn display_form_with_city() {
        let form = parse_display(
            "place=set&city=Portland&lat=45.52&lon=-122.68&timezone=America%2FLos_Angeles&units=f&clock=24",
        )
        .unwrap();
        let mut settings = Settings::default();
        form.apply(&mut settings);
        assert_eq!(settings.weather.as_ref().unwrap().city, "Portland");
        assert_eq!(settings.timezone, "America/Los_Angeles");
        assert_eq!(settings.units, Units::Fahrenheit);
        assert!(settings.clock_24h);
    }

    #[test]
    fn display_form_keeps_the_saved_city() {
        let mut settings = Settings {
            timezone: "Europe/Berlin".into(),
            weather: Some(WeatherLocation {
                city: "Berlin".into(),
                lat: 52.5,
                lon: 13.4,
            }),
            ..Settings::default()
        };
        let keep = parse_display("place=keep&units=c&clock=12").unwrap();
        assert_eq!(keep.place, PlaceChoice::Keep);
        assert!(!keep.clock_24h);
        keep.apply(&mut settings);
        assert_eq!(settings.weather.as_ref().unwrap().city, "Berlin");
        assert_eq!(settings.timezone, "Europe/Berlin");
    }

    #[test]
    fn display_form_errors() {
        let errors =
            parse_display("place=set&city=Portland&lat=91&lon=0&timezone=Nowhere&units=k&clock=13")
                .unwrap_err();
        assert_eq!(errors.len(), 3);
        assert!(
            errors.contains_key("place")
                && errors.contains_key("units")
                && errors.contains_key("clock")
        );
    }

    #[test]
    fn status_never_leaks_secrets() {
        let settings = Settings {
            wifi_ssid: "Home".into(),
            wifi_pass: "secret-password".into(),
            spotify_refresh_token: Some("secret-token".into()),
            ..Settings::default()
        };
        let json = Status::new(&settings, "OnAir-Setup-1A2B", WifiState::Connected).to_json();
        assert!(!json.contains("secret"));
        assert!(json.contains(r#""wifi":"connected""#));
        assert!(json.contains(r#""spotify_signed_in":true"#));
    }

    #[test]
    fn networks_sorted_and_deduplicated() {
        let n = |ssid: &str, signal| Network {
            ssid: ssid.into(),
            signal,
            secure: true,
        };
        let json = networks_json(&[n("B", 40), n("A", 90), n("", 99), n("B", 70)]);
        assert_eq!(
            json,
            r#"[{"ssid":"A","signal":90,"secure":true},{"ssid":"B","signal":70,"secure":true}]"#
        );
        assert_eq!(signal_from_dbm(-50), 100);
        assert_eq!(signal_from_dbm(-75), 50);
        assert_eq!(signal_from_dbm(-100), 0);
    }

    #[test]
    fn places_have_labels_and_need_a_timezone() {
        let places = crate::weather::parse_geocoding(include_bytes!(
            "../tests/fixtures/openmeteo_geocode_portland.json"
        ))
        .unwrap();
        let json = places_json(&places);
        assert!(
            json.starts_with(r#"[{"label":"Portland, Oregon, United States","city":"Portland""#),
            "{json}"
        );
        let no_zone = Place {
            name: "X".into(),
            region: None,
            country: None,
            lat: 0.0,
            lon: 0.0,
            timezone: None,
        };
        assert_eq!(places_json(&[no_zone]), "[]");
    }

    #[test]
    fn done_needs_wifi_spotify_and_city() {
        let mut settings = Settings::default();
        assert!(missing_before_done(&settings).unwrap().contains("WiFi"));
        settings.wifi_ssid = "Home".into();
        assert!(missing_before_done(&settings).unwrap().contains("Spotify"));
        settings.spotify_client_id = "cid".into();
        settings.spotify_refresh_token = Some("rt".into());
        assert!(missing_before_done(&settings).unwrap().contains("city"));
        settings.weather = Some(WeatherLocation {
            city: "Portland".into(),
            lat: 45.5,
            lon: -122.7,
        });
        assert_eq!(missing_before_done(&settings), None);
    }

    #[test]
    fn json_replies() {
        let mut errors = FieldErrors::new();
        errors.insert("ssid", "Pick a network.");
        assert_eq!(
            errors_json(&errors),
            r#"{"ok":false,"errors":{"ssid":"Pick a network."}}"#
        );
        assert_eq!(message_json("nope"), r#"{"ok":false,"message":"nope"}"#);
        assert_eq!(
            authorize_json("https://x"),
            r#"{"ok":true,"authorize_url":"https://x"}"#
        );
        assert!(PAGE.contains("<title>On-Air Setup</title>"));
    }
}
