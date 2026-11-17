//! Follows Spotify with `core::spotify_sync::SpotifySync`: refreshes the
//! token, polls what's playing and downloads covers, and passes what
//! changed to the display. Until the setup portal saves a sign-in, the
//! client ID and refresh token come in at build time (`SPOTIFY_CLIENT_ID`
//! and `ON_AIR_SPOTIFY_REFRESH_TOKEN`, or the token the simulator saved).

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

use crate::https::HttpsClient;
use crate::sntp_task;

const CLIENT_ID: Option<&str> = option_env!("SPOTIFY_CLIENT_ID");
const REFRESH_TOKEN: Option<&str> = option_env!("ON_AIR_SPOTIFY_REFRESH_TOKEN");

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
pub async fn spotify_task(client: &'static HttpsClient, seed: u32) {
    let (Some(client_id), Some(refresh_token)) = (CLIENT_ID, REFRESH_TOKEN) else {
        warn!(
            "spotify: no sign-in; set SPOTIFY_CLIENT_ID and sign in with `just sim --live` \
             (or set ON_AIR_SPOTIFY_REFRESH_TOKEN), then rebuild"
        );
        UPDATES.send(SpotifyUpdate::SignInNeeded).await;
        return;
    };
    // Certificates can't be checked until the clock is set.
    while sntp_task::unix_seconds().is_none() {
        Timer::after_millis(500).await;
    }

    let idle_after_ms = DEFAULT_IDLE_AFTER_MIN as u64 * 60_000;
    let mut sync = SpotifySync::new(client_id, refresh_token, idle_after_ms, seed);
    let mut last_item: Option<String> = None;
    loop {
        match sync.next(now()) {
            Next::Send(request) => {
                let outcome = client.send(&request).await;
                let mut updates = Vec::new();
                sync.handle(now(), outcome, &mut |output| {
                    if let Some(update) = report(output, &mut last_item) {
                        updates.push(update);
                    }
                });
                for update in updates {
                    UPDATES.send(update).await;
                }
            }
            // Playback buttons will wake this early once they're fitted.
            Next::Wait(ms) => Timer::after_millis(ms).await,
            Next::Stopped => {
                warn!("spotify: signed out; sign in with `just sim --live` and rebuild");
                return;
            }
        }
    }
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
        // Saving it comes with the settings storage.
        Output::RefreshTokenChanged(_) => {
            warn!("spotify: refresh token rotated (not saved yet)");
            return None;
        }
        Output::Online => {
            info!("spotify: online");
            SpotifyUpdate::Reachable(true)
        }
        Output::Offline => {
            warn!("spotify: offline");
            SpotifyUpdate::Reachable(false)
        }
        Output::SignInNeeded => {
            warn!("spotify: Spotify rejected the refresh token");
            SpotifyUpdate::SignInNeeded
        }
    })
}
