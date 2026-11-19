mod realtime;

use ferry_engine::{receive_display_unit, send_display_unit};
use ferry_net::Framed;
use ferry_proto::limits::MAX_CHUNK_LEN;
use ferry_proto::{Channel, DisplayAccessUnit, DisplayPacket, DisplayTrack, Message};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ferry_engine::resident::{self, Command, DisplayAction, Host};
use ferry_ipc::{Endpoint, Notice};
use ferry_platform::{DisplayBackend, DisplaySource, DisplayViewer, MediaFrame, MediaTrack};
use ferry_platform_mock::{
    MockCapture, MockClipboard, MockDisplays, MockInject, MockOverlay, MockUpdater,
};
use ferry_trust::{Peer, PeerList};
use ferry_types::{
    Config, Display, DisplayId, InputEvent, KeyAction, KeyCode, MouseButton, Point, Rect, Role,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpSocket};
use tokio::time::{sleep, timeout};

use crate::support::Device;

#[tokio::test]
async fn large_video_frame_round_trips_on_the_media_connection() {
    let (a, b) = tokio::io::duplex(4096);
    let mut sender = Framed::new(a, Channel::DisplayStream);
    let mut receiver = Framed::new(b, Channel::DisplayStream);
    let frame = DisplayAccessUnit {
        track: DisplayTrack::Video,
        timestamp_us: 42,
        bytes: vec![0xa5; MAX_CHUNK_LEN * 2 + 1],
    };
    let (sent, received) = tokio::join!(
        send_display_unit(&mut sender, frame.clone()),
        receive_display_unit(&mut receiver)
    );
    sent.unwrap();
    assert_eq!(received.unwrap(), Some(frame));
}

#[tokio::test]
async fn closing_mid_frame_never_yields_a_partial_unit() {
    let (a, b) = tokio::io::duplex(4096);
    let mut sender = Framed::new(a, Channel::DisplayStream);
    let mut receiver = Framed::new(b, Channel::DisplayStream);
    sender
        .send(&Message::DisplayPacket(DisplayPacket {
            track: DisplayTrack::Video,
            timestamp_us: 42,
            end: false,
            bytes: vec![1, 2, 3],
        }))
        .await
        .unwrap();
    drop(sender);
    assert!(matches!(
        receive_display_unit(&mut receiver).await,
        Err(ferry_engine::Error::Protocol(_))
    ));
}

#[tokio::test]
async fn a_short_network_stall_does_not_discard_an_incomplete_video_frame() {
    let (a, b) = tokio::io::duplex(4096);
    let mut sender = Framed::new(a, Channel::DisplayStream);
    let mut receiver = Framed::new(b, Channel::DisplayStream);
    let frame = DisplayAccessUnit {
        track: DisplayTrack::Video,
        timestamp_us: 42,
        bytes: vec![0xa5; MAX_CHUNK_LEN * 3 + 1],
    };
    let expected = frame.clone();
    let delayed_receive = async {
        // A hotspot driver can stop draining the socket briefly. The caller
        // must finish this access unit rather than tear down a healthy session.
        sleep(Duration::from_millis(600)).await;
        receive_display_unit(&mut receiver).await
    };
    let (sent, received) = tokio::join!(send_display_unit(&mut sender, frame), delayed_receive);
    sent.expect("a subsecond transport stall should not end display mode");
    assert_eq!(received.unwrap(), Some(expected));
    println!("media transport: complete access unit survived a 600ms blocked write");
}

/// These tests run two resident engines over real loopback TLS. The platform
/// stalls or emits bursts in the same public interfaces as a slow GPU/mouse.
#[derive(Default)]
struct MediaProbe {
    realtime_mode: AtomicU32,
    source_start_delay_ms: AtomicU32,
    bitrate: AtomicU32,
    datagrams_sent: AtomicU32,
    keyframes: AtomicU32,
    udp_delay_ms: AtomicU32,
    generated: AtomicU32,
    presented: AtomicU32,
    active_sources: AtomicU32,
    pending_input: Mutex<VecDeque<InputEvent>>,
    injected: Mutex<Vec<InputEvent>>,
    presentation_delay_ms: AtomicU32,
    presentation_started: AtomicU32,
    payload_size: AtomicU32,
    ages_ms: Mutex<Vec<f64>>,
    consumed_tracks: Mutex<Vec<MediaTrack>>,
    started: Mutex<Option<Instant>>,
}

struct ProbeSource(Arc<MediaProbe>, bool);

