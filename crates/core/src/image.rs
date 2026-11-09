//! Cover art and frame processing: JPEG decode, resize, gamma, crossfade
//! and the current cap.
//!
//! Frames are 64×64 RGB888, row-major, 3 bytes per pixel.

use alloc::vec;
use alloc::vec::Vec;
use zune_core::bytestream::ZCursor;
use zune_core::colorspace::ColorSpace;
use zune_core::options::DecoderOptions;
use zune_jpeg::JpegDecoder;

pub const SIZE: usize = 64;
pub const FRAME_LEN: usize = SIZE * SIZE * 3;
/// Refuse to decode covers bigger than this (memory guard).
pub const MAX_SOURCE_DIM: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    /// Not a JPEG we can read.
    Invalid,
    /// Larger than [`MAX_SOURCE_DIM`].
    TooLarge,
}

/// Decode a cover JPEG into a 64×64 frame, resizing if needed.
pub fn decode_art(jpeg: &[u8]) -> Result<Vec<u8>, DecodeError> {
    let options = DecoderOptions::default()
        .jpeg_set_out_colorspace(ColorSpace::RGB)
        .set_max_width(MAX_SOURCE_DIM)
        .set_max_height(MAX_SOURCE_DIM);
    let mut decoder = JpegDecoder::new_with_options(ZCursor::new(jpeg), options);
    decoder.decode_headers().map_err(|_| DecodeError::Invalid)?;
    let info = decoder.info().ok_or(DecodeError::Invalid)?;
    let (w, h) = (info.width as usize, info.height as usize);
    if w > MAX_SOURCE_DIM || h > MAX_SOURCE_DIM {
        return Err(DecodeError::TooLarge);
    }
    let pixels = decoder.decode().map_err(|_| DecodeError::Invalid)?;
    if pixels.len() != w * h * 3 {
        return Err(DecodeError::Invalid);
    }
    Ok(if w == SIZE && h == SIZE {
        pixels
    } else {
        resize_to_panel(&pixels, w, h)
    })
}

/// Resize any RGB888 image to 64×64. Non-square images are center-cropped.
/// Shrinking averages each source block (box filter); growing repeats
/// pixels.
pub fn resize_to_panel(src: &[u8], width: usize, height: usize) -> Vec<u8> {
    let side = width.min(height);
    let (x0, y0) = ((width - side) / 2, (height - side) / 2);
    let mut out = vec![0u8; FRAME_LEN];
    for oy in 0..SIZE {
        let sy0 = y0 + oy * side / SIZE;
        let sy1 = (y0 + (oy + 1) * side / SIZE).max(sy0 + 1);
        for ox in 0..SIZE {
            let sx0 = x0 + ox * side / SIZE;
            let sx1 = (x0 + (ox + 1) * side / SIZE).max(sx0 + 1);
            let mut sum = [0u32; 3];
            for sy in sy0..sy1 {
                for sx in sx0..sx1 {
                    let i = (sy * width + sx) * 3;
                    for c in 0..3 {
                        sum[c] += src[i + c] as u32;
                    }
                }
            }
            let n = ((sy1 - sy0) * (sx1 - sx0)) as u32;
            let o = (oy * SIZE + ox) * 3;
            for c in 0..3 {
                out[o + c] = ((sum[c] + n / 2) / n) as u8;
            }
        }
    }
    out
}

/// Lookup table mapping 8-bit sRGB-ish values to linear LED drive levels.
pub fn gamma_table(gamma: f32) -> [u8; 256] {
    let mut t = [0u8; 256];
    for (i, v) in t.iter_mut().enumerate() {
        *v = libm::roundf(libm::powf(i as f32 / 255.0, gamma) * 255.0) as u8;
    }
    t
}

/// Blend two frames: `t` = 0 gives `from`, 255 gives `to`.
pub fn crossfade(from: &[u8], to: &[u8], t: u8, out: &mut [u8]) {
    let t = t as u16;
    for ((o, &a), &b) in out.iter_mut().zip(from).zip(to) {
        *o = ((a as u16 * (255 - t) + b as u16 * t + 127) / 255) as u8;
    }
}

/// Scale every channel by `brightness / 255`.
pub fn scale(frame: &mut [u8], brightness: u8) {
    let b = brightness as u16;
    for v in frame.iter_mut() {
        *v = ((*v as u16 * b + 127) / 255) as u8;
    }
}

/// Panel current model for the cap.
#[derive(Debug, Clone, Copy)]
pub struct PowerModel {
    /// Panel current at full white, full brightness. Measure on the real panel.
    pub full_white_ma: u32,
    /// Most the panel may draw. The 2.5 A total cap minus the board's
    /// worst case (about 0.35 A).
    pub panel_budget_ma: u32,
}

