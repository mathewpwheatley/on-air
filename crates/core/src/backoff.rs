//! Exponential backoff with jitter, shared by WiFi, Spotify and weather
//! retries.

/// Retry delays that double on each failure up to a ceiling. Each delay is
/// picked at random from the upper half of the current window, so a fleet of
/// retries (or one device after a router reboot) doesn't hit in lockstep.
#[derive(Debug, Clone)]
pub struct Backoff {
    base_ms: u32,
    max_ms: u32,
    attempt: u32,
}

impl Backoff {
    pub const fn new(base_ms: u32, max_ms: u32) -> Self {
        Self {
            base_ms,
            max_ms,
            attempt: 0,
        }
    }

    /// WiFi reconnects: 1 s doubling to 60 s.
    pub const fn wifi() -> Self {
        Self::new(1_000, 60_000)
    }

    /// Spotify outages: 2 s doubling to 60 s.
    pub const fn spotify() -> Self {
        Self::new(2_000, 60_000)
    }

    /// Weather errors: 30 s doubling to 15 min.
    pub const fn weather() -> Self {
        Self::new(30_000, 15 * 60_000)
    }

    /// Delay before the next retry. `random` is any random number, such as
    /// a value from the hardware RNG.
    pub fn next_delay_ms(&mut self, random: u32) -> u32 {
        let ceiling = self.ceiling_ms();
        self.attempt = self.attempt.saturating_add(1);
        let floor = ceiling / 2;
        floor + random % (ceiling - floor + 1)
    }

    /// Call after a success, so the next failure starts from the base delay.
    pub fn reset(&mut self) {
        self.attempt = 0;
    }

    /// Failures since the last reset.
    pub fn attempts(&self) -> u32 {
        self.attempt
    }

    fn ceiling_ms(&self) -> u32 {
        let grown = (self.base_ms as u64) << self.attempt.min(32);
        grown.min(self.max_ms as u64) as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[test]
    fn doubles_then_caps() {
        let mut b = Backoff::new(1_000, 60_000);
        let floors: Vec<u32> = (0..10).map(|_| b.next_delay_ms(0)).collect();
        assert_eq!(
            floors,
            [
                500, 1_000, 2_000, 4_000, 8_000, 16_000, 30_000, 30_000, 30_000, 30_000
            ]
        );
    }

    #[rstest]
    fn jitter_stays_in_upper_half_of_window(#[values(0, 1, 7, 12_345, u32::MAX)] random: u32) {
        let delay = Backoff::wifi().next_delay_ms(random);
        assert!((500..=1_000).contains(&delay), "{delay}");
    }

    #[test]
    fn many_failures_never_overflow() {
        let mut b = Backoff::weather();
        for _ in 0..100 {
            assert!(b.next_delay_ms(u32::MAX) <= 15 * 60_000);
        }
        assert_eq!(b.attempts(), 100);
        b.reset();
        assert_eq!(b.attempts(), 0);
    }
}
