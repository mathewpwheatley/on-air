//! Messages between the window thread and the background sources.

use on_air_core::app::{CommandFailure, NetStatus};
use on_air_core::spotify::{Command, NowPlaying};
use on_air_core::weather::Reading;

/// From a source to the window.
#[derive(Debug)]
pub enum Update {
    Net(NetStatus),
    /// The current cover, 64×64 RGB888, or `None` when there's no art.
    /// Sent before the `NowPlaying` it belongs to.
    Art(Option<Vec<u8>>),
    NowPlaying(Option<NowPlaying>),
    SignInNeeded,
    SignedIn,
    CommandFailed(CommandFailure),
    Weather(Reading),
    /// IANA timezone of the weather city, used when `--timezone` isn't given.
    Timezone(String),
}

/// From the window to the music source.
#[derive(Debug, Clone, Copy)]
pub enum Control {
    Spotify(Command),
    /// Fake player only: stop playing entirely.
    StopPlayback,
}
