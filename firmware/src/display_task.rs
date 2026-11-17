//! Owns the panel: runs the app state machine from core, draws its screen
//! with `on-air-ui`, and passes each frame through `core::frame` for the
//! cover crossfade, gamma, the current cap and the brightness.

use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;

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
use on_air_core::app::{App, Event, NetStatus, Playback};
use on_air_core::frame::FramePipeline;
use on_air_core::image::{DEFAULT_POWER, FRAME_LEN};
use on_air_core::settings::IdleMode;
use on_air_core::spotify::NowPlaying;
use on_air_core::tide::{self, TideSupport, TideTurn};
use on_air_core::timezone::{self, TimeZone};
use on_air_core::weather::{Reading, STALE_AFTER_S};
use on_air_ui::{Framebuffer, Scene, TideTimes, Track, WeatherInfo};
use static_cell::ConstStaticCell;

use crate::config::Effective;
use crate::input_task::BRIGHTNESS;
use crate::spotify_task::{self, SpotifyUpdate};
use crate::weather_task::{self, WeatherUpdate};
use crate::{https, sntp_task, wifi_task};

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

/// Redraw this often: smooth enough for the crossfade and scrolling text.
const FRAME_MS: u64 = 30;
/// Log the refresh rate this often.
const STATS_MS: u64 = 60_000;

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
pub async fn display_task(
    hub75: Hub75<Async, PanelFrame>,
    mut back: &'static mut PanelFrame,
    setup_network: &'static str,
    settings: &'static Effective,
) -> ! {
    let settings = &settings.settings;
    let idle_mode = settings.idle_mode;
    let mut drawn = Box::new(Framebuffer::new());
    let mut shown = vec![0u8; FRAME_LEN];
    let mut pipeline = FramePipeline::for_panel(DEFAULT_POWER);
    let mut brightness = BRIGHTNESS.wait().await;
    let zone = timezone::find(&settings.timezone).unwrap_or(TimeZone::UTC);
    let clock_24h = settings.clock_24h;
    info!("display: timezone {}", settings.timezone);

    let units = settings.units;
    let mut app = App::new(idle_mode);
    app.set_idle_after_ms(settings.idle_after_ms());
    app.set_tides_available(settings.has_tides());
    info!("display: idle screen {idle_mode:?}");
    let mut fetching_for: Option<IdleMode> = None;
    let mut weather: Option<(Reading, Instant)> = None;
    let mut tide_turns: Vec<TideTurn> = Vec::new();
    let mut art: Option<Vec<u8>> = None;
    let mut track: Option<NowPlaying> = None;
    let mut spotify_reachable: Option<bool> = None;
    let mut shown_net: Option<NetStatus> = None;
    let mut shown_certificate_problem = false;
    let mut stats_from = (Instant::now(), hub75.frame_count());
    let mut ticker = Ticker::every(Duration::from_millis(FRAME_MS));

    loop {
        let now = Instant::now().as_millis();
        if let Some(level) = BRIGHTNESS.try_take() {
            brightness = level;
        }

        while let Ok(update) = spotify_task::UPDATES.try_receive() {
            match update {
                SpotifyUpdate::Art(cover) => {
                    if cover.is_some() {
                        pipeline.start_crossfade(now);
                    }
                    art = cover;
                }
                SpotifyUpdate::NowPlaying(Some(playing)) => {
                    let playback = Playback::Track {
                        is_playing: playing.is_playing,
                        has_art: art.is_some(),
                    };
                    app.handle(Event::Playback(playback), now);
                    track = Some(playing);
                }
                SpotifyUpdate::NowPlaying(None) => {
                    app.handle(Event::Playback(Playback::Nothing), now);
                    track = None;
                }
                SpotifyUpdate::Reachable(reachable) => spotify_reachable = Some(reachable),
                SpotifyUpdate::SignInNeeded => {
                    app.handle(Event::SignInNeeded, now);
                }
                SpotifyUpdate::CommandFailed(failure) => {
                    app.handle(Event::CommandFailed(failure), now);
                }
            }
        }

        while let Ok(update) = weather_task::UPDATES.try_receive() {
            match update {
                WeatherUpdate::Weather(reading) => weather = Some((reading, Instant::now())),
                WeatherUpdate::Tides(turns) => tide_turns = turns,
                WeatherUpdate::TideSupport(support) => {
                    app.set_tides_available(support == TideSupport::Available);
                }
            }
        }
        // The weather task fetches only what the selected idle screen shows.
        if fetching_for != Some(app.idle_mode()) {
            fetching_for = Some(app.idle_mode());
            weather_task::IDLE_MODE.signal(app.idle_mode());
        }

        // WiFi, or Spotify being out of reach while the WiFi is up.
        let net = match (wifi_task::status(), spotify_reachable) {
            (NetStatus::Online, Some(false)) => NetStatus::Lost,
            (status, _) => status,
        };
        if shown_net != Some(net) {
            shown_net = Some(net);
            app.handle(Event::Net(net), now);
        }
        let certificate_problem = https::certificate_problem();
        if certificate_problem != shown_certificate_problem {
            shown_certificate_problem = certificate_problem;
            app.handle(Event::CertificateProblem(certificate_problem), now);
        }

        let unix_s = sntp_task::unix_seconds();
        let local = |at: Option<i64>| at.and_then(|s| timezone::local_time(&zone, s));
        let scene = Scene {
            view: app.view(now),
            art: art.as_deref(),
            time: local(unix_s),
            clock_24h,
            weather: weather.map(|(reading, at)| WeatherInfo {
                reading,
                stale: at.elapsed().as_secs() > STALE_AFTER_S,
                units,
            }),
            tides_available: app.tides_available(),
            tides: unix_s.filter(|_| !tide_turns.is_empty()).map(|unix_s| {
                let next = tide::next_tides(&tide_turns, unix_s);
                TideTimes {
                    high: local(next.high),
                    low: local(next.low),
                }
            }),
            track: track.as_ref().map(|t| Track {
                title: &t.title,
                artist: &t.artist,
                album: t.album.as_deref(),
            }),
            setup_network,
            now_ms: now,
        };
        on_air_ui::draw(&scene, &mut *drawn).expect("framebuffer can't fail");
        pipeline.render(now, drawn.as_bytes(), brightness, &mut shown);

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
            info!("panel refresh: {hz} Hz, brightness {brightness}");
            stats_from = (Instant::now(), count);
        }
        ticker.next().await;
    }
}
