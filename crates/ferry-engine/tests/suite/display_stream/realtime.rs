//! 用真实 TLS 会话和可故障注入的测试数据报验证运行时，而非替代原生 RTP 测试。
use super::*;
use ferry_platform::{DisplayDatagram, DisplayTransportStats};
use std::net::{SocketAddr, UdpSocket};

pub(super) struct ProbeDatagram {
    socket: UdpSocket,
    probe: Arc<MediaProbe>,
    stats: DisplayTransportStats,
}
impl ProbeDatagram {
    pub(super) fn new(probe: Arc<MediaProbe>) -> Self {
        Self {
            socket: UdpSocket::bind("127.0.0.1:0").unwrap(),
            probe,
            stats: Default::default(),
        }
    }
}
impl DisplayDatagram for ProbeDatagram {
    fn local_port(&self) -> u16 {
        self.socket.local_addr().unwrap().port()
    }
    fn set_peer(&mut self, addr: SocketAddr, _token: u64) -> ferry_platform::Result<()> {
        self.socket.connect(addr).unwrap();
        Ok(())
    }
    fn set_key(&mut self, _key: [u8; 32]) -> ferry_platform::Result<()> {
        Ok(())
    }
    fn probe(&mut self, _source: bool, _wait: Duration) -> ferry_platform::Result<bool> {
        Ok(self.probe.realtime_mode.load(Ordering::SeqCst) != 1)
    }
    fn pacing(&mut self, _bits: u32) {}
    fn stats(&mut self) -> DisplayTransportStats {
        self.stats
    }
    fn send(&mut self, frame: MediaFrame) -> ferry_platform::Result<()> {
        let sent = self.probe.datagrams_sent.fetch_add(1, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(u64::from(
            self.probe.udp_delay_ms.load(Ordering::SeqCst),
        )));
        if self.probe.realtime_mode.load(Ordering::SeqCst) == 2 && sent > 5 {
            return Ok(());
        }
        let mut bytes = frame.timestamp_us.to_le_bytes().to_vec();
        bytes.push(u8::from(frame.track == MediaTrack::Audio));
        bytes.extend_from_slice(&frame.bytes);
        self.socket.send(&bytes).unwrap();
        Ok(())
    }
    fn receive(&mut self, wait: Duration) -> ferry_platform::Result<Option<MediaFrame>> {
        self.socket.set_read_timeout(Some(wait)).unwrap();
        let mut bytes = [0u8; 65535];
        let Ok(length) = self.socket.recv(&mut bytes) else {
            return Ok(None);
        };
        self.stats.frames += 1;
        Ok(Some(MediaFrame {
            timestamp_us: u64::from_le_bytes(bytes[..8].try_into().unwrap()),
            track: if bytes[8] == 0 {
                MediaTrack::Video
            } else {
                MediaTrack::Audio
            },
            bytes: bytes[9..length].to_vec(),
        }))
    }
}

#[tokio::test]
async fn slow_encoder_startup_does_not_discard_a_successful_udp_channel() {
    for reverse in [false, true] {
        let controller = Arc::new(MediaProbe::default());
        let controlled = Arc::new(MediaProbe::default());
        controller.realtime_mode.store(3, Ordering::SeqCst);
        controlled.realtime_mode.store(3, Ordering::SeqCst);
        let (a, b) = paired_probes(controller.clone(), controlled.clone()).await;
        let (source, viewer) = if reverse {
            (&controlled, &controller)
        } else {
            (&controller, &controlled)
        };
        source.source_start_delay_ms.store(2200, Ordering::SeqCst);
        a.handle.send(Command::Display(if reverse {
            DisplayAction::ExtendControlled
        } else {
            DisplayAction::ExtendController
        }));
        until("超过旧 1.5 秒时限仍通过 UDP 接收", || {
            viewer.presented.load(Ordering::SeqCst) > 5
        })
        .await;
        assert!(
            source.datagrams_sent.load(Ordering::SeqCst) > 5,
            "编码器正常启动不应触发 TCP 回退"
        );
        stop_and_quit(a, b, source).await;
    }
}

#[tokio::test]
async fn pausing_removes_resources_and_the_same_command_resumes_the_direction() {
    let controller = Arc::new(MediaProbe::default());
    let controlled = Arc::new(MediaProbe::default());
    controller.realtime_mode.store(3, Ordering::SeqCst);
    controlled.realtime_mode.store(3, Ordering::SeqCst);
    let (a, b) = paired_probes(controller.clone(), controlled.clone()).await;
    a.handle
        .send(Command::Display(DisplayAction::ExtendControlled));
    until("反向扩展首帧", || {
        controller.presented.load(Ordering::SeqCst) > 0
    })
    .await;
    a.handle.send(Command::Display(DisplayAction::TogglePause));
    until("暂停回收虚拟屏", || {
        controlled.active_sources.load(Ordering::SeqCst) == 0
    })
    .await;
    a.handle.send(Command::Display(DisplayAction::TogglePause));
    until("恢复上一方向", || {
        controlled.active_sources.load(Ordering::SeqCst) == 1
    })
    .await;
    let before = controller.presented.load(Ordering::SeqCst);
    until("恢复后有新画面", || {
        controller.presented.load(Ordering::SeqCst) > before
    })
    .await;
    stop_and_quit(a, b, &controlled).await;
}

