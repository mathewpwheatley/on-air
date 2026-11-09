//! The setup portal, served on this computer at http://127.0.0.1:8080.
//!
//! The core's `Portal` handles every request; this file only runs the web
//! server and acts on its events. WiFi is pretend: the network list is made
//! up and joining works after a moment, unless the password is
//! `wrongpassword`. City search and Spotify sign-in are real.

use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use on_air_core::http::Method;
use on_air_core::portal::{self, Event, Handled, Network, Portal, WifiState};
use on_air_core::settings::Settings;
use reqwest::blocking::Client;
use tiny_http::{Header, Request, Response, Server};

use crate::http_client;
use crate::messages::Update;

pub const ADDRESS: &str = "127.0.0.1:8080";

/// Start the portal server in the background, optionally opening it in the
/// browser.
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
    let wifi = if settings.has_wifi() {
        WifiState::Connected
    } else {
        WifiState::NotSet
    };
    let mut portal = Portal::new(settings, &network_name);
    portal.set_networks(fake_networks());
    portal.set_wifi(wifi);
    let portal = Arc::new(Mutex::new(portal));

    let url = format!("http://{ADDRESS}/");
    println!("setup portal at {url}");
    if open_browser {
        let _ = open::that(&url);
    }
    std::thread::spawn(move || {
        let http = http_client::client();
        for request in server.incoming_requests() {
            handle(request, &portal, &http, &updates);
        }
    });
}

fn fake_networks() -> Vec<Network> {
    let network = |ssid: &str, dbm: i8, secure| Network {
        ssid: ssid.into(),
        signal: portal::signal_from_dbm(dbm),
        secure,
    };
    vec![
        network("Home WiFi", -48, true),
        network("Coffee Shop", -80, false),
        network("Neighbors 5G", -71, true),
    ]
}

fn handle(
    mut request: Request,
    portal: &Arc<Mutex<Portal>>,
    http: &Client,
    updates: &Sender<Update>,
) {
    let method = match request.method() {
        tiny_http::Method::Get => Method::Get,
        tiny_http::Method::Post => Method::Post,
        _ => {
            let _ = request.respond(Response::from_string("Not found").with_status_code(404));
            return;
        }
    };
    let target = request.url().to_string();
    let mut body = String::new();
    let _ = request.as_reader().read_to_string(&mut body);

    let mut events = Vec::new();
    let mut random = |buffer: &mut [u8]| {
        let _ = getrandom::fill(buffer);
    };
    let handled = portal
        .lock()
        .unwrap()
        .handle(method, &target, &body, &mut random, &mut |e| events.push(e));
    let reply = match handled {
        Handled::Reply(reply) => reply,
        Handled::Fetch(fetch) => {
            let outcome = http_client::send(http, &fetch);
            portal
                .lock()
                .unwrap()
                .finish(outcome, &mut |e| events.push(e))
        }
    };
    for event in events {
        act(event, portal, updates);
    }

    let header =
        |name: &str, value: &str| Header::from_bytes(name.as_bytes(), value.as_bytes()).unwrap();
    let mut response = Response::from_string(reply.body())
        .with_status_code(reply.status())
        .with_header(header("Content-Type", reply.content_type()));
    if reply == portal::Reply::Redirect {
        response = response.with_header(header("Location", &format!("http://{ADDRESS}/")));
    }
    if let Err(e) = request.respond(response) {
        eprintln!("portal: reply to {target} failed: {e}");
    }
}

fn act(event: Event, portal: &Arc<Mutex<Portal>>, updates: &Sender<Update>) {
    match event {
        Event::JoinWifi { ssid, password } => {
            // Pretend to join, taking a moment like the real thing.
            let portal = portal.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(1_500));
                let state = if password == "wrongpassword" {
                    WifiState::Failed
                } else {
                    WifiState::Connected
                };
                portal.lock().unwrap().set_wifi(state);
                println!("portal: WiFi {state:?} ({ssid})");
            });
        }
        Event::SignedIn => {
            let p = portal.lock().unwrap();
            let settings = p.settings();
            if let Some(token) = &settings.spotify_refresh_token {
                crate::live::save_refresh_token(&settings.spotify_client_id, token);
                println!("portal: signed in to Spotify (session saved for --live)");
            }
        }
        Event::SettingsChanged => {
            let settings = portal.lock().unwrap().settings().clone();
            let _ = updates.send(Update::DisplaySaved(settings));
        }
        Event::Done => {
            let _ = updates.send(Update::PortalDone);
        }
    }
}
