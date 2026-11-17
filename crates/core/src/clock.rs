//! Wall-clock time from SNTP, without the network. The host keeps a
//! monotonic clock (time since boot); [`Clock`] holds the offset to real
//! time and adjusts it from each SNTP result.
//!
//! ```text
//! let now = clock.unix_micros(since_boot_us);   // timestamps for the request
//! let result = sntp_request(now).await;         // offset = server time - our time
//! clock.adjust(result.offset);
//! let unix_s = clock.unix_seconds(since_boot_us);
//! ```

/// Sync again this often once synced, so drift stays well under a second.
pub const RESYNC_MS: u64 = 4 * 60 * 60 * 1000;

/// Wall-clock time, as an offset from a monotonic clock.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Clock {
    /// Unix time at boot, in microseconds, once synced.
    boot_unix_us: Option<i64>,
}

impl Clock {
    pub const fn new() -> Self {
        Self { boot_unix_us: None }
    }

    /// Whether a sync has set the time.
    pub fn is_set(&self) -> bool {
        self.boot_unix_us.is_some()
    }

    /// What this clock says the time is, for stamping an SNTP request.
    /// Before the first sync it counts from the Unix epoch, which is just
    /// as good: the reply's offset corrects for it.
    pub fn unix_micros(&self, since_boot_us: u64) -> i64 {
        self.boot_unix_us.unwrap_or(0) + since_boot_us as i64
    }

    /// Apply an SNTP result: `offset_us` is how far the server's time is
    /// ahead of [`Clock::unix_micros`].
    pub fn adjust(&mut self, offset_us: i64) {
        self.boot_unix_us = Some(self.boot_unix_us.unwrap_or(0) + offset_us);
    }

    /// Unix time in whole seconds, or `None` before the first sync.
    pub fn unix_seconds(&self, since_boot_us: u64) -> Option<i64> {
        self.boot_unix_us
            .map(|boot| (boot + since_boot_us as i64).div_euclid(1_000_000))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-11-16 00:00:00 UTC.
    const REAL_S: i64 = 1_794_787_200;

    #[test]
    fn unset_until_the_first_sync() {
        let clock = Clock::new();
        assert!(!clock.is_set());
        assert_eq!(clock.unix_seconds(5_000_000), None);
        assert_eq!(clock.unix_micros(5_000_000), 5_000_000);
    }

    #[test]
    fn first_sync_sets_the_time() {
        let mut clock = Clock::new();
        let since_boot = 7_500_000;
        // The server is ahead of the epoch-based count by this much.
        let offset = REAL_S * 1_000_000 - clock.unix_micros(since_boot);
        clock.adjust(offset);
        assert!(clock.is_set());
        assert_eq!(clock.unix_seconds(since_boot), Some(REAL_S));
        // And it keeps counting.
        assert_eq!(
            clock.unix_seconds(since_boot + 60_000_000),
            Some(REAL_S + 60)
        );
    }

    #[test]
    fn later_syncs_correct_drift_both_ways() {
        let mut clock = Clock::new();
        clock.adjust(REAL_S * 1_000_000);
        clock.adjust(2_000_000);
        assert_eq!(clock.unix_seconds(0), Some(REAL_S + 2));
        clock.adjust(-3_000_000);
        assert_eq!(clock.unix_seconds(0), Some(REAL_S - 1));
    }
}
