use ferry_platform::{DisplayBackend, DisplayDatagram, MediaFrame, MediaTrack};
use ferry_platform_windows::WinDisplayBackend;
use std::net::UdpSocket;
use std::os::windows::io::AsRawSocket;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

fn endpoint() -> Box<dyn DisplayDatagram> {
    WinDisplayBackend.datagram(false).unwrap().unwrap()
}

struct Relay {
    front: UdpSocket,
    back: UdpSocket,
    stop: Arc<AtomicBool>,
    dropped: Arc<AtomicUsize>,
    task: Option<std::thread::JoinHandle<()>>,
}
impl Relay {
    fn new() -> Self {
        let front = UdpSocket::bind("127.0.0.1:0").unwrap();
        let back = UdpSocket::bind("127.0.0.1:0").unwrap();
        // The fault relay must not accidentally add unrelated kernel drops
        // when other tests/compiler processes are consuming CPU.
        unsafe extern "system" {
            fn setsockopt(
                socket: usize,
                level: i32,
                option: i32,
                value: *const u8,
                length: i32,
            ) -> i32;
        }
        for socket in [&front, &back] {
            let buffer = 1024 * 1024_i32;
            // SAFETY: a live UDP socket and an initialized 4-byte SO_RCVBUF value.
            assert_eq!(
                unsafe {
                    setsockopt(
                        socket.as_raw_socket() as usize,
                        0xffff,
                        0x1002,
                        (&buffer as *const i32).cast(),
                        4,
                    )
                },
                0
            );
        }
        Self {
            front,
            back,
            stop: Arc::new(AtomicBool::new(false)),
            dropped: Arc::new(AtomicUsize::new(0)),
            task: None,
        }
    }
    fn run(&mut self, left: u16, right: u16, loss: usize, corrupt: bool) {
        let front = self.front.try_clone().unwrap();
        let back = self.back.try_clone().unwrap();
        front.set_nonblocking(true).unwrap();
        back.set_nonblocking(true).unwrap();
        let stop = self.stop.clone();
        let dropped = self.dropped.clone();
        self.task = Some(std::thread::spawn(move || {
            let mut packet = [0u8; 1500];
            let mut number = 0usize;
            let mut pending: Option<Vec<u8>> = None;
            while !stop.load(Ordering::Acquire) {
                if let Ok((length, _)) = front.recv_from(&mut packet) {
                    number += 1;
                    if loss > 0 && number.is_multiple_of(loss) {
                        dropped.fetch_add(1, Ordering::Relaxed);
                        continue;
                    }
                    if corrupt {
                        packet[length - 1] ^= 1;
                    }
                    if let Some(previous) = pending.take() {
                        back.send_to(&packet[..length], ("127.0.0.1", right))
                            .unwrap();
                        back.send_to(&previous, ("127.0.0.1", right)).unwrap();
                        if number.is_multiple_of(10) {
                            back.send_to(&previous, ("127.0.0.1", right)).unwrap();
                        }
                    } else {
                        pending = Some(packet[..length].to_vec());
                    }
                } else if let Some(previous) = pending.take() {
                    back.send_to(&previous, ("127.0.0.1", right)).unwrap();
                }
                if let Ok((length, _)) = back.recv_from(&mut packet) {
                    front
                        .send_to(&packet[..length], ("127.0.0.1", left))
                        .unwrap();
                }
                std::thread::yield_now();
            }
        }));
    }
}
impl Drop for Relay {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(task) = self.task.take() {
            task.join().unwrap();
        }
    }
}

fn frame(length: usize, track: MediaTrack) -> MediaFrame {
    MediaFrame {
        track,
        timestamp_us: 123_400,
        bytes: (0..length).map(|n| (n as u8).wrapping_mul(17)).collect(),
    }
}

