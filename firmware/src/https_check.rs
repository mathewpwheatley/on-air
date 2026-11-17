//! The P5 TLS test: once the clock is set, makes one HTTPS GET to each host
//! the device needs, with certificates verified against the pinned roots in
//! `certs/roots.pem`, and logs the status and timing. Temporary, until the
//! Spotify and weather tasks make real requests.

use core::convert::Infallible;
use core::ffi::CStr;

use embassy_net::Stack;
use embassy_net::dns::DnsQueryType;
use embassy_net::tcp::TcpSocket;
use embassy_time::{Duration, Instant, Timer, with_timeout};
use embedded_io_async::Write;
use esp_hal::rng::Rng;
use log::{info, warn};
use mbedtls_rs::sys::hook::wall_clock::{MbedtlsWallClock, hook_wall_clock};
use mbedtls_rs::sys::tm;
use mbedtls_rs::{Certificate, ClientSessionConfig, Session, SessionConfig, Tls, X509};
use on_air_core::timezone::{self, TimeZone};
use static_cell::StaticCell;

use crate::sntp_task;

/// The root certificates, as one NUL-terminated PEM bundle for mbedtls.
const ROOTS: &CStr =
    match CStr::from_bytes_with_nul(concat!(include_str!("../certs/roots.pem"), "\0").as_bytes()) {
        Ok(roots) => roots,
        Err(_) => panic!("certs/roots.pem must be text with no NUL bytes"),
    };

const CHECKS: [(&CStr, &str); 5] = [
    (c"accounts.spotify.com", "/robots.txt"),
    (c"api.spotify.com", "/v1/me/player"),
    (
        c"i.scdn.co",
        "/image/ab67616d00001e02f33072178773789a4c2b8110",
    ),
    (
        c"api.open-meteo.com",
        "/v1/forecast?latitude=45.5&longitude=-122.7&current=temperature_2m",
    ),
    (
        c"geocoding-api.open-meteo.com",
        "/v1/search?name=Portland&count=1",
    ),
];

/// How long to wait for a server to answer our TLS close before dropping
/// the connection.
const CLOSE_GRACE_MS: u64 = 1_000;

/// TLS randomness from the hardware RNG, which is a true random source
/// while the radio is on.
struct HardwareRng(Rng);

impl rand_core::TryRng for HardwareRng {
    type Error = Infallible;

    fn try_next_u32(&mut self) -> Result<u32, Infallible> {
        Ok(self.0.random())
    }

    fn try_next_u64(&mut self) -> Result<u64, Infallible> {
        Ok((self.0.random() as u64) << 32 | self.0.random() as u64)
    }

    fn try_fill_bytes(&mut self, dst: &mut [u8]) -> Result<(), Infallible> {
        self.0.read(dst);
        Ok(())
    }
}

impl rand_core::TryCryptoRng for HardwareRng {}

/// Gives mbedtls the time from SNTP, for certificate dates. `None` until
/// the first sync, which makes every certificate fail rather than pass.
struct SntpWallClock;

impl MbedtlsWallClock for SntpWallClock {
    fn instant(&self) -> Option<tm> {
        let time = timezone::local_time(&TimeZone::UTC, sntp_task::unix_seconds()?)?;
        Some(tm {
            tm_sec: time.second().into(),
            tm_min: time.minute().into(),
            tm_hour: time.hour().into(),
            tm_mday: time.day().into(),
            tm_mon: (time.month() - 1).into(),
            tm_year: (time.year() - 1900).into(),
            tm_wday: time.weekday().to_sunday_zero_offset().into(),
            tm_yday: (time.day_of_year() - 1).into(),
            tm_isdst: 0,
        })
    }
}

