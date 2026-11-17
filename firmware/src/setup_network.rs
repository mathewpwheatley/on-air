//! The board's own setup network: an open access point with a DHCP server
//! and, for the P5 AP+STA test, a one-page web server at
//! `http://192.168.4.1` showing that the home WiFi stays up alongside it.
//! The setup portal (P7) replaces the page.

use core::net::{Ipv4Addr, SocketAddr, SocketAddrV4};

use alloc::format;
use embassy_net::tcp::TcpSocket;
use embassy_net::{Ipv4Cidr, Stack, StaticConfigV4};
use embassy_time::{Duration, Timer, with_timeout};
use embedded_io_async::Write;
use log::{info, warn};
use on_air_core::timezone::{self, TimeZone};

use crate::sntp_task;

/// The board's address on its setup network.
pub const ADDRESS: Ipv4Addr = Ipv4Addr::new(192, 168, 4, 1);

/// Network settings for the setup network's stack.
pub fn stack_config() -> embassy_net::Config {
    embassy_net::Config::ipv4_static(StaticConfigV4 {
        address: Ipv4Cidr::new(ADDRESS, 24),
        gateway: Some(ADDRESS),
        dns_servers: Default::default(),
    })
}

/// Hands out addresses to phones that join the setup network.
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
    // Up to 8 phones at once is plenty for setup.
    let mut server = Server::<_, 8>::new_with_et(ADDRESS);
    loop {
        let options = ServerOptions::new(ADDRESS, Some(&mut gateway));
        if let Err(error) = io::server::run(&mut server, &options, &mut socket, &mut packet).await {
            warn!("setup network: DHCP error: {error:?}");
        }
        Timer::after_millis(500).await;
    }
}

/// Answers every request on port 80 with a short status page.
#[embassy_executor::task]
pub async fn page_task(stack: Stack<'static>, network_name: &'static str) -> ! {
    let mut rx_buffer = [0u8; 1024];
    let mut tx_buffer = [0u8; 1024];
    loop {
        let mut socket = TcpSocket::new(stack, &mut rx_buffer, &mut tx_buffer);
        socket.set_timeout(Some(Duration::from_secs(10)));
        if socket.accept(80).await.is_err() {
            continue;
        }
        info!(
            "setup network: page requested by {:?}",
            socket.remote_endpoint()
        );
        // Read whatever the browser sends; the reply is the same for any path.
        let mut request = [0u8; 512];
        let _ = with_timeout(Duration::from_secs(2), socket.read(&mut request)).await;

        let time = sntp_task::unix_seconds()
            .and_then(|s| timezone::local_time(&TimeZone::UTC, s))
            .map(|t| format!("{t} UTC"))
            .unwrap_or_else(|| "not synced yet".into());
        let body = format!(
            "<!doctype html><meta name=viewport content=\"width=device-width\">\
             <title>On-Air</title><h1>On-Air</h1>\
             <p>You're on the setup network <b>{network_name}</b>.</p>\
             <p>Home WiFi: <b>{}</b><br>Time: {time}</p>",
            if crate::wifi_task::is_online() {
                "online"
            } else {
                "offline"
            },
        );
        let head = format!(
            "HTTP/1.0 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let _ = socket.write_all(head.as_bytes()).await;
        let _ = socket.write_all(body.as_bytes()).await;
        let _ = socket.flush().await;
        socket.close();
        Timer::after_millis(100).await;
    }
}
