//! Desktop simulator: the real On Air app state, screens and inputs in a
//! window, with either a fake player or your real Spotify account.

mod fake;
mod live;
mod messages;
mod weather;

use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use clap::Parser;
use embedded_graphics::{pixelcolor::Rgb888, prelude::*, primitives::Rectangle};
use embedded_graphics_simulator::sdl2::Keycode;
use embedded_graphics_simulator::{
    OutputSettingsBuilder, SimulatorDisplay, SimulatorEvent, Window,
};
use on_air_core::app::{Action, App, Event, NetStatus, Playback};
use on_air_core::input::{ADC_MAX, Buttons, DEFAULT_FLOOR, InputEvent, Knob, POLL_INTERVAL_MS};
use on_air_core::settings::IdleMode;
use on_air_core::timezone::{self, TimeZone};
use on_air_core::weather::{Reading, STALE_AFTER_S, Units};
use on_air_ui::{Framebuffer, HEIGHT, Scene, WIDTH, WeatherInfo};

use messages::{Control, Update};

const KEYS_HELP: &str = "\
Keys:
  1  Play/Pause    2  Previous    3  Next    4  Mode (hold 5 s: setup portal)
  1+4 held 10 s    factory reset
  - / +            brightness knob down / up (hold to repeat)
  W  toggle WiFi lost    S  toggle Spotify sign-in needed
  N  stop playback (fake player)    Esc  leave the setup portal";

/// Environment variables the options read, e.g. from `.env`.
const ENV_KEYS: [&str; 8] = [
    "SPOTIFY_CLIENT_ID",
    "ON_AIR_LIVE",
    "ON_AIR_CITY",
    "ON_AIR_FAHRENHEIT",
    "ON_AIR_CLOCK_24H",
    "ON_AIR_TIMEZONE",
    "ON_AIR_RECORD",
    "ON_AIR_SCALE",
];

/// How far one press of - or + turns the simulated knob, in ADC counts.
const KNOB_STEP: i32 = 256;

#[derive(Parser)]
#[command(about = "Desktop simulator for the On Air panel", after_help = KEYS_HELP)]
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

    if args.live {
        let Some(client_id) = args.client_id.clone().filter(|id| !id.trim().is_empty()) else {
            eprintln!(
                "--live needs your Spotify app's Client ID: --client-id or SPOTIFY_CLIENT_ID"
            );
            std::process::exit(2);
        };
        let (tx, record) = (update_tx.clone(), args.record.clone());
        std::thread::spawn(move || live::run(client_id, record, tx, control_rx));
    } else {
        let tx = update_tx.clone();
        std::thread::spawn(move || fake::run(tx, control_rx));
    }
    match args.city.clone() {
        Some(city) => {
            let units = if args.fahrenheit {
                Units::Fahrenheit
            } else {
                Units::Celsius
            };
            let tx = update_tx.clone();
            std::thread::spawn(move || weather::run(city, units, tx));
        }
        None => weather::fake(update_tx.clone()),
    }
    drop(update_tx);

    let units = if args.fahrenheit {
        Units::Fahrenheit
    } else {
        Units::Celsius
    };
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
    let mut window = Window::new("On Air", &settings);
    let mut display = SimulatorDisplay::<Rgb888>::new(Size::new(WIDTH, HEIGHT));
    let mut framebuffer = Framebuffer::new();

    let start = Instant::now();
    let mut app = App::new(IdleMode::DigitalClock);
    let mut buttons = Buttons::new();
    let mut knob = Knob::new(DEFAULT_FLOOR);
    let mut knob_position: i32 = ADC_MAX as i32 * 3 / 4;
    let mut brightness: u8 = 255;
    let mut pressed = [false; 4];
    let mut art: Option<Vec<u8>> = None;
    let mut weather: Option<(Reading, Instant)> = None;
    let mut no_wifi = false;
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
                            no_wifi = !no_wifi;
                            let status = if no_wifi {
                                NetStatus::Lost
                            } else {
                                NetStatus::Online
                            };
                            app.handle(Event::Net(status), now);
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
                        Keycode::ESCAPE => {
                            app.handle(Event::Net(NetStatus::Online), now);
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
                    Action::OpenPortal => {
                        println!("setup portal opened (Esc to leave)");
                        app.handle(Event::Net(NetStatus::Portal), now);
                    }
                    Action::FactoryReset => {
                        println!("factory reset");
                        app = App::new(IdleMode::DigitalClock);
                        app.handle(Event::Net(NetStatus::Online), now);
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

        // Background updates.
        while let Ok(update) = updates.try_recv() {
            match update {
                Update::Net(status) => {
                    if !no_wifi {
                        app.handle(Event::Net(status), now);
                    }
                }
                Update::Art(frame) => art = frame,
                Update::NowPlaying(Some(now_playing)) => {
                    let playback = Playback::Track {
                        is_playing: now_playing.is_playing,
                        has_art: art.is_some(),
                    };
                    app.handle(Event::Playback(playback), now);
                }
                Update::NowPlaying(None) => {
                    app.handle(Event::Playback(Playback::Nothing), now);
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
                Update::Timezone(name) => {
                    if args.timezone.is_none()
                        && let Some(found) = find_timezone(&name)
                    {
                        zone = found;
                    }
                }
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
            clock_24h: args.clock_24h,
            weather: weather.map(|(reading, at)| WeatherInfo {
                reading,
                stale: at.elapsed().as_secs() > STALE_AFTER_S,
                units,
            }),
            setup_network: "OnAir-Setup-1A2B",
            now_ms: now,
        };
        on_air_ui::draw(&scene, &mut framebuffer).expect("framebuffer can't fail");
        on_air_core::image::scale(framebuffer.as_bytes_mut(), brightness);
        let colors = framebuffer
            .as_bytes()
            .chunks_exact(3)
            .map(|p| Rgb888::new(p[0], p[1], p[2]));
        display
            .fill_contiguous(
                &Rectangle::new(Point::zero(), Size::new(WIDTH, HEIGHT)),
                colors,
            )
            .expect("simulator display can't fail");

        std::thread::sleep(Duration::from_millis(POLL_INTERVAL_MS));
    }
}