impl DisplaySource for ProbeSource {
    fn set_bitrate(&mut self, value: u32) -> ferry_platform::Result<()> {
        self.0.bitrate.store(value, Ordering::SeqCst);
        Ok(())
    }
    fn request_keyframe(&mut self) -> ferry_platform::Result<()> {
        self.0.keyframes.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    fn next_frame(&mut self, wait: Duration) -> ferry_platform::Result<Option<MediaFrame>> {
        std::thread::sleep(wait.min(Duration::from_millis(2)));
        let stamp = self
            .0
            .started
            .lock()
            .unwrap()
            .unwrap()
            .elapsed()
            .as_micros() as u64;
        let sequence = self.0.generated.fetch_add(1, Ordering::SeqCst) + 1;
        let mut bytes = sequence.to_le_bytes().to_vec();
        bytes.resize(
            self.0.payload_size.load(Ordering::SeqCst).max(4) as usize,
            0,
        );
        if self.0.realtime_mode.load(Ordering::SeqCst) > 0 {
            bytes.extend_from_slice(&[0, 0, 1, 0x65, 1]);
        }
        Ok(Some(MediaFrame {
            track: if self.1 && sequence.is_multiple_of(3) {
                MediaTrack::Audio
            } else {
                MediaTrack::Video
            },
            timestamp_us: stamp,
            bytes,
        }))
    }

    fn input(&mut self, event: InputEvent) -> ferry_platform::Result<()> {
        self.0.injected.lock().unwrap().push(event);
        Ok(())
    }
}

impl Drop for ProbeSource {
    fn drop(&mut self) {
        self.0.active_sources.fetch_sub(1, Ordering::SeqCst);
    }
}

struct ProbeViewer {
    local: Arc<MediaProbe>,
    source: Arc<MediaProbe>,
}

impl DisplayViewer for ProbeViewer {
    fn present(&mut self, frame: MediaFrame) -> ferry_platform::Result<bool> {
        // H.264 P-frames depend on earlier frames. Backpressure must prevent
        // stale capture, without dropping already encoded reference frames.
        let sequence = u32::from_le_bytes(frame.bytes[..4].try_into().unwrap());
        if self.local.realtime_mode.load(Ordering::SeqCst) == 0 {
            assert_eq!(
                sequence as usize,
                self.local.consumed_tracks.lock().unwrap().len() + 1
            );
        }
        self.local
            .presentation_started
            .fetch_add(1, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(u64::from(
            self.local.presentation_delay_ms.load(Ordering::SeqCst),
        )));
        self.local.consumed_tracks.lock().unwrap().push(frame.track);
        if frame.track == MediaTrack::Video {
            let age = self.source.started.lock().unwrap().unwrap().elapsed()
                - Duration::from_micros(frame.timestamp_us);
            self.local
                .ages_ms
                .lock()
                .unwrap()
                .push(age.as_secs_f64() * 1000.0);
            self.local.presented.fetch_add(1, Ordering::SeqCst);
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn input(&mut self) -> ferry_platform::Result<Option<InputEvent>> {
        Ok(self.local.pending_input.lock().unwrap().pop_front())
    }
}

#[derive(Clone)]
struct ProbeBackend {
    local: Arc<MediaProbe>,
    remote: Arc<MediaProbe>,
}

impl DisplayBackend for ProbeBackend {
    fn datagram(
        &self,
        _audio: bool,
    ) -> ferry_platform::Result<Option<Box<dyn ferry_platform::DisplayDatagram>>> {
        if self.local.realtime_mode.load(Ordering::SeqCst) == 0 {
            return Ok(None);
        }
        Ok(Some(Box::new(realtime::ProbeDatagram::new(
            self.local.clone(),
        ))))
    }
    fn source(
        &self,
        _peer: [u8; 32],
        audio: bool,
    ) -> ferry_platform::Result<Box<dyn DisplaySource>> {
        std::thread::sleep(Duration::from_millis(u64::from(
            self.local.source_start_delay_ms.load(Ordering::SeqCst),
        )));
        *self.local.started.lock().unwrap() = Some(Instant::now());
        self.local.active_sources.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(ProbeSource(self.local.clone(), audio)))
    }

    fn viewer(&self, _audio: bool) -> ferry_platform::Result<Box<dyn DisplayViewer>> {
        Ok(Box::new(ProbeViewer {
            local: self.local.clone(),
            source: self.remote.clone(),
        }))
    }
}

#[derive(Clone)]
struct ProbeHost(ProbeBackend);

impl Host for ProbeHost {
    type Capture = MockCapture;
    type Inject = MockInject;
    type Displays = MockDisplays;
    type Clipboard = MockClipboard;
    type Overlay = MockOverlay;
    type Updater = MockUpdater;

