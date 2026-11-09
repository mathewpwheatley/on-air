//! When to poll Spotify next.

use alloc::string::String;

use crate::spotify::{Command, NowPlaying};

/// Poll interval while music is playing, or stopped for less than the idle
/// time.
pub const POLL_PLAYING_MS: u32 = 5_000;
/// Poll interval once the panel has gone idle.
pub const POLL_IDLE_MS: u32 = 30_000;
/// Poll interval after a long stretch with nothing playing, e.g. overnight.
pub const POLL_DEEP_IDLE_MS: u32 = 60_000;
/// How long nothing has to play before polling slows to
/// [`POLL_DEEP_IDLE_MS`], or the idle time if that's longer.
pub const DEEP_IDLE_AFTER_MS: u64 = 30 * 60_000;
/// After a playback button, poll quickly at these gaps until Spotify shows
/// the change, then go back to the normal schedule. Spotify can take a
/// second or two to report a skip.
pub const FOLLOW_UP_DELAYS_MS: [u32; 5] = [400, 600, 1_000, 1_500, 2_000];
/// Poll this long after the current track should end, so the next track has
/// started by the time we ask.
pub const TRACK_END_GRACE_MS: u32 = 750;
/// Never poll faster than this, even right at a track boundary.
pub const MIN_POLL_MS: u32 = 1_000;

/// Quick polling after a playback button, until Spotify reports the change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FollowUp {
    expect: Expect,
    polls: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Expect {
    Playing(bool),
    /// Next or Previous: a different item, or the same one restarted
    /// (Previous partway into a song goes back to its start).
    NewTrack {
        item_id: Option<String>,
        progress_ms: u32,
    },
}

impl FollowUp {
    /// Start quick polling for `command`, given what was playing before it.
    pub fn new(command: Command, before: Option<&NowPlaying>) -> Self {
        let expect = match command {
            Command::Play => Expect::Playing(true),
            Command::Pause => Expect::Playing(false),
            Command::Next | Command::Previous => Expect::NewTrack {
                item_id: before.map(|np| np.item_id.clone()),
                progress_ms: before.map_or(0, |np| np.progress_ms),
            },
        };
        Self { expect, polls: 0 }
    }

    /// Delay before the first quick poll.
    pub fn first_delay_ms(&self) -> u32 {
        FOLLOW_UP_DELAYS_MS[0]
    }

    /// Call after each poll with what it found. Returns the delay to the
    /// next quick poll, or `None` once the change shows up (or the quick
    /// polls run out), when the normal schedule takes over.
    pub fn after_poll(&mut self, latest: Option<&NowPlaying>) -> Option<u32> {
        self.polls += 1;
        if self.seen(latest) {
            return None;
        }
        FOLLOW_UP_DELAYS_MS.get(self.polls).copied()
    }

    fn seen(&self, latest: Option<&NowPlaying>) -> bool {
        match (&self.expect, latest) {
            (Expect::Playing(playing), Some(np)) => np.is_playing == *playing,
            (Expect::Playing(playing), None) => !playing,
            (Expect::NewTrack { .. }, None) => false,
            (
                Expect::NewTrack {
                    item_id,
                    progress_ms,
                },
                Some(np),
            ) => {
                item_id.as_deref() != Some(np.item_id.as_str())
                    || np.progress_ms.saturating_add(1_500) < *progress_ms
            }
        }
    }
}

/// What the last poll found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PollState {
    Playing {
        progress_ms: u32,
        duration_ms: u32,
    },
    /// Paused or nothing playing, for this long.
    NotPlaying {
        for_ms: u64,
    },
}

/// Delay until the next poll. `idle_after_ms` is the owner's idle time
/// (`Settings::idle_after_ms`).
pub fn next_poll_ms(state: PollState, idle_after_ms: u64) -> u32 {
    match state {
        PollState::Playing {
            progress_ms,
            duration_ms,
        } => {
            let until_end = duration_ms
                .saturating_sub(progress_ms)
                .saturating_add(TRACK_END_GRACE_MS);
            until_end.clamp(MIN_POLL_MS, POLL_PLAYING_MS)
        }
        PollState::NotPlaying { for_ms } if for_ms < idle_after_ms => POLL_PLAYING_MS,
        PollState::NotPlaying { for_ms } if for_ms < DEEP_IDLE_AFTER_MS.max(idle_after_ms) => {
            POLL_IDLE_MS
        }
        PollState::NotPlaying { .. } => POLL_DEEP_IDLE_MS,
    }
}

