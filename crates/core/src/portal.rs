//! The setup portal: the page the owner's phone opens on the setup network,
//! and the logic behind it.
//!
//! [`Portal`] handles every request without doing any I/O. The host's web
//! server (`picoserve` on the device, `tiny_http` in the simulator) passes
//! each request in and sends back the [`Reply`]. When a request needs the
//! internet (city search, the Spotify token exchange), [`Portal::handle`]
//! returns [`Handled::Fetch`]; the host sends that request and passes the
//! outcome to [`Portal::finish`] for the reply. [`Event`]s tell the host
//! what to act on: join a network, save settings, restart.
//!
//! Routes:
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
//! | `POST /api/display` (form) | [`parse_display`], check a new city for tide data, save, reply [`display_saved_json`] |
//! | `POST /api/done` | save everything and restart |

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use serde::Serialize;

use crate::auth::{self, CallbackError, Pkce, TokenError};
use crate::http::{self, Method, NetworkError, Outcome};
use crate::settings::{IDLE_AFTER_MIN_RANGE, Settings, WeatherLocation};
use crate::tide::{self, TideSupport};
use crate::timezone;
use crate::weather;
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
    /// Minutes before the idle screen shows. `None` leaves it as it is.
    pub idle_after_min: Option<u8>,
}

impl DisplayForm {
    pub fn apply(&self, settings: &mut Settings) {
        match &self.place {
            PlaceChoice::Keep => {}
            PlaceChoice::Set(location, zone) => {
                if settings.weather.as_ref() != Some(location) {
                    // A new city needs its own tide check.
                    settings.tides = TideSupport::Unknown;
                }
                settings.weather = Some(location.clone());
                settings.timezone = zone.clone();
            }
        }
        settings.units = self.units;
        settings.clock_24h = self.clock_24h;
        if let Some(minutes) = self.idle_after_min {
            settings.idle_after_min = minutes;
        }
    }
}

/// Read and check the display form. Fields: `place` (`keep`, or `set` with
/// `city`, `lat`, `lon` and `timezone` from the search), `units` (`c` or
/// `f`), `clock` (`12` or `24`) and `idle_minutes` (1–30).
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

    let idle_after_min = match get("idle_minutes") {
        "" => None,
        value => match value.parse::<u8>() {
            Ok(m) if IDLE_AFTER_MIN_RANGE.contains(&m) => Some(m),
            _ => {
                errors.insert("idle_minutes", "Pick between 1 and 30 minutes.");
                None
            }
        },
    };

    if errors.is_empty() {
        Ok(DisplayForm {
            place,
            units,
            clock_24h,
            idle_after_min,
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
    pub idle_minutes: u8,
    /// Whether the city has tide data: `yes`, `no` or `unknown`.
    pub tides: &'static str,
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
            idle_minutes: settings.idle_after_min,
            tides: tides_name(settings.tides),
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
    list.sort_by_key(|n| core::cmp::Reverse(n.signal));
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

fn tides_name(tides: TideSupport) -> &'static str {
    match tides {
        TideSupport::Available => "yes",
        TideSupport::Unavailable => "no",
        TideSupport::Unknown => "unknown",
    }
}

/// `{"ok":true,"tides":…}`, the reply to saving the display settings, so
/// the page can say whether the tide screen is on offer.
pub fn display_saved_json(tides: TideSupport) -> String {
    #[derive(Serialize)]
    struct Reply {
        ok: bool,
        tides: &'static str,
    }
    serde_json::to_string(&Reply {
        ok: true,
        tides: tides_name(tides),
    })
    .unwrap_or_default()
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

/// A reply for the host's web server to send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    /// [`PAGE`], as HTML.
    Page,
    Json(String),
    /// `302` to `/`, for captive-portal probes.
    Redirect,
    NotFound,
}

impl Reply {
    pub fn status(&self) -> u16 {
        match self {
            Reply::Page | Reply::Json(_) => 200,
            Reply::Redirect => 302,
            Reply::NotFound => 404,
        }
    }

    pub fn content_type(&self) -> &'static str {
        match self {
            Reply::Page => "text/html; charset=utf-8",
            Reply::Json(_) => "application/json",
            Reply::Redirect | Reply::NotFound => "text/plain",
        }
    }

    pub fn body(&self) -> &str {
        match self {
            Reply::Page => PAGE,
            Reply::Json(json) => json,
            Reply::Redirect => "",
            Reply::NotFound => "Not found",
        }
    }
}

/// What [`Portal::handle`] needs from the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Handled {
    Reply(Reply),
    /// Send this request, then pass the outcome to [`Portal::finish`] for
    /// the reply.
    Fetch(http::Request),
}

