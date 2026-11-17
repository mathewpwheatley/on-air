//! The setup network's services: DHCP and DNS that point phones at the
//! board, so they open the setup portal by themselves, and the web server
//! that passes each request to `core::portal::Portal`. The radio only runs
//! the setup network while the portal is open (see `wifi_task`).

use core::net::{Ipv4Addr, SocketAddr, SocketAddrV4};

use alloc::string::String;
use alloc::vec::Vec;
use embassy_net::tcp::TcpSocket;
use embassy_net::{Ipv4Cidr, Stack, StaticConfigV4};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_time::{Duration, Timer, with_timeout};
use embedded_io_async::Write;
use esp_hal::rng::Rng;
use log::{info, warn};
use on_air_core::http::{self, DecodedRequest, IncomingRequest, Method};
use on_air_core::portal::{Event, Handled, Portal, Reply};
use on_air_core::settings::Settings;
use static_cell::StaticCell;

use crate::https::HttpsClient;
use crate::storage::SettingsStore;
use crate::wifi_task::{self, WifiCommand};

/// The board's address on its setup network.
pub const ADDRESS: Ipv4Addr = Ipv4Addr::new(192, 168, 4, 1);
/// Where captive-portal probes and the DHCP captive-portal option point.
const PORTAL_URL: &str = "http://192.168.4.1/";
/// The largest request read: the forms are small.
const MAX_REQUEST_BYTES: usize = 8 * 1024;
/// How long the page's network request waits for a scan to finish.
const SCAN_WAIT: Duration = Duration::from_secs(8);

/// Network settings for the setup network's stack.
pub fn stack_config() -> embassy_net::Config {
    embassy_net::Config::ipv4_static(StaticConfigV4 {
        address: Ipv4Cidr::new(ADDRESS, 24),
        gateway: Some(ADDRESS),
        dns_servers: Default::default(),
    })
}

/// Hands out addresses to phones that join the setup network, naming the
/// board as their DNS server and the portal as the captive portal.
#[embassy_executor::task]
pub async fn dhcp_task(stack: Stack<'static>) -> ! {
    use edge_dhcp::io::{self, DEFAULT_SERVER_PORT};
    use edge_dhcp::server::{Server, ServerOptions};
    use edge_nal::UdpBind;
    use edge_nal_embassy::{Udp, UdpBuffers};

    let buffers = UdpBuffers::<1, 1024, 1024, 4>::new();
    let udp = Udp::new(stack, &buffers);
    let mut socket = udp
        .bind(SocketAddr::V4(SocketAddrV4::new(
            Ipv4Addr::UNSPECIFIED,
            DEFAULT_SERVER_PORT,
        )))
        .await
        .expect("DHCP port already in use");
    let mut packet = [0u8; 1500];
    let mut gateway = [ADDRESS];
    let dns = [ADDRESS];
    // Up to 8 phones at once is plenty for setup.
    let mut server = Server::<_, 8>::new_with_et(ADDRESS);
    loop {
        let mut options = ServerOptions::new(ADDRESS, Some(&mut gateway));
        options.dns = &dns;
        options.captive_url = Some(PORTAL_URL);
        if let Err(error) = io::server::run(&mut server, &options, &mut socket, &mut packet).await {
            warn!("setup network: DHCP error: {error:?}");
        }
        Timer::after_millis(500).await;
    }
}

/// Answers every DNS lookup with the board's address, so a phone's
/// "is there internet?" check lands on the portal.
#[embassy_executor::task]
pub async fn dns_task(stack: Stack<'static>) -> ! {
    use edge_nal_embassy::{Udp, UdpBuffers};

    let buffers = UdpBuffers::<1, 512, 512, 4>::new();
    let udp = Udp::new(stack, &buffers);
    let mut tx = [0u8; 512];
    let mut rx = [0u8; 512];
    loop {
        let result = edge_captive::io::run(
            &udp,
            SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 53)),
            &mut tx,
            &mut rx,
            ADDRESS,
            core::time::Duration::from_secs(60),
        )
        .await;
        if let Err(error) = result {
            warn!("setup network: DNS error: {error:?}");
        }
        Timer::after_millis(500).await;
    }
}

