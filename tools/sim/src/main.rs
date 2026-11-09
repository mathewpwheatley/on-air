//! Desktop simulator: shows the 64×64 panel in a window, scaled up.

use embedded_graphics::{pixelcolor::Rgb888, prelude::*};
use embedded_graphics_simulator::{
    OutputSettingsBuilder, SimulatorDisplay, SimulatorEvent, Window,
};
use on_air_ui::{HEIGHT, WIDTH, draw_test_pattern};

fn main() {
    let mut display = SimulatorDisplay::<Rgb888>::new(Size::new(WIDTH, HEIGHT));
    draw_test_pattern(&mut display).expect("simulator display can't fail");

    let settings = OutputSettingsBuilder::new()
        .scale(8)
        .pixel_spacing(1)
        .build();
    let mut window = Window::new("On Air", &settings);

    'running: loop {
        window.update(&display);
        for event in window.events() {
            if let SimulatorEvent::Quit = event {
                break 'running;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(16));
    }
}
