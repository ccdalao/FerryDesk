use ferry_platform::{NetworkClass, NetworkPriority};
use ferry_platform_windows::WinNetworkPriority;
#[test]
#[ignore = "需要真实局域网 TCP 目标，SCREENFERRY_QOS_TEST_PEER 指定已授权的测试端口"]
fn windows_accepts_control_priority_for_a_real_lan_socket() {
    let peer = std::env::var("SCREENFERRY_QOS_TEST_PEER")
        .expect("需要现场 LAN 测试目标")
        .parse()
        .unwrap();
    let socket =
        std::net::TcpStream::connect_timeout(&peer, std::time::Duration::from_secs(2)).unwrap();
    let flow = WinNetworkPriority
        .tcp(&socket, NetworkClass::Control)
        .expect("Windows 没有授予控制流 QoS");
    flow.set_class(NetworkClass::Data).unwrap();
    flow.set_class(NetworkClass::Control).unwrap();
    drop(flow);
    assert!(
        socket.peer_addr().is_ok(),
        "撤销流登记不应关闭调用方的 socket"
    );
    println!("qWAVE Control / Data / Control accepted for {peer}");
}