/// What the web-server tasks share. One for the setup network, and one for
/// the home network's Spotify sign-in.
pub struct PortalShared {
    portal: Mutex<CriticalSectionRawMutex, Portal>,
    /// On the home network: only the Spotify sign-in is open.
    home: bool,
    store: Option<&'static SettingsStore>,
    client: &'static HttpsClient,
    /// Whether the portal has a network list yet.
    have_networks: Mutex<CriticalSectionRawMutex, bool>,
}

impl PortalShared {
    /// The setup network's portal.
    pub fn setup(
        settings: Settings,
        network_name: &str,
        store: Option<&'static SettingsStore>,
        client: &'static HttpsClient,
    ) -> &'static PortalShared {
        static SHARED: StaticCell<PortalShared> = StaticCell::new();
        SHARED.init(Self::new(settings, network_name, false, store, client))
    }

    /// The home network's Spotify sign-in.
    pub fn home(
        settings: Settings,
        network_name: &str,
        store: Option<&'static SettingsStore>,
        client: &'static HttpsClient,
    ) -> &'static PortalShared {
        static SHARED: StaticCell<PortalShared> = StaticCell::new();
        SHARED.init(Self::new(settings, network_name, true, store, client))
    }

    fn new(
        settings: Settings,
        network_name: &str,
        home: bool,
        store: Option<&'static SettingsStore>,
        client: &'static HttpsClient,
    ) -> PortalShared {
        let mut portal = Portal::new(settings, network_name);
        if home {
            // It's reached over the home WiFi, so that's connected.
            portal.set_wifi(on_air_core::portal::WifiState::Connected);
        }
        PortalShared {
            portal: Mutex::new(portal),
            home,
            store,
            client,
            have_networks: Mutex::new(false),
        }
    }
}

/// One web-server task. Phones open several connections at once, so a few
/// of these run on the setup network, and one on the home network while
/// Spotify sign-in is needed.
#[embassy_executor::task(pool_size = 4)]
pub async fn web_task(stack: Stack<'static>, shared: &'static PortalShared) -> ! {
    let mut rx_buffer = [0u8; 2048];
    // The page is about 20 KB; a bigger send buffer means fewer round trips.
    let mut tx_buffer = [0u8; 4096];
    loop {
        let mut socket = TcpSocket::new(stack, &mut rx_buffer, &mut tx_buffer);
        socket.set_timeout(Some(Duration::from_secs(10)));
        if socket.accept(80).await.is_err() {
            continue;
        }
        let Some(request) = read_request(&mut socket).await else {
            socket.abort();
            continue;
        };
        let (reply, restart) = handle(shared, request).await;
        let _ = socket.write_all(&reply).await;
        let _ = socket.flush().await;
        socket.close();
        // Let the close go out before the socket is reused.
        Timer::after_millis(50).await;
        if restart {
            info!("portal: setup finished; restarting");
            Timer::after_millis(1_000).await;
            esp_hal::system::software_reset();
        }
    }
}

async fn read_request(socket: &mut TcpSocket<'_>) -> Option<IncomingRequest> {
    let mut received = Vec::new();
    let mut chunk = [0u8; 512];
    loop {
        let read = with_timeout(Duration::from_secs(5), socket.read(&mut chunk))
            .await
            .ok()?
            .ok()?;
        if read == 0 {
            return None;
        }
        received.extend_from_slice(&chunk[..read]);
        match http::decode_request(&received) {
            DecodedRequest::Complete(request) => return Some(request),
            DecodedRequest::Incomplete if received.len() < MAX_REQUEST_BYTES => {}
            _ => return None,
        }
    }
}

