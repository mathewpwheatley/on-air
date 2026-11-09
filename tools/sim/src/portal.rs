//! The setup portal, served on this computer at http://127.0.0.1:8080.
//!
//! Uses the same page and logic as the device. WiFi is pretend: the network
//! list is made up and "joining" always works after a moment, unless the
//! password is `wrongpassword`. City search and Spotify sign-in are real.

use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use on_air_core::auth::{self, TokenError};
use on_air_core::portal::{self, Network, SignIn, Status, WifiState};
use on_air_core::settings::Settings;
use on_air_core::weather;
use reqwest::header::CONTENT_TYPE;
use tiny_http::{Header, Method, Request, Response, Server};

use crate::messages::Update;

pub const ADDRESS: &str = "127.0.0.1:8080";

struct State {
    settings: Settings,
    network_name: String,
    wifi: WifiState,
    sign_in: Option<SignIn>,
}

/// Start the portal server in the background and open it in the browser.
pub fn start(
    settings: Settings,
    network_name: String,
    updates: Sender<Update>,
    open_browser: bool,
) {
    let server = match Server::http(ADDRESS) {
        Ok(server) => server,
        Err(e) => {
            eprintln!("portal: can't listen on {ADDRESS}: {e}");
            return;
        }
    };
    let state = Arc::new(Mutex::new(State {
        wifi: if settings.has_wifi() {
            WifiState::Connected
        } else {
            WifiState::NotSet
        },
        settings,
        network_name,
        sign_in: None,
    }));
    let url = format!("http://{ADDRESS}/");
    println!("setup portal at {url}");
    if open_browser {
        let _ = open::that(&url);
    }

    std::thread::spawn(move || {
        let http = reqwest::blocking::Client::new();
        for request in server.incoming_requests() {
            handle(request, &state, &http, &updates);
        }
    });
}

fn handle(
    mut request: Request,
    state: &Arc<Mutex<State>>,
    http: &reqwest::blocking::Client,
    updates: &Sender<Update>,
) {
    let url = request.url().to_string();
    let path = url.split('?').next().unwrap_or("").to_string();
    let mut body = String::new();
    let _ = request.as_reader().read_to_string(&mut body);

    let reply = match (request.method(), path.as_str()) {
        (Method::Get, "/") => Reply::Html(portal::PAGE.to_string()),
        (Method::Get, p) if portal::is_captive_probe(p) => Reply::Redirect,
        (Method::Get, "/api/status") => {
            let s = state.lock().unwrap();
            Reply::Json(Status::new(&s.settings, &s.network_name, s.wifi).to_json())
        }
        (Method::Get, "/api/networks") => Reply::Json(portal::networks_json(&fake_networks())),
        (Method::Get, "/api/places") => Reply::Json(places(&url, http)),
        (Method::Post, "/api/wifi") => Reply::Json(wifi(&body, state)),
        (Method::Post, "/api/spotify/start") => Reply::Json(spotify_start(&body, state)),
        (Method::Post, "/api/spotify/finish") => Reply::Json(spotify_finish(&body, state, http)),
        (Method::Post, "/api/display") => Reply::Json(display(&body, state, updates)),
        (Method::Post, "/api/done") => {
            let s = state.lock().unwrap();
            match portal::missing_before_done(&s.settings) {
                Some(message) => Reply::Json(portal::message_json(message)),
                None => {
                    let _ = updates.send(Update::PortalDone);
                    Reply::Json(portal::ok_json())
                }
            }
        }
        _ => Reply::NotFound,
    };

    let header =
        |name: &str, value: &str| Header::from_bytes(name.as_bytes(), value.as_bytes()).unwrap();
    let result = match reply {
        Reply::Html(html) => request.respond(
            Response::from_string(html)
                .with_header(header("Content-Type", "text/html; charset=utf-8")),
        ),
        Reply::Json(json) => request.respond(
            Response::from_string(json).with_header(header("Content-Type", "application/json")),
        ),
        Reply::Redirect => request.respond(
            Response::empty(302).with_header(header("Location", &format!("http://{ADDRESS}/"))),
        ),
        Reply::NotFound => {
            request.respond(Response::from_string("Not found").with_status_code(404))
        }
    };
    if let Err(e) = result {
        eprintln!("portal: reply to {path} failed: {e}");
    }
}

enum Reply {
    Html(String),
    Json(String),
    Redirect,
    NotFound,
}

fn fake_networks() -> Vec<Network> {
    let n = |ssid: &str, dbm: i8, secure| Network {
        ssid: ssid.into(),
        signal: portal::signal_from_dbm(dbm),
        secure,
    };
    vec![
        n("Home WiFi", -48, true),
        n("Coffee Shop", -80, false),
        n("Neighbors 5G", -71, true),
    ]
}

