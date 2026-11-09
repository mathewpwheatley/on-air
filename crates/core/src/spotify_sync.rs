//! The Spotify loop, without the network: decides which request to send
//! next, reads each reply, and reports what changed. The host (a firmware
//! task or a simulator thread) only sends requests and waits.
//!
//! ```text
//! loop {
//!     match sync.next(now) {
//!         Next::Send(request) => {
//!             let outcome = send(&request);        // the host's HTTP client
//!             sync.handle(now, outcome, &mut emit);
//!         }
//!         Next::Wait(ms) => {
//!             // Sleep up to `ms`, waking early for a playback button:
//!             // sync.command(command)
//!         }
//!         Next::Stopped => break,                  // sign in again, then start a new SpotifySync
//!     }
//! }
//! ```
//!
//! It covers token refresh (and saving a rotated refresh token), polling on
//! the schedule, downloading covers only when they change, playback
//! commands with quick follow-up polls, and retries on 401, 429 and outages.

use alloc::string::String;
use alloc::vec::Vec;

use crate::app::CommandFailure;
use crate::auth::{self, TokenError};
use crate::backoff::Backoff;
use crate::http::{Method, NetworkError, Outcome, Request, Response};
use crate::image;
use crate::schedule::{self, FollowUp, PollState};
use crate::spotify::{self, Command, CommandResult, NowPlaying, PlayerResponse};

/// What the host should do next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Next {
    /// Send this request, then pass the outcome to [`SpotifySync::handle`].
    Send(Request),
    /// Nothing to do for this many milliseconds, unless a button is pressed.
    Wait(u64),
    /// Spotify no longer accepts the refresh token. Sign in again.
    Stopped,
}

/// What changed, for the app and the display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Output {
    /// What's playing, or `None` for nothing. Always sent after the [`Output::Art`]
    /// for a new cover.
    NowPlaying(Option<NowPlaying>),
    /// The current cover, 64×64 RGB888, or `None` when there's no art.
    Art(Option<Vec<u8>>),
    CommandFailed(CommandFailure),
    /// Spotify rotated the refresh token. Save it right away.
    RefreshTokenChanged(String),
    /// Spotify can be reached again.
    Online,
    /// Spotify can't be reached.
    Offline,
    /// The refresh token was rejected. [`SpotifySync::next`] now returns
    /// [`Next::Stopped`].
    SignInNeeded,
}

struct Access {
    token: String,
    issued_at_ms: u64,
    expires_in_s: u32,
}

/// The request waiting for its outcome.
enum InFlight {
    Refresh,
    Poll,
    Command(Command),
    Art(String),
}

pub struct SpotifySync {
    client_id: String,
    refresh_token: String,
    access: Option<Access>,
    in_flight: Option<InFlight>,
    stopped: bool,
    online: Option<bool>,
    backoff: Backoff,
    random: u32,
    idle_after_ms: u64,

    next_poll_at: u64,
    refresh_not_before: u64,
    unauthorized_polls: u8,

    queued_command: Option<Command>,
    command_retried: bool,
    follow_up: Option<FollowUp>,

    latest: Option<NowPlaying>,
    not_playing_since: Option<u64>,
    /// The art URL whose cover was last sent (`Some(None)`: "no art" was sent).
    shown_art_url: Option<Option<String>>,
    art_to_fetch: Option<String>,
    /// A new item waiting for its cover to download.
    held_now_playing: Option<NowPlaying>,
}

impl SpotifySync {
    /// `seed` is any random number, used to spread out retries.
    pub fn new(client_id: &str, refresh_token: &str, idle_after_ms: u64, seed: u32) -> Self {
        Self {
            client_id: client_id.into(),
            refresh_token: refresh_token.into(),
            access: None,
            in_flight: None,
            stopped: false,
            online: None,
            backoff: Backoff::spotify(),
            random: seed | 1,
            idle_after_ms,
            next_poll_at: 0,
            refresh_not_before: 0,
            unauthorized_polls: 0,
            queued_command: None,
            command_retried: false,
            follow_up: None,
            latest: None,
            not_playing_since: None,
            shown_art_url: None,
            art_to_fetch: None,
            held_now_playing: None,
        }
    }

    /// The owner's idle time; polling slows down after it.
    pub fn set_idle_after_ms(&mut self, idle_after_ms: u64) {
        self.idle_after_ms = idle_after_ms;
    }

    /// A playback button was pressed. It's sent on the next [`SpotifySync::next`].
    pub fn command(&mut self, command: Command) {
        if !self.stopped {
            self.queued_command = Some(command);
        }
    }