/// Answer one request with the portal, acting on what it asks for. Also
/// says whether to restart once the reply is sent: setup is finished.
async fn handle(shared: &PortalShared, request: IncomingRequest) -> (Vec<u8>, bool) {
    let Some(method) = request.method.filter(|m| *m != Method::Put) else {
        return (encode(&Reply::NotFound), false);
    };
    let body = String::from_utf8_lossy(&request.body);
    if request.target.starts_with("/api/networks") {
        refresh_networks(shared).await;
    }

    let mut events = Vec::new();
    let handled = {
        let mut portal = shared.portal.lock().await;
        if let Some(state) = wifi_task::JOIN_STATE.try_take() {
            portal.set_wifi(state);
        }
        if let Some(networks) = wifi_task::NETWORKS.try_take() {
            if !networks.is_empty()
                && let Ok(mut have) = shared.have_networks.try_lock()
            {
                *have = true;
            }
            portal.set_networks(networks);
        }
        let rng = Rng::new();
        let mut random = |buffer: &mut [u8]| rng.read(buffer);
        let mut emit = |event| events.push(event);
        if shared.home {
            portal.handle_home(method, &request.target, &body, &mut random, &mut emit)
        } else {
            portal.handle(method, &request.target, &body, &mut random, &mut emit)
        }
    };
    let reply = match handled {
        Handled::Reply(reply) => reply,
        // City search and the Spotify token exchange, over the home network.
        Handled::Fetch(fetch) => {
            let outcome = shared.client.send(&fetch).await;
            shared
                .portal
                .lock()
                .await
                .finish(outcome, &mut |event| events.push(event))
        }
    };
    let mut restart = false;
    for event in events {
        restart |= act(shared, event).await;
    }
    (encode(&reply), restart)
}

fn encode(reply: &Reply) -> Vec<u8> {
    let headers: &[(&str, &str)] = if *reply == Reply::Redirect {
        &[("Location", PORTAL_URL)]
    } else {
        &[]
    };
    http::encode_response(
        reply.status(),
        reply.content_type(),
        headers,
        reply.body().as_bytes(),
    )
}

/// Make sure the portal has a network list before the page gets it: the
/// page asks once, so an empty reply would leave its list empty. The radio
/// scans before it brings the setup network up, so that list is normally
/// here already. It rescans only when there's none, because a scan takes
/// the radio off the setup network's channel for seconds, stalling the
/// phone. Holding `have_networks` makes a second request wait for the same
/// scan.
async fn refresh_networks(shared: &PortalShared) {
    let mut have = shared.have_networks.lock().await;
    let networks = match wifi_task::NETWORKS.try_take() {
        Some(networks) => Some(networks),
        None if *have => return,
        None => {
            wifi_task::COMMANDS.send(WifiCommand::Scan).await;
            match with_timeout(SCAN_WAIT, wifi_task::NETWORKS.wait()).await {
                Ok(networks) => Some(networks),
                Err(_) => {
                    warn!("portal: no network list after {} s", SCAN_WAIT.as_secs());
                    None
                }
            }
        }
    };
    if let Some(networks) = networks {
        *have = !networks.is_empty();
        shared.portal.lock().await.set_networks(networks);
    }
}

/// Act on a portal event. True when the board should restart.
async fn act(shared: &PortalShared, event: Event) -> bool {
    match event {
        Event::JoinWifi { ssid, password } => {
            wifi_task::COMMANDS
                .send(WifiCommand::Join { ssid, password })
                .await;
            false
        }
        // From the home network: keep just the sign-in, and restart so the
        // Spotify task picks it up.
        Event::SignedIn if shared.home => {
            let settings = shared.portal.lock().await.settings().clone();
            if let Some(store) = shared.store {
                store
                    .update(|saved| {
                        saved.spotify_client_id = settings.spotify_client_id;
                        saved.spotify_refresh_token = settings.spotify_refresh_token;
                    })
                    .await;
            }
            info!("portal: signed in to Spotify; restarting");
            true
        }
        Event::SignedIn | Event::SettingsChanged => {
            save(shared).await;
            false
        }
        Event::Done => {
            save(shared).await;
            true
        }
    }
}

/// Save everything the portal has, which marks the settings as set up.
async fn save(shared: &PortalShared) {
    let settings = shared.portal.lock().await.settings().clone();
    match shared.store {
        Some(store) => store.update(|saved| *saved = settings).await,
        None => warn!("portal: no settings partition, so nothing is saved"),
    }
}