#[embassy_executor::task]
pub async fn https_check_task(stack: Stack<'static>) {
    static RNG: StaticCell<HardwareRng> = StaticCell::new();
    static WALL_CLOCK: SntpWallClock = SntpWallClock;
    // SAFETY: hooked once, before any TLS session exists, and never unhooked.
    unsafe { hook_wall_clock(Some(&WALL_CLOCK)) };
    let tls = match Tls::new(RNG.init(HardwareRng(Rng::new()))) {
        Ok(tls) => tls,
        Err(error) => {
            warn!("https: TLS failed to start: {error:?}");
            return;
        }
    };

    stack.wait_config_up().await;
    while sntp_task::unix_seconds().is_none() {
        Timer::after_millis(500).await;
    }

    let mut passed = 0;
    for (host, path) in CHECKS {
        let started = Instant::now();
        match get(&tls, stack, host, path).await {
            Ok(status) => {
                passed += 1;
                info!(
                    "https: {} -> {status} in {} ms",
                    host.to_str().unwrap_or("?"),
                    started.elapsed().as_millis()
                );
            }
            Err(reason) => warn!("https: {} failed: {reason}", host.to_str().unwrap_or("?")),
        }
    }
    info!("https: {passed} of {} hosts passed", CHECKS.len());
    info!("{}", esp_alloc::HEAP.stats());
}

/// One GET; returns the response's status line.
async fn get(
    tls: &Tls<'_>,
    stack: Stack<'static>,
    host: &CStr,
    path: &str,
) -> Result<heapless::String<64>, &'static str> {
    let host_name = host.to_str().map_err(|_| "bad host name")?;
    let mut lap = Instant::now();
    // Milliseconds since the previous stage ended.
    let mut stage = || {
        let ms = lap.elapsed().as_millis();
        lap = Instant::now();
        ms
    };
    let addresses = stack
        .dns_query(host_name, DnsQueryType::A)
        .await
        .map_err(|_| "DNS lookup failed")?;
    let address = *addresses.first().ok_or("DNS found no address")?;
    let dns_ms = stage();

    let mut rx_buffer = [0u8; 4096];
    let mut tx_buffer = [0u8; 2048];
    let mut socket = TcpSocket::new(stack, &mut rx_buffer, &mut tx_buffer);
    socket.set_timeout(Some(Duration::from_secs(20)));
    socket
        .connect((address, 443))
        .await
        .map_err(|_| "TCP connect failed")?;
    let connect_ms = stage();

    let config = ClientSessionConfig {
        ca_chain: Some(Certificate::new(X509::PEM(ROOTS)).map_err(|_| "bad root certificates")?),
        server_name: Some(host),
        ..ClientSessionConfig::new()
    };
    let mut session = Session::new(tls.reference(), socket, &SessionConfig::Client(config))
        .map_err(|_| "TLS setup failed")?;
    session
        .connect()
        .await
        .map_err(|_| "TLS handshake failed (certificate or protocol)")?;
    let handshake_ms = stage();

    let request = [
        "GET ",
        path,
        " HTTP/1.1\r\nHost: ",
        host_name,
        "\r\nUser-Agent: On-Air\r\nConnection: close\r\n\r\n",
    ];
    for part in request {
        session
            .write_all(part.as_bytes())
            .await
            .map_err(|_| "write failed")?;
    }

    // Only the status line is needed.
    let mut buffer = [0u8; 256];
    let mut filled = 0;
    while filled < buffer.len() && !buffer[..filled].contains(&b'\n') {
        let read = session
            .read(&mut buffer[filled..])
            .await
            .map_err(|_| "read failed")?;
        if read == 0 {
            break;
        }
        filled += read;
    }
    let response_ms = stage();
    // The response is already in, so don't wait long for the server to
    // answer our close: some (Open-Meteo's geocoding API) never do, which
    // would otherwise hold the socket for its full 20 s timeout.
    let _ = with_timeout(Duration::from_millis(CLOSE_GRACE_MS), session.close()).await;
    let close_ms = stage();
    info!(
        "https: {host_name} stages: DNS {dns_ms} ms, connect {connect_ms} ms, \
         handshake {handshake_ms} ms, response {response_ms} ms, close {close_ms} ms"
    );
    let line = buffer[..filled]
        .split(|&b| b == b'\r' || b == b'\n')
        .next()
        .and_then(|line| core::str::from_utf8(line).ok())
        .ok_or("no status line")?;
    let mut status = heapless::String::new();
    let _ = status.push_str(&line[..line.len().min(64)]);
    Ok(status)
}
