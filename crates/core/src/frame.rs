//! Turns each drawn frame into what the panel shows: a crossfade when the
//! cover changes, gamma correction, the current cap and the brightness, in
//! one step.
//!
//! ```text
//! if cover_changed { pipeline.start_crossfade(now) }
//! ui::draw(&scene, &mut drawn);
//! let shown = pipeline.render(now, drawn.as_bytes(), knob_brightness, &mut panel_frame);
//! if shown.animating { redraw again soon }
//! ```

use alloc::vec;
use alloc::vec::Vec;

use crate::image::{self, PowerModel};

/// How long a crossfade between covers takes.
pub const CROSSFADE_MS: u64 = 300;
/// Gamma for the LED panel, so mid-tones don't look washed out.
pub const PANEL_GAMMA: f32 = 2.2;

/// What [`FramePipeline::render`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rendered {
    /// The brightness used, after the current cap.
    pub brightness: u8,
    /// A crossfade is still running: render again soon.
    pub animating: bool,
}

pub struct FramePipeline {
    gamma: [u8; 256],
    /// Output LED drive levels (the panel) rather than colors (a screen).
    panel: bool,
    power: PowerModel,
    /// The frame as LED drive levels, for the current estimate.
    levels: Vec<u8>,
    /// The last drawn frame, before gamma and brightness. The starting
    /// point of the next crossfade.
    last: Vec<u8>,
    /// What's on the panel when the current crossfade began, and when.
    crossfade: Option<(Vec<u8>, u64)>,
}

impl FramePipeline {
    /// For the LED panel: outputs gamma-corrected drive levels.
    pub fn for_panel(power: PowerModel) -> Self {
        Self::new(true, power)
    }

    /// For a computer screen, like the simulator's window, which applies its
    /// own gamma. The current cap still works from what the panel would
    /// draw, so brightness matches the device.
    pub fn for_screen(power: PowerModel) -> Self {
        Self::new(false, power)
    }

    fn new(panel: bool, power: PowerModel) -> Self {
        Self {
            gamma: image::gamma_table(PANEL_GAMMA),
            panel,
            power,
            levels: Vec::new(),
            last: Vec::new(),
            crossfade: None,
        }
    }

    /// Fade from what's on screen now to whatever is drawn next, over
    /// [`CROSSFADE_MS`]. Call when a new cover arrives.
    pub fn start_crossfade(&mut self, now: u64) {
        if !self.last.is_empty() {
            self.crossfade = Some((self.last.clone(), now));
        }
    }