#[tokio::test]
async fn congested_viewer_reduces_quality_and_still_forwards_input() {
    let controller = Arc::new(MediaProbe::default());
    let controlled = Arc::new(MediaProbe::default());
    controller.realtime_mode.store(3, Ordering::SeqCst);
    controlled.realtime_mode.store(3, Ordering::SeqCst);
    let (a, b) = paired_probes(controller.clone(), controlled.clone()).await;
    controlled.presentation_delay_ms.store(30, Ordering::SeqCst);
    a.handle
        .send(Command::Display(DisplayAction::ExtendController));
    until("积压后立即降低码率", || {
        controller.bitrate.load(Ordering::SeqCst) > 0
            && controller.bitrate.load(Ordering::SeqCst) < 24_000_000
    })
    .await;
    let event = InputEvent::Key {
        code: KeyCode::keyboard(5),
        action: KeyAction::Up,
    };
    controlled.pending_input.lock().unwrap().push_back(event);
    until("降低画质期间仍转发按键", || {
        controller.injected.lock().unwrap().contains(&event)
    })
    .await;
    stop_and_quit(a, b, &controller).await;
}

#[tokio::test]
async fn slow_udp_send_does_not_stall_display_input_in_either_direction() {
    for reverse in [false, true] {
        let controller = Arc::new(MediaProbe::default());
        let controlled = Arc::new(MediaProbe::default());
        controller.realtime_mode.store(3, Ordering::SeqCst);
        controlled.realtime_mode.store(3, Ordering::SeqCst);
        let (a, b) = paired_probes(controller.clone(), controlled.clone()).await;
        let (source, viewer) = if reverse {
            (&controlled, &controller)
        } else {
            (&controller, &controlled)
        };
        source.udp_delay_ms.store(500, Ordering::SeqCst);
        a.handle.send(Command::Display(if reverse {
            DisplayAction::ExtendControlled
        } else {
            DisplayAction::ExtendController
        }));
        until("首个慢 UDP 帧", || {
            viewer.presented.load(Ordering::SeqCst) > 0
        })
        .await;
        let before = source.datagrams_sent.load(Ordering::SeqCst);
        until("下一次 UDP send 正在阻塞", || {
            source.datagrams_sent.load(Ordering::SeqCst) > before
        })
        .await;
        let event = InputEvent::Key {
            code: KeyCode::keyboard(4),
            action: KeyAction::Up,
        };
        let start = Instant::now();
        viewer.pending_input.lock().unwrap().push_back(event);
        timeout(
            Duration::from_millis(250),
            until("慢发包期间输入必须及时注入", || {
                source.injected.lock().unwrap().contains(&event)
            }),
        )
        .await
        .expect("500ms UDP 发送不应阻塞输入 250ms");
        println!(
            "slow UDP source direction reverse={reverse}: input reached worker in {:.2}ms",
            start.elapsed().as_secs_f64() * 1000.0
        );
        stop_and_quit(a, b, source).await;
    }
}

#[tokio::test]
async fn failed_udp_probe_falls_back_to_tcp_in_both_display_directions() {
    for reverse in [false, true] {
        let controller = Arc::new(MediaProbe::default());
        let controlled = Arc::new(MediaProbe::default());
        controller.realtime_mode.store(1, Ordering::SeqCst);
        controlled.realtime_mode.store(1, Ordering::SeqCst);
        let (a, b) = paired_probes(controller.clone(), controlled.clone()).await;
        let (source, viewer) = if reverse {
            (&controlled, &controller)
        } else {
            (&controller, &controlled)
        };
        a.handle.send(Command::Display(if reverse {
            DisplayAction::ExtendControlled
        } else {
            DisplayAction::ExtendController
        }));
        until("UDP 探测失败后 TCP 仍呈现", || {
            viewer.presented.load(Ordering::SeqCst) > 5
        })
        .await;
        assert_eq!(source.datagrams_sent.load(Ordering::SeqCst), 0);
        stop_and_quit(a, b, source).await;
    }
}

#[tokio::test]
async fn running_udp_blackhole_returns_to_tcp_and_requests_idr_without_losing_input() {
    for reverse in [false, true] {
        let controller = Arc::new(MediaProbe::default());
        let controlled = Arc::new(MediaProbe::default());
        controller.realtime_mode.store(2, Ordering::SeqCst);
        controlled.realtime_mode.store(2, Ordering::SeqCst);
        let (a, b) = paired_probes(controller.clone(), controlled.clone()).await;
        let (source, viewer) = if reverse {
            (&controlled, &controller)
        } else {
            (&controller, &controlled)
        };
        a.handle.send(Command::Display(if reverse {
            DisplayAction::ExtendControlled
        } else {
            DisplayAction::ExtendController
        }));
        until("UDP 首帧", || viewer.presented.load(Ordering::SeqCst) > 0).await;
        until("已进入 UDP 黑洞", || {
            source.datagrams_sent.load(Ordering::SeqCst) > 10
        })
        .await;
        let requested = source.keyframes.load(Ordering::SeqCst);
        until("UDP 黑洞后同步回退并请求 IDR", || {
            source.keyframes.load(Ordering::SeqCst) > requested
        })
        .await;
        // The fake UDP transport delivers at most six frames. More than ten
        // presentations prove the reliable fallback actually took place.
        until("TCP 恢复已超过全部 UDP 可交付帧", || {
            viewer.presented.load(Ordering::SeqCst) > 10
        })
        .await;
        let before = viewer.presented.load(Ordering::SeqCst);
        until("TCP 回退后继续呈现", || {
            viewer.presented.load(Ordering::SeqCst) > before + 3
        })
        .await;
        let event = InputEvent::Key {
            code: KeyCode::keyboard(4),
            action: KeyAction::Up,
        };
        viewer.pending_input.lock().unwrap().push_back(event);
        until("回退后输入回传", || {
            source.injected.lock().unwrap().contains(&event)
        })
        .await;
        assert!(source.datagrams_sent.load(Ordering::SeqCst) > 5);
        stop_and_quit(a, b, source).await;
    }
}
