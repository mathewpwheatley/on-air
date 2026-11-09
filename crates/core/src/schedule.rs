//! When to poll Spotify next.

/// Poll interval while music is playing, or stopped for less than
/// [`IDLE_AFTER_MS`].
pub const POLL_PLAYING_MS: u32 = 5_000;
/// Poll interval once the panel has gone idle.
pub const POLL_IDLE_MS: u32 = 30_000;
/// How long nothing has to play before the panel goes idle.
pub const IDLE_AFTER_MS: u64 = 120_000;
/// Poll this long after the current track should end, so the next track has
/// started by the time we ask.
pub const TRACK_END_GRACE_MS: u32 = 750;
/// Never poll faster than this, even right at a track boundary.
pub const MIN_POLL_MS: u32 = 1_000;

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

/// Delay until the next poll.
pub fn next_poll_ms(state: PollState) -> u32 {
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
        PollState::NotPlaying { for_ms } if for_ms < IDLE_AFTER_MS => POLL_PLAYING_MS,
        PollState::NotPlaying { .. } => POLL_IDLE_MS,
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

    #[test]
    fn playing_polls_every_5s_mid_track() {
        let s = PollState::Playing {
            progress_ms: 10_000,
            duration_ms: 200_000,
        };
        assert_eq!(next_poll_ms(s), 5_000);
    }

    #[test]
    fn playing_polls_just_after_track_end() {
        let s = PollState::Playing {
            progress_ms: 197_000,
            duration_ms: 200_000,
        };
        assert_eq!(next_poll_ms(s), 3_750);
    }

    #[test]
    fn never_faster_than_min_even_past_the_end() {
        let s = PollState::Playing {
            progress_ms: 200_500,
            duration_ms: 200_000,
        };
        assert_eq!(next_poll_ms(s), MIN_POLL_MS);
    }

    #[test]
    fn not_playing_slows_down_once_idle() {
        assert_eq!(next_poll_ms(PollState::NotPlaying { for_ms: 0 }), 5_000);
        assert_eq!(
            next_poll_ms(PollState::NotPlaying { for_ms: 119_999 }),
            5_000
        );
        assert_eq!(
            next_poll_ms(PollState::NotPlaying { for_ms: 120_000 }),
            30_000
        );
    }

    #[test]
    fn retry_after_wins_when_longer() {
        assert_eq!(respect_retry_after(5_000, Some(20)), 20_000);
        assert_eq!(respect_retry_after(5_000, Some(1)), 5_000);
        assert_eq!(respect_retry_after(5_000, None), 5_000);
    }
}
