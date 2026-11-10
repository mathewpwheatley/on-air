//! The WiFi connection, without the radio: decides when to join the home
//! network, when to retry, and when to fall back to the setup hotspot. The
//! host does the joining and reports back.
//!
//! ```text
//! loop {
//!     match wifi.next(now) {
//!         WifiNext::Join => start_joining(),       // then report Joined or Failed
//!         WifiNext::StartPortal => start_hotspot(),
//!         WifiNext::Wait(ms) => {}                 // sleep, waking early on a WiFi event
//!         WifiNext::Idle => {}                     // wait for a WiFi event
//!     }
//!     app.handle(Event::Net(wifi.status()), now);
//! }
//! ```
//!
//! - No saved network: straight to the setup hotspot.
//! - At boot, if the saved network can't be joined within 30 s, the setup
//!   hotspot starts, so a changed password can be fixed from a phone.
//! - Once it has been online, a dropped connection is retried with backoff
//!   (up to 60 s) for as long as it takes, without opening the hotspot.

use crate::app::NetStatus;
use crate::backoff::Backoff;

/// How long to keep trying the saved network at boot before starting the
/// setup hotspot.
pub const BOOT_JOIN_TIMEOUT_MS: u64 = 30_000;
/// How long one join attempt may take before it counts as failed.
pub const JOIN_ATTEMPT_MS: u64 = 15_000;

/// What the host should do next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WifiNext {
    /// Start joining the saved network, then report [`WifiEvent::Joined`]
    /// or [`WifiEvent::Failed`].
    Join,
    /// Start the setup hotspot, DNS and portal.
    StartPortal,
    /// Nothing to do for this many milliseconds, unless a WiFi event comes.
    Wait(u64),
    /// Nothing to do until a WiFi event comes.
    Idle,
}

/// What the radio reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WifiEvent {
    Joined,
    /// A join attempt failed, or the connection dropped.
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Booting: trying the saved network, until `deadline`.
    Connecting {
        deadline: u64,
    },
    Online,
    /// Was online and dropped.
    Lost,
    Portal {
        started: bool,
    },
}

#[derive(Debug, Clone)]
pub struct WifiLink {
    state: State,
    /// When the current join attempt started, if one is running.
    joining_since: Option<u64>,
    retry_at: u64,
    backoff: Backoff,
    random: u32,
}

impl WifiLink {
    /// `has_network` is whether a home network is saved. `seed` is any
    /// random number, used to spread out retries.
    pub fn new(has_network: bool, now: u64, seed: u32) -> Self {
        let state = if has_network {
            State::Connecting {
                deadline: now + BOOT_JOIN_TIMEOUT_MS,
            }
        } else {
            State::Portal { started: false }
        };
        Self {
            state,
            joining_since: None,
            retry_at: now,
            backoff: Backoff::wifi(),
            random: seed | 1,
        }
    }

    /// For the app's status screens and the "No WiFi" badge.
    pub fn status(&self) -> NetStatus {
        match self.state {
            State::Connecting { .. } => NetStatus::Connecting,
            State::Online => NetStatus::Online,
            State::Lost => NetStatus::Lost,
            State::Portal { .. } => NetStatus::Portal,
        }
    }

    /// The owner held Mode to reopen setup.
    pub fn open_portal(&mut self) {
        if !matches!(self.state, State::Portal { .. }) {
            self.state = State::Portal { started: false };
            self.joining_since = None;
        }
    }

    pub fn next(&mut self, now: u64) -> WifiNext {
        match self.state {
            State::Portal { started: false } => {
                self.state = State::Portal { started: true };
                return WifiNext::StartPortal;
            }
            State::Portal { started: true } | State::Online => return WifiNext::Idle,
            State::Connecting { deadline } if now >= deadline => {
                self.state = State::Portal { started: true };
                self.joining_since = None;
                return WifiNext::StartPortal;
            }
            State::Connecting { .. } | State::Lost => {}
        }

        if let Some(since) = self.joining_since {
            if now < since + JOIN_ATTEMPT_MS {
                return self.wait_until(now, since + JOIN_ATTEMPT_MS);
            }
            // The attempt never reported back: treat it as failed.
            self.handle(now, WifiEvent::Failed);
        }
        if now >= self.retry_at {
            self.joining_since = Some(now);
            return WifiNext::Join;
        }
        self.wait_until(now, self.retry_at)
    }

    pub fn handle(&mut self, now: u64, event: WifiEvent) {
        if matches!(self.state, State::Portal { .. }) {
            return;
        }
        self.joining_since = None;
        match event {
            WifiEvent::Joined => {
                self.state = State::Online;
                self.backoff.reset();
            }
            WifiEvent::Failed => {
                if self.state == State::Online {
                    self.state = State::Lost;
                }
                self.retry_at = now + self.backoff.next_delay_from(&mut self.random) as u64;
            }
        }
    }

