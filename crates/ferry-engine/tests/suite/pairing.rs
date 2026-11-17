use std::time::Duration;

use ferry_engine::{DisplayConnection, Error, Event, Events};
use ferry_net::Tls;
use ferry_proto::{
    Capabilities, Channel, DisplayControl, DisplayDirection, Hello, Message, RejectReason,
};
use ferry_trust::{Identity, PeerList};
use tokio::time::timeout;

use crate::support::{AutoConfirm, Rig, Setup};

#[tokio::test]
async fn first_connection_pairs_after_both_sides_confirm() {
    let rig = Rig::start().await;
    rig.enter().await;

    let (mac_seen, pc_seen) = (rig.mac_confirm.requests(), rig.pc_confirm.requests());
    assert_eq!((mac_seen.len(), pc_seen.len()), (1, 1), "两边各确认一次");
    assert_eq!(mac_seen[0].code, pc_seen[0].code, "两边显示同一个配对码");
    assert_eq!(mac_seen[0].peer_name, "pc");
    assert_eq!(pc_seen[0].peer_name, "mac");

    let mac_id = rig.mac.store.identity("mac").unwrap();
    let pc_id = rig.pc.store.identity("pc").unwrap();
    assert_eq!(mac_seen[0].peer_fingerprint, pc_id.fingerprint());
    assert_eq!(pc_seen[0].peer_fingerprint, mac_id.fingerprint());
    assert!(
        rig.mac
            .store
            .peers()
            .unwrap()
            .contains(&pc_id.fingerprint())
    );
    assert!(
        rig.pc
            .store
            .peers()
            .unwrap()
            .contains(&mac_id.fingerprint())
    );
}

#[tokio::test]
async fn paired_devices_reconnect_without_asking_again() {
    let rig = Rig::start().await;
    rig.enter().await;
    let (mac, pc) = rig.stop();

    let rig = Rig::with(Setup {
        mac,
        pc,
        ..Setup::default()
    })
    .await;
    rig.enter().await;
    assert!(rig.mac_confirm.requests().is_empty());
    assert!(rig.pc_confirm.requests().is_empty());
}

#[tokio::test]
async fn the_server_saying_no_stops_the_client() {
    let rig = Rig::with(Setup {
        mac_confirm: AutoConfirm::answering(false),
        ..Setup::default()
    })
    .await;

    let error = timeout(Duration::from_secs(5), rig.client)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(error, Error::PairingRejected), "{error}");
    assert!(rig.mac.store.peers().unwrap().is_empty());
    assert!(rig.pc.store.peers().unwrap().is_empty());
    assert!(!rig.capture.is_grabbed());
}

#[tokio::test]
async fn the_client_saying_no_also_stops_it() {
    let rig = Rig::with(Setup {
        pc_confirm: AutoConfirm::answering(false),
        ..Setup::default()
    })
    .await;

    let error = timeout(Duration::from_secs(5), rig.client)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(error, Error::PairingRejected), "{error}");
    assert!(rig.mac.store.peers().unwrap().is_empty());
    assert!(rig.pc.store.peers().unwrap().is_empty());
}

#[tokio::test]
async fn a_client_the_server_forgot_pairs_again() {
    let rig = Rig::start().await;
    rig.enter().await;
    let (mac, pc) = rig.stop();
    // Mac 上删掉了这台 PC，PC 还记得 Mac。
    mac.store.save_peers(&PeerList::new()).unwrap();

    let rig = Rig::with(Setup {
        mac,
        pc,
        ..Setup::default()
    })
    .await;
    rig.enter().await;
    assert_eq!(rig.mac_confirm.requests().len(), 1);
    assert_eq!(rig.pc_confirm.requests().len(), 1);
}

