//! The hardware watchdog, fed by `core::supervisor::Supervisor`. Each
//! watched task checks in regularly, even while waiting; the supervisor task
//! feeds the RTC watchdog only while every task has checked in within its
//! limit. A stuck task stops the feeding, and the watchdog resets the board
//! 30 s later. Anything that blocks the whole executor stops the supervisor
//! too, with the same result.

use core::cell::RefCell;

use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_time::{Instant, Timer};
use esp_hal::rtc_cntl::{Rwdt, RwdtStage, RwdtStageAction};
use log::{info, warn};
use on_air_core::supervisor::{Health, Supervisor, TaskId};

/// The watchdog resets the board when it isn't fed for this long.
const WATCHDOG_TIMEOUT_S: u64 = 30;
/// The supervisor checks the tasks, and feeds the watchdog, this often.
const FEED_EVERY_MS: u64 = 5_000;
/// Long waits are cut into slices this long, checking in between.
const SLICE_MS: u64 = 5_000;

/// The display and buttons should never go this long without a frame or a poll.
pub const FAST_TASK_MS: u64 = 5_000;
/// Network tasks: one HTTPS request can take up to 30s.
pub const NETWORK_TASK_MS: u64 = 60_000;

static SUPERVISOR: Mutex<CriticalSectionRawMutex, RefCell<Supervisor>> =
    Mutex::new(RefCell::new(Supervisor::new()));

fn now() -> u64 {
    Instant::now().as_millis()
}

/// A task's place in the supervisor.
#[derive(Debug, Clone, Copy)]
pub struct Watch(TaskId);

/// Start watching a task that will check in at least every `limit_ms`.
pub fn watch(name: &'static str, limit_ms: u64) -> Watch {
    Watch(SUPERVISOR.lock(|s| s.borrow_mut().watch(name, limit_ms, now())))
}

impl Watch {
    pub fn check_in(self) {
        SUPERVISOR.lock(|s| s.borrow_mut().check_in(self.0, now()));
    }

    /// Wait `ms`, checking in along the way.
    pub async fn sleep(self, ms: u64) {
        let until = now() + ms;
        loop {
            self.check_in();
            let left = until.saturating_sub(now());
            if left == 0 {
                return;
            }
            Timer::after_millis(left.min(SLICE_MS)).await;
        }
    }

    /// For a task with nothing more to do: keep checking in, so it isn't
    /// taken for stuck.
    pub async fn idle(self) -> ! {
        loop {
            self.sleep(SLICE_MS).await;
        }
    }
}

/// Feeds the watchdog while every watched task is healthy.
#[embassy_executor::task]
pub async fn supervisor_task(mut rwdt: Rwdt) -> ! {
    rwdt.set_timeout(
        RwdtStage::Stage0,
        esp_hal::time::Duration::from_secs(WATCHDOG_TIMEOUT_S),
    );
    rwdt.set_stage_action(RwdtStage::Stage0, RwdtStageAction::ResetSystem);
    rwdt.enable();
    info!("watchdog: on, resetting after {WATCHDOG_TIMEOUT_S} s without a feed");
    let mut reported = false;
    loop {
        match SUPERVISOR.lock(|s| s.borrow().health(now())) {
            Health::Healthy => {
                rwdt.feed();
                reported = false;
            }
            Health::Stuck(name) => {
                if !reported {
                    warn!("watchdog: {name} is stuck; the board resets in {WATCHDOG_TIMEOUT_S} s");
                    reported = true;
                }
            }
        }
        Timer::after_millis(FEED_EVERY_MS).await;
    }
}