    /// Wait until `at`, but no later than the boot deadline.
    fn wait_until(&self, now: u64, at: u64) -> WifiNext {
        let at = match self.state {
            State::Connecting { deadline } => at.min(deadline),
            _ => at,
        };
        WifiNext::Wait(at.saturating_sub(now))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    fn link() -> WifiLink {
        WifiLink::new(true, 0, 7)
    }

    #[test]
    fn no_saved_network_starts_the_portal_once() {
        let mut w = WifiLink::new(false, 0, 7);
        assert_eq!(w.status(), NetStatus::Portal);
        assert_eq!(w.next(0), WifiNext::StartPortal);
        assert_eq!(w.next(0), WifiNext::Idle);
        // Radio events don't leave the portal.
        w.handle(10, WifiEvent::Joined);
        assert_eq!(w.status(), NetStatus::Portal);
    }

    #[test]
    fn joins_at_boot() {
        let mut w = link();
        assert_eq!(w.status(), NetStatus::Connecting);
        assert_eq!(w.next(0), WifiNext::Join);
        assert_eq!(w.next(10), WifiNext::Wait(JOIN_ATTEMPT_MS - 10));
        w.handle(2_000, WifiEvent::Joined);
        assert_eq!(w.status(), NetStatus::Online);
        assert_eq!(w.next(2_000), WifiNext::Idle);
    }

    #[test]
    fn boot_retries_then_falls_back_to_the_portal_after_30_s() {
        let mut w = link();
        let mut now = 0;
        let mut joins = 0;
        loop {
            match w.next(now) {
                WifiNext::Join => {
                    joins += 1;
                    now += 1_000;
                    w.handle(now, WifiEvent::Failed);
                }
                WifiNext::Wait(ms) => {
                    assert!(ms > 0);
                    now += ms;
                }
                WifiNext::StartPortal => break,
                WifiNext::Idle => panic!("stuck at {now}"),
            }
        }
        assert_eq!(now, BOOT_JOIN_TIMEOUT_MS);
        assert!(joins >= 3, "{joins}");
        assert_eq!(w.status(), NetStatus::Portal);
    }

    #[test]
    fn a_join_that_never_reports_back_counts_as_failed() {
        let mut w = link();
        assert_eq!(w.next(0), WifiNext::Join);
        // No event. Once the attempt times out, it waits to retry.
        let WifiNext::Wait(_) = w.next(JOIN_ATTEMPT_MS) else {
            panic!()
        };
        assert_eq!(w.status(), NetStatus::Connecting);
    }

    #[test]
    fn dropped_connection_retries_with_backoff_and_never_opens_the_portal() {
        let mut w = link();
        w.next(0);
        w.handle(1_000, WifiEvent::Joined);
        w.handle(5_000, WifiEvent::Failed);
        assert_eq!(w.status(), NetStatus::Lost);

        let mut now = 5_000;
        let mut waits = Vec::new();
        for _ in 0..12 {
            loop {
                match w.next(now) {
                    WifiNext::Wait(ms) => now += ms,
                    WifiNext::Join => break,
                    other => panic!("{other:?} at {now}"),
                }
            }
            let failed_at = now;
            w.handle(now, WifiEvent::Failed);
            let WifiNext::Wait(ms) = w.next(now) else {
                panic!()
            };
            waits.push(ms);
            now = failed_at + ms;
        }
        assert_eq!(w.status(), NetStatus::Lost);
        assert!(waits.iter().all(|&ms| ms <= 60_000), "{waits:?}");
        // Grows, then settles in the upper half of 60 s.
        assert!(waits[0] <= 2_000 && waits[11] >= 30_000, "{waits:?}");

        w.handle(now, WifiEvent::Joined);
        assert_eq!(w.status(), NetStatus::Online);
    }

    #[rstest]
    #[case::connecting(|_: &mut WifiLink| {})]
    #[case::online(|w: &mut WifiLink| w.handle(0, WifiEvent::Joined))]
    #[case::lost(|w: &mut WifiLink| {
        w.handle(0, WifiEvent::Joined);
        w.handle(0, WifiEvent::Failed);
    })]
    fn holding_mode_opens_the_portal(#[case] setup: fn(&mut WifiLink)) {
        let mut w = link();
        setup(&mut w);
        w.open_portal();
        assert_eq!(w.status(), NetStatus::Portal);
        assert_eq!(w.next(100), WifiNext::StartPortal);
        assert_eq!(w.next(100), WifiNext::Idle);
    }

    #[test]
    fn reconnect_after_a_drop_starts_from_a_short_delay() {
        let mut w = link();
        w.next(0);
        w.handle(0, WifiEvent::Joined);
        w.handle(1_000, WifiEvent::Failed);
        let WifiNext::Wait(first) = w.next(1_000) else {
            panic!()
        };
        assert!((500..=1_000).contains(&first), "{first}");
    }
}
