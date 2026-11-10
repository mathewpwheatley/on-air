//! Desktop simulator: the real On-Air app state, screens and inputs in a
//! window, with either a fake player or your real Spotify account.

mod fake;
mod http_client;
mod live;
mod messages;
mod portal;
mod weather;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use clap::Parser;
use embedded_graphics::{pixelcolor::Rgb888, prelude::*, primitives::Rectangle};
use embedded_graphics_simulator::sdl2::Keycode;
use embedded_graphics_simulator::{
    OutputSettingsBuilder, SimulatorDisplay, SimulatorEvent, Window,
};
use on_air_core::app::{Action, App, Event, NetStatus, Playback};
use on_air_core::frame::FramePipeline;
use on_air_core::image::DEFAULT_POWER;
use on_air_core::input::{ADC_MAX, Buttons, DEFAULT_FLOOR, InputEvent, Knob, POLL_INTERVAL_MS};
use on_air_core::settings::{DEFAULT_IDLE_AFTER_MIN, IdleMode, Settings, WeatherLocation};
use on_air_core::spotify::NowPlaying;
use on_air_core::tide::{self, TideSupport, TideTurn};
use on_air_core::timezone::{self, TimeZone};
use on_air_core::weather::{Reading, STALE_AFTER_S, Units};
use on_air_core::wifi::{WifiEvent, WifiLink, WifiNext};
use on_air_ui::{Framebuffer, HEIGHT, Scene, TideTimes, Track, WIDTH, WeatherInfo};

use messages::{Control, Update};

const KEYS_HELP: &str = "\
Keys:
  1  Play/Pause (hold 2 s: track info)    2  Previous    3  Next
  4  Mode (hold 5 s: setup portal)
  1+4 held 10 s    factory reset
  - / +            brightness knob down / up (hold to repeat)
  W  turn the pretend WiFi router off / on    S  toggle Spotify sign-in needed
  N  stop playback (fake player)    Esc  leave the setup portal
Setup portal: hold 4 for 5 s (or start with --portal), then use http://127.0.0.1:8080";

/// Environment variables the options read, e.g. from `.env`.
const ENV_KEYS: [&str; 9] = [
    "SPOTIFY_CLIENT_ID",
    "ON_AIR_LIVE",
    "ON_AIR_CITY",
    "ON_AIR_FAHRENHEIT",
    "ON_AIR_CLOCK_24H",
    "ON_AIR_TIMEZONE",
    "ON_AIR_RECORD",
    "ON_AIR_IDLE_MINUTES",
    "ON_AIR_SCALE",
];

/// How far one press of - or + turns the simulated knob, in ADC counts.
const KNOB_STEP: i32 = 256;

#[derive(Parser)]
#[command(about = "Desktop simulator for the On-Air panel", after_help = KEYS_HELP)]
struct Args {
    /// Use your real Spotify account instead of the built-in fake player.
    #[arg(long, env = "ON_AIR_LIVE")]
    live: bool,
    /// Client ID of your Spotify developer app (needed for --live).
    #[arg(long, env = "SPOTIFY_CLIENT_ID")]
    client_id: Option<String>,
    /// City for real weather from Open-Meteo. Without it, the weather is fake.
    #[arg(long, env = "ON_AIR_CITY")]
    city: Option<String>,
    /// Show temperatures in Fahrenheit.
    #[arg(long, env = "ON_AIR_FAHRENHEIT")]
    fahrenheit: bool,
    /// Use a 24-hour clock.
    #[arg(long, env = "ON_AIR_CLOCK_24H")]
    clock_24h: bool,
    /// IANA timezone for the clock. Defaults to the city's, then this computer's.
    #[arg(long, env = "ON_AIR_TIMEZONE")]
    timezone: Option<String>,
    /// Save each new currently-playing response to this folder (with --live).
    #[arg(long, env = "ON_AIR_RECORD")]
    record: Option<PathBuf>,
    /// Minutes with nothing playing before the idle screen shows.
    #[arg(long, env = "ON_AIR_IDLE_MINUTES", default_value_t = DEFAULT_IDLE_AFTER_MIN,
          value_parser = clap::value_parser!(u8).range(1..=30))]
    idle_minutes: u8,
    /// Open the setup portal at start, as if Mode had been held for 5 s.
    #[arg(long)]
    portal: bool,
    /// Don't open pages in the browser; just print their addresses.
    #[arg(long)]
    no_browser: bool,
    /// Window pixels per LED.
    #[arg(long, env = "ON_AIR_SCALE", default_value_t = 8)]
    scale: u32,
}

pub fn random_u32() -> u32 {
    getrandom::u32().unwrap_or(0)
}

