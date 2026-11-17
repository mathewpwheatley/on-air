//! Owns the panel. For bring-up it cycles test patterns at the brightness
//! from the UP and DOWN buttons, through the same frame pipeline the
//! screens will use, so gamma and the current cap apply.

use alloc::boxed::Box;
use alloc::vec;

use embassy_time::{Duration, Instant, Ticker};
use embedded_graphics::pixelcolor::Rgb888;
use embedded_graphics::prelude::*;
use esp_hal::Async;
use esp_hal::peripherals::{DMA_CH0, LCD_CAM};
use esp_hal::time::Rate;
use esp_hub75::framebuffer::bitplane::plain::DmaFrameBuffer;
use esp_hub75::framebuffer::compute_rows;
use esp_hub75::{Hub75, Hub75Pins16};
use log::info;
use on_air_core::frame::FramePipeline;
use on_air_core::image::{DEFAULT_POWER, FRAME_LEN};
use on_air_ui::Framebuffer;
use static_cell::ConstStaticCell;

use crate::input_task::BRIGHTNESS;

const COLUMNS: usize = on_air_ui::WIDTH as usize;
const ROWS: usize = on_air_ui::HEIGHT as usize;
/// Bits per color channel. Each extra bit halves the refresh rate; 6 bits
/// at 20 MHz refreshes well above 100 Hz.
const PLANES: usize = 6;
const PANEL_CLOCK_MHZ: u32 = 20;

/// The DMA framebuffer the panel reads from. Lives in internal RAM, since
/// the driver can't read from PSRAM.
pub type PanelFrame = DmaFrameBuffer<{ compute_rows(ROWS) }, COLUMNS, PLANES>;

// Built in place, so the 20-odd KB never goes through the stack.
static FRONT: ConstStaticCell<PanelFrame> = ConstStaticCell::new(PanelFrame::new());
static BACK: ConstStaticCell<PanelFrame> = ConstStaticCell::new(PanelFrame::new());

/// How long each bring-up pattern shows.
const PATTERN_MS: u64 = 3_000;
/// Redraw this often.
const FRAME_MS: u64 = 30;
/// Log the refresh rate this often.
const STATS_MS: u64 = 5_000;

#[derive(Debug, Clone, Copy)]
enum Pattern {
    Solid(&'static str, Rgb888),
    Bars,
    Addressing,
}

const PATTERNS: [Pattern; 6] = [
    Pattern::Solid("red", Rgb888::RED),
    Pattern::Solid("green", Rgb888::GREEN),
    Pattern::Solid("blue", Rgb888::BLUE),
    Pattern::Solid("white", Rgb888::WHITE),
    Pattern::Bars,
    Pattern::Addressing,
];

impl Pattern {
    fn at(now_ms: u64) -> (usize, Pattern) {
        let index = (now_ms / PATTERN_MS) as usize % PATTERNS.len();
        (index, PATTERNS[index])
    }

    fn name(self) -> &'static str {
        match self {
            Pattern::Solid(name, _) => name,
            Pattern::Bars => "color bars and grey ramp",
            Pattern::Addressing => "addressing: red top-left, green top-right, blue bottom-left",
        }
    }

    fn draw(self, target: &mut Framebuffer) {
        let result = match self {
            Pattern::Solid(_, color) => target.clear(color),
            Pattern::Bars => on_air_ui::draw_test_pattern(target),
            Pattern::Addressing => on_air_ui::draw_addressing_pattern(target),
        };
        result.expect("framebuffer can't fail");
    }
}

/// Start refreshing the panel. Returns the driver and the spare
/// framebuffer for [`display_task`].
pub fn start_panel(
    lcd_cam: LCD_CAM<'static>,
    pins: Hub75Pins16<'static>,
    dma: DMA_CH0<'static>,
) -> (Hub75<Async, PanelFrame>, &'static mut PanelFrame) {
    let descriptors = esp_hub75::hub75_dma_descriptors!(PanelFrame);
    let hub75 = Hub75::new_async(
        lcd_cam,
        pins,
        dma,
        descriptors,
        Rate::from_mhz(PANEL_CLOCK_MHZ),
        FRONT.take(),
    )
    .expect("panel driver failed to start");
    (hub75, BACK.take())
}

#[embassy_executor::task]
pub async fn display_task(hub75: Hub75<Async, PanelFrame>, mut back: &'static mut PanelFrame) -> ! {
    let mut drawn = Box::new(Framebuffer::new());
    let mut shown = vec![0u8; FRAME_LEN];
    let mut pipeline = FramePipeline::for_panel(DEFAULT_POWER);
    let mut brightness = BRIGHTNESS.wait().await;
    let mut shown_pattern = None;
    let mut stats_from = (Instant::now(), hub75.frame_count());
    let mut ticker = Ticker::every(Duration::from_millis(FRAME_MS));

    loop {
        if let Some(level) = BRIGHTNESS.try_take() {
            brightness = level;
        }
        let now = Instant::now().as_millis();
        let (index, pattern) = Pattern::at(now);
        pattern.draw(&mut drawn);
        let rendered = pipeline.render(now, drawn.as_bytes(), brightness, &mut shown);
        if shown_pattern != Some(index) {
            shown_pattern = Some(index);
            info!(
                "pattern: {}, brightness {brightness} (after the cap: {})",
                pattern.name(),
                rendered.brightness
            );
        }

        for (i, pixel) in shown.as_chunks::<3>().0.iter().enumerate() {
            let point = Point::new((i % COLUMNS) as i32, (i / COLUMNS) as i32);
            back.set_pixel(point, Rgb888::new(pixel[0], pixel[1], pixel[2]));
        }
        back = match hub75.swap(back) {
            Ok(mut swap) => {
                swap.wait_for_done().await;
                swap.wait().unwrap_or_else(|(error, frame)| {
                    info!("panel error: {error}");
                    frame
                })
            }
            Err((error, frame)) => {
                info!("panel swap failed: {error}");
                frame
            }
        };

        let (since, frames) = stats_from;
        if since.elapsed().as_millis() >= STATS_MS {
            let count = hub75.frame_count();
            let hz = (count.wrapping_sub(frames) as u64 * 1000) / since.elapsed().as_millis();
            info!("panel refresh: {hz} Hz");
            stats_from = (Instant::now(), count);
        }
        ticker.next().await;
    }
}
