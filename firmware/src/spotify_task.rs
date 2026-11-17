//! Follows Spotify with `core::spotify_sync::SpotifySync`: refreshes the
//! token, polls what's playing and downloads covers, and passes what
//! changed to the display. Rotated refresh tokens are saved to flash at
//! once. The sign-in comes from the setup portal, or in development from
//! `.env` and the simulator's sign-in (see `config`).

use alloc::string::String;
use alloc::vec::Vec;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_time::{Instant, Timer};
use log::{info, warn};
use on_air_core::app::CommandFailure;
use on_air_core::settings::DEFAULT_IDLE_AFTER_MIN;
use on_air_core::spotify::NowPlaying;
use on_air_core::spotify_sync::{Next, Output, SpotifySync};

use crate::config::{self, Effective};
use crate::https::HttpsClient;
use crate::sntp_task;
use crate::storage::SettingsStore;

/// What the display needs to know from Spotify.
pub enum SpotifyUpdate {
    /// The current cover, 64×64 RGB888. Sent before the `NowPlaying` it
    /// belongs to.
    Art(Option<Vec<u8>>),
    NowPlaying(Option<NowPlaying>),
    /// Whether Spotify can be reached.
    Reachable(bool),
    SignInNeeded,
    CommandFailed(CommandFailure),
}

/// Spotify updates, for the display task.
pub static UPDATES: Channel<CriticalSectionRawMutex, SpotifyUpdate, 8> = Channel::new();

fn now() -> u64 {
    Instant::now().as_millis()
}

#[embassy_executor::task]
pub async fn spotify_task(
    client: &'static HttpsClient,
    store: Option<&'static SettingsStore>,
    settings: &'static Effective,
    seed: u32,
) {
    let client_id = settings.settings.spotify_client_id.as_str();
    // The saved token first: it's the newest if Spotify has rotated it. Then,
    // in development, the one from the build.
    let mut tokens: Vec<(&str, String)> = Vec::new();
    if let Some(token) = &settings.settings.spotify_refresh_token {
        tokens.push(("saved", token.clone()));
    }
    if !settings.from_portal
        && let Some(token) = config::SPOTIFY_REFRESH_TOKEN
        && !tokens.iter().any(|(_, t)| t == token)
    {
        tokens.push(("build-time", token.into()));
    }
    if client_id.is_empty() || tokens.is_empty() {
        warn!("spotify: not signed in; sign in from the setup portal");
        UPDATES.send(SpotifyUpdate::SignInNeeded).await;
        return;
    }

    // Certificates can't be checked until the clock is set.
    while sntp_task::unix_seconds().is_none() {
        Timer::after_millis(500).await;
    }
    for (source, token) in tokens {
        info!("spotify: signing in with the {source} refresh token");
        follow(client, store, client_id, &token, seed).await;
        warn!("spotify: Spotify rejected the {source} refresh token");
    }
    warn!("spotify: signed out; sign in again from the setup portal");
    UPDATES.send(SpotifyUpdate::SignInNeeded).await;
}

/// Follow Spotify with one refresh token, until Spotify rejects it.
async fn follow(
    client: &HttpsClient,
    store: Option<&SettingsStore>,
    client_id: &str,
    refresh_token: &str,
    seed: u32,
) {
    let idle_after_ms = DEFAULT_IDLE_AFTER_MIN as u64 * 60_000;
    let mut sync = SpotifySync::new(client_id, refresh_token, idle_after_ms, seed);
    let mut last_item: Option<String> = None;
    loop {
        match sync.next(now()) {
            Next::Send(request) => {
                let outcome = client.send(&request).await;
                let mut outputs = Vec::new();
                sync.handle(now(), outcome, &mut |output| outputs.push(output));
                for output in outputs {
                    if let Output::RefreshTokenChanged(token) = &output {
                        save_token(store, client_id, token).await;
                    }
                    if let Some(update) = report(output, &mut last_item) {
                        UPDATES.send(update).await;
                    }
                }
            }
            // Playback buttons will wake this early once they're fitted.
            Next::Wait(ms) => Timer::after_millis(ms).await,
            Next::Stopped => return,
        }
    }
}

/// Save a rotated refresh token right away: Spotify may already have
/// retired the old one.
async fn save_token(store: Option<&SettingsStore>, client_id: &str, token: &str) {
    let Some(store) = store else {
        warn!("spotify: refresh token rotated, but there's no storage to save it in");
        return;
    };
    info!("spotify: refresh token rotated; saving it");
    store
        .update(|settings| {
            settings.spotify_client_id = client_id.into();
            settings.spotify_refresh_token = Some(token.into());
        })
        .await;
}

/// Log an output and turn it into an update for the display. A track is
/// logged only when it changes.
fn report(output: Output, last_item: &mut Option<String>) -> Option<SpotifyUpdate> {
    Some(match output {
        Output::NowPlaying(playing) => {
            let item = playing.as_ref().map(|p| p.item_id.clone());
            if item != *last_item {
                match &playing {
                    Some(p) => info!("spotify: {} by {}", p.title, p.artist),
                    None => info!("spotify: nothing playing"),
                }
                *last_item = item;
            }
            SpotifyUpdate::NowPlaying(playing)
        }
        Output::Art(cover) => SpotifyUpdate::Art(cover),
        Output::CommandFailed(failure) => {
            warn!("spotify: command failed: {failure:?}");
            SpotifyUpdate::CommandFailed(failure)
        }
        // Saved by `follow`.
        Output::RefreshTokenChanged(_) => return None,
        Output::Online => {
            info!("spotify: online");
            SpotifyUpdate::Reachable(true)
        }
        Output::Offline => {
            warn!("spotify: offline");
            SpotifyUpdate::Reachable(false)
        }
        // `spotify_task` tries the next token first, and tells the display
        // only when none is left.
        Output::SignInNeeded => return None,
    })
}
