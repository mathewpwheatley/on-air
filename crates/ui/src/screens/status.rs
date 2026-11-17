//! Setup, connecting and sign-in screens.

use embedded_graphics::{pixelcolor::Rgb888, prelude::*, primitives::Rectangle};
use qrcodegen_no_heap::{QrCode, QrCodeEcc, Version};

use crate::icons::{self, Icon};
use crate::text::{self, Align, DIM, GREY, SMALL, TINY, WHITE};

const QR_VERSION: Version = Version::new(3);
const QR_BUFFER: usize = QR_VERSION.buffer_len();

/// QR code for joining the open setup network, with a WiFi icon and its
/// name underneath.
pub fn setup<D>(network: &str, target: &mut D) -> Result<(), D::Error>
where
    D: DrawTarget<Color = Rgb888>,
{
    // Escape the characters the WiFi QR format reserves.
    let mut payload = [0u8; 96];
    let mut len = 0;
    for &b in b"WIFI:S:"
        .iter()
        .chain(network.as_bytes().iter().flat_map(|b| {
            let escaped: &[u8] = match b {
                b'\\' => b"\\\\",
                b';' => b"\\;",
                b',' => b"\\,",
                b':' => b"\\:",
                b'"' => b"\\\"",
                _ => core::slice::from_ref(b),
            };
            escaped.iter()
        }))
        .chain(b";;".iter())
    {
        if len < payload.len() {
            payload[len] = b;
            len += 1;
        }
    }
    let payload = core::str::from_utf8(&payload[..len]).unwrap_or("");

    draw_qr(payload, 56, target)?;

    // The WiFi icon before the name, so it reads as a network to join.
    // Together they're centered under the QR code.
    const GAP: i32 = 2;
    let icon_size = Icon::Wifi.size();
    let left = 32 - (icon_size.width as i32 + GAP + text::width(&SMALL, network) as i32) / 2;
    icons::draw(
        Icon::Wifi,
        Point::new(left, 64 - icon_size.height as i32),
        1,
        target,
    )?;
    text::draw(
        &SMALL,
        network,
        Point::new(left + icon_size.width as i32 + GAP, 60),
        Align::Left,
        WHITE,
        target,
    )?;
    Ok(())
}

pub fn connecting<D>(now_ms: u64, target: &mut D) -> Result<(), D::Error>
where
    D: DrawTarget<Color = Rgb888>,
{
    icons::draw_centered(Icon::Wifi, 32, 14, 2, target)?;
    text::draw(
        &SMALL,
        "CONNECTING",
        Point::new(32, 42),
        Align::Center,
        GREY,
        target,
    )?;
    // Three dots lighting up in turn.
    let lit = (now_ms / 400 % 4) as i32;
    for i in 0..3 {
        let color = if i < lit { WHITE } else { DIM };
        target.fill_solid(
            &Rectangle::new(Point::new(26 + i * 5, 52), Size::new(2, 2)),
            color,
        )?;
    }
    Ok(())
}

/// A QR code for `payload`, centered in the top `height` rows, at 2× when
/// it fits.
fn draw_qr<D>(payload: &str, height: i32, target: &mut D) -> Result<(), D::Error>
where
    D: DrawTarget<Color = Rgb888>,
{
    let mut temp = [0u8; QR_BUFFER];
    let mut out = [0u8; QR_BUFFER];
    let Ok(qr) = QrCode::encode_text(
        payload,
        &mut temp,
        &mut out,
        QrCodeEcc::Low,
        Version::MIN,
        QR_VERSION,
        None,
        true,
    ) else {
        return Ok(());
    };
    let modules = qr.size();
    let scale = if modules * 2 + 4 <= height { 2 } else { 1 };
    let side = modules * scale + 2 * scale; // one-module quiet zone each side
    let origin = Point::new((64 - side) / 2, (height - side) / 2);
    target.fill_solid(
        &Rectangle::new(origin, Size::new(side as u32, side as u32)),
        WHITE,
    )?;
    for y in 0..modules {
        for x in 0..modules {
            if qr.get_module(x, y) {
                let at = origin + Point::new((x + 1) * scale, (y + 1) * scale);
                target.fill_solid(
                    &Rectangle::new(at, Size::new(scale as u32, scale as u32)),
                    Rgb888::BLACK,
                )?;
            }
        }
    }
    Ok(())
}

/// Spotify isn't signed in. With `url` (the board's sign-in page on the home
/// network), a QR code to scan with a phone on the same WiFi; without it,
/// how to reach setup.
pub fn sign_in_needed<D>(url: Option<&str>, target: &mut D) -> Result<(), D::Error>
where
    D: DrawTarget<Color = Rgb888>,
{
    if let Some(url) = url {
        draw_qr(url, 56, target)?;
        text::draw(
            &TINY,
            "SPOTIFY SIGN IN",
            Point::new(32, 61),
            Align::Center,
            WHITE,
            target,
        )?;
        return Ok(());
    }
    icons::draw_centered(Icon::Music, 32, 6, 2, target)?;
    text::draw(
        &SMALL,
        "SIGN IN",
        Point::new(32, 34),
        Align::Center,
        WHITE,
        target,
    )?;
    text::draw(
        &TINY,
        "HOLD MODE",
        Point::new(32, 46),
        Align::Center,
        GREY,
        target,
    )?;
    text::draw(
        &TINY,
        "TO SET UP",
        Point::new(32, 54),
        Align::Center,
        GREY,
        target,
    )?;
    Ok(())
}