    /// The next request to send, or how long to wait.
    pub fn next(&mut self, now: u64) -> Next {
        if self.stopped {
            return Next::Stopped;
        }
        debug_assert!(self.in_flight.is_none(), "handle the last request first");

        if self
            .access
            .as_ref()
            .is_none_or(|a| auth::needs_refresh(now / 1000, a.issued_at_ms / 1000, a.expires_in_s))
        {
            if now < self.refresh_not_before {
                return Next::Wait(self.refresh_not_before - now);
            }
            self.in_flight = Some(InFlight::Refresh);
            return Next::Send(Request {
                method: Method::Post,
                url: auth::TOKEN_URL.into(),
                bearer: None,
                form_body: Some(auth::refresh_body(&self.client_id, &self.refresh_token)),
            });
        }
        let bearer = self.access.as_ref().map(|a| a.token.clone());

        if let Some(command) = self.queued_command.take() {
            let (method, url) = command.request();
            self.in_flight = Some(InFlight::Command(command));
            return Next::Send(Request {
                method,
                url: url.into(),
                bearer,
                form_body: None,
            });
        }
        if let Some(url) = self.art_to_fetch.take() {
            self.in_flight = Some(InFlight::Art(url.clone()));
            return Next::Send(Request::get(url));
        }
        if now >= self.next_poll_at {
            self.in_flight = Some(InFlight::Poll);
            return Next::Send(Request {
                bearer,
                ..Request::get(spotify::CURRENTLY_PLAYING_URL)
            });
        }
        Next::Wait(self.next_poll_at - now)
    }

    /// Read the outcome of the request [`SpotifySync::next`] returned.
    pub fn handle(&mut self, now: u64, outcome: Outcome, emit: &mut impl FnMut(Output)) {
        let Some(request) = self.in_flight.take() else {
            return;
        };
        match request {
            InFlight::Refresh => self.on_refresh(now, outcome, emit),
            InFlight::Poll => match outcome {
                Ok(response) => {
                    self.set_online(true, emit);
                    self.on_poll(now, response, emit);
                }
                Err(NetworkError) => {
                    self.set_online(false, emit);
                    self.next_poll_at = now + self.retry_delay();
                }
            },
            InFlight::Command(command) => self.on_command(now, command, outcome, emit),
            InFlight::Art(url) => {
                let cover = match outcome {
                    Ok(r) if r.status == 200 => image::decode_art(&r.body).ok(),
                    _ => None,
                };
                self.shown_art_url = Some(Some(url));
                emit(Output::Art(cover));
                if let Some(now_playing) = self.held_now_playing.take() {
                    emit(Output::NowPlaying(Some(now_playing)));
                }
            }
        }
    }

    fn on_refresh(&mut self, now: u64, outcome: Outcome, emit: &mut impl FnMut(Output)) {
        let response = match outcome {
            Ok(response) => response,
            Err(NetworkError) => {
                self.set_online(false, emit);
                self.refresh_not_before = now + self.retry_delay();
                return;
            }
        };
        match auth::parse_token_response(response.status, &response.body) {
            Ok(tokens) => {
                self.backoff.reset();
                self.set_online(true, emit);
                if let Some(rotated) = tokens.refresh_token
                    && rotated != self.refresh_token
                {
                    self.refresh_token = rotated.clone();
                    emit(Output::RefreshTokenChanged(rotated));
                }
                self.access = Some(Access {
                    token: tokens.access_token,
                    issued_at_ms: now,
                    expires_in_s: tokens.expires_in_s,
                });
            }
            Err(TokenError::InvalidGrant | TokenError::InvalidClient) => self.stop(emit),
            Err(_) => self.refresh_not_before = now + self.retry_delay(),
        }
    }