#[tokio::test]
async fn a_stranger_cannot_start_a_session() {
    let rig = Rig::with(Setup {
        start_client: false,
        ..Setup::default()
    })
    .await;

    let stranger = Tls::new(&Identity::generate("陌生人").unwrap()).unwrap();
    let (mut framed, _) = stranger.connect(rig.addr, Channel::Input).await.unwrap();
    let hello = Hello::new(Channel::Input, Capabilities::all(), 0, "陌生人");
    match ferry_net::initiate(&mut framed, hello).await {
        Err(ferry_net::Error::Rejected(r)) => assert_eq!(r.reason, RejectReason::NOT_PAIRED),
        other => panic!("应被拒绝，实际 {other:?}"),
    }
    assert!(rig.mac_confirm.requests().is_empty(), "不配对就不打扰用户");
}

#[tokio::test]
async fn a_stranger_cannot_join_someone_elses_session() {
    let rig = Rig::start().await;
    rig.enter().await;

    let stranger = Tls::new(&Identity::generate("陌生人").unwrap()).unwrap();
    // 会话号是递增的小整数，很好猜；挡住它的是证书指纹。
    for channel in [Channel::Data, Channel::DisplayStream] {
        for session in 1..=4 {
            let (mut framed, _) = stranger.connect(rig.addr, channel).await.unwrap();
            let hello = Hello::new(channel, Capabilities::all(), session, "陌生人");
            match ferry_net::initiate(&mut framed, hello).await {
                Err(ferry_net::Error::Rejected(r)) => {
                    assert_eq!(r.reason, RejectReason::UNKNOWN_SESSION);
                }
                other => panic!("{channel}会话 {session} 应被拒绝，实际 {other:?}"),
            }
        }
    }
    // 原来的会话不受影响。
    rig.leave().await;
    rig.enter().await;
}

#[tokio::test]
async fn display_connection_reuses_the_paired_certificate_and_input_session() {
    let rig = Rig::start().await;
    rig.enter().await;
    let (mac, pc) = rig.stop();
    let (events, mut changes) = Events::channel();
    let (events, mut display) = events.with_display_stream();
    let rig = Rig::with(Setup {
        mac,
        pc,
        start_client: false,
        mac_events: events,
        ..Setup::default()
    })
    .await;

    let identity = rig.pc.store.identity("pc").unwrap();
    let tls = Tls::new(&identity).unwrap();
    let (mut input, _) = tls.connect(rig.addr, Channel::Input).await.unwrap();
    let caps = Capabilities::DISPLAY_STREAM;
    let welcome = ferry_net::initiate(&mut input, Hello::new(Channel::Input, caps, 0, "pc"))
        .await
        .unwrap();
    assert!(welcome.capabilities.contains(caps));
    assert!(matches!(
        input.recv().await.unwrap(),
        Some(Message::Displays(_))
    ));
    input
        .send(&Message::Displays(vec![crate::support::display(
            1920, 1080, 100,
        )]))
        .await
        .unwrap();
    timeout(Duration::from_secs(5), async {
        loop {
            if matches!(
                changes.recv().await.unwrap(),
                Event::Connected {
                    display_stream: true,
                    ..
                }
            ) {
                break;
            }
        }
    })
    .await
    .unwrap();
    let control = DisplayControl::Start {
        id: 94,
        direction: DisplayDirection::ExtendPeer,
        audio: true,
    };
    input
        .send(&Message::DisplayControl(control.clone()))
        .await
        .unwrap();
    timeout(Duration::from_secs(5), async {
        loop {
            if let Event::DisplayControl {
                peer,
                session,
                control: got,
            } = changes.recv().await.unwrap()
            {
                assert_eq!(peer, identity.fingerprint());
                assert_eq!(session, welcome.session);
                assert_eq!(got, control);
                break;
            }
        }
    })
    .await
    .unwrap();

    let Some(DisplayConnection::SendControl(sender)) =
        timeout(Duration::from_secs(5), display.recv())
            .await
            .unwrap()
    else {
        panic!("需要服务端的扩展屏控制发送端");
    };
    assert_eq!(sender.peer, identity.fingerprint());
    assert!(sender.send(DisplayControl::Ready { id: 94 }));
    timeout(Duration::from_secs(5), async {
        loop {
            match input.recv().await.unwrap().unwrap() {
                Message::DisplayControl(control) => {
                    assert_eq!(control, DisplayControl::Ready { id: 94 });
                    break;
                }
                Message::Ping { nonce } => input.send(&Message::Pong { nonce }).await.unwrap(),
                _ => {}
            }
        }
    })
    .await
    .unwrap();

    let (mut stream, _) = tls.connect(rig.addr, Channel::DisplayStream).await.unwrap();
    ferry_net::initiate(
        &mut stream,
        Hello::new(Channel::DisplayStream, caps, welcome.session, "pc"),
    )
    .await
    .unwrap();
    stream.send(&Message::DisplayOpen { id: 94 }).await.unwrap();
    let Some(DisplayConnection::Incoming(incoming)) =
        timeout(Duration::from_secs(5), display.recv())
            .await
            .unwrap()
    else {
        panic!("需要客户端建立的扩展屏媒体连接");
    };
    assert_eq!(incoming.peer, identity.fingerprint());
    assert_eq!(incoming.session, welcome.session);
    assert_eq!(incoming.id, 94);
    assert_eq!(incoming.framed.channel(), Channel::DisplayStream);
    assert!(rig.mac_confirm.requests().is_empty());

    rig.mac.store.save_peers(&PeerList::new()).unwrap();
    let (mut revoked, _) = tls.connect(rig.addr, Channel::DisplayStream).await.unwrap();
    let result = ferry_net::initiate(
        &mut revoked,
        Hello::new(Channel::DisplayStream, caps, welcome.session, "pc"),
    )
    .await;
    assert!(
        matches!(result, Err(ferry_net::Error::Rejected(r)) if r.reason == RejectReason::UNKNOWN_SESSION)
    );
}