    fn device_name(&self) -> String {
        "media-test".into()
    }
    fn check_permissions(&self, _role: Role) -> ferry_platform::Result<()> {
        Ok(())
    }
    fn capture(&self) -> ferry_platform::Result<Self::Capture> {
        Ok(MockCapture::default())
    }
    fn inject(&self) -> ferry_platform::Result<Self::Inject> {
        Ok(MockInject::default())
    }
    fn displays(&self) -> Self::Displays {
        MockDisplays::new(vec![Display {
            id: DisplayId(1),
            bounds: Rect::new(0, 0, 1920, 1080),
            scale_percent: 100,
            primary: true,
        }])
    }
    fn clipboard(&self) -> Self::Clipboard {
        MockClipboard::new()
    }
    fn overlay(&self) -> Self::Overlay {
        MockOverlay::new()
    }
    fn updater(&self) -> Self::Updater {
        MockUpdater::default()
    }
    fn update_source(&self) -> resident::UpdateSource {
        resident::UpdateSource {
            manifest_url: "http://127.0.0.1:9/update.json".into(),
            keys: Vec::new(),
        }
    }
    fn autostart(&self) -> bool {
        false
    }
    fn set_autostart(&self, _on: bool) -> ferry_platform::Result<()> {
        Ok(())
    }
    fn open_settings(&self) {}
    fn open_permission_settings(&self) {}
    fn listen_port(&self) -> u16 {
        0
    }
    fn invite_port(&self) -> u16 {
        0
    }
    fn display_backend(&self) -> Option<Arc<dyn DisplayBackend>> {
        Some(Arc::new(self.0.clone()))
    }
}

struct ProbeDaemon {
    _device: Device,
    handle: resident::Handle,
    task: tokio::task::JoinHandle<ferry_engine::Result<()>>,
}

impl ProbeDaemon {
    async fn start(device: Device, backend: ProbeBackend, config: Config) -> (Self, u16) {
        device.store.save_config(&config).unwrap();
        let (control, handle) = resident::control();
        let task = tokio::spawn(resident::run(
            device.store.clone(),
            ProbeHost(backend),
            control,
        ));
        let path = device.store.ipc_endpoint_path();
        let port = timeout(Duration::from_secs(5), async {
            let endpoint = loop {
                if let Ok(Some(endpoint)) = Endpoint::load(&path) {
                    break endpoint;
                }
                sleep(Duration::from_millis(10)).await;
            };
            let mut ui = ferry_ipc::connect(&endpoint).await.unwrap();
            loop {
                if let Some(Notice::State { state }) = ui.recv::<Notice>().await.unwrap()
                    && let Some(port) = state.local.port
                {
                    break port;
                }
            }
        })
        .await
        .expect("resident did not announce its input port");
        (
            Self {
                _device: device,
                handle,
                task,
            },
            port,
        )
    }

