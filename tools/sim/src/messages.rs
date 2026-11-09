//! Messages between the window thread and the background sources.

use on_air_core::app::{CommandFailure, NetStatus};
use on_air_core::settings::Settings;
use on_air_core::spotify::{Command, NowPlaying};
use on_air_core::tide::{TideSupport, TideTurn};
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
    /// Whether the weather city has tide data.
    TideSupport(TideSupport),
    /// Upcoming highs and lows, in time order.
    Tides(Vec<TideTurn>),
    /// IANA timezone of the weather city, used when `--timezone` isn't given.
    Timezone(String),
    /// The setup portal saved new clock and weather settings.
    DisplaySaved(Settings),
    /// The owner pressed Finish setup.
    PortalDone,
}

/// From the window to the music source.
#[derive(Debug, Clone, Copy)]
pub enum Control {
    Spotify(Command),
    /// Fake player only: stop playing entirely.
    StopPlayback,
}
