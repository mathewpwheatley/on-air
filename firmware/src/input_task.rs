//! Reads the inputs and passes on what they mean.

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Instant, Ticker};
use esp_hal::gpio::Input;
use on_air_core::input::{ADC_MAX, DEFAULT_FLOOR, KnobButtons, POLL_INTERVAL_MS};

/// The latest panel brightness, 0–255.
pub static BRIGHTNESS: Signal<CriticalSectionRawMutex, u8> = Signal::new();

/// Turns UP and DOWN presses into brightness, starting at three-quarters
/// of the pretend knob's travel.
#[embassy_executor::task]
pub async fn knob_buttons_task(buttons: [Input<'static>; 2]) -> ! {
    let mut knob = KnobButtons::new(DEFAULT_FLOOR, ADC_MAX / 4 * 3);
    let mut ticker = Ticker::every(Duration::from_millis(POLL_INTERVAL_MS));
    loop {
        let pressed = buttons.each_ref().map(Input::is_low);
        if let Some(level) = knob.update(pressed, Instant::now().as_millis()) {
            BRIGHTNESS.signal(level);
        }
        ticker.next().await;
    }
}
