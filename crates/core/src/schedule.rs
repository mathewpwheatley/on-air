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
    use rstest::rstest;

    const MINUTE: u64 = 60_000;

    fn playing(progress_ms: u32) -> PollState {
        PollState::Playing {
            progress_ms,
            duration_ms: 200_000,
        }
    }

    fn stopped(for_ms: u64) -> PollState {
        PollState::NotPlaying { for_ms }
    }

    #[rstest]
    #[case::mid_track(playing(10_000), MINUTE, 5_000)]
    #[case::just_after_track_end(playing(197_000), MINUTE, 3_750)]
    #[case::never_faster_than_min_past_the_end(playing(200_500), MINUTE, MIN_POLL_MS)]
    #[case::just_stopped(stopped(0), MINUTE, 5_000)]
    #[case::stopped_not_idle_yet(stopped(59_999), MINUTE, 5_000)]
    #[case::idle(stopped(MINUTE), MINUTE, 30_000)]
    #[case::idle_before_deep_idle(stopped(30 * MINUTE - 1), MINUTE, 30_000)]
    #[case::deep_idle(stopped(30 * MINUTE), MINUTE, 60_000)]
    #[case::overnight(stopped(12 * 60 * MINUTE), MINUTE, 60_000)]
    #[case::ten_minute_setting_not_idle_yet(stopped(9 * MINUTE), 10 * MINUTE, 5_000)]
    #[case::ten_minute_setting_idle(stopped(10 * MINUTE), 10 * MINUTE, 30_000)]
    // With the longest idle time, deep idle still starts once idle.
    #[case::thirty_minute_setting_not_idle_yet(stopped(30 * MINUTE - 1), 30 * MINUTE, 5_000)]
    #[case::thirty_minute_setting_idle(stopped(30 * MINUTE), 30 * MINUTE, 60_000)]
    fn poll_interval(#[case] state: PollState, #[case] idle_after_ms: u64, #[case] expected: u32) {
        assert_eq!(next_poll_ms(state, idle_after_ms), expected);
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

    #[rstest]
    fn follow_up_gives_up_after_the_last_quick_poll(
        #[values(Command::Play, Command::Pause, Command::Next, Command::Previous)] command: Command,
    ) {
        // Whatever the button, a poll that shows no change keeps going
        // until the quick polls run out.
        let before = track("a", command == Command::Pause, 0);
        let unchanged = track("a", command == Command::Pause, 0);
        let mut f = FollowUp::new(command, Some(&before));
        let delays: Vec<_> = (0..6).map(|_| f.after_poll(Some(&unchanged))).collect();
        assert_eq!(
            delays,
            [Some(600), Some(1_000), Some(1_500), Some(2_000), None, None]
        );
    }

    #[rstest]
    #[case::next_to_a_new_song(
        Command::Next,
        track("a", true, 60_000),
        Some(track("b", true, 300))
    )]
    #[case::previous_restarts_the_same_song(
        Command::Previous,
        track("a", true, 90_000),
        Some(track("a", true, 500))
    )]
    #[case::play_starts(Command::Play, track("a", false, 1_000), Some(track("a", true, 1_200)))]
    #[case::pause_stops(
        Command::Pause,
        track("a", true, 1_000),
        Some(track("a", false, 1_000))
    )]
    #[case::pause_with_nothing_left(Command::Pause, track("a", true, 1_000), None)]
    fn follow_up_ends_once_the_change_shows(
        #[case] command: Command,
        #[case] before: NowPlaying,
        #[case] after: Option<NowPlaying>,
    ) {
        let mut f = FollowUp::new(command, Some(&before));
        assert_eq!(f.after_poll(after.as_ref()), None);
    }

    #[rstest]
    #[case::longer_wins(Some(20), 20_000)]
    #[case::shorter_ignored(Some(1), 5_000)]
    #[case::none(None, 5_000)]
    fn retry_after(#[case] retry_after_s: Option<u32>, #[case] expected: u32) {
        assert_eq!(respect_retry_after(5_000, retry_after_s), expected);
    }
}
