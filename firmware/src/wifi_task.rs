//! Joins the home network, driven by `core::wifi::WifiLink`. Until the
//! setup portal saves a network, the name and password come from `.env` at
//! build time (`ON_AIR_WIFI_SSID` and `ON_AIR_WIFI_PASSWORD`).

use embassy_futures::select::{Either, select};
use embassy_net::Runner;
use embassy_time::{Duration, Instant, Timer, with_timeout};
use esp_radio::wifi::sta::StationConfig;
use esp_radio::wifi::{Config, Interface, WifiController};
use log::{info, warn};
use on_air_core::wifi::{JOIN_ATTEMPT_MS, WifiEvent, WifiLink, WifiNext};

const SSID: Option<&str> = option_env!("ON_AIR_WIFI_SSID");
const PASSWORD: &str = match option_env!("ON_AIR_WIFI_PASSWORD") {
    Some(password) => password,
    None => "",
};

fn now() -> u64 {
    Instant::now().as_millis()
}

#[embassy_executor::task]
pub async fn wifi_task(mut controller: WifiController<'static>, seed: u32) -> ! {
    let ssid = SSID.filter(|s| !s.is_empty());
    let mut link = WifiLink::new(ssid.is_some(), now(), seed);
    match ssid {
        Some(ssid) => {
            let config = StationConfig::default()
                .with_ssid(ssid)
                .with_password(PASSWORD.into());
            if let Err(error) = controller.set_config(&Config::Station(config)) {
                warn!("wifi: couldn't set up the station: {error:?}");
            }
        }
        None => warn!("wifi: no network saved; set ON_AIR_WIFI_SSID in .env and rebuild"),
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
    link.handle(now(), WifiEvent::Failed);
}

/// Runs the network stack: DHCP, ARP and the sockets.
#[embassy_executor::task]
pub async fn net_task(mut runner: Runner<'static, Interface<'static>>) -> ! {
    runner.run().await
}