fn places(url: &str, http: &reqwest::blocking::Client) -> String {
    let query = url
        .split_once('?')
        .map(|(_, q)| q)
        .and_then(|q| form_value(q, "q"))
        .unwrap_or_default();
    match crate::weather::get(http, &weather::geocoding_url(&query, 5))
        .and_then(|b| weather::parse_geocoding(&b).ok())
    {
        Some(places) => portal::places_json(&places),
        None => portal::message_json("Couldn't search for cities right now. Try again."),
    }
}

/// One value from a query string or form body.
fn form_value(encoded: &str, key: &str) -> Option<String> {
    form_urlencoded::parse(encoded.as_bytes())
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.into_owned())
}

fn wifi(body: &str, state: &Arc<Mutex<State>>) -> String {
    let form = match portal::parse_wifi(body) {
        Ok(form) => form,
        Err(errors) => return portal::errors_json(&errors),
    };
    {
        let mut s = state.lock().unwrap();
        s.settings.wifi_ssid = form.ssid.clone();
        s.settings.wifi_pass = form.password.clone();
        s.wifi = WifiState::Connecting;
    }
    let state = state.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(1_500));
        let mut s = state.lock().unwrap();
        s.wifi = if form.password == "wrongpassword" {
            WifiState::Failed
        } else {
            WifiState::Connected
        };
        if s.wifi == WifiState::Failed {
            s.settings.wifi_ssid.clear();
        }
        println!("portal: WiFi {:?} ({})", s.wifi, form.ssid);
    });
    portal::ok_json()
}

fn spotify_start(body: &str, state: &Arc<Mutex<State>>) -> String {
    let client_id = form_value(body, "client_id").unwrap_or_default();
    let mut random = [0u8; 32];
    let mut state_random = [0u8; 16];
    if getrandom::fill(&mut random).is_err() || getrandom::fill(&mut state_random).is_err() {
        return portal::message_json("Couldn't start sign-in. Try again.");
    }
    match SignIn::new(&client_id, &random, &state_random) {
        Ok(sign_in) => {
            let url = sign_in.authorize_url();
            state.lock().unwrap().sign_in = Some(sign_in);
            portal::authorize_json(&url)
        }
        Err(errors) => portal::errors_json(&errors),
    }
}

fn spotify_finish(
    body: &str,
    state: &Arc<Mutex<State>>,
    http: &reqwest::blocking::Client,
) -> String {
    let pasted = form_value(body, "callback_url").unwrap_or_default();
    let Some(sign_in) = state.lock().unwrap().sign_in.clone() else {
        return portal::message_json("Tap Connect Spotify first.");
    };
    let code = match sign_in.code_from(&pasted) {
        Ok(code) => code,
        Err(e) => return portal::message_json(portal::callback_message(&e)),
    };
    let response = http
        .post(auth::TOKEN_URL)
        .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(sign_in.exchange_body(&code))
        .send();
    let Ok(response) = response else {
        return portal::message_json("Couldn't reach Spotify. Check the WiFi and try again.");
    };
    let status = response.status().as_u16();
    let body = response.bytes().map(|b| b.to_vec()).unwrap_or_default();
    match auth::parse_token_response(status, &body) {
        Ok(tokens) => match tokens.refresh_token {
            Some(refresh_token) => {
                crate::live::save_refresh_token(&sign_in.client_id, &refresh_token);
                let mut s = state.lock().unwrap();
                s.settings.spotify_client_id = sign_in.client_id.clone();
                s.settings.spotify_refresh_token = Some(refresh_token);
                s.sign_in = None;
                println!("portal: signed in to Spotify (session saved for --live)");
                portal::ok_json()
            }
            None => portal::message_json(
                "Spotify didn't send everything needed. Tap Connect Spotify to try again.",
            ),
        },
        Err(TokenError::InvalidGrant) => portal::message_json(
            "That sign-in has expired or was already used. Tap Connect Spotify to try again.",
        ),
        Err(TokenError::InvalidClient) => portal::message_json(
            "Spotify doesn't recognise that Client ID. Check it in your Spotify app's settings.",
        ),
        Err(_) => portal::message_json("Spotify sign-in failed. Tap Connect Spotify to try again."),
    }
}

fn display(body: &str, state: &Arc<Mutex<State>>, updates: &Sender<Update>) -> String {
    match portal::parse_display(body) {
        Ok(form) => {
            let mut s = state.lock().unwrap();
            form.apply(&mut s.settings);
            let _ = updates.send(Update::DisplaySaved(s.settings.clone()));
            portal::ok_json()
        }
        Err(errors) => portal::errors_json(&errors),
    }
}
