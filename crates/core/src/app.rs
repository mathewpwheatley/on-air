//! The main state machine: decides what the panel shows and what the
//! buttons do.
//!
//! Pure and clock-free: every call takes the current time in milliseconds
//! from any monotonic clock.

use crate::input::{Button, InputEvent};
use crate::settings::IdleMode;
use crate::spotify::Command;

/// How long a button icon stays on screen.
pub const FEEDBACK_MS: u64 = 500;
/// How long an error icon stays on screen.
pub const NOTICE_MS: u64 = 2_000;
/// How long the song title, artist and album show.
pub const TRACK_INFO_MS: u64 = 5_000;
/// How long the mode name and position dots show after a Mode tap.
pub const MODE_LABEL_MS: u64 = 1_500;
/// How long a Mode tap previews the next idle screen over album art,
/// after the mode name.
pub const PREVIEW_MS: u64 = 2_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetStatus {
    Connecting,
    Online,
    Lost,
    Portal,
}

/// What the latest Spotify poll found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Playback {
    /// `has_art` is true once the cover is decoded and ready to show.
    Track {
        is_playing: bool,
        has_art: bool,
    },
    Nothing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandFailure {
    PremiumRequired,
    NoActiveDevice,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Net(NetStatus),
    Playback(Playback),
    /// The refresh token was rejected; the owner has to sign in again.
    SignInNeeded,
    SignedIn,
    Input(InputEvent),
    CommandFailed(CommandFailure),
}

/// Something for another task to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Spotify(Command),
    SaveIdleMode(IdleMode),
    OpenPortal,
    FactoryReset,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Art { dimmed: bool },
    Idle(IdleMode),
    Setup,
    Connecting,
    SignInNeeded,
}

