//! Buttons and the brightness knob.
//!
//! The firmware samples the pins and calls `update` with the raw readings
//! and the current time; these types turn that into events.

use debouncr::{DebouncerStateful, Edge, Repeat4, debounce_stateful_4};

/// The four buttons, in pin order (A1–A4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    PlayPause = 0,
    Previous = 1,
    Next = 2,
    Mode = 3,
}

impl Button {
    pub const ALL: [Button; 4] = [
        Button::PlayPause,
        Button::Previous,
        Button::Next,
        Button::Mode,
    ];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputEvent {
    /// Released before any hold action fired.
    Tap(Button),
    /// Play/Pause held alone for [`TRACK_INFO_HOLD_MS`].
    ShowTrackInfo,
    /// Mode held for [`PORTAL_HOLD_MS`].
    OpenPortal,
    /// Mode + Play held together for [`RESET_HOLD_MS`].
    FactoryReset,
}

/// Poll the buttons this often. A press or release counts after four
/// steady readings in a row, so 20 ms.
pub const POLL_INTERVAL_MS: u64 = 5;
pub const TRACK_INFO_HOLD_MS: u64 = 2_000;
pub const PORTAL_HOLD_MS: u64 = 5_000;
pub const RESET_HOLD_MS: u64 = 10_000;

/// Debounces the buttons and detects taps, the Mode hold and the
/// Mode + Play hold.
pub struct Buttons {
    debounce: [DebouncerStateful<u8, Repeat4>; 4],
    down_since: [Option<u64>; 4],
    /// The press was used by a hold, so its release isn't a tap.
    consumed: [bool; 4],
    combo_since: Option<u64>,
    reset_fired: bool,
}

impl Default for Buttons {
    fn default() -> Self {
        Self::new()
    }
}

impl Buttons {
    pub fn new() -> Self {
        Self {
            debounce: core::array::from_fn(|_| debounce_stateful_4(false)),
            down_since: [None; 4],
            consumed: [false; 4],
            combo_since: None,
            reset_fired: false,
        }
    }

    /// `pressed[i]` is true while button `i` is held (after inverting the
    /// active-low pin). Call every [`POLL_INTERVAL_MS`]. `now_ms` is any
    /// monotonic clock.
    pub fn update(&mut self, pressed: [bool; 4], now_ms: u64, mut emit: impl FnMut(InputEvent)) {
        let mut released = [false; 4];
        for i in 0..4 {
            match self.debounce[i].update(pressed[i]) {
                Some(Edge::Rising) => {
                    self.down_since[i] = Some(now_ms);
                    self.consumed[i] = false;
                }
                Some(Edge::Falling) => released[i] = true,
                None => {}
            }
        }

        let mode = Button::Mode as usize;
        let play = Button::PlayPause as usize;

        // Mode + Play: factory reset. Holding both also cancels the Mode
        // hold and both taps.
        if self.down_since[mode].is_some() && self.down_since[play].is_some() {
            let since = *self.combo_since.get_or_insert(now_ms);
            self.consumed[mode] = true;
            self.consumed[play] = true;
            if !self.reset_fired && now_ms - since >= RESET_HOLD_MS {
                self.reset_fired = true;
                emit(InputEvent::FactoryReset);
            }
        } else {
            self.combo_since = None;
            self.reset_fired = false;
        }

        // Play alone held: show the track info once.
        if let Some(since) = self.down_since[play]
            && !self.consumed[play]
            && now_ms - since >= TRACK_INFO_HOLD_MS
        {
            self.consumed[play] = true;
            emit(InputEvent::ShowTrackInfo);
        }

        // Mode alone held: open the portal once.
        if let Some(since) = self.down_since[mode]
            && !self.consumed[mode]
            && now_ms - since >= PORTAL_HOLD_MS
        {
            self.consumed[mode] = true;
            emit(InputEvent::OpenPortal);
        }

        for (i, button) in Button::ALL.iter().enumerate() {
            if released[i] {
                if !self.consumed[i] {
                    emit(InputEvent::Tap(*button));
                }
                self.down_since[i] = None;
            }
        }
    }
}

/// 12-bit ADC full scale.
pub const ADC_MAX: u16 = 4095;
/// Readings below/above these count as fully down/up, since the ESP32-S3
/// ADC is unreliable at the very ends.
const DEAD_LOW: i32 = 40;
const DEAD_HIGH: i32 = 4055;
/// Ignore changes smaller than this many ADC counts, so a still knob doesn't
/// flicker between two levels.
const HYSTERESIS: i32 = 24;
/// Default minimum brightness: about 5%. The knob never blanks the panel.
pub const DEFAULT_FLOOR: u8 = 13;

/// Turns raw knob readings (20 Hz) into a brightness from `floor` to 255.
#[derive(Debug, Clone)]
pub struct Knob {
    floor: u8,
    /// Smoothed reading, ×16 for precision.
    filtered_x16: Option<i32>,
    reported: Option<i32>,
}

impl Knob {
    pub fn new(floor: u8) -> Self {
        Self {
            floor,
            filtered_x16: None,
            reported: None,
        }
    }