    fn on_poll(&mut self, now: u64, response: Response, emit: &mut impl FnMut(Output)) {
        match spotify::parse_player_response(
            response.status,
            response.retry_after.as_deref(),
            &response.body,
        ) {
            PlayerResponse::Playing(now_playing) => {
                self.backoff.reset();
                self.unauthorized_polls = 0;
                let state = if now_playing.is_playing {
                    self.not_playing_since = None;
                    PollState::Playing {
                        progress_ms: now_playing.progress_ms,
                        duration_ms: now_playing.duration_ms,
                    }
                } else {
                    self.not_playing_for(now)
                };
                self.schedule_poll(now, state, Some(&now_playing));
                self.latest = Some(now_playing.clone());

                if self.shown_art_url.as_ref() != Some(&now_playing.art_url) {
                    match &now_playing.art_url {
                        Some(url) => {
                            // Send the cover first, then this item.
                            self.art_to_fetch = Some(url.clone());
                            self.held_now_playing = Some(now_playing);
                            return;
                        }
                        None => {
                            self.shown_art_url = Some(None);
                            emit(Output::Art(None));
                        }
                    }
                }
                emit(Output::NowPlaying(Some(now_playing)));
            }
            PlayerResponse::Nothing => {
                self.backoff.reset();
                self.unauthorized_polls = 0;
                self.latest = None;
                let state = self.not_playing_for(now);
                self.schedule_poll(now, state, None);
                emit(Output::NowPlaying(None));
            }
            PlayerResponse::Unauthorized => {
                self.unauthorized_polls += 1;
                if self.unauthorized_polls >= 2 {
                    self.stop(emit);
                } else {
                    // The access token went stale early: refresh, then poll again.
                    self.access = None;
                    self.next_poll_at = now;
                }
            }
            PlayerResponse::RateLimited { retry_after_s } => {
                self.next_poll_at = now + retry_after_s as u64 * 1000;
            }
            PlayerResponse::Failed { .. } => {
                self.next_poll_at = now + self.retry_delay();
            }
        }
    }

    fn on_command(
        &mut self,
        now: u64,
        command: Command,
        outcome: Outcome,
        emit: &mut impl FnMut(Output),
    ) {
        let result = match outcome {
            Ok(r) => spotify::parse_command_response(r.status, r.retry_after.as_deref()),
            Err(NetworkError) => {
                emit(Output::CommandFailed(CommandFailure::Other));
                return;
            }
        };
        match result {
            CommandResult::Ok => {
                self.command_retried = false;
                let follow_up = FollowUp::new(command, self.latest.as_ref());
                self.next_poll_at = now + follow_up.first_delay_ms() as u64;
                self.follow_up = Some(follow_up);
            }
            CommandResult::Unauthorized if !self.command_retried => {
                self.command_retried = true;
                self.access = None;
                self.queued_command = Some(command);
            }
            CommandResult::Unauthorized => {
                self.command_retried = false;
                self.stop(emit);
            }
            CommandResult::PremiumRequired => {
                emit(Output::CommandFailed(CommandFailure::PremiumRequired))
            }
            CommandResult::NoActiveDevice => {
                emit(Output::CommandFailed(CommandFailure::NoActiveDevice))
            }
            CommandResult::RateLimited { .. } | CommandResult::Failed { .. } => {
                emit(Output::CommandFailed(CommandFailure::Other))
            }
        }
    }

    fn not_playing_for(&mut self, now: u64) -> PollState {
        let since = *self.not_playing_since.get_or_insert(now);
        PollState::NotPlaying {
            for_ms: now - since,
        }
    }

    fn schedule_poll(&mut self, now: u64, state: PollState, latest: Option<&NowPlaying>) {
        let mut delay = schedule::next_poll_ms(state, self.idle_after_ms);
        if let Some(follow_up) = &mut self.follow_up {
            match follow_up.after_poll(latest) {
                Some(quick) => delay = delay.min(quick),
                None => self.follow_up = None,
            }
        }
        self.next_poll_at = now + delay as u64;
    }

    fn retry_delay(&mut self) -> u64 {
        // xorshift32: enough randomness to spread out retries.
        self.random ^= self.random << 13;
        self.random ^= self.random >> 17;
        self.random ^= self.random << 5;
        self.backoff.next_delay_ms(self.random) as u64
    }

    fn set_online(&mut self, online: bool, emit: &mut impl FnMut(Output)) {
        if self.online != Some(online) {
            self.online = Some(online);
            emit(if online {
                Output::Online
            } else {
                Output::Offline
            });
        }
    }

