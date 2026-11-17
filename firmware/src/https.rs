//! The HTTPS client every network task shares: sends `core::http`
//! requests over mbedtls, verifying certificates against the pinned roots
//! in `certs/roots.pem` and checking their dates against the SNTP clock.

use core::cell::RefCell;
use core::convert::Infallible;
use core::ffi::CStr;

use alloc::ffi::CString;
use alloc::string::String;
use alloc::vec::Vec;
use embassy_net::Stack;
use embassy_net::dns::DnsQueryType;
use embassy_net::tcp::TcpSocket;
use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_time::{Duration, Instant, with_timeout};
use embedded_io_async::Write;
use esp_hal::rng::Rng;
use log::{info, warn};
use mbedtls_rs::sys::hook::wall_clock::{MbedtlsWallClock, hook_wall_clock};
use mbedtls_rs::sys::{MBEDTLS_ERR_X509_CERT_VERIFY_FAILED, tm};
use mbedtls_rs::{
    Certificate, ClientSessionConfig, Session, SessionConfig, SessionError, Tls, X509,
};
use on_air_core::http::{self, Decoded, NetworkError, Outcome, Request};
use on_air_core::timezone::{self, TimeZone};
use static_cell::StaticCell;

use crate::sntp_task;

/// The root certificates, as one NUL-terminated PEM bundle for mbedtls.
const ROOTS: &CStr =
    match CStr::from_bytes_with_nul(concat!(include_str!("../certs/roots.pem"), "\0").as_bytes()) {
        Ok(roots) => roots,
        Err(_) => panic!("certs/roots.pem must be text with no NUL bytes"),
    };

/// Give up on a request after this long, from DNS to the last byte.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// How long to wait for a server to answer our TLS close before dropping
/// the connection. Some (Open-Meteo's geocoding API) never answer.
const CLOSE_GRACE: Duration = Duration::from_secs(1);
/// The largest response kept. Covers are about 20 KB and Spotify's JSON a
/// few KB.
const MAX_RESPONSE_BYTES: usize = 256 * 1024;

/// Hosts whose certificates were rejected on their latest handshake.
static REJECTED_HOSTS: Mutex<CriticalSectionRawMutex, RefCell<Vec<String>>> =
    Mutex::new(RefCell::new(Vec::new()));

/// Whether any host's certificate is being rejected: expired, from a root
/// not in `certs/roots.pem`, or for the wrong host name. A firmware update
/// fixes it, not the WiFi.
pub fn certificate_problem() -> bool {
    REJECTED_HOSTS.lock(|hosts| !hosts.borrow().is_empty())
}

fn note_certificate(host: &str, rejected: bool) {
    REJECTED_HOSTS.lock(|hosts| {
        let mut hosts = hosts.borrow_mut();
        let known = hosts.iter().position(|h| h == host);
        match (rejected, known) {
            (true, None) => {
                warn!(
                    "https: {host}'s certificate is rejected; the pinned roots may need updating"
                );
                hosts.push(host.into());
            }
            (false, Some(index)) => {
                info!("https: {host}'s certificate is accepted again");
                hosts.remove(index);
            }
            _ => {}
        }
    });
}

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

pub struct HttpsClient {
    tls: Tls<'static>,
    stack: Stack<'static>,
}

impl HttpsClient {
    /// Set up TLS. Call once; the client is shared by every task.
    pub fn start(stack: Stack<'static>) -> &'static HttpsClient {
        static RNG: StaticCell<HardwareRng> = StaticCell::new();
        static WALL_CLOCK: SntpWallClock = SntpWallClock;
        static CLIENT: StaticCell<HttpsClient> = StaticCell::new();
        // SAFETY: hooked once, before any TLS session exists, and never
        // unhooked.
        unsafe { hook_wall_clock(Some(&WALL_CLOCK)) };
        let tls = Tls::new(RNG.init(HardwareRng(Rng::new()))).expect("TLS failed to start");
        CLIENT.init(HttpsClient { tls, stack })
    }

    /// Send one request. Failures are logged with the reason and come back
    /// as [`NetworkError`], which the core loops retry with backoff.
    pub async fn send(&self, request: &Request) -> Outcome {
        let Some(target) = http::target(&request.url) else {
            warn!("https: not an https URL: {}", request.url);
            return Err(NetworkError);
        };
        let started = Instant::now();
        let result = with_timeout(REQUEST_TIMEOUT, self.exchange(request, &target)).await;
        match result {
            Ok(Ok(response)) => {
                info!(
                    "https: {} {} -> {} ({} bytes, {} ms)",
                    target.host,
                    target.path.split('?').next().unwrap_or(""),
                    response.status,
                    response.body.len(),
                    started.elapsed().as_millis()
                );
                Ok(response)
            }
            Ok(Err(reason)) => {
                warn!("https: {} failed: {reason}", target.host);
                Err(NetworkError)
            }
            Err(_) => {
                warn!("https: {} timed out", target.host);
                Err(NetworkError)
            }
        }
    }

    async fn exchange(
        &self,
        request: &Request,
        target: &http::Target<'_>,
    ) -> Result<http::Response, &'static str> {
        let addresses = self
            .stack
            .dns_query(target.host, DnsQueryType::A)
            .await
            .map_err(|_| "DNS lookup failed")?;
        let address = *addresses.first().ok_or("DNS found no address")?;

        let mut rx_buffer = [0u8; 4096];
        let mut tx_buffer = [0u8; 2048];
        let mut socket = TcpSocket::new(self.stack, &mut rx_buffer, &mut tx_buffer);
        socket.set_timeout(Some(Duration::from_secs(20)));
        socket
            .connect((address, 443))
            .await
            .map_err(|_| "TCP connect failed")?;

        let host = CString::new(target.host).map_err(|_| "bad host name")?;
        let config = ClientSessionConfig {
            ca_chain: Some(
                Certificate::new(X509::PEM(ROOTS)).map_err(|_| "bad root certificates")?,
            ),
            server_name: Some(&host),
            ..ClientSessionConfig::new()
        };
        let mut session =
            Session::new(self.tls.reference(), socket, &SessionConfig::Client(config))
                .map_err(|_| "TLS setup failed")?;
        match session.connect().await {
            Ok(()) => note_certificate(target.host, false),
            Err(SessionError::MbedTls(error))
                if error.code() == MBEDTLS_ERR_X509_CERT_VERIFY_FAILED =>
            {
                note_certificate(target.host, true);
                return Err("certificate rejected (expired, untrusted root or wrong host)");
            }
            Err(error) => {
                warn!("https: {} handshake: {error:?}", target.host);
                return Err("TLS handshake failed");
            }
        }

        session
            .write_all(&http::encode(request, target))
            .await
            .map_err(|_| "write failed")?;

        let mut received = Vec::new();
        let mut chunk = [0u8; 2048];
        let response = loop {
            let read = session.read(&mut chunk).await.map_err(|_| "read failed")?;
            received.extend_from_slice(&chunk[..read]);
            if received.len() > MAX_RESPONSE_BYTES {
                return Err("response too large");
            }
            match http::decode(&received, read == 0) {
                Decoded::Complete(response) => break response,
                Decoded::Incomplete => {}
                Decoded::Invalid => return Err("invalid HTTP response"),
            }
        };
        let _ = with_timeout(CLOSE_GRACE, session.close()).await;
        Ok(response)
    }
}
