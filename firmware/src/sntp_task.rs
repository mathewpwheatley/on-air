//! Keeps the wall clock right with SNTP: once the network is up, then
//! every few hours, retrying with backoff when a sync fails. The offset
//! arithmetic lives in `core::clock`.

use core::cell::Cell;
use core::net::{IpAddr, SocketAddr};

use embassy_net::dns::DnsQueryType;
use embassy_net::udp::{PacketMetadata, UdpSocket};
use embassy_net::{IpAddress, Stack};
use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_time::{Duration, Instant, Timer, with_timeout};
use log::{info, warn};
use on_air_core::backoff::Backoff;
use on_air_core::clock::{Clock, RESYNC_MS};
use on_air_core::timezone::{self, TimeZone};
use sntpc::{NtpContext, NtpTimestampGenerator};
use sntpc_net_embassy::UdpSocketWrapper;

const NTP_SERVER: &str = "pool.ntp.org";
const NTP_PORT: u16 = 123;
/// Give up on a reply after this long and retry later.
const REPLY_TIMEOUT_MS: u64 = 5_000;

static CLOCK: Mutex<CriticalSectionRawMutex, Cell<Clock>> = Mutex::new(Cell::new(Clock::new()));

fn since_boot_us() -> u64 {
    Instant::now().as_micros()
}

/// Unix time in whole seconds, or `None` until the first sync.
pub fn unix_seconds() -> Option<i64> {
    CLOCK.lock(|clock| clock.get().unix_seconds(since_boot_us()))
}

/// Stamps SNTP requests with the clock's current time.
#[derive(Debug, Clone, Copy, Default)]
struct Timestamps {
    unix_us: i64,
}

impl NtpTimestampGenerator for Timestamps {
    fn init(&mut self) {
        self.unix_us = CLOCK.lock(|clock| clock.get().unix_micros(since_boot_us()));
    }

    fn timestamp_sec(&self) -> u64 {
        self.unix_us.div_euclid(1_000_000) as u64
    }

    fn timestamp_subsec_micros(&self) -> u32 {
        self.unix_us.rem_euclid(1_000_000) as u32
    }
}

#[embassy_executor::task]
pub async fn sntp_task(stack: Stack<'static>, seed: u32) -> ! {
    let mut rx_meta = [PacketMetadata::EMPTY; 1];
    let mut rx_buffer = [0u8; 128];
    let mut tx_meta = [PacketMetadata::EMPTY; 1];
    let mut tx_buffer = [0u8; 128];
    let mut socket = UdpSocket::new(
        stack,
        &mut rx_meta,
        &mut rx_buffer,
        &mut tx_meta,
        &mut tx_buffer,
    );
    socket.bind(NTP_PORT).expect("SNTP port already in use");
    let socket = UdpSocketWrapper::new(socket);

    let mut backoff = Backoff::time_sync();
    let mut random = seed;
    loop {
        stack.wait_config_up().await;
        match sync(stack, &socket).await {
            Ok(()) => {
                backoff.reset();
                Timer::after_millis(RESYNC_MS).await;
            }
            Err(reason) => {
                let delay_ms = backoff.next_delay_from(&mut random);
                warn!(
                    "time: sync failed ({reason}), retrying in {} s",
                    delay_ms / 1000
                );
                Timer::after_millis(delay_ms as u64).await;
            }
        }
    }
}

async fn sync(stack: Stack<'static>, socket: &UdpSocketWrapper<'_>) -> Result<(), &'static str> {
    let addresses = stack
        .dns_query(NTP_SERVER, DnsQueryType::A)
        .await
        .map_err(|_| "DNS lookup failed")?;
    let IpAddress::Ipv4(address) = *addresses.first().ok_or("DNS found no address")?;
    let server = SocketAddr::new(IpAddr::V4(address), NTP_PORT);

    let context = NtpContext::new(Timestamps::default());
    let result = with_timeout(
        Duration::from_millis(REPLY_TIMEOUT_MS),
        sntpc::get_time(server, socket, context),
    )
    .await
    .map_err(|_| "no reply")?
    .map_err(|_| "bad reply")?;

    CLOCK.lock(|cell| {
        let mut clock = cell.get();
        clock.adjust(result.offset());
        cell.set(clock);
    });
    if let Some(time) = unix_seconds().and_then(|s| timezone::local_time(&TimeZone::UTC, s)) {
        info!(
            "time: {time} UTC (from {server}, corrected by {} ms)",
            result.offset() / 1000
        );
    }
    Ok(())
}