fn button_index(key: Keycode) -> Option<usize> {
    match key {
        Keycode::NUM_1 => Some(0),
        Keycode::NUM_2 => Some(1),
        Keycode::NUM_3 => Some(2),
        Keycode::NUM_4 => Some(3),
        _ => None,
    }
}

/// What the portal starts with: this run's options, as if they'd been saved
/// on a device. WiFi starts empty so the portal begins at step 1.
fn portal_settings(args: &Args, units: Units, clock_24h: bool) -> Settings {
    let client_id = args.client_id.clone().unwrap_or_default();
    Settings {
        spotify_refresh_token: live::load_refresh_token(&client_id),
        spotify_client_id: client_id,
        timezone: args.timezone.clone().unwrap_or_default(),
        // The city's position isn't known here; the portal only shows its name.
        weather: args.city.clone().map(|city| WeatherLocation {
            city,
            lat: 0.0,
            lon: 0.0,
        }),
        units,
        clock_24h,
        idle_after_min: args.idle_minutes,
        ..Settings::default()
    }
}

fn find_timezone(name: &str) -> Option<TimeZone> {
    let zone = timezone::find(name);
    if zone.is_none() {
        eprintln!("unknown timezone {name:?}");
    }
    zone
}

fn main() {
    // Settings can also come from a .env file in the repo (see .env.example).
    // Command-line options win over it.
    let _ = dotenvy::dotenv();
    // Treat `KEY=` as unset, so a half-filled .env doesn't break parsing.
    for key in ENV_KEYS {
        if std::env::var_os(key).is_some_and(|v| v.is_empty()) {
            // SAFETY: no other threads exist yet, so nothing reads the
            // environment while it changes.
            unsafe { std::env::remove_var(key) };
        }
    }
    let args = Args::parse();
    println!("{KEYS_HELP}\n");

    let (update_tx, updates) = mpsc::channel::<Update>();
    let (controls, control_rx) = mpsc::channel::<Control>();

    // Shared with the Spotify thread, which polls more slowly once idle.
    let idle_after_ms = Arc::new(AtomicU64::new(args.idle_minutes as u64 * 60_000));
    if args.live {
        let Some(client_id) = args.client_id.clone().filter(|id| !id.trim().is_empty()) else {
            eprintln!(
                "--live needs your Spotify app's Client ID: --client-id or SPOTIFY_CLIENT_ID"
            );
            std::process::exit(2);
        };
        let (tx, record, idle) = (
            update_tx.clone(),
            args.record.clone(),
            idle_after_ms.clone(),
        );
        std::thread::spawn(move || live::run(client_id, record, idle, tx, control_rx));
    } else {
        let tx = update_tx.clone();
        std::thread::spawn(move || fake::run(tx, control_rx));
    }
    let mut units = if args.fahrenheit {
        Units::Fahrenheit
    } else {
        Units::Celsius
    };
    // Tells the weather thread which idle screen is selected, since it only
    // fetches what that screen needs. `None` with fake weather.
    let mut weather_idle_mode = IdleMode::DigitalClock;
    let mut weather_controls: Option<Sender<IdleMode>> = match args.city.clone() {
        Some(city) => Some(weather::start_city(
            city,
            units,
            weather_idle_mode,
            update_tx.clone(),
        )),
        None => {
            weather::fake(update_tx.clone());
            None
        }
    };
    // Kept for the setup portal, which starts later.
    let portal_updates = update_tx;
    let mut portal_started = false;
    let mut clock_24h = args.clock_24h;
    let mut weather_location = portal_settings(&args, units, clock_24h).weather;
    let mut zone = args
        .timezone
        .as_deref()
        .and_then(find_timezone)
        .or_else(|| {
            iana_time_zone::get_timezone()
                .ok()
                .and_then(|n| timezone::find(&n))
        })
        .unwrap_or(TimeZone::UTC);

    let settings = OutputSettingsBuilder::new()
        .scale(args.scale)
        .pixel_spacing(1)
        .build();
    let mut window = Window::new("On-Air", &settings);
    let mut display = SimulatorDisplay::<Rgb888>::new(Size::new(WIDTH, HEIGHT));
    let mut framebuffer = Framebuffer::new();
    let mut pipeline = FramePipeline::for_screen(DEFAULT_POWER);
    let mut shown = vec![0u8; (WIDTH * HEIGHT * 3) as usize];

    let start = Instant::now();
    // A made-up MAC address; the device uses its real one.
    let setup_network = on_air_core::setup::network_name([0x34, 0x85, 0x18, 0x00, 0x1a, 0x2b]);
    let mut app = App::new(IdleMode::DigitalClock);
    app.set_idle_after_ms(idle_after_ms.load(Ordering::Relaxed));
    let mut track: Option<NowPlaying> = None;
    let mut buttons = Buttons::new();
    let mut knob = Knob::new(DEFAULT_FLOOR);
    let mut knob_position: i32 = ADC_MAX as i32 * 3 / 4;
    let mut brightness: u8 = 255;
    let mut pressed = [false; 4];

    // The real WiFi state machine, joining a pretend router that W turns
    // off and on. A join takes half a second.
    let mut router_up = true;
    let mut wifi = WifiLink::new(true, 0, random_u32());
    let mut join_done_at: Option<u64> = None;
    if args.portal {
        wifi.open_portal();
    }
    // What Spotify last said, so an outage shows the "No WiFi" badge too.
    let mut spotify_online: Option<bool> = None;
    let mut shown_net: Option<NetStatus> = None;
    let mut art: Option<Vec<u8>> = None;
    let mut weather: Option<(Reading, Instant)> = None;
    let mut tide_turns: Vec<TideTurn> = Vec::new();
    let mut sign_in_needed = false;

    loop {
        let now = start.elapsed().as_millis() as u64;

        // Inputs.
        window.update(&display);
        let events: Vec<_> = window.events().collect();
        for event in events {
            match event {
                SimulatorEvent::Quit => return,
                // Brightness first, so holding a key repeats.
                SimulatorEvent::KeyDown { keycode, .. }
                    if matches!(
                        keycode,
                        Keycode::MINUS
                            | Keycode::KP_MINUS
                            | Keycode::EQUALS
                            | Keycode::PLUS
                            | Keycode::KP_PLUS
                    ) =>
                {
                    let step = if matches!(keycode, Keycode::MINUS | Keycode::KP_MINUS) {
                        -KNOB_STEP
                    } else {
                        KNOB_STEP
                    };
                    knob_position = (knob_position + step).clamp(0, ADC_MAX as i32);
                }
                SimulatorEvent::KeyDown {
                    keycode,
                    repeat: false,
                    ..
                } => {
                    if let Some(i) = button_index(keycode) {
                        pressed[i] = true;
                    }
                    match keycode {
                        Keycode::W => {
                            router_up = !router_up;
                            println!("router {}", if router_up { "on" } else { "off" });
                            if !router_up && wifi.status() == NetStatus::Online {
                                wifi.handle(now, WifiEvent::Failed);
                            }
                        }
                        Keycode::S => {
                            sign_in_needed = !sign_in_needed;
                            let event = if sign_in_needed {
                                Event::SignInNeeded
                            } else {
                                Event::SignedIn
                            };
                            app.handle(event, now);
                        }
                        Keycode::N => {
                            let _ = controls.send(Control::StopPlayback);
                        }
                        // Like finishing setup: restart and join the network.
                        Keycode::ESCAPE => {
                            wifi = WifiLink::new(true, now, random_u32());
                            join_done_at = None;
                        }
                        _ => {}
                    }
                }
                SimulatorEvent::KeyUp { keycode, .. } => {
                    if let Some(i) = button_index(keycode) {
                        pressed[i] = false;
                    }
                }
                _ => {}
            }
        }

        let mut inputs = Vec::new();
        buttons.update(pressed, now, |e| inputs.push(e));
        for input in inputs {
            if let Some(action) = app.handle(Event::Input(input), now) {
                match action {
                    Action::Spotify(command) => {
                        let _ = controls.send(Control::Spotify(command));
                    }
                    Action::SaveIdleMode(mode) => println!("idle screen: {mode:?} (saved)"),
                    Action::OpenPortal => wifi.open_portal(),
                    // Settings are erased, so setup starts over.
                    Action::FactoryReset => {
                        println!("factory reset");
                        app = App::new(IdleMode::DigitalClock);
                        app.set_idle_after_ms(idle_after_ms.load(Ordering::Relaxed));
                        wifi = WifiLink::new(false, now, random_u32());
                        join_done_at = None;
                        shown_net = None;
                    }
                }
            }
            if let InputEvent::Tap(button) = input {
                println!("tap: {button:?}");
            }
        }
        if let Some(level) = knob.update(knob_position as u16) {
            brightness = level;
        }

        // WiFi.
        if let Some(at) = join_done_at
            && now >= at
        {
            join_done_at = None;
            let event = if router_up {
                WifiEvent::Joined
            } else {
                WifiEvent::Failed
            };
            println!("wifi: {event:?}");
            wifi.handle(now, event);
        }
        match wifi.next(now) {
            WifiNext::Join => {
                println!("wifi: joining");
                join_done_at = Some(now + 500);
            }
            WifiNext::StartPortal => {
                println!("setup portal opened (Esc to leave)");
                if !portal_started {
                    portal_started = true;
                    let settings = portal_settings(&args, units, clock_24h);
                    portal::start(
                        settings,
                        setup_network.clone(),
                        portal_updates.clone(),
                        !args.no_browser,
                    );
                }
            }
            WifiNext::Wait(_) | WifiNext::Idle => {}
        }

        // Background updates.
        while let Ok(update) = updates.try_recv() {
            match update {
                Update::Net(status) => spotify_online = Some(status == NetStatus::Online),
                Update::Art(frame) => {
                    if frame.is_some() {
                        pipeline.start_crossfade(now);
                    }
                    art = frame;
                }
                Update::NowPlaying(Some(now_playing)) => {
                    let playback = Playback::Track {
                        is_playing: now_playing.is_playing,
                        has_art: art.is_some(),
                    };
                    app.handle(Event::Playback(playback), now);
                    track = Some(now_playing);
                }
                Update::NowPlaying(None) => {
                    app.handle(Event::Playback(Playback::Nothing), now);
                    track = None;
                }
                Update::SignInNeeded => {
                    sign_in_needed = true;
                    app.handle(Event::SignInNeeded, now);
                }
                Update::SignedIn => {
                    sign_in_needed = false;
                    app.handle(Event::SignedIn, now);
                }
                Update::CommandFailed(failure) => {
                    app.handle(Event::CommandFailed(failure), now);
                }
                Update::Weather(reading) => weather = Some((reading, Instant::now())),
                Update::TideSupport(support) => {
                    app.set_tides_available(support == TideSupport::Available);
                }
                Update::Tides(turns) => tide_turns = turns,
                Update::DisplaySaved(settings) => {
                    clock_24h = settings.clock_24h;
                    app.set_idle_after_ms(settings.idle_after_ms());
                    idle_after_ms.store(settings.idle_after_ms(), Ordering::Relaxed);
                    if let Some(found) = timezone::find(&settings.timezone) {
                        zone = found;
                    }
                    app.set_tides_available(settings.has_tides());
                    // Restart the weather only if the city or units changed.
                    // Replacing the controls stops the old thread.
                    if settings.weather != weather_location || settings.units != units {
                        units = settings.units;
                        weather_location = settings.weather.clone();
                        weather = None;
                        tide_turns.clear();
                        weather_idle_mode = app.idle_mode();
                        weather_controls = settings.weather.map(|place| {
                            weather::start_at(
                                place.lat,
                                place.lon,
                                units,
                                settings.tides,
                                weather_idle_mode,
                                portal_updates.clone(),
                            )
                        });
                    }
                    println!("display settings applied");
                }
                Update::PortalDone => {
                    println!("setup finished");
                    wifi = WifiLink::new(true, now, random_u32());
                    join_done_at = None;
                }
                Update::Timezone(name) => {
                    if args.timezone.is_none()
                        && let Some(found) = find_timezone(&name)
                    {
                        zone = found;
                    }
                }
            }
        }

        // Network status: WiFi, or Spotify being out of reach while the
        // WiFi is up.
        let net = match (wifi.status(), spotify_online) {
            (NetStatus::Online, Some(false)) => NetStatus::Lost,
            (status, _) => status,
        };
        if shown_net != Some(net) {
            shown_net = Some(net);
            app.handle(Event::Net(net), now);
        }

        // The weather thread follows the selected idle screen.
        if app.idle_mode() != weather_idle_mode {
            weather_idle_mode = app.idle_mode();
            if let Some(controls) = &weather_controls {
                let _ = controls.send(weather_idle_mode);
            }
        }

        // Draw.
        let unix_s = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let scene = Scene {
            view: app.view(now),
            art: art.as_deref(),
            time: timezone::local_time(&zone, unix_s),
            clock_24h,
            weather: weather.map(|(reading, at)| WeatherInfo {
                reading,
                stale: at.elapsed().as_secs() > STALE_AFTER_S,
                units,
            }),
            tides_available: app.tides_available(),
            tides: (!tide_turns.is_empty()).then(|| {
                let next = tide::next_tides(&tide_turns, unix_s);
                let local = |at: Option<i64>| at.and_then(|s| timezone::local_time(&zone, s));
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
            setup_network: &setup_network,
            now_ms: now,
        };
        on_air_ui::draw(&scene, &mut framebuffer).expect("framebuffer can't fail");
        pipeline.render(now, framebuffer.as_bytes(), brightness, &mut shown);
        let colors = shown.chunks_exact(3).map(|p| Rgb888::new(p[0], p[1], p[2]));
        display
            .fill_contiguous(
                &Rectangle::new(Point::zero(), Size::new(WIDTH, HEIGHT)),
                colors,
            )
            .expect("simulator display can't fail");

        std::thread::sleep(Duration::from_millis(POLL_INTERVAL_MS));
    }
}
