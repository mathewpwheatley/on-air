//! Follows Spotify with `core::spotify_sync::SpotifySync`: refreshes the
//! token, polls what's playing and downloads covers. Until the setup portal
//! saves a sign-in, the client ID and refresh token come in at build time
//! (`SPOTIFY_CLIENT_ID` and `ON_AIR_SPOTIFY_REFRESH_TOKEN`, or the token the
//! simulator saved).

use embassy_time::{Instant, Timer};
use log::{info, warn};
use on_air_core::settings::DEFAULT_IDLE_AFTER_MIN;
use on_air_core::spotify_sync::{Next, Output, SpotifySync};

use crate::https::HttpsClient;
use crate::sntp_task;

const CLIENT_ID: Option<&str> = option_env!("SPOTIFY_CLIENT_ID");
const REFRESH_TOKEN: Option<&str> = option_env!("ON_AIR_SPOTIFY_REFRESH_TOKEN");

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
        return;
    };
    // Certificates can't be checked until the clock is set.
    while sntp_task::unix_seconds().is_none() {
        Timer::after_millis(500).await;
    }

    let idle_after_ms = DEFAULT_IDLE_AFTER_MIN as u64 * 60_000;
    let mut sync = SpotifySync::new(client_id, refresh_token, idle_after_ms, seed);
    loop {
        match sync.next(now()) {
            Next::Send(request) => {
                let outcome = client.send(&request).await;
                sync.handle(now(), outcome, &mut report);
            }
            // Playback buttons will wake this early once they're fitted.
            Next::Wait(ms) => Timer::after_millis(ms).await,
            Next::Stopped => {
                warn!("spotify: signed out; sign in again and rebuild");
                return;
            }
        }
    }
}

fn report(output: Output) {
    match output {
        Output::NowPlaying(Some(playing)) => info!(
            "spotify: {} {} by {} ({} / {} s)",
            if playing.is_playing {
                "playing"
            } else {
                "paused:"
            },
            playing.title,
            playing.artist,
            playing.progress_ms / 1000,
            playing.duration_ms / 1000
        ),
        Output::NowPlaying(None) => info!("spotify: nothing playing"),
        Output::Art(Some(cover)) => info!("spotify: new cover, {} bytes decoded", cover.len()),
        Output::Art(None) => info!("spotify: no cover"),
        Output::CommandFailed(failure) => warn!("spotify: command failed: {failure:?}"),
        // Saving it comes with the settings storage.
        Output::RefreshTokenChanged(_) => warn!("spotify: refresh token rotated (not saved yet)"),
        Output::Online => info!("spotify: online"),
        Output::Offline => warn!("spotify: offline"),
        Output::SignInNeeded => warn!("spotify: Spotify rejected the refresh token"),
    }
}