    fn stop(&mut self, emit: &mut impl FnMut(Output)) {
        self.stopped = true;
        emit(Output::SignInNeeded);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use rstest::rstest;

    const TOKEN_OK: &[u8] = include_bytes!("../tests/fixtures/spotify_token_ok.json");
    const TOKEN_NO_REFRESH: &[u8] =
        include_bytes!("../tests/fixtures/spotify_token_ok_no_refresh.json");
    const TRACK: &[u8] = include_bytes!("../tests/fixtures/spotify_recorded_track.json");
    const LOCAL_FILE: &[u8] = include_bytes!("../tests/fixtures/spotify_local_file.json");
    const COVER: &[u8] = include_bytes!("../tests/fixtures/art_64.jpg");
    const FIVE_MINUTES: u64 = 5 * 60_000;

    fn sync() -> SpotifySync {
        SpotifySync::new("cid", "AQC-new-refresh-token", FIVE_MINUTES, 42)
    }

    /// Expect a request to be sent, and return it.
    fn expect_send(sync: &mut SpotifySync, now: u64) -> Request {
        match sync.next(now) {
            Next::Send(request) => request,
            other => panic!("expected a request, got {other:?}"),
        }
    }

    fn reply(sync: &mut SpotifySync, now: u64, outcome: Outcome) -> Vec<Output> {
        let mut out = Vec::new();
        sync.handle(now, outcome, &mut |o| out.push(o));
        out
    }

    /// Get through the first token refresh.
    fn signed_in() -> SpotifySync {
        let mut s = sync();
        let request = expect_send(&mut s, 0);
        assert_eq!(request.url, auth::TOKEN_URL);
        assert!(
            request
                .form_body
                .unwrap()
                .contains("grant_type=refresh_token")
        );
        reply(&mut s, 0, Ok(Response::new(200, TOKEN_NO_REFRESH)));
        s
    }

    #[test]
    fn first_poll_sends_cover_before_the_track() {
        let mut s = signed_in();
        let poll = expect_send(&mut s, 0);
        assert_eq!(poll.url, spotify::CURRENTLY_PLAYING_URL);
        assert_eq!(poll.bearer.as_deref(), Some("BQD-access-token-2"));
        assert!(reply(&mut s, 0, Ok(Response::new(200, TRACK))).is_empty());

        let art = expect_send(&mut s, 10);
        assert!(art.url.starts_with("https://i.scdn.co/image/"));
        assert_eq!(art.bearer, None);
        let out = reply(&mut s, 10, Ok(Response::new(200, COVER)));
        assert!(matches!(&out[0], Output::Art(Some(frame)) if frame.len() == image::FRAME_LEN));
        assert!(matches!(&out[1], Output::NowPlaying(Some(np)) if np.title == "Someone Like You"));

        // Same cover next time: no second download.
        match s.next(10) {
            Next::Wait(ms) => assert!(ms > 0 && ms <= 5_000),
            other => panic!("{other:?}"),
        }
        expect_send(&mut s, 5_000);
        let out = reply(&mut s, 5_000, Ok(Response::new(200, TRACK)));
        assert!(matches!(out.as_slice(), [Output::NowPlaying(Some(_))]));
    }

    #[test]
    fn item_without_art_sends_no_art_first() {
        let mut s = signed_in();
        expect_send(&mut s, 0);
        let out = reply(&mut s, 0, Ok(Response::new(200, LOCAL_FILE)));
        assert_eq!(out[0], Output::Art(None));
        assert!(matches!(&out[1], Output::NowPlaying(Some(_))));
    }

    #[test]
    fn rotated_refresh_token_is_reported() {
        let mut s = SpotifySync::new("cid", "old-token", FIVE_MINUTES, 1);
        expect_send(&mut s, 0);
        let out = reply(&mut s, 0, Ok(Response::new(200, TOKEN_OK)));
        assert!(out.contains(&Output::RefreshTokenChanged("AQC-new-refresh-token".into())));
        assert!(out.contains(&Output::Online));
    }

    #[test]
    fn rejected_refresh_token_stops() {
        let mut s = sync();
        expect_send(&mut s, 0);
        let out = reply(
            &mut s,
            0,
            Ok(Response::new(400, &br#"{"error":"invalid_grant"}"#[..])),
        );
        assert_eq!(out, vec![Output::SignInNeeded]);
        assert_eq!(s.next(0), Next::Stopped);
        s.command(Command::Play);
        assert_eq!(s.next(0), Next::Stopped);
    }

    #[test]
    fn unauthorized_poll_refreshes_then_retries() {
        let mut s = signed_in();
        expect_send(&mut s, 0);
        assert!(reply(&mut s, 0, Ok(Response::new(401, &b""[..]))).is_empty());
        let refresh = expect_send(&mut s, 0);
        assert_eq!(refresh.url, auth::TOKEN_URL);
        reply(&mut s, 0, Ok(Response::new(200, TOKEN_NO_REFRESH)));
        assert_eq!(expect_send(&mut s, 0).url, spotify::CURRENTLY_PLAYING_URL);
        // A second 401 in a row, even with a fresh token, means the
        // sign-in is gone.
        assert_eq!(
            reply(&mut s, 0, Ok(Response::new(401, &b""[..]))),
            vec![Output::SignInNeeded]
        );
        assert_eq!(s.next(0), Next::Stopped);
    }

    #[test]
    fn rate_limit_waits_as_asked() {
        let mut s = signed_in();
        expect_send(&mut s, 0);
        let mut limited = Response::new(429, &b""[..]);
        limited.retry_after = Some("12".into());
        reply(&mut s, 0, Ok(limited));
        assert_eq!(s.next(0), Next::Wait(12_000));
    }

    #[test]
    fn outage_goes_offline_backs_off_and_recovers() {
        let mut s = signed_in();
        expect_send(&mut s, 0);
        assert_eq!(reply(&mut s, 0, Err(NetworkError)), vec![Output::Offline]);
        let Next::Wait(wait) = s.next(0) else {
            panic!()
        };
        assert!((1_000..=2_000).contains(&wait), "{wait}");
        expect_send(&mut s, wait);
        let out = reply(&mut s, wait, Ok(Response::new(204, &b""[..])));
        assert_eq!(out, vec![Output::Online, Output::NowPlaying(None)]);
    }

    #[test]
    fn command_is_sent_first_then_polled_quickly() {
        let mut s = signed_in();
        expect_send(&mut s, 0);
        reply(&mut s, 0, Ok(Response::new(204, &b""[..])));
        s.command(Command::Next);
        let command = expect_send(&mut s, 100);
        assert_eq!(command.method, Method::Post);
        assert!(command.url.ends_with("/me/player/next"));
        assert_eq!(command.bearer.as_deref(), Some("BQD-access-token-2"));
        assert!(reply(&mut s, 100, Ok(Response::new(204, &b""[..]))).is_empty());
        assert_eq!(s.next(100), Next::Wait(400));
    }

    #[rstest]
    #[case::premium_required(Ok(Response::new(403, &b""[..])), CommandFailure::PremiumRequired)]
    #[case::no_active_device(Ok(Response::new(404, &b""[..])), CommandFailure::NoActiveDevice)]
    #[case::rate_limited(Ok(Response::new(429, &b""[..])), CommandFailure::Other)]
    #[case::server_error(Ok(Response::new(500, &b""[..])), CommandFailure::Other)]
    #[case::network_error(Err(NetworkError), CommandFailure::Other)]
    fn command_failures_are_reported(#[case] outcome: Outcome, #[case] failure: CommandFailure) {
        let mut s = signed_in();
        s.command(Command::Play);
        expect_send(&mut s, 0);
        assert_eq!(
            reply(&mut s, 0, outcome),
            vec![Output::CommandFailed(failure)]
        );
        // Polling carries on afterwards.
        assert_eq!(expect_send(&mut s, 0).url, spotify::CURRENTLY_PLAYING_URL);
    }

    #[test]
    fn unauthorized_command_refreshes_then_retries() {
        let mut s = signed_in();
        expect_send(&mut s, 0);
        reply(&mut s, 0, Ok(Response::new(204, &b""[..])));
        s.command(Command::Next);
        expect_send(&mut s, 100);
        assert!(reply(&mut s, 100, Ok(Response::new(401, &b""[..]))).is_empty());
        assert_eq!(expect_send(&mut s, 100).url, auth::TOKEN_URL);
        reply(&mut s, 100, Ok(Response::new(200, TOKEN_NO_REFRESH)));
        assert!(expect_send(&mut s, 100).url.ends_with("/me/player/next"));
    }

    #[test]
    fn idle_polling_follows_the_idle_time() {
        let mut s = signed_in();
        s.set_idle_after_ms(60_000);
        expect_send(&mut s, 0);
        reply(&mut s, 0, Ok(Response::new(204, &b""[..])));
        assert_eq!(s.next(0), Next::Wait(5_000));
        expect_send(&mut s, 60_000);
        reply(&mut s, 60_000, Ok(Response::new(204, &b""[..])));
        assert_eq!(s.next(60_000), Next::Wait(30_000));
    }

    #[test]
    fn token_is_refreshed_before_it_expires() {
        let mut s = signed_in();
        expect_send(&mut s, 0);
        reply(&mut s, 0, Ok(Response::new(204, &b""[..])));
        // The token lasts an hour; five minutes before that, refresh first.
        let at = (3_600 - 300) * 1000;
        assert_eq!(expect_send(&mut s, at).url, auth::TOKEN_URL);
    }
}