/// Something the host has to act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// Start joining this network, then report back with [`Portal::set_wifi`].
    JoinWifi { ssid: String, password: String },
    /// Spotify sign-in finished; the Client ID and refresh token are in
    /// [`Portal::settings`]. Save them.
    SignedIn,
    /// Clock and weather settings changed. Save and apply them.
    SettingsChanged,
    /// The owner pressed Finish setup. Save everything and restart.
    Done,
}

enum Pending {
    Places,
    TokenExchange(SignIn),
    /// Display settings with a new city, waiting on its tide check.
    Display(DisplayForm),
}

/// The setup portal's state: settings being edited, WiFi progress and any
/// Spotify sign-in in progress.
pub struct Portal {
    settings: Settings,
    network_name: String,
    wifi: WifiState,
    networks: Vec<Network>,
    sign_in: Option<SignIn>,
    pending: Option<Pending>,
}

impl Portal {
    pub fn new(settings: Settings, network_name: &str) -> Self {
        Self {
            settings,
            network_name: network_name.into(),
            wifi: WifiState::NotSet,
            networks: Vec::new(),
            sign_in: None,
            pending: None,
        }
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// Networks from the latest WiFi scan, for the network list.
    pub fn set_networks(&mut self, networks: Vec<Network>) {
        self.networks = networks;
    }

    /// Progress joining the home WiFi.
    pub fn set_wifi(&mut self, state: WifiState) {
        self.wifi = state;
    }

    /// Handle one request. `target` is the path with any query string.
    /// `random` fills a buffer from the hardware random number generator.
    pub fn handle(
        &mut self,
        method: Method,
        target: &str,
        body: &str,
        random: &mut impl FnMut(&mut [u8]),
        events: &mut impl FnMut(Event),
    ) -> Handled {
        let (path, query) = target.split_once('?').unwrap_or((target, ""));
        let json = |s: String| Handled::Reply(Reply::Json(s));
        match (method, path) {
            (Method::Get, "/") => Handled::Reply(Reply::Page),
            (Method::Get, p) if is_captive_probe(p) => Handled::Reply(Reply::Redirect),
            (Method::Get, "/api/status") => {
                json(Status::new(&self.settings, &self.network_name, self.wifi).to_json())
            }
            (Method::Get, "/api/networks") => json(networks_json(&self.networks)),
            (Method::Get, "/api/places") => {
                let city = form_fields(query).remove("q").unwrap_or_default();
                if city.is_empty() {
                    return json(String::from("[]"));
                }
                self.pending = Some(Pending::Places);
                Handled::Fetch(http::Request::get(weather::geocoding_url(&city, 5)))
            }
            (Method::Post, "/api/wifi") => match parse_wifi(body) {
                Ok(form) => {
                    self.settings.wifi_ssid = form.ssid.clone();
                    self.settings.wifi_pass = form.password.clone();
                    self.wifi = WifiState::Connecting;
                    events(Event::JoinWifi {
                        ssid: form.ssid,
                        password: form.password,
                    });
                    json(ok_json())
                }
                Err(errors) => json(errors_json(&errors)),
            },
            (Method::Post, "/api/spotify/start") => {
                let client_id = form_fields(body).remove("client_id").unwrap_or_default();
                let mut random_bytes = [0u8; 48];
                random(&mut random_bytes);
                let (verifier_random, state_random) = random_bytes.split_at(32);
                let verifier_random: [u8; 32] = verifier_random.try_into().unwrap_or([0; 32]);
                let state_random: [u8; 16] = state_random.try_into().unwrap_or([0; 16]);
                match SignIn::new(&client_id, &verifier_random, &state_random) {
                    Ok(sign_in) => {
                        let url = sign_in.authorize_url();
                        self.sign_in = Some(sign_in);
                        json(authorize_json(&url))
                    }
                    Err(errors) => json(errors_json(&errors)),
                }
            }
            (Method::Post, "/api/spotify/finish") => {
                let pasted = form_fields(body).remove("callback_url").unwrap_or_default();
                let Some(sign_in) = self.sign_in.clone() else {
                    return json(message_json("Tap Connect Spotify first."));
                };
                match sign_in.code_from(&pasted) {
                    Ok(code) => {
                        let request = http::Request {
                            method: Method::Post,
                            url: auth::TOKEN_URL.into(),
                            bearer: None,
                            form_body: Some(sign_in.exchange_body(&code)),
                        };
                        self.pending = Some(Pending::TokenExchange(sign_in));
                        Handled::Fetch(request)
                    }
                    Err(e) => json(message_json(callback_message(&e))),
                }
            }
            (Method::Post, "/api/display") => match parse_display(body) {
                Ok(form) => match &form.place {
                    // Check a new city for tide data before saving, so the
                    // page can say whether the tide screen is on offer.
                    PlaceChoice::Set(location, _)
                        if self.settings.weather.as_ref() != Some(location)
                            || self.settings.tides == TideSupport::Unknown =>
                    {
                        let url = tide::tide_url(location.lat, location.lon);
                        self.pending = Some(Pending::Display(form));
                        Handled::Fetch(http::Request::get(url))
                    }
                    _ => {
                        form.apply(&mut self.settings);
                        events(Event::SettingsChanged);
                        json(display_saved_json(self.settings.tides))
                    }
                },
                Err(errors) => json(errors_json(&errors)),
            },
            (Method::Post, "/api/done") => {
                if self.wifi != WifiState::Connected {
                    return json(message_json("Connect to your WiFi first (step 1)."));
                }
                match missing_before_done(&self.settings) {
                    Some(message) => json(message_json(message)),
                    None => {
                        events(Event::Done);
                        json(ok_json())
                    }
                }
            }
            _ => Handled::Reply(Reply::NotFound),
        }
    }

    /// The reply for a request that needed a [`Handled::Fetch`].
    pub fn finish(&mut self, outcome: Outcome, events: &mut impl FnMut(Event)) -> Reply {
        let reply = match self.pending.take() {
            Some(Pending::Places) => match outcome {
                Ok(r) if r.status == 200 => match weather::parse_geocoding(&r.body) {
                    Ok(places) => places_json(&places),
                    Err(_) => message_json("Couldn't search for cities right now. Try again."),
                },
                _ => message_json("Couldn't search for cities right now. Try again."),
            },
            Some(Pending::TokenExchange(sign_in)) => self.finish_sign_in(sign_in, outcome, events),
            Some(Pending::Display(form)) => {
                form.apply(&mut self.settings);
                // A failed check stays unknown and is tried again later.
                self.settings.tides = match outcome {
                    Ok(r) if r.status == 200 => tide::support_from(&r.body),
                    _ => TideSupport::Unknown,
                };
                events(Event::SettingsChanged);
                display_saved_json(self.settings.tides)
            }
            None => message_json("Something went wrong. Try again."),
        };
        Reply::Json(reply)
    }

    fn finish_sign_in(
        &mut self,
        sign_in: SignIn,
        outcome: Outcome,
        events: &mut impl FnMut(Event),
    ) -> String {
        let response = match outcome {
            Ok(response) => response,
            Err(NetworkError) => {
                return message_json("Couldn't reach Spotify. Check the WiFi and try again.");
            }
        };
        match auth::parse_token_response(response.status, &response.body) {
            Ok(tokens) => match tokens.refresh_token {
                Some(refresh_token) => {
                    self.settings.spotify_client_id = sign_in.client_id;
                    self.settings.spotify_refresh_token = Some(refresh_token);
                    self.sign_in = None;
                    events(Event::SignedIn);
                    ok_json()
                }
                None => message_json(
                    "Spotify didn't send everything needed. Tap Connect Spotify to try again.",
                ),
            },
            Err(TokenError::InvalidGrant) => message_json(
                "That sign-in has expired or was already used. Tap Connect Spotify to try again.",
            ),
            Err(TokenError::InvalidClient) => message_json(
                "Spotify doesn't recognise that Client ID. Check it in your Spotify app's settings.",
            ),
            Err(_) => message_json("Spotify sign-in failed. Tap Connect Spotify to try again."),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::Response;
    use rstest::rstest;

    fn random(buffer: &mut [u8]) {
        buffer.fill(7);
    }

    /// Run one request through the portal, answering any fetch with `fetched`.
    fn request(
        portal: &mut Portal,
        method: Method,
        target: &str,
        body: &str,
        fetched: Option<Outcome>,
        events: &mut Vec<Event>,
    ) -> Reply {
        let mut push = |e| events.push(e);
        match portal.handle(method, target, body, &mut random, &mut push) {
            Handled::Reply(reply) => reply,
            Handled::Fetch(_) => portal.finish(fetched.expect("a fetch outcome"), &mut push),
        }
    }

    #[test]
    fn full_setup_flow() {
        let mut portal = Portal::new(Settings::default(), "OnAir-1A2B");
        let mut events = Vec::new();

        assert_eq!(
            request(&mut portal, Method::Get, "/", "", None, &mut events),
            Reply::Page
        );
        assert_eq!(
            request(
                &mut portal,
                Method::Get,
                "/hotspot-detect.html",
                "",
                None,
                &mut events
            ),
            Reply::Redirect
        );

        // WiFi.
        let reply = request(
            &mut portal,
            Method::Post,
            "/api/wifi",
            "ssid=Home&password=hunter22",
            None,
            &mut events,
        );
        assert_eq!(reply, Reply::Json(ok_json()));
        assert_eq!(
            events,
            [Event::JoinWifi {
                ssid: "Home".into(),
                password: "hunter22".into()
            }]
        );
        portal.set_wifi(WifiState::Connected);

        // Spotify: start, then paste back.
        let Reply::Json(start) = request(
            &mut portal,
            Method::Post,
            "/api/spotify/start",
            "client_id=0123456789abcdef0123456789abcdef",
            None,
            &mut events,
        ) else {
            panic!()
        };
        let state = start
            .split("state=")
            .nth(1)
            .unwrap()
            .trim_end_matches("\"}");
        let callback = alloc::format!(
            "callback_url=http%3A%2F%2F127.0.0.1%3A8888%2Fcallback%3Fcode%3Dabc%26state%3D{state}"
        );
        let mut handled = Vec::new();
        match portal.handle(
            Method::Post,
            "/api/spotify/finish",
            &callback,
            &mut random,
            &mut |e| handled.push(e),
        ) {
            Handled::Fetch(token_request) => {
                assert_eq!(token_request.url, auth::TOKEN_URL);
                assert!(token_request.form_body.unwrap().contains("code=abc"));
            }
            other => panic!("{other:?}"),
        }
        let token_ok = include_bytes!("../tests/fixtures/spotify_token_ok.json");
        let reply = portal.finish(Ok(Response::new(200, &token_ok[..])), &mut |e| {
            events.push(e)
        });
        assert_eq!(reply, Reply::Json(ok_json()));
        assert_eq!(
            portal.settings().spotify_refresh_token.as_deref(),
            Some("AQC-new-refresh-token")
        );

        // Finish is refused until there's a city.
        let Reply::Json(refused) = request(
            &mut portal,
            Method::Post,
            "/api/done",
            "",
            None,
            &mut events,
        ) else {
            panic!()
        };
        assert!(refused.contains("city"));

        // City search goes out to the network.
        let geocode = include_bytes!("../tests/fixtures/openmeteo_geocode_portland.json");
        let Reply::Json(places) = request(
            &mut portal,
            Method::Get,
            "/api/places?q=Portland",
            "",
            Some(Ok(Response::new(200, &geocode[..]))),
            &mut events,
        ) else {
            panic!()
        };
        assert!(places.contains("Portland, Oregon"));

        let display = "place=set&city=Portland&lat=45.52&lon=-122.68&timezone=America%2FLos_Angeles&units=f&clock=12&idle_minutes=5";
        let inland = include_bytes!("../tests/fixtures/openmeteo_marine_denver.json");
        assert_eq!(
            request(
                &mut portal,
                Method::Post,
                "/api/display",
                display,
                Some(Ok(Response::new(200, &inland[..]))),
                &mut events,
            ),
            Reply::Json(display_saved_json(TideSupport::Unavailable))
        );
        assert_eq!(
            request(
                &mut portal,
                Method::Post,
                "/api/done",
                "",
                None,
                &mut events
            ),
            Reply::Json(ok_json())
        );
        assert_eq!(
            events,
            [
                Event::JoinWifi {
                    ssid: "Home".into(),
                    password: "hunter22".into()
                },
                Event::SignedIn,
                Event::SettingsChanged,
                Event::Done
            ]
        );
    }

    #[test]
    fn done_needs_a_connected_wifi() {
        let settings = Settings {
            wifi_ssid: "Home".into(),
            spotify_client_id: "cid".into(),
            spotify_refresh_token: Some("rt".into()),
            weather: Some(WeatherLocation {
                city: "X".into(),
                lat: 0.0,
                lon: 0.0,
            }),
            ..Settings::default()
        };
        let mut portal = Portal::new(settings, "n");
        let mut events = Vec::new();
        portal.set_wifi(WifiState::Failed);
        let Reply::Json(refused) = request(
            &mut portal,
            Method::Post,
            "/api/done",
            "",
            None,
            &mut events,
        ) else {
            panic!()
        };
        assert!(refused.contains("WiFi"));
        portal.set_wifi(WifiState::Connected);
        assert_eq!(
            request(
                &mut portal,
                Method::Post,
                "/api/done",
                "",
                None,
                &mut events
            ),
            Reply::Json(ok_json())
        );
    }

    #[rstest]
    #[case::city_search_offline(
        Method::Get,
        "/api/places?q=x",
        "",
        Some(Err(NetworkError)),
        "Couldn't search"
    )]
    #[case::city_search_server_error(
        Method::Get,
        "/api/places?q=x",
        "",
        Some(Ok(Response::new(500, &b""[..]))),
        "Couldn't search"
    )]
    #[case::city_search_unreadable(
        Method::Get,
        "/api/places?q=x",
        "",
        Some(Ok(Response::new(200, &b"oops"[..]))),
        "Couldn't search"
    )]
    #[case::sign_in_not_started(
        Method::Post,
        "/api/spotify/finish",
        "callback_url=x",
        None,
        "Connect Spotify first"
    )]
    #[case::sign_in_bad_client_id(
        Method::Post,
        "/api/spotify/start",
        "client_id=nope",
        None,
        "client_id"
    )]
    #[case::wifi_missing_name(Method::Post, "/api/wifi", "ssid=", None, "ssid")]
    fn failures_get_friendly_messages(
        #[case] method: Method,
        #[case] target: &str,
        #[case] body: &str,
        #[case] fetched: Option<Outcome>,
        #[case] message: &str,
    ) {
        let mut portal = Portal::new(Settings::default(), "n");
        let reply = request(&mut portal, method, target, body, fetched, &mut Vec::new());
        let Reply::Json(json) = reply else {
            panic!("{reply:?}")
        };
        assert!(json.starts_with(r#"{"ok":false"#), "{json}");
        assert!(json.contains(message), "{json}");
    }

    #[rstest]
    #[case::empty_city_search(Method::Get, "/api/places?q=", Reply::Json("[]".into()))]
    #[case::unknown_path(Method::Get, "/nope", Reply::NotFound)]
    #[case::wrong_method(Method::Get, "/api/wifi", Reply::NotFound)]
    #[case::captive_probe(Method::Get, "/generate_204", Reply::Redirect)]
    #[case::page(Method::Get, "/", Reply::Page)]
    fn simple_replies(#[case] method: Method, #[case] target: &str, #[case] expected: Reply) {
        let mut portal = Portal::new(Settings::default(), "n");
        assert_eq!(
            request(&mut portal, method, target, "", None, &mut Vec::new()),
            expected
        );
    }

    #[rstest]
    fn captive_probes_are_recognised(
        #[values(
            "/generate_204",
            "/hotspot-detect.html",
            "/connecttest.txt",
            "/ncsi.txt",
            "/generate_204?x=1"
        )]
        path: &str,
    ) {
        assert!(is_captive_probe(path));
    }

    #[rstest]
    fn other_paths_are_not_captive_probes(
        #[values("/", "/api/status", "/favicon.ico")] path: &str,
    ) {
        assert!(!is_captive_probe(path));
    }

    #[rstest]
    #[case::name_with_spaces("ssid=Home+WiFi&password=hunter22", "Home WiFi", "hunter22")]
    #[case::open_network("ssid=Cafe&password=", "Cafe", "")]
    #[case::spaces_in_password_kept("ssid=Home&password=+spaces+ok+", "Home", " spaces ok ")]
    #[case::shortest_password("ssid=Home&password=12345678", "Home", "12345678")]
    #[case::longest_password(&alloc::format!("ssid=Home&password={}", "p".repeat(63)), "Home", &"p".repeat(63))]
    #[case::raw_hex_key(&alloc::format!("ssid=Home&password={}", "a".repeat(64)), "Home", &"a".repeat(64))]
    #[case::longest_name(&alloc::format!("ssid={}", "x".repeat(32)), &"x".repeat(32), "")]
    fn wifi_form_accepts(#[case] body: &str, #[case] ssid: &str, #[case] password: &str) {
        assert_eq!(
            parse_wifi(body),
            Ok(WifiForm {
                ssid: ssid.into(),
                password: password.into()
            })
        );
    }

    #[rstest]
    #[case::no_name("ssid=&password=hunter22", &["ssid"])]
    #[case::name_too_long(&alloc::format!("ssid={}", "x".repeat(33)), &["ssid"])]
    #[case::password_too_short("ssid=Home&password=short", &["password"])]
    #[case::password_too_long(&alloc::format!("ssid=Home&password={}", "p".repeat(65)), &["password"])]
    #[case::sixty_four_but_not_hex(&alloc::format!("ssid=Home&password={}", "g".repeat(64)), &["password"])]
    #[case::both("ssid=&password=short", &["password", "ssid"])]
    fn wifi_form_rejects(#[case] body: &str, #[case] fields: &[&str]) {
        let errors = parse_wifi(body).unwrap_err();
        assert_eq!(errors.keys().copied().collect::<Vec<_>>(), fields);
    }

    #[rstest]
    #[case::trimmed_and_lowercased(
        " 0123456789ABCDEF0123456789abcdef ",
        Some("0123456789abcdef0123456789abcdef")
    )]
    #[case::too_short("too-short", None)]
    #[case::not_hex(&"g".repeat(32), None)]
    #[case::too_long(&"a".repeat(33), None)]
    #[case::empty("", None)]
    fn client_ids(#[case] typed: &str, #[case] expected: Option<&str>) {
        assert_eq!(parse_client_id(typed).ok().as_deref(), expected);
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
    fn saving_a_new_city_checks_for_tides() {
        let coast = include_bytes!("../tests/fixtures/openmeteo_marine_san_francisco.json");
        let san_francisco = "place=set&city=San+Francisco&lat=37.77&lon=-122.42&timezone=America%2FLos_Angeles&units=f&clock=12&idle_minutes=5";
        let mut portal = Portal::new(Settings::default(), "OnAir-1A2B");
        let mut events = Vec::new();

        // The check fails: saved anyway, with tides unknown.
        match portal.handle(
            Method::Post,
            "/api/display",
            san_francisco,
            &mut random,
            &mut |e| events.push(e),
        ) {
            Handled::Fetch(request) => assert!(request.url.starts_with(tide::MARINE_URL)),
            other => panic!("{other:?}"),
        }
        let reply = portal.finish(Err(NetworkError), &mut |e| events.push(e));
        assert_eq!(reply, Reply::Json(display_saved_json(TideSupport::Unknown)));
        assert_eq!(
            portal.settings().weather.as_ref().unwrap().city,
            "San Francisco"
        );
        assert_eq!(events, [Event::SettingsChanged]);

        // Saving the same city again retries the check.
        let reply = request(
            &mut portal,
            Method::Post,
            "/api/display",
            san_francisco,
            Some(Ok(Response::new(200, &coast[..]))),
            &mut events,
        );
        assert_eq!(
            reply,
            Reply::Json(display_saved_json(TideSupport::Available))
        );
        assert!(portal.settings().has_tides());
        assert!(
            Status::new(portal.settings(), "x", WifiState::Connected)
                .to_json()
                .contains(r#""tides":"yes""#)
        );

        // Changing only the units doesn't check again.
        let reply = request(
            &mut portal,
            Method::Post,
            "/api/display",
            "place=keep&units=c&clock=12&idle_minutes=5",
            None,
            &mut events,
        );
        assert_eq!(
            reply,
            Reply::Json(display_saved_json(TideSupport::Available))
        );
        assert_eq!(portal.settings().units, Units::Celsius);
    }

    #[test]
    fn display_form_with_city() {
        let form = parse_display(
            "place=set&city=Portland&lat=45.52&lon=-122.68&timezone=America%2FLos_Angeles&units=f&clock=24&idle_minutes=12",
        )
        .unwrap();
        let mut settings = Settings::default();
        form.apply(&mut settings);
        assert_eq!(settings.weather.as_ref().unwrap().city, "Portland");
        assert_eq!(settings.timezone, "America/Los_Angeles");
        assert_eq!(settings.units, Units::Fahrenheit);
        assert!(settings.clock_24h);
        assert_eq!(settings.idle_after_min, 12);
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

    const GOOD_PLACE: &str =
        "place=set&city=Portland&lat=45.52&lon=-122.68&timezone=America%2FLos_Angeles";

    /// One bad field at a time, with everything else valid.
    #[rstest]
    #[case::latitude_out_of_range("place=set&city=Portland&lat=91&lon=0&timezone=UTC", "", "place")]
    #[case::longitude_out_of_range(
        "place=set&city=Portland&lat=0&lon=181&timezone=UTC",
        "",
        "place"
    )]
    #[case::latitude_not_a_number("place=set&city=Portland&lat=x&lon=0&timezone=UTC", "", "place")]
    #[case::unknown_timezone("place=set&city=Portland&lat=0&lon=0&timezone=Nowhere", "", "place")]
    #[case::no_city_name("place=set&city=&lat=0&lon=0&timezone=UTC", "", "place")]
    #[case::units(GOOD_PLACE, "&units=k", "units")]
    #[case::clock(GOOD_PLACE, "&clock=13", "clock")]
    #[case::idle_too_short(GOOD_PLACE, "&idle_minutes=0", "idle_minutes")]
    #[case::idle_too_long(GOOD_PLACE, "&idle_minutes=31", "idle_minutes")]
    #[case::idle_not_a_number(GOOD_PLACE, "&idle_minutes=soon", "idle_minutes")]
    fn display_form_errors(#[case] place: &str, #[case] rest: &str, #[case] field: &str) {
        let errors = parse_display(&alloc::format!("{place}{rest}")).unwrap_err();
        assert_eq!(errors.keys().copied().collect::<Vec<_>>(), [field]);
    }

    #[test]
    fn display_form_reports_every_bad_field() {
        let errors =
            parse_display("place=set&city=Portland&lat=91&lon=0&timezone=Nowhere&units=k&clock=13&idle_minutes=31")
                .unwrap_err();
        assert_eq!(errors.len(), 4);
    }

    #[test]
    fn display_form_without_idle_time_keeps_the_saved_one() {
        assert_eq!(
            parse_display("place=keep&units=c&clock=12")
                .unwrap()
                .idle_after_min,
            None
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
        let json = Status::new(&settings, "OnAir-1A2B", WifiState::Connected).to_json();
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
    }

    #[rstest]
    #[case::very_strong(-30, 100)]
    #[case::strong(-50, 100)]
    #[case::middling(-75, 50)]
    #[case::weakest(-100, 0)]
    #[case::below_weakest(-120, 0)]
    fn signal_strength(#[case] dbm: i8, #[case] percent: u8) {
        assert_eq!(signal_from_dbm(dbm), percent);
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