#[tokio::test]
async fn display_media_can_open_from_the_paired_client_without_a_second_pairing() {
    let (server_events, _) = Events::channel();
    let (server_events, mut server_display) = server_events.with_display_stream();
    let (client_events, _) = Events::channel();
    let (client_events, mut client_display) = client_events.with_display_stream();
    let rig = Rig::with(Setup {
        mac_events: server_events,
        pc_events: client_events,
        ..Setup::default()
    })
    .await;
    let Some(DisplayConnection::OpenMedia(opener)) =
        timeout(Duration::from_secs(5), client_display.recv())
            .await
            .unwrap()
    else {
        panic!("需要客户端的媒体连接发起端");
    };
    assert_eq!(
        opener.peer,
        rig.mac.store.identity("mac").unwrap().fingerprint()
    );
    let mut outgoing = opener.open(17).await.unwrap();
    let Some(DisplayConnection::SendControl(_)) =
        timeout(Duration::from_secs(5), server_display.recv())
            .await
            .unwrap()
    else {
        panic!("需要服务端的控制连接发送端");
    };
    let Some(DisplayConnection::Incoming(mut incoming)) =
        timeout(Duration::from_secs(5), server_display.recv())
            .await
            .unwrap()
    else {
        panic!("需要对端收到的媒体连接");
    };
    assert_eq!(incoming.id, 17);
    let video = Message::DisplayPacket(ferry_proto::DisplayPacket {
        track: ferry_proto::DisplayTrack::Video,
        timestamp_us: 10_000,
        end: true,
        bytes: vec![1, 2, 3],
    });
    outgoing.send(&video).await.unwrap();
    assert_eq!(incoming.framed.recv().await.unwrap(), Some(video));
    assert_eq!(rig.pc_confirm.requests().len(), 1);
    assert_eq!(rig.mac_confirm.requests().len(), 1);
}

/// 不填地址：服务端广播，客户端在局域网里找到它、配对、开始会话。
/// 需要本机网络支持组播，默认跳过：`cargo test -p ferry-engine -- --ignored`
#[tokio::test]
#[ignore = "需要本机网络支持组播"]
async fn a_client_without_an_address_finds_and_pairs_with_the_server() {
    let rig = Rig::with(Setup {
        discover: true,
        ..Setup::default()
    })
    .await;
    // 发现需要一点时间，多推几次。
    for _ in 0..5 {
        if rig.mac_confirm.requests().len() == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    rig.enter().await;
    assert_eq!(rig.pc_confirm.requests().len(), 1);
}
