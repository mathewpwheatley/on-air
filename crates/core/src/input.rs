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
    use rstest::rstest;

    const UP: [bool; 4] = [false; 4];

    fn press(buttons: &[Button]) -> [bool; 4] {
        let mut p = UP;
        for &b in buttons {
            p[b as usize] = true;
        }
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

    /// Hold `held` until `until` ms, then let go. Returns the events while
    /// held, and all events once released.
    fn hold(held: &[Button], until: u64) -> (Vec<InputEvent>, Vec<InputEvent>) {
        let mut b = Buttons::new();
        let mut events = Vec::new();
        run(&mut b, press(held), 0, until, &mut events);
        let while_held = events.clone();
        run(
            &mut b,
            UP,
            until + POLL_INTERVAL_MS,
            until + 100,
            &mut events,
        );
        (while_held, events)
    }

    #[rstest]
    fn short_press_is_a_tap_on_release(
        #[values(Button::PlayPause, Button::Previous, Button::Next, Button::Mode)] button: Button,
    ) {
        let (while_held, all) = hold(&[button], 100);
        assert!(while_held.is_empty());
        assert_eq!(all, [InputEvent::Tap(button)]);
    }

    #[test]
    fn bounces_shorter_than_debounce_are_ignored() {
        let mut b = Buttons::new();
        let mut ev = Vec::new();
        for t in (0..60).step_by(5) {
            // Chatter: toggles every poll, never steady for four readings.
            let state = if t % 10 == 0 {
                press(&[Button::PlayPause])
            } else {
                UP
            };
            b.update(state, t, |e| ev.push(e));
        }
        run(&mut b, UP, 60, 200, &mut ev);
        assert!(ev.is_empty(), "{ev:?}");
    }

    /// A long hold fires its action once, while still held, and letting go
    /// afterwards doesn't add a tap.
    #[rstest]
    #[case::play_shows_track_info(&[Button::PlayPause], TRACK_INFO_HOLD_MS, InputEvent::ShowTrackInfo)]
    #[case::mode_opens_portal(&[Button::Mode], PORTAL_HOLD_MS, InputEvent::OpenPortal)]
    #[case::mode_and_play_reset_without_portal(
        &[Button::Mode, Button::PlayPause],
        RESET_HOLD_MS,
        InputEvent::FactoryReset,
    )]
    fn long_hold_fires_once(
        #[case] held: &[Button],
        #[case] hold_ms: u64,
        #[case] expected: InputEvent,
    ) {
        let (while_held, all) = hold(held, hold_ms * 3 / 2);
        assert_eq!(while_held, [expected]);
        assert_eq!(all, [expected]);
    }

    /// Letting go before the hold time is up.
    #[rstest]
    #[case::play_is_a_tap(&[Button::PlayPause], TRACK_INFO_HOLD_MS, &[InputEvent::Tap(Button::PlayPause)])]
    #[case::mode_is_a_tap(&[Button::Mode], PORTAL_HOLD_MS, &[InputEvent::Tap(Button::Mode)])]
    #[case::mode_and_play_do_nothing(&[Button::Mode, Button::PlayPause], RESET_HOLD_MS, &[])]
    fn released_before_the_hold_time(
        #[case] held: &[Button],
        #[case] hold_ms: u64,
        #[case] expected: &[InputEvent],
    ) {
        let (while_held, all) = hold(held, hold_ms - 100);
        assert!(while_held.is_empty(), "{while_held:?}");
        assert_eq!(all, expected);
    }

    #[rstest]
    #[case::fully_down(0, DEFAULT_FLOOR)]
    #[case::fully_up(ADC_MAX, 255)]
    fn knob_maps_ends_to_floor_and_full(#[case] position: u16, #[case] level: u8) {
        assert_eq!(Knob::new(DEFAULT_FLOOR).update(position), Some(level));
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
