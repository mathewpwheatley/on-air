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
use on_air_core::settings::Settings;
use on_air_firmware::config::{self, Effective};
use on_air_firmware::{
    board, display_task, https, input_task, setup_network, sntp_task, spotify_task, storage,
    weather_task, wifi_task,
};
use static_cell::StaticCell;
use tinyrlibc as _;

use esp_backtrace as _;

extern crate alloc;

use alloc::string::String;

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

    // PSRAM first, so ordinary allocations (TLS, responses, covers, frames)
    // land there. WiFi and the scheduler ask for internal memory by name, so
    // the internal regions stay free for them.
    let psram = esp_hal::psram::Psram::new(peripherals.PSRAM, Default::default());
    let (_, psram_size) = psram.raw_parts();
    if psram_size > 0 {
        esp_alloc::psram_allocator!(&psram);
    }
    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 73744);
    // A little more internal RAM for WiFi, from the stack space. Kept small:
    // the stack has to hold mbedtls's handshakes, and with PSRAM first this
    // region showed no use.
    esp_alloc::heap_allocator!(size: 32 * 1024);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let software_interrupts = SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, software_interrupts.software_interrupt0);

    info!(
        "Embassy initialized! Panel is {}x{}",
        on_air_ui::WIDTH,
        on_air_ui::HEIGHT
    );

    info!("PSRAM: {} KB", psram_size / 1024);

    // The settings to run with: saved by the setup portal, or in
    // development filled in from `.env`.
    let store = storage::SettingsStore::open(peripherals.FLASH).await;
    let saved = match store {
        Some(store) => store.get().await,
        None => Settings::default(),
    };
    static SETTINGS: StaticCell<Effective> = StaticCell::new();
    let settings: &'static Effective = SETTINGS.init(config::effective(saved));
    info!(
        "settings: {}",
        if settings.from_portal {
            "from the setup portal"
        } else {
            "from .env (not set up yet)"
        }
    );

    let (wifi_controller, interfaces) = esp_radio::wifi::new(peripherals.WIFI, Default::default())
        .expect("Failed to initialize Wi-Fi controller");
    let rng = Rng::new();
    static NET_RESOURCES: StaticCell<StackResources<8>> = StaticCell::new();
    let net_seed = (rng.random() as u64) << 32 | rng.random() as u64;
    let (stack, net_runner) = embassy_net::new(
        interfaces.station,
        embassy_net::Config::dhcpv4(Default::default()),
        NET_RESOURCES.init(StackResources::new()),
        net_seed,
    );
    spawner.spawn(wifi_task::net_task(net_runner).expect("net task already running"));
    let https_client = https::HttpsClient::start(stack);

    // The setup network, on its own stack with a fixed address. Its
    // services only see traffic while the portal has it up.
    static SETUP_NETWORK_NAME: StaticCell<String> = StaticCell::new();
    let setup_network_name: &'static str = SETUP_NETWORK_NAME.init(
        on_air_core::setup::network_name(interfaces.access_point.mac_address()),
    );
    static SETUP_RESOURCES: StaticCell<StackResources<6>> = StaticCell::new();
    let (setup_stack, setup_runner) = embassy_net::new(
        interfaces.access_point,
        setup_network::stack_config(),
        SETUP_RESOURCES.init(StackResources::new()),
        net_seed.rotate_left(17),
    );
    spawner.spawn(wifi_task::net_task(setup_runner).expect("setup net task already running"));
    spawner.spawn(setup_network::dhcp_task(setup_stack).expect("DHCP task already running"));
    spawner.spawn(setup_network::dns_task(setup_stack).expect("DNS task already running"));
    let portal = setup_network::PortalShared::setup(
        settings.settings.clone(),
        setup_network_name,
        store,
        https_client,
    );
    for _ in 0..3 {
        spawner.spawn(setup_network::web_task(setup_stack, portal).expect("too many web tasks"));
    }

    // The Spotify sign-in page on the home network, only while there's no
    // sign-in to use (the panel shows its address as a QR code).
    let sign_in_open = settings.settings.spotify_refresh_token.is_none()
        && (settings.from_portal || config::SPOTIFY_REFRESH_TOKEN.is_none());
    if sign_in_open {
        let home = setup_network::PortalShared::home(
            stack,
            settings.settings.clone(),
            setup_network_name,
            store,
            https_client,
        );
        spawner.spawn(setup_network::web_task(stack, home).expect("too many web tasks"));
        info!("spotify: not signed in; sign-in page open on the home network");
    }

    spawner.spawn(
        wifi_task::wifi_task(
            wifi_controller,
            rng.random(),
            setup_network_name,
            settings.settings.wifi_ssid.clone(),
            settings.settings.wifi_pass.clone(),
        )
        .expect("wifi task already running"),
    );
    spawner.spawn(sntp_task::sntp_task(stack, rng.random()).expect("sntp task already running"));
    spawner.spawn(
        spotify_task::spotify_task(https_client, store, settings, rng.random())
            .expect("spotify task already running"),
    );
    spawner.spawn(
        weather_task::weather_task(https_client, store, settings, rng.random())
            .expect("weather task already running"),
    );

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
        display_task::display_task(
            hub75,
            spare_frame,
            setup_network_name,
            settings,
            sign_in_open.then_some(stack),
        )
        .expect("display task already running"),
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