    async fn quit(self) {
        self.handle.send(Command::Quit);
        timeout(Duration::from_secs(5), self.task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
}

async fn paired_probes(
    controller: Arc<MediaProbe>,
    controlled: Arc<MediaProbe>,
) -> (ProbeDaemon, ProbeDaemon) {
    let (a, b, _) = paired_probes_with_relay(controller, controlled, false, false).await;
    (a, b)
}

async fn paired_probes_with_relay(
    controller: Arc<MediaProbe>,
    controlled: Arc<MediaProbe>,
    relay: bool,
    audio: bool,
) -> (ProbeDaemon, ProbeDaemon, Option<MediaRelay>) {
    let a = Device::new();
    let b = Device::new();
    let a_id = a.store.identity("media-controller").unwrap().fingerprint();
    let b_id = b.store.identity("media-controlled").unwrap().fingerprint();
    for (device, fingerprint) in [(&a, b_id), (&b, a_id)] {
        let mut peers = PeerList::new();
        peers.add(Peer {
            fingerprint,
            name: "paired".into(),
            addr: None,
        });
        device.store.save_peers(&peers).unwrap();
    }
    let (a, port) = ProbeDaemon::start(
        a,
        ProbeBackend {
            local: controller.clone(),
            remote: controlled.clone(),
        },
        Config {
            role: Role::Server,
            announce: false,
            display_audio: audio,
            ..Default::default()
        },
    )
    .await;
    let relay = if relay {
        Some(MediaRelay::start(port).await)
    } else {
        None
    };
    let port = relay.as_ref().map_or(port, |relay| relay.port);
    // The client's local listening port is intentionally absent, so start it
    // with the same endpoint-wait path but use the controller's port below.
    let config = Config {
        role: Role::Client,
        server: format!("127.0.0.1:{port}"),
        announce: false,
        display_audio: audio,
        ..Default::default()
    };
    b.store.save_config(&config).unwrap();
    let (control, handle) = resident::control();
    let task = tokio::spawn(resident::run(
        b.store.clone(),
        ProbeHost(ProbeBackend {
            local: controlled,
            remote: controller,
        }),
        control,
    ));
    let b = ProbeDaemon {
        _device: b,
        handle,
        task,
    };
    for mut connected in [a.handle.summary(), b.handle.summary()] {
        timeout(Duration::from_secs(5), async {
            while !connected.borrow().display_available {
                connected.changed().await.unwrap();
            }
        })
        .await
        .expect("paired media devices did not connect");
    }
    (a, b, relay)
}

/// The relay caps the source's advertised receive window and can stop only
/// source-to-viewer traffic. Input in the other direction remains routable.
struct MediaRelay {
    port: u16,
    blocked: Arc<AtomicBool>,
    held: Arc<AtomicU32>,
    task: tokio::task::JoinHandle<()>,
}

impl MediaRelay {
    async fn start(target_port: u16) -> Self {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        let blocked = Arc::new(AtomicBool::new(false));
        let held = Arc::new(AtomicU32::new(0));
        let pause = blocked.clone();
        let count = held.clone();
        let task = tokio::spawn(async move {
            let mut links = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    incoming = listener.accept() => {
                        let (client, _) = incoming.unwrap();
                        let pause = pause.clone();
                        let count = count.clone();
                        links.spawn(async move {
                            let socket = TcpSocket::new_v4().unwrap();
                            socket.set_recv_buffer_size(4096).unwrap();
                            let server = socket.connect((std::net::Ipv4Addr::LOCALHOST, target_port).into()).await.unwrap();
                            let (mut from_client, mut to_client) = client.into_split();
                            let (mut from_server, mut to_server) = server.into_split();
                            let input = tokio::io::copy(&mut from_client, &mut to_server);
                            let video = async {
                                let mut bytes = [0; 4096];
                                loop {
                                    let n = from_server.read(&mut bytes).await?;
                                    if n == 0 { return Ok::<(), std::io::Error>(()); }
                                    if pause.load(Ordering::SeqCst) {
                                        count.fetch_add(1, Ordering::SeqCst);
                                        while pause.load(Ordering::SeqCst) {
                                            sleep(Duration::from_millis(1)).await;
                                        }
                                    }
                                    to_client.write_all(&bytes[..n]).await?;
                                }
                            };
                            tokio::select! { _ = input => {}, _ = video => {} }
                        });
                    }
                    _ = links.join_next(), if !links.is_empty() => {}
                }
            }
        });
        Self {
            port,
            blocked,
            held,
            task,
        }
    }
}

impl Drop for MediaRelay {
    fn drop(&mut self) {
        self.blocked.store(false, Ordering::SeqCst);
        self.task.abort();
    }
}

async fn until(what: &str, mut ready: impl FnMut() -> bool) {
    timeout(Duration::from_secs(5), async {
        while !ready() {
            sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out: {what}"));
}

async fn stop_and_quit(a: ProbeDaemon, b: ProbeDaemon, source: &MediaProbe) {
    a.handle.send(Command::Display(DisplayAction::Stop));
    until("cancelled media source released its resources", || {
        source.active_sources.load(Ordering::SeqCst) == 0
    })
    .await;
    a.quit().await;
    b.quit().await;
}

#[tokio::test]
async fn a_mouse_burst_during_slow_video_preserves_clicks_and_the_display_session() {
    for reverse in [false, true] {
        let controller = Arc::new(MediaProbe::default());
        let controlled = Arc::new(MediaProbe::default());
        let (a, b) = paired_probes(controller.clone(), controlled.clone()).await;
        let (source, viewer) = if reverse {
            (&controlled, &controller)
        } else {
            (&controller, &controlled)
        };
        viewer.presentation_delay_ms.store(40, Ordering::SeqCst);
        a.handle.send(Command::Display(if reverse {
            DisplayAction::ExtendControlled
        } else {
            DisplayAction::ExtendController
        }));
        until("first slow frame", || {
            viewer.presented.load(Ordering::SeqCst) > 0
        })
        .await;
        let key_down = InputEvent::Key {
            code: KeyCode::keyboard(4),
            action: KeyAction::Down,
        };
        let key_up = InputEvent::Key {
            code: KeyCode::keyboard(4),
            action: KeyAction::Up,
        };
        let click_down = InputEvent::Button {
            button: MouseButton::LEFT,
            pressed: true,
            position: Point::new(4095, 100),
        };
        let click_up = InputEvent::Button {
            button: MouseButton::LEFT,
            pressed: false,
            position: Point::new(4095, 100),
        };
        {
            let mut input = viewer.pending_input.lock().unwrap();
            for x in 0..4096 {
                input.push_back(InputEvent::PointerMotion {
                    position: Point::new(x, 100),
                });
            }
            input.extend([click_down, click_up, key_down, key_up]);
        }
        until("motion and both key transitions", || {
            source.injected.lock().unwrap().contains(&key_up)
        })
        .await;
        let injected = source.injected.lock().unwrap().clone();
        let down_at = injected.iter().position(|e| *e == click_down).unwrap();
        assert_eq!(
            injected[down_at - 1],
            InputEvent::PointerMotion {
                position: Point::new(4095, 100),
            }
        );
        assert_eq!(injected[down_at + 1..], [click_up, key_down, key_up]);
        println!(
            "TLS loopback {}: 4096 motion burst yielded {} ordered injected events; final position, click and key transitions preserved",
            if reverse { "reverse" } else { "forward" },
            injected.len(),
        );
        let presented = viewer.presented.load(Ordering::SeqCst);
        until("stream remains live after input burst", || {
            viewer.presented.load(Ordering::SeqCst) >= presented + 3
        })
        .await;
        assert_eq!(source.active_sources.load(Ordering::SeqCst), 1);
        stop_and_quit(a, b, source.as_ref()).await;
    }
}

#[tokio::test]
async fn a_stalled_viewer_backpressures_capture_and_recovers_to_fresh_frames() {
    let source = Arc::new(MediaProbe::default());
    let viewer = Arc::new(MediaProbe::default());
    let (a, b) = paired_probes(source.clone(), viewer.clone()).await;
    a.handle
        .send(Command::Display(DisplayAction::ExtendController));
    until("initial video", || {
        viewer.presented.load(Ordering::SeqCst) >= 3
    })
    .await;

    let before = viewer.presentation_started.load(Ordering::SeqCst);
    viewer.presentation_delay_ms.store(600, Ordering::SeqCst);
    until("stalled presentation started", || {
        viewer.presentation_started.load(Ordering::SeqCst) > before
    })
    .await;
    sleep(Duration::from_millis(150)).await;
    let produced = source.generated.load(Ordering::SeqCst);
    let consumed = viewer.presented.load(Ordering::SeqCst);
    println!(
        "TLS loopback paused viewer: generated={produced}, presented={consumed}, backlog={}",
        produced.saturating_sub(consumed)
    );
    assert!(
        produced <= consumed + 2,
        "a blocked renderer accumulated {} pre-encoded frames",
        produced.saturating_sub(consumed)
    );
    viewer.presentation_delay_ms.store(0, Ordering::SeqCst);
    until("stalled presentation completed", || {
        viewer.presented.load(Ordering::SeqCst) > consumed
    })
    .await;
    until("fresh video after the stall", || {
        let ages = viewer.ages_ms.lock().unwrap();
        ages.len() >= (consumed + 5) as usize && ages.iter().rev().take(3).all(|ms| *ms < 150.0)
    })
    .await;
    println!(
        "TLS loopback recovered latest frame ages_ms={:?}",
        viewer
            .ages_ms
            .lock()
            .unwrap()
            .iter()
            .rev()
            .take(3)
            .collect::<Vec<_>>()
    );
    assert_eq!(source.active_sources.load(Ordering::SeqCst), 1);
    stop_and_quit(a, b, &source).await;
}

#[tokio::test]
async fn video_socket_backpressure_does_not_block_the_return_input_path() {
    let source = Arc::new(MediaProbe::default());
    let viewer = Arc::new(MediaProbe::default());
    let (a, b, relay) = paired_probes_with_relay(source.clone(), viewer.clone(), true, false).await;
    let relay = relay.unwrap();
    a.handle
        .send(Command::Display(DisplayAction::ExtendController));
    until("initial relay video", || {
        viewer.presented.load(Ordering::SeqCst) >= 3
    })
    .await;
    relay.blocked.store(true, Ordering::SeqCst);
    source
        .payload_size
        .store(ferry_proto::MAX_ACCESS_UNIT as u32, Ordering::SeqCst);
    until("relay holds a source write", || {
        relay.held.load(Ordering::SeqCst) > 0
    })
    .await;
    // The small TCP receive window and a full-size access unit force the source
    // TLS writer to await socket readiness. The reader must still accept input.
    sleep(Duration::from_millis(50)).await;
    let key = InputEvent::Key {
        code: KeyCode::keyboard(4),
        action: KeyAction::Up,
    };
    let sent = Instant::now();
    viewer.pending_input.lock().unwrap().push_back(key);
    until("return input while video writes are blocked", || {
        source.injected.lock().unwrap().contains(&key)
    })
    .await;
    assert!(relay.blocked.load(Ordering::SeqCst));
    println!(
        "TLS loopback blocked video socket: return input delivered in {:.2}ms",
        sent.elapsed().as_secs_f64() * 1000.0
    );
    source.payload_size.store(4, Ordering::SeqCst);
    relay.blocked.store(false, Ordering::SeqCst);
    let presented = viewer.presented.load(Ordering::SeqCst);
    until("video resumes after socket unblocks", || {
        viewer.presented.load(Ordering::SeqCst) >= presented + 3
    })
    .await;
    assert_eq!(source.active_sources.load(Ordering::SeqCst), 1);
    stop_and_quit(a, b, &source).await;
}

#[tokio::test]
async fn interleaved_audio_and_video_keep_wire_order_while_video_acknowledgements_advance() {
    let source = Arc::new(MediaProbe::default());
    let viewer = Arc::new(MediaProbe::default());
    viewer.presentation_delay_ms.store(8, Ordering::SeqCst);
    let (a, b, _) = paired_probes_with_relay(source.clone(), viewer.clone(), false, true).await;
    a.handle
        .send(Command::Display(DisplayAction::ExtendController));
    until("interleaved audio and video both consumed", || {
        viewer.presented.load(Ordering::SeqCst) >= 12
            && viewer.consumed_tracks.lock().unwrap().len() >= 18
    })
    .await;
    {
        let tracks = viewer.consumed_tracks.lock().unwrap();
        // The platform checks the sequence of every access unit. Keeping three
        // or more video credit windows moving also proves that interleaved audio
        // and the following video fences cannot deadlock each other.
        for (index, track) in tracks.iter().enumerate() {
            assert_eq!(
                *track,
                if (index + 1).is_multiple_of(3) {
                    MediaTrack::Audio
                } else {
                    MediaTrack::Video
                }
            );
        }
        println!(
            "TLS loopback interleaved media: {} video and {} audio units consumed in exact source order",
            tracks
                .iter()
                .filter(|track| **track == MediaTrack::Video)
                .count(),
            tracks
                .iter()
                .filter(|track| **track == MediaTrack::Audio)
                .count(),
        );
    }
    assert_eq!(source.active_sources.load(Ordering::SeqCst), 1);
    stop_and_quit(a, b, &source).await;
}

#[tokio::test]
async fn audio_follows_the_display_source_in_both_extension_directions() {
    for (action, source_is_controller) in [
        (DisplayAction::ExtendController, true),
        (DisplayAction::ExtendControlled, false),
    ] {
        let controller = Arc::new(MediaProbe::default());
        let controlled = Arc::new(MediaProbe::default());
        let (a, b, _) =
            paired_probes_with_relay(controller.clone(), controlled.clone(), false, true).await;
        let source = if source_is_controller {
            &controller
        } else {
            &controlled
        };
        let viewer = if source_is_controller {
            &controlled
        } else {
            &controller
        };

        a.handle.send(Command::Display(action));
        until("directional audio reaches the extension viewer", || {
            viewer
                .consumed_tracks
                .lock()
                .unwrap()
                .contains(&MediaTrack::Audio)
        })
        .await;

        assert!(source.generated.load(Ordering::SeqCst) > 0);
        assert!(
            viewer
                .consumed_tracks
                .lock()
                .unwrap()
                .contains(&MediaTrack::Video)
        );
        stop_and_quit(a, b, source.as_ref()).await;
    }
}
