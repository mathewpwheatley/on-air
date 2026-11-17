//! The MatrixPortal S3's pins, in one place. From Adafruit's pinout.

use esp_hal::gpio::{Input, InputConfig, Pull};
use esp_hal::peripherals::{GPIO6, GPIO7};

/// The HUB75 connector's pins, as [`esp_hub75::Hub75Pins16`]. A macro
/// rather than a function so it can take just these pins out of
/// `peripherals` and leave the rest.
///
/// 64×64 panels use the E address line, which reaches the connector
/// through the board's address-E solder jumper.
#[macro_export]
macro_rules! panel_pins {
    ($peripherals:ident) => {{
        use esp_hal::gpio::Pin as _;
        esp_hub75::Hub75Pins16 {
            red1: $peripherals.GPIO42.degrade(),
            grn1: $peripherals.GPIO41.degrade(),
            blu1: $peripherals.GPIO40.degrade(),
            red2: $peripherals.GPIO38.degrade(),
            grn2: $peripherals.GPIO39.degrade(),
            blu2: $peripherals.GPIO37.degrade(),
            addr0: $peripherals.GPIO45.degrade(),
            addr1: $peripherals.GPIO36.degrade(),
            addr2: $peripherals.GPIO48.degrade(),
            addr3: $peripherals.GPIO35.degrade(),
            addr4: $peripherals.GPIO21.degrade(),
            blank: $peripherals.GPIO14.degrade(),
            clock: $peripherals.GPIO2.degrade(),
            latch: $peripherals.GPIO47.degrade(),
        }
    }};
}

/// The board's UP and DOWN buttons, next to the USB port. Active low.
/// They turn the brightness up and down; holding both opens the setup portal.
pub fn knob_buttons(up: GPIO6<'static>, down: GPIO7<'static>) -> [Input<'static>; 2] {
    let config = InputConfig::default().with_pull(Pull::Up);
    [Input::new(up, config), Input::new(down, config)]
}
