//! The MatrixPortal S3's pins, in one place.

use esp_hal::gpio::{Input, InputConfig, Pull};
use esp_hal::peripherals::{GPIO6, GPIO7};

/// The board's UP and DOWN buttons, next to the USB port. Active low.
/// They stand in for the brightness knob until it's fitted.
pub fn knob_buttons(up: GPIO6<'static>, down: GPIO7<'static>) -> [Input<'static>; 2] {
    let config = InputConfig::default().with_pull(Pull::Up);
    [Input::new(up, config), Input::new(down, config)]
}
