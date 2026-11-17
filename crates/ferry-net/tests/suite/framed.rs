use ferry_net::{Error, Framed};
use ferry_proto::{Channel, Codec, Message};
use tokio::io::{AsyncWriteExt, duplex};

#[tokio::test]
async fn absolute_motion_backlog_is_collapsed_without_crossing_a_button_or_heartbeat() {
    use ferry_types::{InputEvent, MouseButton, Point};
    let (a, b) = duplex(65536);
    let mut tx = Framed::new(a, Channel::Input);
    let mut rx = Framed::new(b, Channel::Input);
    for x in 0..100 {
        tx.send(&Message::Input(InputEvent::PointerMotion {
            position: Point::new(x, 5),
        }))
        .await
        .unwrap();
    }
    let button = Message::Input(InputEvent::Button {
        button: MouseButton::LEFT,
        pressed: true,
        position: Point::new(99, 5),
    });
    tx.send(&button).await.unwrap();
    tx.send(&Message::Input(InputEvent::PointerMotion {
        position: Point::new(101, 5),
    }))
    .await
    .unwrap();
    tx.send(&Message::Ping { nonce: 7 }).await.unwrap();
    assert_eq!(
        rx.recv_latest_pointer().await.unwrap(),
        Some(Message::Input(InputEvent::PointerMotion {
            position: Point::new(99, 5)
        }))
    );
    assert_eq!(rx.recv_latest_pointer().await.unwrap(), Some(button));
    assert_eq!(
        rx.recv_latest_pointer().await.unwrap(),
        Some(Message::Input(InputEvent::PointerMotion {
            position: Point::new(101, 5)
        }))
    );
    assert_eq!(
        rx.recv_latest_pointer().await.unwrap(),
        Some(Message::Ping { nonce: 7 })
    );
}

#[tokio::test]
async fn split_preserves_prefetched_frames_and_can_reply_without_more_input() {
    let (a, b) = duplex(4096);
    let mut peer = Framed::new(a, Channel::DisplayStream);
    let mut receiver = Framed::new(b, Channel::DisplayStream);
    peer.send(&Message::Ping { nonce: 1 }).await.unwrap();
    peer.send(&Message::Ping { nonce: 2 }).await.unwrap();
    assert_eq!(
        receiver.recv().await.unwrap(),
        Some(Message::Ping { nonce: 1 })
    );
    let (mut read, mut write) = receiver.split();
    assert_eq!(read.recv().await.unwrap(), Some(Message::Ping { nonce: 2 }));
    write.send(&Message::Pong { nonce: 2 }).await.unwrap();
    assert_eq!(peer.recv().await.unwrap(), Some(Message::Pong { nonce: 2 }));
}

#[tokio::test]
async fn messages_survive_a_byte_at_a_time_stream() {
    let (a, b) = duplex(1);
    let mut tx = Framed::new(a, Channel::Input);
    let mut rx = Framed::new(b, Channel::Input);

    let sent = [
        Message::Ping { nonce: 1 },
        Message::Leave { seq: 7 },
        Message::Pong { nonce: 2 },
    ];
    let send = async {
        for m in &sent {
            tx.send(m).await.unwrap();
        }
        drop(tx);
    };
    let receive = async {
        let mut got = Vec::new();
        while let Some(m) = rx.recv().await.unwrap() {
            got.push(m);
        }
        got
    };
    let ((), got) = tokio::join!(send, receive);
    assert_eq!(got, sent);
}

#[tokio::test]
async fn closing_mid_frame_is_an_error() {
    let (mut a, b) = duplex(64);
    let mut frame = Vec::new();
    Codec::new(Channel::Input)
        .encode(&Message::Ping { nonce: 9 }, &mut frame)
        .unwrap();
    a.write_all(&frame[..frame.len() - 1]).await.unwrap();
    drop(a);

    let mut rx = Framed::new(b, Channel::Input);
    assert!(matches!(rx.recv().await, Err(Error::TruncatedFrame)));
}

#[tokio::test]
async fn protocol_violations_surface_as_decode_errors() {
    let (mut a, b) = duplex(64);
    a.write_all(&[0x77, 0x77, 0, 0, 0, 0]).await.unwrap();

    let mut rx = Framed::new(b, Channel::Input);
    assert!(matches!(rx.recv().await, Err(Error::Decode(_))));
}
