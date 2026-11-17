#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]
#![deny(clippy::large_stack_frames)]

use esp_hal::clock::CpuClock;
use esp_hal::interrupt::software::SoftwareInterruptControl;
use esp_hal::timer::timg::TimerGroup;

use log::info;

use embassy_executor::Spawner;
use embassy_net::StackResources;
use esp_hal::rng::Rng;
use on_air_firmware::{board, display_task, input_task, sntp_task, wifi_task};
use static_cell::StaticCell;

use esp_backtrace as _;

extern crate alloc;

// This creates a default app-descriptor required by the esp-idf bootloader.
// For more information see: <https://docs.espressif.com/projects/esp-idf/en/stable/esp32/api-reference/system/app_image_format.html#application-description>
esp_bootloader_esp_idf::esp_app_desc!();

#[allow(
    clippy::large_stack_frames,
    reason = "it's not unusual to allocate larger buffers etc. in main"
)]
#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    // generator version: 1.2.0

    esp_println::logger::init_logger_from_env();

    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 73744);
    // WiFi takes much of the first region. This one comes out of the
    // otherwise-unused stack space, for the display's frame buffers.
    esp_alloc::heap_allocator!(size: 96 * 1024);
    let psram = esp_hal::psram::Psram::new(peripherals.PSRAM, Default::default());
    let (_, psram_size) = psram.raw_parts();
    if psram_size > 0 {
        esp_alloc::psram_allocator!(&psram);
    }

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let software_interrupts = SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, software_interrupts.software_interrupt0);

    info!(
        "Embassy initialized! Panel is {}x{}",
        on_air_ui::WIDTH,
        on_air_ui::HEIGHT
    );

    info!("PSRAM: {} KB", psram_size / 1024);

    let (wifi_controller, interfaces) = esp_radio::wifi::new(peripherals.WIFI, Default::default())
        .expect("Failed to initialize Wi-Fi controller");
    let rng = Rng::new();
    static NET_RESOURCES: StaticCell<StackResources<4>> = StaticCell::new();
    let net_seed = (rng.random() as u64) << 32 | rng.random() as u64;
    let (stack, net_runner) = embassy_net::new(
        interfaces.station,
        embassy_net::Config::dhcpv4(Default::default()),
        NET_RESOURCES.init(StackResources::new()),
        net_seed,
    );
    spawner.spawn(wifi_task::net_task(net_runner).expect("net task already running"));
    spawner.spawn(
        wifi_task::wifi_task(wifi_controller, rng.random()).expect("wifi task already running"),
    );
    spawner.spawn(sntp_task::sntp_task(stack, rng.random()).expect("sntp task already running"));

    let knob_buttons = board::knob_buttons(peripherals.GPIO6, peripherals.GPIO7);
    spawner.spawn(
        input_task::knob_buttons_task(knob_buttons).expect("knob buttons task already running"),
    );

    let (hub75, spare_frame) = display_task::start_panel(
        peripherals.LCD_CAM,
        on_air_firmware::panel_pins!(peripherals),
        peripherals.DMA_CH0,
    );
    spawner.spawn(
        display_task::display_task(hub75, spare_frame).expect("display task already running"),
    );

    // After WiFi start-up and the display's first frame have allocated.
    embassy_time::Timer::after_secs(1).await;
    info!("{}", esp_alloc::HEAP.stats());

    stack.wait_config_up().await;
    if let Some(config) = stack.config_v4() {
        info!("network: online at {}", config.address);
    }

    loop {
        core::future::pending::<()>().await;
    }
}