/// Stretch a delay to honour a `429 Retry-After`.
pub fn respect_retry_after(delay_ms: u32, retry_after_s: Option<u32>) -> u32 {
    match retry_after_s {
        Some(s) => delay_ms.max(s.saturating_mul(1_000)),
        None => delay_ms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINUTE: u64 = 60_000;

    #[test]
    fn playing_polls_every_5s_mid_track() {
        let s = PollState::Playing {
            progress_ms: 10_000,
            duration_ms: 200_000,
        };
        assert_eq!(next_poll_ms(s, MINUTE), 5_000);
    }

    #[test]
    fn playing_polls_just_after_track_end() {
        let s = PollState::Playing {
            progress_ms: 197_000,
            duration_ms: 200_000,
        };
        assert_eq!(next_poll_ms(s, MINUTE), 3_750);
    }

    #[test]
    fn never_faster_than_min_even_past_the_end() {
        let s = PollState::Playing {
            progress_ms: 200_500,
            duration_ms: 200_000,
        };
        assert_eq!(next_poll_ms(s, MINUTE), MIN_POLL_MS);
    }

    #[test]
    fn not_playing_slows_down_once_idle() {
        assert_eq!(
            next_poll_ms(PollState::NotPlaying { for_ms: 0 }, MINUTE),
            5_000
        );
        assert_eq!(
            next_poll_ms(PollState::NotPlaying { for_ms: 59_999 }, MINUTE),
            5_000
        );
        assert_eq!(
            next_poll_ms(PollState::NotPlaying { for_ms: 60_000 }, MINUTE),
            30_000
        );
    }

    #[test]
    fn idle_time_comes_from_the_setting() {
        let ten_minutes = 10 * MINUTE;
        let s = |for_ms| PollState::NotPlaying { for_ms };
        assert_eq!(next_poll_ms(s(9 * MINUTE), ten_minutes), 5_000);
        assert_eq!(next_poll_ms(s(ten_minutes), ten_minutes), 30_000);
        // With the longest idle time, deep idle still starts once idle.
        let thirty_minutes = 30 * MINUTE;
        assert_eq!(next_poll_ms(s(thirty_minutes - 1), thirty_minutes), 5_000);
        assert_eq!(next_poll_ms(s(thirty_minutes), thirty_minutes), 60_000);
    }

    #[test]
    fn long_idle_slows_down_further() {
        let thirty_minutes = 30 * 60_000;
        assert_eq!(
            next_poll_ms(
                PollState::NotPlaying {
                    for_ms: thirty_minutes - 1
                },
                MINUTE,
            ),
            30_000
        );
        assert_eq!(
            next_poll_ms(
                PollState::NotPlaying {
                    for_ms: thirty_minutes
                },
                MINUTE,
            ),
            60_000
        );
        assert_eq!(
            next_poll_ms(
                PollState::NotPlaying {
                    for_ms: 12 * 60 * 60_000
                },
                MINUTE,
            ),
            60_000
        );
    }

    fn track(id: &str, playing: bool, progress_ms: u32) -> NowPlaying {
        NowPlaying {
            item_id: id.into(),
            title: id.into(),
            artist: String::new(),
            album: None,
            is_playing: playing,
            progress_ms,
            duration_ms: 200_000,
            art_url: None,
        }
    }

    #[test]
    fn follow_up_keeps_polling_until_the_skip_shows() {
        let before = track("a", true, 60_000);
        let mut f = FollowUp::new(Command::Next, Some(&before));
        assert_eq!(f.first_delay_ms(), 400);
        // Spotify still reports the old song for a while.
        assert_eq!(f.after_poll(Some(&track("a", true, 60_400))), Some(600));
        assert_eq!(f.after_poll(Some(&track("a", true, 61_000))), Some(1_000));
        // Then the new one: back to the normal schedule.
        assert_eq!(f.after_poll(Some(&track("b", true, 300))), None);
    }

    #[test]
    fn follow_up_gives_up_after_the_last_quick_poll() {
        let before = track("a", true, 0);
        let mut f = FollowUp::new(Command::Next, Some(&before));
        let delays: Vec<_> = (0..6).map(|_| f.after_poll(Some(&before))).collect();
        assert_eq!(
            delays,
            [Some(600), Some(1_000), Some(1_500), Some(2_000), None, None]
        );
    }

    #[test]
    fn previous_partway_through_restarts_the_same_song() {
        let before = track("a", true, 90_000);
        let mut f = FollowUp::new(Command::Previous, Some(&before));
        assert_eq!(f.after_poll(Some(&track("a", true, 500))), None);
    }

    #[test]
    fn play_and_pause_wait_for_the_playing_state() {
        let paused = track("a", false, 1_000);
        let mut f = FollowUp::new(Command::Play, Some(&paused));
        assert_eq!(f.after_poll(Some(&paused)), Some(600));
        assert_eq!(f.after_poll(Some(&track("a", true, 1_200))), None);

        let playing = track("a", true, 1_000);
        let mut f = FollowUp::new(Command::Pause, Some(&playing));
        assert_eq!(f.after_poll(None), None);
    }

    #[test]
    fn retry_after_wins_when_longer() {
        assert_eq!(respect_retry_after(5_000, Some(20)), 20_000);
        assert_eq!(respect_retry_after(5_000, Some(1)), 5_000);
        assert_eq!(respect_retry_after(5_000, None), 5_000);
    }
}