    /// Returns a new brightness when it changes enough to apply.
    pub fn update(&mut self, raw: u16) -> Option<u8> {
        let raw_x16 = raw.min(ADC_MAX) as i32 * 16;
        let filtered = match self.filtered_x16 {
            // Exponential moving average with weight 1/4.
            Some(f) => f + (raw_x16 - f) / 4,
            None => raw_x16,
        };
        self.filtered_x16 = Some(filtered);
        let position = filtered / 16;

        let moved = match self.reported {
            Some(r) => (position - r).abs() >= HYSTERESIS || at_end(position) != at_end(r),
            None => true,
        };
        if !moved {
            return None;
        }
        self.reported = Some(position);
        Some(curve(position, self.floor))
    }
}

fn at_end(position: i32) -> Option<bool> {
    if position <= DEAD_LOW {
        Some(false)
    } else if position >= DEAD_HIGH {
        Some(true)
    } else {
        None
    }
}

/// Perceptual curve: brightness grows with the square of knob travel, so the
/// low end has fine control.
fn curve(position: i32, floor: u8) -> u8 {
    let span = DEAD_HIGH - DEAD_LOW;
    let travel = (position - DEAD_LOW).clamp(0, span) as i64; // 0..=span
    let level = travel * travel * 1000 / (span as i64 * span as i64); // 0..=1000
    let floor = floor as i64;
    (floor + (255 - floor) * level / 1000) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    const UP: [bool; 4] = [false; 4];

    fn press(b: Button) -> [bool; 4] {
        let mut p = UP;
        p[b as usize] = true;
        p
    }

    /// Feed one state from `from` to `to` ms at the poll interval, collecting events.
    fn run(buttons: &mut Buttons, state: [bool; 4], from: u64, to: u64, out: &mut Vec<InputEvent>) {
        let mut t = from;
        while t <= to {
            buttons.update(state, t, |e| out.push(e));
            t += POLL_INTERVAL_MS;
        }
    }

    #[test]
    fn short_press_is_a_tap_on_release() {
        let mut b = Buttons::new();
        let mut ev = Vec::new();
        run(&mut b, press(Button::Next), 0, 100, &mut ev);
        assert!(ev.is_empty());
        run(&mut b, UP, 105, 200, &mut ev);
        assert_eq!(ev, [InputEvent::Tap(Button::Next)]);
    }

    #[test]
    fn bounces_shorter_than_debounce_are_ignored() {
        let mut b = Buttons::new();
        let mut ev = Vec::new();
        for t in (0..60).step_by(5) {
            // Chatter: toggles every poll, never steady for four readings.
            let state = if t % 10 == 0 {
                press(Button::PlayPause)
            } else {
                UP
            };
            b.update(state, t, |e| ev.push(e));
        }
        run(&mut b, UP, 60, 200, &mut ev);
        assert!(ev.is_empty(), "{ev:?}");
    }

    #[test]
    fn mode_hold_opens_portal_once_and_no_tap() {
        let mut b = Buttons::new();
        let mut ev = Vec::new();
        run(&mut b, press(Button::Mode), 0, 8_000, &mut ev);
        assert_eq!(ev, [InputEvent::OpenPortal]);
        run(&mut b, UP, 8_005, 8_100, &mut ev);
        assert_eq!(ev, [InputEvent::OpenPortal]);
    }

    #[test]
    fn play_hold_shows_track_info_once_and_no_tap() {
        let mut b = Buttons::new();
        let mut ev = Vec::new();
        run(&mut b, press(Button::PlayPause), 0, 1_900, &mut ev);
        assert!(ev.is_empty());
        run(&mut b, press(Button::PlayPause), 1_905, 4_000, &mut ev);
        assert_eq!(ev, [InputEvent::ShowTrackInfo]);
        run(&mut b, UP, 4_005, 4_100, &mut ev);
        assert_eq!(ev, [InputEvent::ShowTrackInfo]);
    }

    #[test]
    fn mode_tap_under_hold_time() {
        let mut b = Buttons::new();
        let mut ev = Vec::new();
        run(&mut b, press(Button::Mode), 0, 4_000, &mut ev);
        run(&mut b, UP, 4_005, 4_100, &mut ev);
        assert_eq!(ev, [InputEvent::Tap(Button::Mode)]);
    }

    #[test]
    fn mode_plus_play_resets_once_without_opening_portal() {
        let mut b = Buttons::new();
        let mut ev = Vec::new();
        let mut both = UP;
        both[Button::Mode as usize] = true;
        both[Button::PlayPause as usize] = true;
        run(&mut b, both, 0, 15_000, &mut ev);
        assert_eq!(ev, [InputEvent::FactoryReset]);
        run(&mut b, UP, 15_005, 15_100, &mut ev);
        assert_eq!(ev, [InputEvent::FactoryReset]);
    }

    #[test]
    fn letting_go_of_combo_early_does_nothing() {
        let mut b = Buttons::new();
        let mut ev = Vec::new();
        let mut both = UP;
        both[Button::Mode as usize] = true;
        both[Button::PlayPause as usize] = true;
        run(&mut b, both, 0, 3_000, &mut ev);
        run(&mut b, UP, 3_005, 3_100, &mut ev);
        assert!(ev.is_empty(), "{ev:?}");
    }

    #[test]
    fn knob_maps_ends_to_floor_and_full() {
        let mut k = Knob::new(DEFAULT_FLOOR);
        assert_eq!(k.update(0), Some(DEFAULT_FLOOR));
        let mut k = Knob::new(DEFAULT_FLOOR);
        assert_eq!(k.update(ADC_MAX), Some(255));
    }

    #[test]
    fn knob_curve_is_monotonic_and_fine_at_the_low_end() {
        let mut last = 0;
        for p in (0..=ADC_MAX as i32).step_by(16) {
            let v = curve(p, DEFAULT_FLOOR);
            assert!(v >= last);
            last = v;
        }
        // Halfway round gives about a quarter of the range above the floor.
        let mid = curve(2048, DEFAULT_FLOOR);
        assert!((70..=80).contains(&mid), "{mid}");
    }

    #[test]
    fn knob_ignores_noise_but_follows_real_moves() {
        let mut k = Knob::new(DEFAULT_FLOOR);
        assert!(k.update(2000).is_some());
        for noise in [2005, 1995, 2010, 1990, 2003] {
            assert_eq!(k.update(noise), None);
        }
        // A real turn settles at a new level within a few samples.
        let mut changed = false;
        for _ in 0..10 {
            changed |= k.update(3000).is_some();
        }
        assert!(changed);
    }

    #[test]
    fn knob_reaches_the_floor_when_turned_fully_down() {
        let mut k = Knob::new(DEFAULT_FLOOR);
        k.update(500);
        let mut last = None;
        for _ in 0..50 {
            if let Some(v) = k.update(0) {
                last = Some(v);
            }
        }
        assert_eq!(last, Some(DEFAULT_FLOOR));
    }
}
