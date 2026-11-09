//! Real Spotify, using the same sign-in, polling and parsing code as the
//! device. Only the HTTP client differs.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use on_air_core::app::{CommandFailure, NetStatus};
use on_air_core::auth::{self, Pkce, TokenError, Tokens};
use on_air_core::backoff::Backoff;
use on_air_core::image;
use on_air_core::schedule::{self, PollState};
use on_air_core::spotify::{self, Command, CommandResult, Method, PlayerResponse};
use reqwest::blocking::{Client, Response};
use reqwest::header::{AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE, RETRY_AFTER};
use serde::{Deserialize, Serialize};

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

fn load_refresh_token(client_id: &str) -> Option<String> {
    let text = std::fs::read_to_string(session_path()).ok()?;
    let session: Session = serde_json::from_str(&text).ok()?;
    (session.client_id == client_id).then_some(session.refresh_token)
}

fn save_refresh_token(client_id: &str, refresh_token: &str) {
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

enum Failure {
    /// Network trouble or an outage: retry later.
    Transient,
    /// The owner has to sign in again.
    SignInNeeded,
}

struct Token {
    access: String,
    issued_at: Instant,
    expires_in_s: u32,
}

impl Token {
    fn needs_refresh(&self) -> bool {
        auth::needs_refresh(self.issued_at.elapsed().as_secs(), 0, self.expires_in_s)
    }
}

struct Spotify {
    http: Client,
    client_id: String,
    refresh_token: String,
    token: Option<Token>,
}

impl Spotify {
    fn refresh(&mut self) -> Result<(), Failure> {
        let response = self
            .http
            .post(auth::TOKEN_URL)
            .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(auth::refresh_body(&self.client_id, &self.refresh_token))
            .send()
            .map_err(|_| Failure::Transient)?;
        let (status, _, body) = read(response)?;
        match auth::parse_token_response(status, &body) {
            Ok(tokens) => {
                self.store(tokens);
                Ok(())
            }
            Err(TokenError::InvalidGrant | TokenError::InvalidClient) => Err(Failure::SignInNeeded),
            Err(e) => {
                eprintln!("spotify: token refresh failed: {e:?}");
                Err(Failure::Transient)
            }
        }
    }

    fn store(&mut self, tokens: Tokens) {
        if let Some(rotated) = tokens.refresh_token {
            self.refresh_token = rotated;
            save_refresh_token(&self.client_id, &self.refresh_token);
        }
        self.token = Some(Token {
            access: tokens.access_token,
            issued_at: Instant::now(),
            expires_in_s: tokens.expires_in_s,
        });
    }

    fn access_token(&mut self) -> Result<String, Failure> {
        if self.token.as_ref().is_none_or(Token::needs_refresh) {
            self.refresh()?;
        }
        Ok(self.token.as_ref().expect("just refreshed").access.clone())
    }

    /// GET or send with the access token, refreshing and retrying once on 401.
    fn authorized(
        &mut self,
        build: impl Fn(&Client, &str) -> reqwest::blocking::RequestBuilder,
    ) -> Result<(u16, Option<String>, Vec<u8>), Failure> {
        for attempt in 0..2 {
            let token = self.access_token()?;
            let response = build(&self.http, &format!("Bearer {token}"))
                .send()
                .map_err(|_| Failure::Transient)?;
            let result = read(response)?;
            if result.0 == 401 && attempt == 0 {
                self.token = None;
                continue;
            }
            return Ok(result);
        }
        Err(Failure::SignInNeeded)
    }

    fn command(&mut self, command: Command) -> Result<CommandResult, Failure> {
        let (method, url) = command.request();
        let (status, retry_after, _) = self.authorized(|http, bearer| {
            let request = match method {
                Method::Put => http.put(url),
                Method::Post => http.post(url),
            };
            request
                .header(AUTHORIZATION, bearer)
                .header(CONTENT_LENGTH, "0")
        })?;
        Ok(spotify::parse_command_response(
            status,
            retry_after.as_deref(),
        ))
    }
}

fn read(response: Response) -> Result<(u16, Option<String>, Vec<u8>), Failure> {
    let status = response.status().as_u16();
    let retry_after = response
        .headers()
        .get(RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .map(String::from);
    let body = response.bytes().map_err(|_| Failure::Transient)?.to_vec();
    Ok((status, retry_after, body))
}

pub fn run(
    client_id: String,
    record: Option<PathBuf>,
    updates: Sender<Update>,
    controls: Receiver<Control>,
) {
    let http = Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("HTTP client");
    let _ = updates.send(Update::Net(NetStatus::Online));
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
        let mut spotify = Spotify {
            http: http.clone(),
            client_id: client_id.clone(),
            refresh_token: token,
            token: None,
        };
        match poll(&mut spotify, record.as_deref(), &updates, &controls) {
            Exit::SignInNeeded => {
                eprintln!("spotify: signed out, starting sign-in again");
                let _ = std::fs::remove_file(session_path());
            }
            Exit::WindowClosed => return,
        }
    }
}

enum Exit {
    SignInNeeded,
    WindowClosed,
}

fn poll(
    spotify: &mut Spotify,
    record: Option<&Path>,
    updates: &Sender<Update>,
    controls: &Receiver<Control>,
) -> Exit {
    let mut backoff = Backoff::spotify();
    let mut delay_ms: u32 = 0;
    let mut sent_art_url: Option<Option<String>> = None;
    let mut not_playing_since: Option<Instant> = None;
    let mut recorded_item: Option<String> = None;
    let mut recorded = 0;

    loop {
        match controls.recv_timeout(Duration::from_millis(delay_ms as u64)) {
            Ok(Control::Spotify(command)) => {
                match spotify.command(command) {
                    Ok(CommandResult::Ok) => {}
                    Ok(CommandResult::PremiumRequired) => {
                        let _ =
                            updates.send(Update::CommandFailed(CommandFailure::PremiumRequired));
                    }
                    Ok(CommandResult::NoActiveDevice) => {
                        let _ = updates.send(Update::CommandFailed(CommandFailure::NoActiveDevice));
                    }
                    Ok(other) => {
                        eprintln!("spotify: {command:?} failed: {other:?}");
                        let _ = updates.send(Update::CommandFailed(CommandFailure::Other));
                    }
                    Err(Failure::SignInNeeded) => return Exit::SignInNeeded,
                    Err(Failure::Transient) => {
                        let _ = updates.send(Update::CommandFailed(CommandFailure::Other));
                    }
                }
                delay_ms = schedule::POLL_AFTER_COMMAND_MS;
                continue;
            }
            Ok(Control::StopPlayback) | Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return Exit::WindowClosed,
        }

        let result = spotify.authorized(|http, bearer| {
            http.get(spotify::CURRENTLY_PLAYING_URL)
                .header(AUTHORIZATION, bearer)
        });
        let (status, retry_after, body) = match result {
            Ok(r) => r,
            Err(Failure::SignInNeeded) => return Exit::SignInNeeded,
            Err(Failure::Transient) => {
                let _ = updates.send(Update::Net(NetStatus::Lost));
                delay_ms = backoff.next_delay_ms(crate::random_u32());
                continue;
            }
        };
        let _ = updates.send(Update::Net(NetStatus::Online));

        let state = match spotify::parse_player_response(status, retry_after.as_deref(), &body) {
            PlayerResponse::Playing(now_playing) => {
                backoff.reset();
                if let Some(dir) = record
                    && recorded_item.as_deref() != Some(now_playing.item_id.as_str())
                {
                    recorded += 1;
                    let path = dir.join(format!("spotify_recorded_{recorded}.json"));
                    match std::fs::write(&path, &body) {
                        Ok(()) => println!("recorded {}", path.display()),
                        Err(e) => eprintln!("couldn't record {}: {e}", path.display()),
                    }
                    recorded_item = Some(now_playing.item_id.clone());
                }
                if sent_art_url.as_ref() != Some(&now_playing.art_url) {
                    let art = now_playing
                        .art_url
                        .as_deref()
                        .and_then(|url| download_art(&spotify.http, url));
                    let _ = updates.send(Update::Art(art));
                    sent_art_url = Some(now_playing.art_url.clone());
                }
                let state = if now_playing.is_playing {
                    not_playing_since = None;
                    PollState::Playing {
                        progress_ms: now_playing.progress_ms,
                        duration_ms: now_playing.duration_ms,
                    }
                } else {
                    let since = *not_playing_since.get_or_insert_with(Instant::now);
                    PollState::NotPlaying {
                        for_ms: since.elapsed().as_millis() as u64,
                    }
                };
                let _ = updates.send(Update::NowPlaying(Some(now_playing)));
                state
            }
            PlayerResponse::Nothing => {
                backoff.reset();
                let _ = updates.send(Update::NowPlaying(None));
                let since = *not_playing_since.get_or_insert_with(Instant::now);
                PollState::NotPlaying {
                    for_ms: since.elapsed().as_millis() as u64,
                }
            }
            PlayerResponse::Unauthorized => return Exit::SignInNeeded,
            PlayerResponse::RateLimited { retry_after_s } => {
                eprintln!("spotify: rate limited for {retry_after_s} s");
                delay_ms = schedule::respect_retry_after(0, Some(retry_after_s));
                continue;
            }
            PlayerResponse::Failed { status } => {
                delay_ms = backoff.next_delay_ms(crate::random_u32());
                eprintln!(
                    "spotify: currently-playing returned {status}, retrying in {} s",
                    delay_ms / 1000
                );
                continue;
            }
        };
        delay_ms = schedule::next_poll_ms(state);
    }
}

fn download_art(http: &Client, url: &str) -> Option<Vec<u8>> {
    let bytes = http.get(url).send().ok()?.bytes().ok()?;
    match image::decode_art(&bytes) {
        Ok(frame) => Some(frame),
        Err(e) => {
            eprintln!("spotify: couldn't decode cover {url}: {e:?}");
            None
        }
    }
}

/// Browser sign-in. Spotify redirects to 127.0.0.1:8888 on this computer,
/// so a tiny local server catches the callback instead of pasting it back.
fn sign_in(http: &Client, client_id: &str) -> Result<String, String> {
    let mut random = [0u8; 32];
    getrandom::fill(&mut random).map_err(|e| e.to_string())?;
    let pkce = Pkce::from_random(&random);
    let mut state_bytes = [0u8; 16];
    getrandom::fill(&mut state_bytes).map_err(|e| e.to_string())?;
    let state = auth::state_from_random(&state_bytes);

    let server = tiny_http::Server::http("127.0.0.1:8888")
        .map_err(|e| format!("can't listen on 127.0.0.1:8888: {e}"))?;
    let url = auth::authorize_url(client_id, &pkce.challenge, &state);
    println!("Sign in to Spotify in your browser. If it doesn't open, visit:\n{url}");
    let _ = open::that(&url);

    let code = loop {
        let request = server.recv().map_err(|e| e.to_string())?;
        if !request.url().starts_with("/callback") {
            let _ = request
                .respond(tiny_http::Response::from_string("Not found").with_status_code(404));
            continue;
        }
        match auth::parse_callback(request.url(), &state) {
            Ok(code) => {
                let _ = request.respond(tiny_http::Response::from_string(
                    "Signed in to On Air. You can close this tab.",
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

    let response = http
        .post(auth::TOKEN_URL)
        .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(auth::code_exchange_body(client_id, &code, &pkce.verifier))
        .send()
        .map_err(|e| e.to_string())?;
    let status = response.status().as_u16();
    let body = response.bytes().map_err(|e| e.to_string())?;
    let tokens = auth::parse_token_response(status, &body).map_err(|e| format!("{e:?}"))?;
    let refresh_token = tokens
        .refresh_token
        .ok_or("Spotify didn't return a refresh token")?;
    save_refresh_token(client_id, &refresh_token);
    println!("Signed in. Session saved to {}", session_path().display());
    Ok(refresh_token)
}
