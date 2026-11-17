//! The radio: joins the home network, driven by `core::wifi::WifiLink`, and
//! runs the open setup network only while the setup portal is open (no
//! network saved, the saved one can't be joined at boot, or the owner
//! asked for it). While the portal is open it scans for networks and joins
//! the one the owner picks, keeping the setup network up meanwhile.

use core::cell::Cell;

use alloc::string::String;
use alloc::vec::Vec;
use embassy_futures::select::{Either3, select3};
use embassy_net::Runner;
use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Instant, Timer, with_timeout};
use esp_radio::wifi::ap::AccessPointConfig;
use esp_radio::wifi::scan::ScanConfig;
use esp_radio::wifi::sta::StationConfig;
use esp_radio::wifi::{AuthenticationMethod, Config, Interface, WifiController};
use log::{info, warn};
use on_air_core::app::NetStatus;
use on_air_core::portal::{self, Network, WifiState};
use on_air_core::wifi::{JOIN_ATTEMPT_MS, WifiEvent, WifiLink, WifiNext};

/// Requests for the radio, from the portal and the buttons.
pub enum WifiCommand {
    /// The owner held the buttons to reopen setup.
    OpenPortal,
    /// Refresh the network list for the portal.
    Scan,
    /// Join this network, from the portal.
    Join { ssid: String, password: String },
}

pub static COMMANDS: Channel<CriticalSectionRawMutex, WifiCommand, 4> = Channel::new();
/// Progress joining a network picked in the portal.
pub static JOIN_STATE: Signal<CriticalSectionRawMutex, WifiState> = Signal::new();
/// The latest scan, for the portal's network list.
pub static NETWORKS: Signal<CriticalSectionRawMutex, Vec<Network>> = Signal::new();

static STATUS: Mutex<CriticalSectionRawMutex, Cell<NetStatus>> =
    Mutex::new(Cell::new(NetStatus::Connecting));

/// The home network's state, as `WifiLink` sees it. `Portal` while the
/// setup network is up.
pub fn status() -> NetStatus {
    STATUS.lock(Cell::get)
}

fn publish(link: &WifiLink) {
    STATUS.lock(|status| status.set(link.status()));
}

fn now() -> u64 {
    Instant::now().as_millis()
}

/// The radio's configuration: the home network, plus the setup network on
/// `portal`'s channel while the portal is open. The radio has to move the
/// setup network to the home network's channel when it joins, which drops
/// every phone on it, so the setup network starts on that channel instead.
fn radio_config(ssid: &str, password: &str, portal: Option<u8>, network_name: &str) -> Config {
    let station = StationConfig::default()
        .with_ssid(ssid)
        .with_password(password.into());
    match portal {
        None => Config::Station(station),
        Some(channel) => Config::AccessPointStation(
            station,
            AccessPointConfig::default()
                .with_ssid(network_name)
                .with_channel(channel)
                .with_max_connections(4),
        ),
    }
}

/// A network from a scan, with its channel and strength.
struct Found {
    network: Network,
    channel: u8,
    dbm: i8,
}

/// The channel for the setup network: the home network's, if it's in range,
/// else the strongest network's, which is most likely the home one.
fn setup_channel(found: &[Found], ssid: &str) -> u8 {
    found
        .iter()
        .find(|f| !ssid.is_empty() && f.network.ssid == ssid)
        .or_else(|| found.iter().max_by_key(|f| f.dbm))
        .map_or(1, |f| f.channel)
}

#[embassy_executor::task]
pub async fn wifi_task(
    mut controller: WifiController<'static>,
    seed: u32,
    network_name: &'static str,
    mut ssid: String,
    mut password: String,
) -> ! {
    let mut link = WifiLink::new(!ssid.is_empty(), now(), seed);
    // The setup network's channel, while the portal is open.
    let mut portal: Option<u8> = None;
    let mut found: Vec<Found> = Vec::new();
    publish(&link);
    if ssid.is_empty() {
        warn!("wifi: no network saved; opening the setup network");
    }
    if let Err(error) = controller.set_config(&radio_config(&ssid, &password, None, network_name)) {
        warn!("wifi: couldn't start the radio: {error:?}");
    }

    loop {
        let wait_ms = match link.next(now()) {
            WifiNext::Join => {
                info!("wifi: joining {ssid}");
                let event = join(&mut controller).await;
                link.handle(now(), event);
                publish(&link);
                info!("wifi: {:?}", link.status());
                continue;
            }
            WifiNext::StartPortal => {
                publish(&link);
                // Scan first, to start the setup network on the home
                // network's channel.
                found = scan(&mut controller).await;
                let channel = setup_channel(&found, &ssid);
                portal = Some(channel);
                match controller.set_config(&radio_config(&ssid, &password, portal, network_name)) {
                    Ok(()) => {
                        info!("wifi: setup network {network_name} is up on channel {channel}")
                    }
                    Err(error) => warn!("wifi: couldn't start the setup network: {error:?}"),
                }
                continue;
            }
            WifiNext::Wait(ms) => Some(ms),
            WifiNext::Idle => None,
        };

        let connected = controller.is_connected();
        let timer = async {
            match wait_ms {
                Some(ms) => Timer::after_millis(ms).await,
                None => core::future::pending().await,
            }
        };
        let dropped = async {
            if connected {
                let _ = controller.wait_for_disconnect_async().await;
            } else {
                core::future::pending::<()>().await;
            }
        };
        match select3(timer, COMMANDS.receive(), dropped).await {
            Either3::First(()) => {}
            Either3::Third(()) if portal.is_none() => {
                warn!("wifi: connection dropped");
                link.handle(now(), WifiEvent::Failed);
                publish(&link);
            }
            Either3::Third(()) => warn!("wifi: home network dropped while the portal is open"),
            Either3::Second(WifiCommand::OpenPortal) => {
                info!("wifi: opening the setup portal");
                link.open_portal();
            }
            Either3::Second(WifiCommand::Scan) => {
                if portal.is_some() {
                    found = scan(&mut controller).await;
                }
            }
            Either3::Second(WifiCommand::Join {
                ssid: new_ssid,
                password: new_password,
            }) => {
                info!("wifi: portal asked to join {new_ssid}");
                // Pressing Connect again for the network it's already on.
                if controller.is_connected() && new_ssid == ssid && new_password == password {
                    JOIN_STATE.signal(WifiState::Connected);
                    continue;
                }
                if controller.is_connected() {
                    let _ = controller.disconnect_async().await;
                }
                ssid = new_ssid;
                password = new_password;
                let current = portal.unwrap_or(1);
                let channel = found
                    .iter()
                    .find(|f| f.network.ssid == ssid)
                    .map_or(current, |f| f.channel);
                let configured = if channel == current {
                    // Same channel: change only the home network, so phones
                    // on the setup network stay connected.
                    set_station(&ssid, &password, channel)
                } else {
                    // The setup network has to move, which drops phones on
                    // it; they rejoin.
                    warn!("wifi: {ssid} is on channel {channel}, so the setup network moves there");
                    portal = Some(channel);
                    let config = radio_config(&ssid, &password, portal, network_name);
                    controller
                        .set_config(&config)
                        .inspect_err(|error| warn!("wifi: couldn't set up {ssid}: {error:?}"))
                        .is_ok()
                };
                let state = match configured {
                    true => match join(&mut controller).await {
                        WifiEvent::Joined => WifiState::Connected,
                        WifiEvent::Failed => WifiState::Failed,
                    },
                    false => WifiState::Failed,
                };
                info!("wifi: {ssid}: {state:?}");
                JOIN_STATE.signal(state);
            }
        }
    }
}

