//! Decides whether to feed the hardware watchdog. Each task checks in
//! regularly, and the watchdog is only fed while every task has checked in
//! recently. One stuck task stops the feeding, so the watchdog resets the
//! device.
//!
//! ```text
//! // Each task, at least once per its limit, even while waiting:
//! supervisor.check_in(task, now);
//!
//! // The supervisor task, every few seconds:
//! match supervisor.health(now) {
//!     Health::Healthy => watchdog.feed(),
//!     Health::Stuck(name) => log!("{name} is stuck; letting the watchdog reset"),
//! }
//! ```

use alloc::vec::Vec;

/// A task being watched, from [`Supervisor::watch`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaskId(usize);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Health {
    Healthy,
    /// This task hasn't checked in within its limit.
    Stuck(&'static str),
}

#[derive(Debug, Clone)]
struct Watched {
    name: &'static str,
    limit_ms: u64,
    last_check_in: u64,
}

#[derive(Debug, Clone, Default)]
pub struct Supervisor {
    tasks: Vec<Watched>,
}

impl Supervisor {
    pub const fn new() -> Self {
        Self { tasks: Vec::new() }
    }

    /// Start watching a task, which has to check in at least every
    /// `limit_ms`. It counts as checked in now.
    pub fn watch(&mut self, name: &'static str, limit_ms: u64, now: u64) -> TaskId {
        self.tasks.push(Watched {
            name,
            limit_ms,
            last_check_in: now,
        });
        TaskId(self.tasks.len() - 1)
    }

    pub fn check_in(&mut self, task: TaskId, now: u64) {
        if let Some(watched) = self.tasks.get_mut(task.0) {
            watched.last_check_in = watched.last_check_in.max(now);
        }
    }

    /// Healthy while every task has checked in within its limit. Reports
    /// the task that's been quiet longest past its limit.
    pub fn health(&self, now: u64) -> Health {
        self.tasks
            .iter()
            .filter_map(|t| {
                let quiet = now.saturating_sub(t.last_check_in);
                (quiet > t.limit_ms).then(|| (quiet - t.limit_ms, t.name))
            })
            .max_by_key(|&(over, _)| over)
            .map_or(Health::Healthy, |(_, name)| Health::Stuck(name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    fn supervisor() -> (Supervisor, TaskId, TaskId) {
        let mut s = Supervisor::new();
        let display = s.watch("display", 2_000, 0);
        let spotify = s.watch("spotify", 20_000, 0);
        (s, display, spotify)
    }

    #[test]
    fn nothing_watched_is_healthy() {
        assert_eq!(Supervisor::new().health(1_000_000), Health::Healthy);
    }

    #[rstest]
    #[case::just_started(0, Health::Healthy)]
    #[case::at_the_display_limit(2_000, Health::Healthy)]
    #[case::display_quiet_too_long(2_001, Health::Stuck("display"))]
    fn tasks_must_check_in_within_their_limit(#[case] now: u64, #[case] health: Health) {
        let (s, _, _) = supervisor();
        assert_eq!(s.health(now), health);
    }

    #[test]
    fn checking_in_keeps_it_healthy() {
        let (mut s, display, spotify) = supervisor();
        for now in (0..60_000).step_by(1_000) {
            s.check_in(display, now);
            if now % 10_000 == 0 {
                s.check_in(spotify, now);
            }
            assert_eq!(s.health(now), Health::Healthy, "at {now}");
        }
    }

    #[test]
    fn one_stuck_task_is_enough() {
        let (mut s, display, _) = supervisor();
        for now in (0..30_000).step_by(1_000) {
            s.check_in(display, now);
        }
        assert_eq!(s.health(29_000), Health::Stuck("spotify"));
    }

    #[test]
    fn reports_the_task_furthest_past_its_limit() {
        let (s, _, _) = supervisor();
        // Display is 28 s over its limit, Spotify 10 s over.
        assert_eq!(s.health(30_000), Health::Stuck("display"));
    }

    #[test]
    fn a_stuck_task_that_recovers_is_healthy_again() {
        let (mut s, display, spotify) = supervisor();
        assert_eq!(s.health(5_000), Health::Stuck("display"));
        s.check_in(display, 5_000);
        s.check_in(spotify, 5_000);
        assert_eq!(s.health(5_000), Health::Healthy);
    }

    #[test]
    fn an_older_check_in_does_not_move_time_back() {
        let (mut s, display, spotify) = supervisor();
        s.check_in(display, 10_000);
        s.check_in(display, 9_000);
        s.check_in(spotify, 10_000);
        assert_eq!(s.health(11_500), Health::Healthy);
    }
}