/// Shown over the current screen for a moment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Notice {
    /// The idle mode's name and its position in the cycle. Covers the
    /// whole screen.
    Mode(IdleMode),
    /// Title, artist and album over the dimmed cover. Covers the whole
    /// screen.
    TrackInfo,
    Play,
    Pause,
    Next,
    Previous,
    PremiumNeeded,
    NoDevice,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct View {
    pub screen: Screen,
    pub notice: Option<Notice>,
    /// How long the notice has been showing, for scrolling text.
    pub notice_age_ms: u64,
    /// Show the "No WiFi" badge.
    pub no_wifi: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlayState {
    Unknown,
    Playing {
        has_art: bool,
    },
    /// Paused or nothing playing since `since`. `has_art` is whether the last
    /// cover is still available to show dimmed.
    Stopped {
        has_art: bool,
        since: u64,
    },
}

#[derive(Debug, Clone)]
pub struct App {
    net: NetStatus,
    ever_online: bool,
    signed_in: bool,
    play: PlayState,
    idle_mode: IdleMode,
    idle_after_ms: u64,
    preview_until: u64,
    /// The notice, when it started and when it ends.
    notice: Option<(Notice, u64, u64)>,
}

impl App {
    /// Starts with the default idle time; see [`App::set_idle_after_ms`].
    pub fn new(idle_mode: IdleMode) -> Self {
        Self {
            net: NetStatus::Connecting,
            ever_online: false,
            signed_in: true,
            play: PlayState::Unknown,
            idle_mode,
            idle_after_ms: crate::settings::DEFAULT_IDLE_AFTER_MIN as u64 * 60_000,
            preview_until: 0,
            notice: None,
        }
    }

    /// How long nothing has to play before the idle screen shows.
    pub fn set_idle_after_ms(&mut self, idle_after_ms: u64) {
        self.idle_after_ms = idle_after_ms;
    }

    pub fn idle_mode(&self) -> IdleMode {
        self.idle_mode
    }

    /// Whether Spotify reports music playing right now.
    pub fn is_playing(&self) -> bool {
        matches!(self.play, PlayState::Playing { .. })
    }

    pub fn handle(&mut self, event: Event, now: u64) -> Option<Action> {
        match event {
            Event::Net(status) => {
                self.net = status;
                if status == NetStatus::Online {
                    self.ever_online = true;
                }
                None
            }
            Event::Playback(p) => {
                self.on_playback(p, now);
                None
            }
            Event::SignInNeeded => {
                self.signed_in = false;
                None
            }
            Event::SignedIn => {
                self.signed_in = true;
                None
            }
            Event::Input(input) => self.on_input(input, now),
            Event::CommandFailed(f) => {
                let notice = match f {
                    CommandFailure::PremiumRequired => Notice::PremiumNeeded,
                    CommandFailure::NoActiveDevice => Notice::NoDevice,
                    CommandFailure::Other => Notice::Error,
                };
                self.show(notice, now, NOTICE_MS);
                None
            }
        }
    }

    fn show(&mut self, notice: Notice, now: u64, duration_ms: u64) {
        self.notice = Some((notice, now, now + duration_ms));
    }

    fn on_playback(&mut self, p: Playback, now: u64) {
        self.play = match (p, self.play) {
            (
                Playback::Track {
                    is_playing: true,
                    has_art,
                },
                _,
            ) => PlayState::Playing { has_art },
            // Already stopped: keep the original stop time so the idle timer
            // isn't reset by every poll.
            (
                Playback::Track {
                    is_playing: false,
                    has_art,
                },
                PlayState::Stopped { since, .. },
            ) => PlayState::Stopped { has_art, since },
            (
                Playback::Track {
                    is_playing: false,
                    has_art,
                },
                _,
            ) => PlayState::Stopped {
                has_art,
                since: now,
            },
            (Playback::Nothing, PlayState::Stopped { has_art, since }) => {
                PlayState::Stopped { has_art, since }
            }
            (Playback::Nothing, PlayState::Playing { has_art }) => PlayState::Stopped {
                has_art,
                since: now,
            },
            // Nothing playing at boot: no stale art to show, so go idle now.
            (Playback::Nothing, PlayState::Unknown) => PlayState::Stopped {
                has_art: false,
                since: now,
            },
        };
    }

    fn on_input(&mut self, input: InputEvent, now: u64) -> Option<Action> {
        match input {
            InputEvent::Tap(Button::PlayPause) => {
                let (command, notice) = if self.is_playing() {
                    (Command::Pause, Notice::Pause)
                } else {
                    (Command::Play, Notice::Play)
                };
                self.feedback(notice, now);
                Some(Action::Spotify(command))
            }
            InputEvent::Tap(Button::Next) => {
                self.feedback(Notice::Next, now);
                Some(Action::Spotify(Command::Next))
            }
            InputEvent::Tap(Button::Previous) => {
                self.feedback(Notice::Previous, now);
                Some(Action::Spotify(Command::Previous))
            }
            InputEvent::Tap(Button::Mode) => {
                self.idle_mode = self.idle_mode.next();
                self.show(Notice::Mode(self.idle_mode), now, MODE_LABEL_MS);
                if matches!(self.base_screen(now), Screen::Art { .. }) {
                    self.preview_until = now + MODE_LABEL_MS + PREVIEW_MS;
                }
                Some(Action::SaveIdleMode(self.idle_mode))
            }
            InputEvent::ShowTrackInfo => {
                self.show(Notice::TrackInfo, now, TRACK_INFO_MS);
                None
            }
            InputEvent::OpenPortal => Some(Action::OpenPortal),
            InputEvent::FactoryReset => Some(Action::FactoryReset),
        }
    }

    /// Show a button icon. While the panel is in Off mode, show the mode
    /// name instead, so a press makes it clear the device is on.
    fn feedback(&mut self, notice: Notice, now: u64) {
        if self.base_screen(now) == Screen::Idle(IdleMode::Off) {
            self.show(Notice::Mode(IdleMode::Off), now, MODE_LABEL_MS);
        } else {
            self.show(notice, now, FEEDBACK_MS);
        }
    }

    /// The screen ignoring the Mode preview.
    fn base_screen(&self, now: u64) -> Screen {
        if self.net == NetStatus::Portal {
            return Screen::Setup;
        }
        if !self.ever_online {
            return Screen::Connecting;
        }
        if !self.signed_in {
            return Screen::SignInNeeded;
        }
        match self.play {
            PlayState::Playing { has_art: true } => Screen::Art { dimmed: false },
            PlayState::Stopped {
                has_art: true,
                since,
            } if now.saturating_sub(since) < self.idle_after_ms => Screen::Art { dimmed: true },
            _ => Screen::Idle(self.idle_mode),
        }
    }

    /// What to draw right now.
    pub fn view(&self, now: u64) -> View {
        let mut screen = self.base_screen(now);
        if matches!(screen, Screen::Art { .. }) && now < self.preview_until {
            screen = Screen::Idle(self.idle_mode);
        }
        let notice = self.notice.filter(|&(_, _, until)| now < until);
        View {
            screen,
            notice: notice.map(|(n, _, _)| n),
            notice_age_ms: notice.map_or(0, |(_, start, _)| now - start),
            no_wifi: self.net == NetStatus::Lost,
        }
    }

    /// The next time `view` could change on its own (a notice or preview
    /// ending, or going idle), so the display task knows when to wake up.
    pub fn next_change(&self, now: u64) -> Option<u64> {
        let mut times = [None; 3];
        times[0] = self.notice.map(|(_, _, until)| until).filter(|&t| t > now);
        times[1] = Some(self.preview_until).filter(|&t| t > now);
        if let PlayState::Stopped {
            has_art: true,
            since,
        } = self.play
        {
            times[2] = Some(since + self.idle_after_ms).filter(|&t| t > now);
        }
        times.into_iter().flatten().min()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ART: Screen = Screen::Art { dimmed: false };
    const DIM: Screen = Screen::Art { dimmed: true };

    /// Online, with a 1-minute idle time to keep the numbers short.
    fn online(mode: IdleMode) -> App {
        let mut app = App::new(mode);
        app.set_idle_after_ms(60_000);
        app.handle(Event::Net(NetStatus::Online), 0);
        app
    }

    fn track(is_playing: bool) -> Event {
        Event::Playback(Playback::Track {
            is_playing,
            has_art: true,
        })
    }

    fn tap(b: Button) -> Event {
        Event::Input(InputEvent::Tap(b))
    }

    #[test]
    fn boot_shows_connecting_until_online() {
        let mut app = App::new(IdleMode::DigitalClock);
        assert_eq!(app.view(0).screen, Screen::Connecting);
        app.handle(Event::Net(NetStatus::Online), 10);
        assert_eq!(app.view(10).screen, Screen::Idle(IdleMode::DigitalClock));
    }

    #[test]
    fn boot_nothing_playing_goes_straight_to_idle() {
        let mut app = online(IdleMode::AnalogClock);
        app.handle(Event::Playback(Playback::Nothing), 100);
        assert_eq!(app.view(100).screen, Screen::Idle(IdleMode::AnalogClock));
    }

    #[test]
    fn boot_track_playing_shows_art() {
        let mut app = online(IdleMode::DigitalClock);
        app.handle(track(true), 100);
        assert_eq!(app.view(100).screen, ART);
    }

    #[test]
    fn pause_dims_then_play_restores() {
        let mut app = online(IdleMode::DigitalClock);
        app.handle(track(true), 0);
        app.handle(track(false), 1_000);
        assert_eq!(app.view(1_000).screen, DIM);
        app.handle(track(true), 2_000);
        assert_eq!(app.view(2_000).screen, ART);
    }

    #[test]
    fn paused_goes_idle_after_timeout_even_with_repeated_polls() {
        let mut app = online(IdleMode::Weather);
        app.handle(track(true), 0);
        app.handle(track(false), 1_000);
        for t in (6_000..61_000).step_by(5_000) {
            app.handle(track(false), t);
        }
        assert_eq!(app.view(60_999).screen, DIM);
        assert_eq!(app.view(61_000).screen, Screen::Idle(IdleMode::Weather));
    }

    #[test]
    fn nothing_playing_after_track_dims_then_goes_idle() {
        let mut app = online(IdleMode::DigitalClock);
        app.handle(track(true), 0);
        app.handle(Event::Playback(Playback::Nothing), 5_000);
        assert_eq!(app.view(5_000).screen, DIM);
        assert_eq!(
            app.view(65_000).screen,
            Screen::Idle(IdleMode::DigitalClock)
        );
    }

    #[test]
    fn playback_starting_from_idle_shows_art() {
        let mut app = online(IdleMode::DigitalClock);
        app.handle(Event::Playback(Playback::Nothing), 0);
        app.handle(track(true), 500_000);
        assert_eq!(app.view(500_000).screen, ART);
    }

    #[test]
    fn playing_without_art_yet_stays_on_idle_screen() {
        let mut app = online(IdleMode::DigitalClock);
        app.handle(
            Event::Playback(Playback::Track {
                is_playing: true,
                has_art: false,
            }),
            0,
        );
        assert_eq!(app.view(0).screen, Screen::Idle(IdleMode::DigitalClock));
    }

    #[test]
    fn mode_tap_cycles_idle_screen_and_saves() {
        let mut app = online(IdleMode::AnalogClock);
        app.handle(Event::Playback(Playback::Nothing), 0);
        let action = app.handle(tap(Button::Mode), 10);
        assert_eq!(action, Some(Action::SaveIdleMode(IdleMode::DigitalClock)));
        assert_eq!(app.view(10).screen, Screen::Idle(IdleMode::DigitalClock));
        assert_eq!(
            app.view(10).notice,
            Some(Notice::Mode(IdleMode::DigitalClock))
        );
        assert_eq!(app.view(1_510).notice, None);
    }

    #[test]
    fn play_hold_shows_track_info() {
        let mut app = online(IdleMode::DigitalClock);
        app.handle(track(true), 0);
        assert_eq!(
            app.handle(Event::Input(InputEvent::ShowTrackInfo), 50),
            None
        );
        assert_eq!(app.view(50).notice, Some(Notice::TrackInfo));
        assert_eq!(app.view(2_050).notice_age_ms, 2_000);
        assert_eq!(app.view(5_050).notice, None);
        // No play/pause command from the hold.
        assert!(app.is_playing());
    }

    #[test]
    fn button_press_while_off_shows_mode_name() {
        let mut app = online(IdleMode::Off);
        app.handle(Event::Playback(Playback::Nothing), 0);
        assert_eq!(
            app.handle(tap(Button::Next), 10),
            Some(Action::Spotify(Command::Next))
        );
        assert_eq!(app.view(10).notice, Some(Notice::Mode(IdleMode::Off)));
        assert_eq!(app.view(1_510).notice, None);
        assert_eq!(app.view(1_510).screen, Screen::Idle(IdleMode::Off));
    }

    #[test]
    fn mode_tap_over_art_previews_then_returns() {
        let mut app = online(IdleMode::AnalogClock);
        app.handle(track(true), 0);
        app.handle(tap(Button::Mode), 1_000);
        assert_eq!(
            app.view(1_000).notice,
            Some(Notice::Mode(IdleMode::DigitalClock))
        );
        assert_eq!(app.view(3_000).screen, Screen::Idle(IdleMode::DigitalClock));
        // Another tap during the preview moves on and restarts the timer.
        app.handle(tap(Button::Mode), 3_000);
        assert_eq!(app.view(6_000).screen, Screen::Idle(IdleMode::Weather));
        assert_eq!(app.view(6_500).screen, ART);
    }

    #[test]
    fn play_button_toggles_based_on_state() {
        let mut app = online(IdleMode::DigitalClock);
        app.handle(track(true), 0);
        assert_eq!(
            app.handle(tap(Button::PlayPause), 10),
            Some(Action::Spotify(Command::Pause))
        );
        assert_eq!(app.view(10).notice, Some(Notice::Pause));
        assert_eq!(app.view(510).notice, None);

        app.handle(track(false), 1_000);
        assert_eq!(
            app.handle(tap(Button::PlayPause), 1_010),
            Some(Action::Spotify(Command::Play))
        );
        assert_eq!(app.view(1_010).notice, Some(Notice::Play));
    }

    #[test]
    fn next_and_previous() {
        let mut app = online(IdleMode::DigitalClock);
        assert_eq!(
            app.handle(tap(Button::Next), 0),
            Some(Action::Spotify(Command::Next))
        );
        assert_eq!(app.view(0).notice, Some(Notice::Next));
        assert_eq!(
            app.handle(tap(Button::Previous), 0),
            Some(Action::Spotify(Command::Previous))
        );
        assert_eq!(app.view(0).notice, Some(Notice::Previous));
    }

    #[test]
    fn holds_become_actions() {
        let mut app = online(IdleMode::DigitalClock);
        assert_eq!(
            app.handle(Event::Input(InputEvent::OpenPortal), 0),
            Some(Action::OpenPortal)
        );
        assert_eq!(
            app.handle(Event::Input(InputEvent::FactoryReset), 0),
            Some(Action::FactoryReset)
        );
    }

    #[test]
    fn command_failures_show_a_notice() {
        let mut app = online(IdleMode::DigitalClock);
        app.handle(Event::CommandFailed(CommandFailure::PremiumRequired), 0);
        assert_eq!(app.view(1_999).notice, Some(Notice::PremiumNeeded));
        assert_eq!(app.view(2_000).notice, None);
    }

    #[test]
    fn status_screens_take_priority() {
        let mut app = online(IdleMode::DigitalClock);
        app.handle(track(true), 0);
        app.handle(Event::SignInNeeded, 0);
        assert_eq!(app.view(0).screen, Screen::SignInNeeded);
        app.handle(Event::SignedIn, 0);
        assert_eq!(app.view(0).screen, ART);
        app.handle(Event::Net(NetStatus::Portal), 0);
        assert_eq!(app.view(0).screen, Screen::Setup);
    }

    #[test]
    fn lost_wifi_keeps_screen_and_shows_badge() {
        let mut app = online(IdleMode::DigitalClock);
        app.handle(Event::Playback(Playback::Nothing), 0);
        app.handle(Event::Net(NetStatus::Lost), 10);
        let v = app.view(10);
        assert_eq!(v.screen, Screen::Idle(IdleMode::DigitalClock));
        assert!(v.no_wifi);
    }

    #[test]
    fn next_change_reports_soonest_timer() {
        let mut app = online(IdleMode::DigitalClock);
        app.handle(track(true), 0);
        assert_eq!(app.next_change(0), None);
        app.handle(track(false), 1_000);
        assert_eq!(app.next_change(1_000), Some(61_000));
        app.handle(tap(Button::Next), 2_000);
        assert_eq!(app.next_change(2_000), Some(2_500));
    }
}