/// Point the station at another network, leaving the setup network alone.
/// esp-radio's `set_config` always reapplies the access point's settings
/// too, which restarts the setup network and drops every phone on it, so
/// this edits the station's settings in the driver directly. The channel is
/// set so joining doesn't scan every channel, which would also take the
/// radio away from the setup network. False if the driver refuses.
fn set_station(ssid: &str, password: &str, channel: u8) -> bool {
    use esp_wifi_sys_esp32s3::include::{
        esp_wifi_get_config, esp_wifi_set_config, wifi_auth_mode_t_WIFI_AUTH_OPEN,
        wifi_auth_mode_t_WIFI_AUTH_WPA2_PSK, wifi_config_t, wifi_interface_t_WIFI_IF_STA,
    };
    if ssid.len() > 32 || password.len() > 64 {
        return false;
    }
    // SAFETY: the WiFi driver is running (esp-radio started it), the config
    // is read from it before being changed, and only the station's plain
    // byte and integer fields are written.
    unsafe {
        let mut config: wifi_config_t = core::mem::zeroed();
        if esp_wifi_get_config(wifi_interface_t_WIFI_IF_STA, &mut config) != 0 {
            return false;
        }
        let station = &mut config.sta;
        station.ssid = [0; 32];
        station.ssid[..ssid.len()].copy_from_slice(ssid.as_bytes());
        station.password = [0; 64];
        station.password[..password.len()].copy_from_slice(password.as_bytes());
        station.channel = channel;
        station.threshold.authmode = if password.is_empty() {
            wifi_auth_mode_t_WIFI_AUTH_OPEN
        } else {
            wifi_auth_mode_t_WIFI_AUTH_WPA2_PSK
        };
        esp_wifi_set_config(wifi_interface_t_WIFI_IF_STA, &mut config) == 0
    }
}

/// One attempt to join the configured home network.
async fn join(controller: &mut WifiController<'static>) -> WifiEvent {
    let attempt = with_timeout(
        Duration::from_millis(JOIN_ATTEMPT_MS),
        controller.connect_async(),
    )
    .await;
    match attempt {
        Ok(Ok(_)) => WifiEvent::Joined,
        Ok(Err(error)) => {
            warn!("wifi: join failed: {error:?}");
            WifiEvent::Failed
        }
        Err(_) => {
            warn!("wifi: join timed out");
            WifiEvent::Failed
        }
    }
}

/// Scan for networks and pass them to the portal. A scan can fail while
/// the radio is still switching modes, so a failure is retried once.
async fn scan(controller: &mut WifiController<'static>) -> Vec<Found> {
    let mut result = controller.scan_async(&ScanConfig::default()).await;
    if let Err(error) = &result {
        warn!("wifi: scan failed ({error:?}); retrying");
        Timer::after_millis(1_000).await;
        result = controller.scan_async(&ScanConfig::default()).await;
    }
    match result {
        Ok(points) => {
            let found: Vec<Found> = points
                .iter()
                .map(|ap| Found {
                    network: Network {
                        ssid: ap.ssid.as_str().into(),
                        signal: portal::signal_from_dbm(ap.signal_strength),
                        secure: !matches!(ap.auth_method, None | Some(AuthenticationMethod::None)),
                    },
                    channel: ap.channel,
                    dbm: ap.signal_strength,
                })
                .collect();
            info!("wifi: scan found {} networks", found.len());
            NETWORKS.signal(found.iter().map(|f| f.network.clone()).collect());
            found
        }
        Err(error) => {
            warn!("wifi: scan failed: {error:?}");
            Vec::new()
        }
    }
}

/// Runs a network stack: DHCP, ARP and the sockets. One for the home
/// network and one for the setup network.
#[embassy_executor::task(pool_size = 2)]
pub async fn net_task(mut runner: Runner<'static, Interface<'static>>) -> ! {
    runner.run().await
}
