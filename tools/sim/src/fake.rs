//! A pretend Spotify, so the simulator works without an account.

use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use on_air_core::app::NetStatus;
use on_air_core::image;
use on_air_core::spotify::{Command, NowPlaying};

use crate::messages::{Control, Update};

struct Track {
    title: &'static str,
    artist: &'static str,
    album: Option<&'static str>,
    duration_ms: u32,
    art: Option<Vec<u8>>,
}

fn tracks() -> Vec<Track> {
    let fixture = |bytes: &[u8]| image::decode_art(bytes).ok();
    vec![
        Track {
            title: "Gradient",
            artist: "Test Pattern",
            album: Some("Bring-Up Sessions"),
            duration_ms: 40_000,
            art: Some(gradient()),
        },
        Track {
            title: "Red and Blue (Extended Club Mix)",
            artist: "The Fixtures",
            album: Some("Sixty-Four Pixels Squared"),
            duration_ms: 30_000,
            art: fixture(include_bytes!(
                "../../../crates/core/tests/fixtures/art_64.jpg"
            )),
        },
        Track {
            title: "Green and White",
            artist: "The Fixtures",
            album: Some("Sixty-Four Pixels Squared"),
            duration_ms: 30_000,
            art: fixture(include_bytes!(
                "../../../crates/core/tests/fixtures/art_300.jpg"
            )),
        },
        Track {
            title: "No Cover",
            artist: "Local File",
            album: None,
            duration_ms: 20_000,
            art: None,
        },
    ]
}

fn gradient() -> Vec<u8> {
    let mut art = Vec::with_capacity(image::FRAME_LEN);
    for y in 0..64u32 {
        for x in 0..64u32 {
            art.extend([(x * 4) as u8, (y * 4) as u8, (255 - (x + y) * 2) as u8]);
        }
    }
    art
}

pub fn run(updates: Sender<Update>, controls: Receiver<Control>) {
    let tracks = tracks();
    // Pretend to connect for a moment, like the real device.
    std::thread::sleep(Duration::from_millis(1_500));
    let _ = updates.send(Update::Net(NetStatus::Online));

    let mut index = 0;
    let mut playing = true;
    let mut stopped = false;
    let mut progress_ms: u32 = 0;
    let mut last_tick = Instant::now();
    send_track(&updates, &tracks[index], playing, progress_ms, true);

    loop {
        match controls.recv_timeout(Duration::from_millis(500)) {
            Ok(Control::Spotify(command)) => {
                let changed = match command {
                    Command::Play => {
                        playing = true;
                        stopped = false;
                        false
                    }
                    Command::Pause => {
                        playing = false;
                        false
                    }
                    Command::Next => {
                        index = (index + 1) % tracks.len();
                        progress_ms = 0;
                        stopped = false;
                        true
                    }
                    Command::Previous => {
                        index = (index + tracks.len() - 1) % tracks.len();
                        progress_ms = 0;
                        stopped = false;
                        true
                    }
                };
                if stopped {
                    continue;
                }
                send_track(&updates, &tracks[index], playing, progress_ms, changed);
            }
            Ok(Control::StopPlayback) => {
                stopped = true;
                playing = false;
                let _ = updates.send(Update::NowPlaying(None));
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }

        let elapsed = last_tick.elapsed().as_millis() as u32;
        last_tick = Instant::now();
        if stopped {
            continue;
        }
        if playing {
            progress_ms += elapsed;
        }
        let mut changed = false;
        if progress_ms >= tracks[index].duration_ms {
            index = (index + 1) % tracks.len();
            progress_ms = 0;
            changed = true;
        }
        send_track(&updates, &tracks[index], playing, progress_ms, changed);
    }
}

fn send_track(
    updates: &Sender<Update>,
    track: &Track,
    playing: bool,
    progress_ms: u32,
    new_track: bool,
) {
    if new_track {
        let _ = updates.send(Update::Art(track.art.clone()));
    }
    let _ = updates.send(Update::NowPlaying(Some(NowPlaying {
        item_id: track.title.to_string(),
        title: track.title.to_string(),
        artist: track.artist.to_string(),
        album: track.album.map(String::from),
        is_playing: playing,
        progress_ms,
        duration_ms: track.duration_ms,
        art_url: None,
    })));
}
