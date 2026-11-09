//! Real Spotify. The core's `SpotifySync` decides what to do; this thread
//! only sends its requests and waits. Sign-in happens in the browser, with
//! a small local server catching Spotify's redirect.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use on_air_core::app::{CommandFailure, NetStatus};
use on_air_core::auth;
use on_air_core::http::{Method, Request};
use on_air_core::portal::SignIn;
use on_air_core::spotify::{self, PlayerResponse};
use on_air_core::spotify_sync::{Next, Output, SpotifySync};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};

use crate::http_client;
use crate::messages::{Control, Update};

#[derive(Serialize, Deserialize)]
struct Session {
    client_id: String,
    refresh_token: String,
}

fn session_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("on-air-sim")
        .join("session.json")
}

pub fn load_refresh_token(client_id: &str) -> Option<String> {
    let text = std::fs::read_to_string(session_path()).ok()?;
    let session: Session = serde_json::from_str(&text).ok()?;
    (session.client_id == client_id).then_some(session.refresh_token)
}

pub fn save_refresh_token(client_id: &str, refresh_token: &str) {
    let path = session_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let session = Session {
        client_id: client_id.to_string(),
        refresh_token: refresh_token.to_string(),
    };
    if let Err(e) = std::fs::write(&path, serde_json::to_string_pretty(&session).unwrap()) {
        eprintln!("spotify: couldn't save session to {}: {e}", path.display());
    }
}

pub fn run(
    client_id: String,
    record: Option<PathBuf>,
    idle_after_ms: Arc<AtomicU64>,
    updates: Sender<Update>,
    controls: Receiver<Control>,
) {
    if let Some(dir) = &record
        && let Err(e) = std::fs::create_dir_all(dir)
    {
        eprintln!("couldn't create {}: {e}", dir.display());
    }
    let http = http_client::client();
    let start = Instant::now();
    let now = || start.elapsed().as_millis() as u64;
    let mut recorder = Recorder {
        dir: record,
        last_item: None,
        count: 0,
    };
    let mut refresh_token = load_refresh_token(&client_id);

    loop {
        let token = match refresh_token.take() {
            Some(token) => token,
            None => {
                let _ = updates.send(Update::SignInNeeded);
                match sign_in(&http, &client_id) {
                    Ok(token) => {
                        let _ = updates.send(Update::SignedIn);
                        token
                    }
                    Err(e) => {
                        eprintln!("spotify: sign-in failed: {e}");
                        std::thread::sleep(Duration::from_secs(5));
                        continue;
                    }
                }
            }
        };

        let idle = || idle_after_ms.load(Ordering::Relaxed);
        let mut sync = SpotifySync::new(&client_id, &token, idle(), crate::random_u32());
        loop {
            sync.set_idle_after_ms(idle());
            match sync.next(now()) {
                Next::Send(request) => {
                    let outcome = http_client::send(&http, &request);
                    if request.url == spotify::CURRENTLY_PLAYING_URL
                        && let Ok(response) = &outcome
                    {
                        recorder.save(response.status, &response.body);
                    }
                    sync.handle(now(), outcome, &mut |output| {
                        forward(output, &client_id, &updates)
                    });
                }
                Next::Wait(ms) => match controls.recv_timeout(Duration::from_millis(ms)) {
                    Ok(Control::Spotify(command)) => sync.command(command),
                    Ok(Control::StopPlayback) | Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => return,
                },
                Next::Stopped => {
                    eprintln!("spotify: signed out, starting sign-in again");
                    let _ = std::fs::remove_file(session_path());
                    break;
                }
            }
        }
    }
}

/// Pass what changed on to the window.
fn forward(output: Output, client_id: &str, updates: &Sender<Update>) {
    let update = match output {
        Output::NowPlaying(now_playing) => Update::NowPlaying(now_playing),
        Output::Art(cover) => Update::Art(cover),
        Output::CommandFailed(failure) => {
            if failure == CommandFailure::Other {
                eprintln!("spotify: a playback command failed");
            }
            Update::CommandFailed(failure)
        }
        Output::RefreshTokenChanged(token) => {
            save_refresh_token(client_id, &token);
            return;
        }
        Output::Online => Update::Net(NetStatus::Online),
        Output::Offline => Update::Net(NetStatus::Lost),
        Output::SignInNeeded => Update::SignInNeeded,
    };
    let _ = updates.send(update);
}

/// Saves each new currently-playing response, for test fixtures.
struct Recorder {
    dir: Option<PathBuf>,
    last_item: Option<String>,
    count: u32,
}

impl Recorder {
    fn save(&mut self, status: u16, body: &[u8]) {
        let Some(dir) = &self.dir else { return };
        let PlayerResponse::Playing(now_playing) =
            spotify::parse_player_response(status, None, body)
        else {
            return;
        };
        if self.last_item.as_deref() == Some(now_playing.item_id.as_str()) {
            return;
        }
        self.count += 1;
        self.last_item = Some(now_playing.item_id);
        let path: &Path = dir;
        let path = path.join(format!("spotify_recorded_{}.json", self.count));
        match std::fs::write(&path, body) {
            Ok(()) => println!("recorded {}", path.display()),
            Err(e) => eprintln!("couldn't record {}: {e}", path.display()),
        }
    }
}

/// Browser sign-in. Spotify redirects to 127.0.0.1:8888 on this computer,
/// so a tiny local server catches the callback instead of pasting it back.
fn sign_in(http: &Client, client_id: &str) -> Result<String, String> {
    let mut verifier_random = [0u8; 32];
    let mut state_random = [0u8; 16];
    getrandom::fill(&mut verifier_random).map_err(|e| e.to_string())?;
    getrandom::fill(&mut state_random).map_err(|e| e.to_string())?;
    let sign_in = SignIn::new(client_id, &verifier_random, &state_random)
        .map_err(|_| "that Client ID doesn't look right".to_string())?;

    let server = tiny_http::Server::http("127.0.0.1:8888")
        .map_err(|e| format!("can't listen on 127.0.0.1:8888: {e}"))?;
    let url = sign_in.authorize_url();
    println!("Sign in to Spotify in your browser. If it doesn't open, visit:\n{url}");
    let _ = open::that(&url);

    let code = loop {
        let request = server.recv().map_err(|e| e.to_string())?;
        if !request.url().starts_with("/callback") {
            let _ = request
                .respond(tiny_http::Response::from_string("Not found").with_status_code(404));
            continue;
        }
        match sign_in.code_from(request.url()) {
            Ok(code) => {
                let _ = request.respond(tiny_http::Response::from_string(
                    "Signed in to On-Air. You can close this tab.",
                ));
                break code;
            }
            Err(e) => {
                let _ = request.respond(
                    tiny_http::Response::from_string(format!("Sign-in failed: {e:?}"))
                        .with_status_code(400),
                );
                return Err(format!("{e:?}"));
            }
        }
    };

    let request = Request {
        method: Method::Post,
        url: auth::TOKEN_URL.into(),
        bearer: None,
        form_body: Some(sign_in.exchange_body(&code)),
    };
    let response =
        http_client::send(http, &request).map_err(|_| "couldn't reach Spotify".to_string())?;
    let tokens = auth::parse_token_response(response.status, &response.body)
        .map_err(|e| format!("{e:?}"))?;
    let refresh_token = tokens
        .refresh_token
        .ok_or("Spotify didn't return a refresh token")?;
    save_refresh_token(client_id, &refresh_token);
    println!("Signed in. Session saved to {}", session_path().display());
    Ok(refresh_token)
}