pub const DEFAULT_POWER: PowerModel = PowerModel {
    full_white_ma: 4_000,
    panel_budget_ma: 2_150,
};

/// Estimated panel current for a frame of LED drive levels (after gamma)
/// shown at `brightness`.
pub fn estimate_ma(frame: &[u8], brightness: u8, model: &PowerModel) -> u32 {
    let sum: u64 = frame.iter().map(|&v| v as u64).sum();
    let full: u64 = frame.len() as u64 * 255 * 255;
    if full == 0 {
        return 0;
    }
    (sum * brightness as u64 * model.full_white_ma as u64 / full) as u32
}

/// The brightness to actually use so the panel stays inside its budget.
pub fn cap_brightness(frame: &[u8], brightness: u8, model: &PowerModel) -> u8 {
    let estimate = estimate_ma(frame, brightness, model);
    if estimate <= model.panel_budget_ma {
        return brightness;
    }
    let capped = brightness as u64 * model.panel_budget_ma as u64 / estimate as u64;
    capped.max(1) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixel(frame: &[u8], x: usize, y: usize) -> [u8; 3] {
        let i = (y * SIZE + x) * 3;
        [frame[i], frame[i + 1], frame[i + 2]]
    }

    fn close(a: [u8; 3], b: [u8; 3]) -> bool {
        a.iter().zip(b).all(|(&x, y)| x.abs_diff(y) <= 12)
    }

    #[test]
    fn decodes_64px_cover() {
        let f = decode_art(include_bytes!("../tests/fixtures/art_64.jpg")).unwrap();
        assert_eq!(f.len(), FRAME_LEN);
        assert!(
            close(pixel(&f, 8, 32), [255, 0, 0]),
            "{:?}",
            pixel(&f, 8, 32)
        );
        assert!(
            close(pixel(&f, 56, 32), [0, 0, 255]),
            "{:?}",
            pixel(&f, 56, 32)
        );
    }

    #[test]
    fn decodes_and_shrinks_300px_cover() {
        let f = decode_art(include_bytes!("../tests/fixtures/art_300.jpg")).unwrap();
        assert_eq!(f.len(), FRAME_LEN);
        assert!(
            close(pixel(&f, 32, 8), [0, 255, 0]),
            "{:?}",
            pixel(&f, 32, 8)
        );
        assert!(
            close(pixel(&f, 32, 56), [255, 255, 255]),
            "{:?}",
            pixel(&f, 32, 56)
        );
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(decode_art(b"not a jpeg"), Err(DecodeError::Invalid));
        assert_eq!(decode_art(&[]), Err(DecodeError::Invalid));
    }

    #[test]
    fn resize_crops_to_center_square_and_upscales() {
        // 4×2 image: columns red, green, blue, white. The center square is
        // green|blue.
        let src = [
            255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255, //
            255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255,
        ];
        let f = resize_to_panel(&src, 4, 2);
        assert_eq!(pixel(&f, 0, 0), [0, 255, 0]);
        assert_eq!(pixel(&f, 63, 63), [0, 0, 255]);
    }

    #[test]
    fn gamma_table_ends_and_curve() {
        let t = gamma_table(2.2);
        assert_eq!((t[0], t[255]), (0, 255));
        assert!(t[128] < 64, "{}", t[128]);
        assert!(t.windows(2).all(|w| w[0] <= w[1]));
    }

    #[test]
    fn crossfade_endpoints_and_midpoint() {
        let a = [0u8, 100, 255];
        let b = [255u8, 100, 0];
        let mut out = [0u8; 3];
        crossfade(&a, &b, 0, &mut out);
        assert_eq!(out, a);
        crossfade(&a, &b, 255, &mut out);
        assert_eq!(out, b);
        crossfade(&a, &b, 128, &mut out);
        assert_eq!(out, [128, 100, 127]);
    }

    #[test]
    fn scale_brightness() {
        let mut f = [255u8, 128, 0];
        scale(&mut f, 102); // about 40%, the paused dim
        assert_eq!(f, [102, 51, 0]);
    }

    #[test]
    fn current_cap_only_dims_bright_frames() {
        let white = vec![255u8; FRAME_LEN];
        assert_eq!(estimate_ma(&white, 255, &DEFAULT_POWER), 4_000);
        let capped = cap_brightness(&white, 255, &DEFAULT_POWER);
        assert!(estimate_ma(&white, capped, &DEFAULT_POWER) <= DEFAULT_POWER.panel_budget_ma);
        assert_eq!(capped, 137);

        // Typical art: a dim frame passes through unchanged.
        let dim = vec![60u8; FRAME_LEN];
        assert_eq!(cap_brightness(&dim, 255, &DEFAULT_POWER), 255);
    }
}