#[test]
fn apollo_packets_and_moonlight_queue_recover_loss_reordering_duplicates_and_multiblock() {
    let mut sender = endpoint();
    let mut receiver = endpoint();
    let mut relay = Relay::new();
    sender.set_key([7; 32]).unwrap();
    receiver.set_key([7; 32]).unwrap();
    sender.pacing(12_000_000);
    sender
        .set_peer(relay.front.local_addr().unwrap(), 0)
        .unwrap();
    receiver
        .set_peer(relay.back.local_addr().unwrap(), 0)
        .unwrap();
    relay.run(sender.local_port(), receiver.local_port(), 17, false);
    let expected = frame(360_000, MediaTrack::Video).bytes;
    let send = std::thread::spawn(move || {
        sender.send(frame(360_000, MediaTrack::Video)).unwrap();
    });
    let deadline = Instant::now() + Duration::from_secs(3);
    let actual = loop {
        assert!(
            Instant::now() < deadline,
            "Moonlight 没有恢复丢包帧；{:?}",
            receiver.stats()
        );
        if let Some(frame) = receiver.receive(Duration::from_millis(10)).unwrap() {
            break frame;
        }
    };
    send.join().unwrap();
    assert_eq!(actual.bytes, expected);
    assert_eq!(actual.timestamp_us, 123_400);
    assert!(relay.dropped.load(Ordering::Relaxed) > 0);
    assert!(
        receiver.stats().recovered > 0,
        "测试必须实际经过上游 FEC 恢复"
    );
}

#[test]
fn unauthenticated_media_and_wrong_keys_never_reach_the_moonlight_queue() {
    let mut sender = endpoint();
    let mut receiver = endpoint();
    let mut relay = Relay::new();
    sender.set_key([7; 32]).unwrap();
    receiver.set_key([8; 32]).unwrap();
    sender
        .set_peer(relay.front.local_addr().unwrap(), 0)
        .unwrap();
    receiver
        .set_peer(relay.back.local_addr().unwrap(), 0)
        .unwrap();
    relay.run(sender.local_port(), receiver.local_port(), 0, false);
    sender.send(frame(9_000, MediaTrack::Video)).unwrap();
    assert!(
        receiver
            .receive(Duration::from_millis(100))
            .unwrap()
            .is_none()
    );
    assert!(receiver.stats().invalid > 0);
}

#[test]
fn tampered_authenticated_packets_are_rejected_before_fec() {
    let mut sender = endpoint();
    let mut receiver = endpoint();
    let mut relay = Relay::new();
    sender.set_key([7; 32]).unwrap();
    receiver.set_key([7; 32]).unwrap();
    sender
        .set_peer(relay.front.local_addr().unwrap(), 0)
        .unwrap();
    receiver
        .set_peer(relay.back.local_addr().unwrap(), 0)
        .unwrap();
    relay.run(sender.local_port(), receiver.local_port(), 0, true);
    sender.send(frame(9_000, MediaTrack::Video)).unwrap();
    assert!(
        receiver
            .receive(Duration::from_millis(100))
            .unwrap()
            .is_none()
    );
    assert!(receiver.stats().invalid > 0);
    assert_eq!(receiver.stats().frames, 0);
}

#[test]
fn replay_window_rejects_old_ciphertext_across_a_4096_packet_boundary() {
    let front = UdpSocket::bind("127.0.0.1:0").unwrap();
    let back = UdpSocket::bind("127.0.0.1:0").unwrap();
    front
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let mut sender = endpoint();
    let mut receiver = endpoint();
    sender.set_key([12; 32]).unwrap();
    receiver.set_key([12; 32]).unwrap();
    sender.set_peer(front.local_addr().unwrap(), 0).unwrap();
    receiver.set_peer(back.local_addr().unwrap(), 0).unwrap();
    let peer = format!("127.0.0.1:{}", receiver.local_port());
    let mut first = Vec::new();
    let mut bytes = [0u8; 1500];
    for index in 0..2050 {
        sender.send(frame(100, MediaTrack::Video)).unwrap();
        // This access unit has one data shard and one FEC shard.
        for _ in 0..2 {
            let (length, _) = front.recv_from(&mut bytes).unwrap();
            if first.is_empty() {
                first = bytes[..length].to_vec();
            }
            back.send_to(&bytes[..length], &peer).unwrap();
        }
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            assert!(Instant::now() < deadline, "frame {index} missing");
            if receiver
                .receive(Duration::from_millis(2))
                .unwrap()
                .is_some()
            {
                break;
            }
        }
    }
    let _ = receiver.receive(Duration::from_millis(5)).unwrap();
    let before = receiver.stats().received;
    assert!(before >= 4096);
    back.send_to(&first, &peer).unwrap();
    assert!(
        receiver
            .receive(Duration::from_millis(10))
            .unwrap()
            .is_none()
    );
    assert_eq!(
        receiver.stats().received,
        before,
        "旧密文不能重新进入上游队列"
    );
}

