//! Reads the inputs and passes on what they mean.

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Instant, Ticker};
use esp_hal::gpio::Input;
use log::info;
use on_air_core::input::{ADC_MAX, DEFAULT_FLOOR, KnobButtons, POLL_INTERVAL_MS};

use crate::watchdog;
use crate::wifi_task::{self, WifiCommand};

/// The latest panel brightness, 0–255.
pub static BRIGHTNESS: Signal<CriticalSectionRawMutex, u8> = Signal::new();

/// Turns UP and DOWN presses into brightness, starting at three-quarters
/// of the pretend knob's travel. Holding both for 5 s opens the setup
/// portal, standing in for the Mode-button hold.
#[embassy_executor::task]
pub async fn knob_buttons_task(buttons: [Input<'static>; 2]) -> ! {
    let mut knob = KnobButtons::new(DEFAULT_FLOOR, ADC_MAX / 4 * 3);
    let mut ticker = Ticker::every(Duration::from_millis(POLL_INTERVAL_MS));
    let watch = watchdog::watch("buttons", watchdog::FAST_TASK_MS);
    loop {
        watch.check_in();
        let pressed = buttons.each_ref().map(Input::is_low);
        let now = Instant::now().as_millis();
        if let Some(level) = knob.update(pressed, now) {
            BRIGHTNESS.signal(level);
        }
        // Holding both stands in for holding Mode: reopen setup.
        if knob.take_portal_hold(now) {
            info!("buttons: UP and DOWN held; opening the setup portal");
            let _ = wifi_task::COMMANDS.try_send(WifiCommand::OpenPortal);
        }
        ticker.next().await;
    }
}