    /// Process `drawn` (RGB888, as drawn by the UI) into `out`. Both are the
    /// same length.
    pub fn render(&mut self, now: u64, drawn: &[u8], brightness: u8, out: &mut [u8]) -> Rendered {
        let mut animating = false;
        match &self.crossfade {
            Some((from, start)) if from.len() == drawn.len() => {
                let elapsed = now.saturating_sub(*start);
                if elapsed < CROSSFADE_MS {
                    let step = (elapsed * 255 / CROSSFADE_MS) as u8;
                    image::crossfade(from, drawn, step, out);
                    animating = true;
                } else {
                    out.copy_from_slice(drawn);
                    self.crossfade = None;
                }
            }
            _ => {
                out.copy_from_slice(drawn);
                self.crossfade = None;
            }
        }
        // Keep the blend, so a new crossfade mid-way starts from what's
        // actually showing.
        if self.last.len() != out.len() {
            self.last = vec![0; out.len()];
        }
        self.last.copy_from_slice(out);

        if self.levels.len() != out.len() {
            self.levels = vec![0; out.len()];
        }
        for (level, &v) in self.levels.iter_mut().zip(out.iter()) {
            *level = self.gamma[v as usize];
        }
        let brightness = image::cap_brightness(&self.levels, brightness, &self.power);
        if self.panel {
            out.copy_from_slice(&self.levels);
        }
        image::scale(out, brightness);
        Rendered {
            brightness,
            animating,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::DEFAULT_POWER;
    use rstest::rstest;

    const LEN: usize = 64 * 64 * 3;

    fn solid(level: u8) -> Vec<u8> {
        vec![level; LEN]
    }

    fn linear() -> FramePipeline {
        FramePipeline::for_screen(DEFAULT_POWER)
    }

    fn render(
        p: &mut FramePipeline,
        now: u64,
        drawn: &[u8],
        brightness: u8,
    ) -> (Vec<u8>, Rendered) {
        let mut out = vec![0; drawn.len()];
        let r = p.render(now, drawn, brightness, &mut out);
        (out, r)
    }

    #[test]
    fn passes_through_without_a_crossfade() {
        let mut p = linear();
        let (out, r) = render(&mut p, 0, &solid(60), 255);
        assert_eq!(out, solid(60));
        assert_eq!(
            r,
            Rendered {
                brightness: 255,
                animating: false
            }
        );
    }

    #[rstest]
    #[case::start(0, 0, true)]
    #[case::halfway(150, 128, true)]
    #[case::almost_done(299, 254, true)]
    #[case::done(300, 255, false)]
    #[case::long_after(5_000, 255, false)]
    fn crossfade_between_covers(#[case] at: u64, #[case] level: u8, #[case] animating: bool) {
        let mut p = linear();
        render(&mut p, 0, &solid(0), 255);
        p.start_crossfade(1_000);
        let (out, r) = render(&mut p, 1_000 + at, &solid(255), 255);
        // The white frame is capped, so compare before brightness.
        assert_eq!(r.animating, animating);
        let unscaled = (out[0] as u32 * 255 / r.brightness as u32) as i32;
        assert!(
            (unscaled - level as i32).abs() <= 2,
            "{unscaled} vs {level}"
        );
    }

    #[test]
    fn crossfade_with_nothing_shown_yet_is_skipped() {
        let mut p = linear();
        p.start_crossfade(0);
        let (_, r) = render(&mut p, 0, &solid(60), 255);
        assert!(!r.animating);
    }

    #[test]
    fn new_crossfade_mid_way_starts_from_the_blend() {
        let mut p = linear();
        render(&mut p, 0, &solid(0), 255);
        p.start_crossfade(0);
        let (halfway, _) = render(&mut p, 150, &solid(100), 255);
        p.start_crossfade(150);
        let (start, _) = render(&mut p, 150, &solid(0), 255);
        assert_eq!(start, halfway);
    }

    #[test]
    fn gamma_darkens_mid_tones_only_on_the_panel() {
        let mut p = FramePipeline::for_panel(DEFAULT_POWER);
        let drawn = [0, 128, 255].repeat(LEN / 3);
        let (out, _) = render(&mut p, 0, &drawn, 255);
        assert_eq!(out[0], 0);
        assert!(out[1] < 64, "{}", out[1]);
        assert_eq!(out[2], 255);
    }

    #[test]
    fn screen_and_panel_cap_alike() {
        // Mid-grey everywhere: the panel draws far less than full white.
        let drawn = solid(180);
        let (_, screen) = render(&mut linear(), 0, &drawn, 255);
        let (_, panel) = render(&mut FramePipeline::for_panel(DEFAULT_POWER), 0, &drawn, 255);
        assert_eq!(screen.brightness, panel.brightness);
        assert_eq!(screen.brightness, 255);
    }

    #[rstest]
    #[case::dim_art_at_full(60, 255, 255)]
    #[case::white_is_capped(255, 255, 137)]
    #[case::knob_below_the_cap(255, 100, 100)]
    fn brightness_and_current_cap(#[case] level: u8, #[case] knob: u8, #[case] expected: u8) {
        let mut p = linear();
        let (out, r) = render(&mut p, 0, &solid(level), knob);
        assert_eq!(r.brightness, expected);
        assert_eq!(out[0], ((level as u32 * expected as u32 + 127) / 255) as u8);
    }
}
