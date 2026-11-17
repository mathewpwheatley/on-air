//! Joins the home network, driven by `core::wifi::WifiLink`. Until the
//! setup portal saves a network, the name and password come from `.env` at
//! build time (`ON_AIR_WIFI_SSID` and `ON_AIR_WIFI_PASSWORD`).

use core::sync::atomic::{AtomicBool, Ordering};

use embassy_futures::select::{Either, select};
use embassy_net::Runner;
use embassy_time::{Duration, Instant, Timer, with_timeout};
use esp_radio::wifi::ap::AccessPointConfig;
use esp_radio::wifi::sta::StationConfig;
use esp_radio::wifi::{Config, Interface, WifiController};
use log::{info, warn};
use on_air_core::wifi::{JOIN_ATTEMPT_MS, WifiEvent, WifiLink, WifiNext};

const SSID: Option<&str> = option_env!("ON_AIR_WIFI_SSID");
const PASSWORD: &str = match option_env!("ON_AIR_WIFI_PASSWORD") {
    Some(password) => password,
    None => "",
};

static ONLINE: AtomicBool = AtomicBool::new(false);

/// Whether the board is joined to the home network.
pub fn is_online() -> bool {
    ONLINE.load(Ordering::Relaxed)
}

/// For the P5 AP+STA test the setup network stays up alongside the home
/// network. P7 brings it up only while setting up.
fn access_point(network_name: &str) -> AccessPointConfig {
    AccessPointConfig::default()
        .with_ssid(network_name)
        .with_max_connections(4)
}

fn now() -> u64 {
    Instant::now().as_millis()
}

#[embassy_executor::task]
pub async fn wifi_task(
    mut controller: WifiController<'static>,
    seed: u32,
    network_name: &'static str,
) -> ! {
    let ssid = SSID.filter(|s| !s.is_empty());
    let mut link = WifiLink::new(ssid.is_some(), now(), seed);
    let config = match ssid {
        Some(ssid) => Config::AccessPointStation(
            StationConfig::default()
                .with_ssid(ssid)
                .with_password(PASSWORD.into()),
            access_point(network_name),
        ),
        None => {
            warn!("wifi: no network saved; set ON_AIR_WIFI_SSID in .env and rebuild");
            Config::AccessPoint(access_point(network_name))
        }
    };
    match controller.set_config(&config) {
        Ok(()) => info!("wifi: setup network {network_name} is up"),
        Err(error) => warn!("wifi: couldn't start the radio: {error:?}"),
    }

    loop {
        match link.next(now()) {
            WifiNext::Join => {
                info!("wifi: joining {}", ssid.unwrap_or_default());
                let attempt = with_timeout(
                    Duration::from_millis(JOIN_ATTEMPT_MS),
                    controller.connect_async(),
                )
                .await;
                let event = match attempt {
                    Ok(Ok(_)) => WifiEvent::Joined,
                    Ok(Err(error)) => {
                        warn!("wifi: join failed: {error:?}");
                        WifiEvent::Failed
                    }
                    Err(_) => {
                        warn!("wifi: join timed out");
                        WifiEvent::Failed
                    }
                };
                link.handle(now(), event);
                ONLINE.store(event == WifiEvent::Joined, Ordering::Relaxed);
                info!("wifi: {:?}", link.status());
            }
            WifiNext::StartPortal => {
                // The setup portal comes in P7.
                warn!("wifi: would open the setup hotspot now; not built yet");
            }
            WifiNext::Wait(ms) => {
                if controller.is_connected() {
                    let dropped = select(
                        Timer::after_millis(ms),
                        controller.wait_for_disconnect_async(),
                    );
                    if let Either::Second(_) = dropped.await {
                        on_drop(&mut link);
                    }
                } else {
                    Timer::after_millis(ms).await;
                }
            }
            WifiNext::Idle => {
                if controller.is_connected() {
                    let _ = controller.wait_for_disconnect_async().await;
                    on_drop(&mut link);
                } else {
                    // Only the portal idles while offline, and that waits
                    // for P7.
                    Timer::after_secs(60).await;
                }
            }
        }
    }
}

fn on_drop(link: &mut WifiLink) {
    warn!("wifi: connection dropped");
    ONLINE.store(false, Ordering::Relaxed);
    link.handle(now(), WifiEvent::Failed);
}

/// Runs a network stack: DHCP, ARP and the sockets. One for the home
/// network and one for the setup network.
#[embassy_executor::task(pool_size = 2)]
pub async fn net_task(mut runner: Runner<'static, Interface<'static>>) -> ! {
    runner.run().await
}
