use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use ferry_net::{UdpMediaConfig, UdpMediaSocket};
use tokio::net::UdpSocket;

#[tokio::test]
async fn binds_requested_interface_and_exchanges_datagrams() {
    let receiver = UdpSocket::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))
        .await
        .unwrap();
    let receiver_addr = receiver.local_addr().unwrap();
    let sender = UdpMediaSocket::bind(UdpMediaConfig::new(
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
        receiver_addr,
        17,
    ))
    .await
    .unwrap();

    sender.send(b"frame").await.unwrap();
    let mut buffer = [0; 64];
    let (size, from) = receiver.recv_from(&mut buffer).await.unwrap();
    assert_eq!(&buffer[..size], b"frame");
    assert_eq!(from, sender.local_addr().unwrap());
    assert_eq!(sender.session_id(), 17);
    assert_eq!(sender.peer_addr(), receiver_addr);
}

#[tokio::test]
async fn rejects_mixed_ip_versions_before_binding() {
    let result = UdpMediaSocket::bind(UdpMediaConfig::new(
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
        "[::1]:9".parse().unwrap(),
        1,
    ))
    .await;
    assert!(result.is_err());
}