#[test]
fn video_and_audio_keep_separate_rtp_sequences_and_nonces() {
    let mut left = endpoint();
    let mut right = endpoint();
    left.set_key([9; 32]).unwrap();
    right.set_key([9; 32]).unwrap();
    left.set_peer(
        format!("127.0.0.1:{}", right.local_port()).parse().unwrap(),
        0,
    )
    .unwrap();
    right
        .set_peer(
            format!("127.0.0.1:{}", left.local_port()).parse().unwrap(),
            0,
        )
        .unwrap();
    left.send(frame(9000, MediaTrack::Video)).unwrap();
    left.send(frame(3200, MediaTrack::Audio)).unwrap();
    let mut tracks = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(2);
    while tracks.len() < 2 {
        assert!(Instant::now() < deadline);
        if let Some(frame) = right.receive(Duration::from_millis(10)).unwrap() {
            tracks.push(frame.track);
        }
    }
    assert_eq!(tracks, vec![MediaTrack::Video, MediaTrack::Audio]);
}

#[test]
fn encrypted_bidirectional_probe_succeeds_and_an_unreachable_peer_times_out() {
    let mut left = endpoint();
    let mut right = endpoint();
    left.set_key([4; 32]).unwrap();
    right.set_key([4; 32]).unwrap();
    left.set_peer(
        format!("127.0.0.1:{}", right.local_port()).parse().unwrap(),
        0,
    )
    .unwrap();
    right
        .set_peer(
            format!("127.0.0.1:{}", left.local_port()).parse().unwrap(),
            0,
        )
        .unwrap();
    let probe = std::thread::spawn(move || left.probe(true, Duration::from_millis(120)).unwrap());
    assert!(right.probe(false, Duration::from_millis(120)).unwrap());
    assert!(probe.join().unwrap());
    let mut blocked = endpoint();
    blocked.set_key([5; 32]).unwrap();
    blocked.set_peer("127.0.0.1:9".parse().unwrap(), 0).unwrap();
    assert!(!blocked.probe(true, Duration::from_millis(80)).unwrap());
}

#[test]
#[ignore = "实机验收：临时虚拟屏、真实编码、加密 UDP/Moonlight FEC、硬解和呈现 10 秒并恢复"]
fn native_capture_encrypted_udp_decode_present_and_bitrate_reconfigure() {
    let mut sender = endpoint();
    let mut receiver = endpoint();
    sender.set_key([11; 32]).unwrap();
    receiver.set_key([11; 32]).unwrap();
    sender
        .set_peer(
            format!("127.0.0.1:{}", receiver.local_port())
                .parse()
                .unwrap(),
            0,
        )
        .unwrap();
    receiver
        .set_peer(
            format!("127.0.0.1:{}", sender.local_port())
                .parse()
                .unwrap(),
            0,
        )
        .unwrap();
    let mut source = WinDisplayBackend.source([0; 32], false).unwrap();
    source.set_bitrate(12_000_000).unwrap();
    sender.pacing(20_000_000);
    let mut viewer = WinDisplayBackend.viewer(false).unwrap();
    let start = Instant::now();
    let mut count = 0;
    let mut latency = Vec::new();
    let mut adjusted = false;
    let mut recovery = ferry_core::MediaRecovery::default();
    while start.elapsed() < Duration::from_secs(10) {
        if !adjusted && start.elapsed() > Duration::from_secs(5) {
            source.set_bitrate(10_000_000).unwrap();
            source.request_keyframe().unwrap();
            adjusted = true;
        }
        if let Some(frame) = source.next_frame(Duration::from_millis(2)).unwrap() {
            sender.send(frame).unwrap();
        }
        if let Some(frame) = receiver.receive(Duration::from_millis(2)).unwrap() {
            assert!(recovery.accept(&frame.bytes), "首次画面不是独立解码 IDR");
            let timestamp = frame.timestamp_us;
            if viewer.present(frame).unwrap() {
                count += 1;
                if let Some(age) = source.capture_age(timestamp) {
                    latency.push(age.as_secs_f64() * 1000.0);
                }
            }
        }
    }
    latency.sort_by(f64::total_cmp);
    assert!(count > 50, "本机完整原生媒体链路没有持续呈现");
    println!(
        "capture -> NVENC/MFT -> AES-GCM UDP -> Moonlight FEC -> decode/present: count={count}, fps={:.1}, timestamp_age_p50_ms={:.2}, p95_ms={:.2}, stats={:?}, source={}",
        count as f64 / start.elapsed().as_secs_f64(),
        latency[latency.len() / 2],
        latency[latency.len() * 95 / 100],
        receiver.stats(),
        source.description()
    );
    drop(viewer);
    drop(source);
    WinDisplayBackend.recover().unwrap();
}
