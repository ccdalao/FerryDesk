//! 一次扩展屏会话的媒体任务。阻塞的 Windows 操作在媒体线程执行。

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use ferry_core::{DisplaySession, DisplaySessionKey};
use ferry_net::Framed;
use ferry_platform::{DisplayBackend, DisplayDatagram, MediaFrame, MediaTrack};
use ferry_proto::{DisplayAccessUnit, DisplayAssembler, DisplayTrack, Message};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::{interval, timeout};
use tracing::info;

use crate::{Error, Result, send_display_unit};

pub(super) struct ActiveDisplay {
    pub session: DisplaySession,
    pub task: Option<JoinHandle<Result<()>>>,
    pub first_frame: watch::Receiver<bool>,
    pub initiator: bool,
    pub requested: bool,
    pub ready: bool,
    pub options: ferry_types::DisplayOptions,
    pub plan: Option<u32>,
    cancel: watch::Sender<bool>,
}

impl ActiveDisplay {
    pub fn new(session: DisplaySession, initiator: bool) -> Self {
        Self {
            session,
            task: None,
            first_frame: watch::channel(false).1,
            initiator,
            requested: false,
            ready: false,
            options: Default::default(),
            plan: None,
            cancel: watch::Sender::new(false),
        }
    }

    pub fn token(&self) -> watch::Receiver<bool> {
        self.cancel.subscribe()
    }
}

impl Drop for ActiveDisplay {
    fn drop(&mut self) {
        self.cancel.send_replace(true);
        // Dropping a JoinHandle detaches it. Cancellation joins the blocking worker
        // so its window and virtual monitor are destroyed on their owner thread.
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn run_media<S>(
    stream: Framed<S>,
    session: DisplaySession,
    peer_addr: SocketAddr,
    udp: bool,
    backend: Arc<dyn DisplayBackend>,
    cancel: watch::Receiver<bool>,
    first_frame: watch::Sender<bool>,
    options: ferry_types::DisplayOptions,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    if session.local_source {
        run_source(
            stream,
            session.key,
            session.audio,
            peer_addr,
            udp,
            backend,
            cancel,
            options,
        )
        .await
    } else {
        run_viewer(
            stream,
            session.audio,
            session.key.display_session,
            peer_addr,
            udp,
            backend,
            cancel,
            first_frame,
            options,
        )
        .await
    }
}

async fn media_message<S: AsyncRead + Unpin>(stream: &mut Framed<S>) -> Result<Message> {
    timeout(crate::display_stream::MEDIA_TIMEOUT, stream.recv())
        .await
        .map_err(|_| Error::Timeout("扩展屏实时通道协商"))??
        .ok_or(Error::Protocol("扩展屏媒体连接已关闭"))
}

async fn probe_datagram(
    datagram: Option<Box<dyn DisplayDatagram>>,
    source: bool,
) -> Result<Option<Box<dyn DisplayDatagram>>> {
    let Some(mut datagram) = datagram else {
        return Ok(None);
    };
    tokio::task::spawn_blocking(move || {
        if datagram.probe(source, Duration::from_millis(400))? {
            Ok(Some(datagram))
        } else {
            Ok(None)
        }
    })
    .await
    .map_err(worker_error)?
}

async fn negotiate_source<S>(
    stream: &mut Framed<S>,
    id: u64,
    peer_addr: SocketAddr,
    enabled: bool,
    backend: &Arc<dyn DisplayBackend>,
    audio: bool,
) -> Result<Option<Box<dyn DisplayDatagram>>>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    if !enabled {
        return Ok(None);
    }
    let key = ring::rand::generate::<[u8; 32]>(&ring::rand::SystemRandom::new())
        .map_err(|_| Error::Protocol("生成媒体密钥失败"))?
        .expose();
    let mut datagram = backend.datagram(audio).ok().flatten();
    if let Some(value) = datagram.as_mut()
        && value.set_key(key).is_err()
    {
        datagram = None;
    }
    let port = datagram.as_ref().map_or(0, |value| value.local_port());
    send_message(stream, Message::DisplayRealtimeOffer { id, port, key }).await?;
    let Message::DisplayRealtimeReady {
        id: peer_id,
        port: peer_port,
    } = media_message(stream).await?
    else {
        return Err(Error::Protocol("实时媒体端口响应无效"));
    };
    if peer_id != id {
        return Err(Error::Protocol("实时媒体会话不匹配"));
    }
    if port == 0 || peer_port == 0 {
        datagram = None;
    }
    if let Some(value) = datagram.as_mut()
        && value
            .set_peer(SocketAddr::new(peer_addr.ip(), peer_port), 0)
            .is_err()
    {
        datagram = None;
    }
    let datagram = probe_datagram(datagram, true).await?;
    let Message::DisplayRealtimeReady {
        id: peer_id,
        port: verified_port,
    } = media_message(stream).await?
    else {
        return Err(Error::Protocol("实时媒体探测响应无效"));
    };
    if peer_id != id {
        return Err(Error::Protocol("实时媒体探测会话不匹配"));
    }
    let udp = datagram.is_some() && verified_port != 0;
    send_message(stream, Message::DisplayRealtimeCommit { id, udp }).await?;
    info!(udp, peer = %peer_addr, "扩展屏实时媒体协商完成（Apollo RTP / Moonlight FEC / AES-GCM）");
    Ok(if udp { datagram } else { None })
}

async fn negotiate_viewer<S>(
    stream: &mut Framed<S>,
    id: u64,
    peer_addr: SocketAddr,
    enabled: bool,
    backend: &Arc<dyn DisplayBackend>,
    audio: bool,
) -> Result<Option<Box<dyn DisplayDatagram>>>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    if !enabled {
        return Ok(None);
    }
    let Message::DisplayRealtimeOffer {
        id: peer_id,
        port,
        key,
    } = media_message(stream).await?
    else {
        return Err(Error::Protocol("实时媒体端口请求无效"));
    };
    if peer_id != id {
        return Err(Error::Protocol("实时媒体会话不匹配"));
    }
    let mut datagram = if port == 0 {
        None
    } else {
        backend.datagram(audio).ok().flatten()
    };
    if let Some(value) = datagram.as_mut()
        && (value.set_key(key).is_err()
            || value
                .set_peer(SocketAddr::new(peer_addr.ip(), port), 0)
                .is_err())
    {
        datagram = None;
    }
    let local_port = datagram.as_ref().map_or(0, |value| value.local_port());
    send_message(
        stream,
        Message::DisplayRealtimeReady {
            id,
            port: local_port,
        },
    )
    .await?;
    let datagram = probe_datagram(datagram, false).await?;
    let verified_port = datagram.as_ref().map_or(0, |value| value.local_port());
    send_message(
        stream,
        Message::DisplayRealtimeReady {
            id,
            port: verified_port,
        },
    )
    .await?;
    let Message::DisplayRealtimeCommit { id: peer_id, udp } = media_message(stream).await? else {
        return Err(Error::Protocol("实时媒体提交无效"));
    };
    if peer_id != id || (udp && datagram.is_none()) {
        return Err(Error::Protocol("实时媒体提交状态无效"));
    }
    info!(udp, peer = %peer_addr, "扩展屏实时接收端已就绪");
    Ok(if udp { datagram } else { None })
}

// Two frames cover encode/transfer overlap without a long queue of stale P-frames.
const MAX_IN_FLIGHT: usize = 2;
const INPUT_CAPACITY: usize = 256;
const INPUT_POLL: Duration = Duration::from_millis(4);

type Inputs = Arc<std::sync::Mutex<ferry_core::InputQueue>>;

fn input_queue() -> Inputs {
    Arc::new(std::sync::Mutex::new(ferry_core::InputQueue::new(
        INPUT_CAPACITY,
    )))
}

fn enqueue_input(inputs: &Inputs, event: ferry_types::InputEvent) -> Result<()> {
    if inputs.lock().expect("input queue poisoned").push(event) {
        Ok(())
    } else {
        // Keys/buttons are never silently dropped. Continuous mouse motion is
        // coalesced, so only an abnormal flood of ordered events reaches this.
        Err(Error::Protocol("扩展屏按键或按钮事件积压"))
    }
}

async fn send_message<S: AsyncWrite + Unpin>(
    stream: &mut Framed<S>,
    message: Message,
) -> Result<()> {
    timeout(crate::display_stream::MEDIA_TIMEOUT, stream.send(&message))
        .await
        .map_err(|_| Error::Timeout("发送扩展屏控制或输入"))??;
    Ok(())
}

#[derive(Default)]
struct MediaControl {
    bitrate: Option<u32>,
    keyframe: bool,
}

/// Apollo separates network/FEC pacing from capture and input. Keep one queued
/// access unit so a blocked send never stalls input injection or ages a long
/// sequence of already encoded reference frames.
struct UdpSender {
    frames: mpsc::Sender<MediaFrame>,
    bitrate: Arc<std::sync::atomic::AtomicU32>,
    stop: Arc<std::sync::atomic::AtomicBool>,
    task: Option<std::thread::JoinHandle<()>>,
}

impl UdpSender {
    fn new(
        mut datagram: Box<dyn DisplayDatagram>,
        fallback: Arc<std::sync::atomic::AtomicBool>,
        notify: Arc<tokio::sync::Notify>,
    ) -> Result<Self> {
        use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
        let (frames, mut pending) = mpsc::channel::<MediaFrame>(1);
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let bitrate = Arc::new(AtomicU32::new(37_000_000));
        let rate = bitrate.clone();
        let task = std::thread::Builder::new()
            .name("ferry-media-send".into())
            .spawn(move || {
                let mut report_at = std::time::Instant::now();
                while !stopped.load(Ordering::Acquire) {
                    match pending.try_recv() {
                        Ok(frame) => {
                            datagram.pacing(rate.load(Ordering::Relaxed));
                            if let Err(error) = datagram.send(frame) {
                                tracing::warn!(%error, "UDP 发送失效，请求同步回退 TCP");
                                fallback.store(true, Ordering::Release);
                                notify.notify_one();
                                break;
                            }
                        }
                        Err(mpsc::error::TryRecvError::Disconnected) => break,
                        Err(mpsc::error::TryRecvError::Empty) => {
                            std::thread::sleep(Duration::from_millis(1))
                        }
                    }
                    if report_at.elapsed() >= Duration::from_secs(5) {
                        let stats = datagram.stats();
                        info!(?stats, "扩展屏独立 UDP/FEC 发送统计");
                        report_at = std::time::Instant::now();
                    }
                }
            })?;
        Ok(Self {
            frames,
            bitrate,
            stop,
            task: Some(task),
        })
    }
}

impl Drop for UdpSender {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Release);
        if let Some(task) = self.task.take()
            && let Err(error) = task.join()
        {
            tracing::warn!(?error, "UDP 发送工作线程异常退出");
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_source<S: AsyncRead + AsyncWrite + Unpin>(
    stream: Framed<S>,
    key: DisplaySessionKey,
    audio: bool,
    peer_addr: SocketAddr,
    udp_enabled: bool,
    backend: Arc<dyn DisplayBackend>,
    mut cancel: watch::Receiver<bool>,
    options: ferry_types::DisplayOptions,
) -> Result<()> {
    use std::sync::atomic::{AtomicBool, Ordering};
    let mut stream = stream;
    let datagram = negotiate_source(
        &mut stream,
        key.display_session,
        peer_addr,
        udp_enabled,
        &backend,
        audio,
    )
    .await?;
    let using_udp = Arc::new(AtomicBool::new(datagram.is_some()));
    if datagram.is_none() {
        stream.set_network_class(ferry_platform::NetworkClass::Video);
    }
    let worker_udp = using_udp.clone();
    let fallback = Arc::new(AtomicBool::new(false));
    let worker_fallback = fallback.clone();
    let transport_notify = Arc::new(tokio::sync::Notify::new());
    let worker_notify = transport_notify.clone();
    let controls = Arc::new(std::sync::Mutex::new(MediaControl::default()));
    let worker_controls = controls.clone();
    let worker_cancel = cancel.clone();
    let stop = WorkerStop::new();
    let stopped = stop.0.clone();
    let (frames, mut video) = mpsc::channel(1);
    let (requests, mut requested) = mpsc::channel(1);
    let inputs = input_queue();
    let worker_inputs = inputs.clone();
    let mut worker = tokio::task::spawn_blocking(move || -> Result<()> {
        let mut source = backend.source_with_options(key.fingerprint, audio, options)?;
        if let Err(error) = source.set_bitrate(ferry_core::MediaBitrate::default().current()) {
            tracing::warn!(%error, "编码器暂不支持动态码率，保留发送节奏限制");
        }
        let mut datagram = datagram
            .map(|value| UdpSender::new(value, worker_fallback.clone(), worker_notify.clone()))
            .transpose()?;
        info!(backend = %source.description(), udp = worker_udp.load(Ordering::Acquire), "扩展屏发送媒体路径");
        let mut collecting = false;
        let mut report_at = std::time::Instant::now();
        let mut last_idr = std::time::Instant::now() - Duration::from_secs(1);
        let mut produced = 0u64;
        while !*worker_cancel.borrow() && !stopped.load(Ordering::Acquire) && !frames.is_closed() {
            for _ in 0..64 {
                let event = worker_inputs.lock().expect("input queue poisoned").pop();
                match event {
                    Some(event) => source.input(event)?,
                    None => break,
                }
            }
            let control =
                std::mem::take(&mut *worker_controls.lock().expect("media control poisoned"));
            if control.keyframe && last_idr.elapsed() >= Duration::from_millis(100) {
                source.request_keyframe()?;
                last_idr = std::time::Instant::now();
            }
            if let Some(bitrate) = control.bitrate {
                match source.set_bitrate(bitrate) {
                    Ok(()) => info!(bitrate, "热点反馈已调整编码码率"),
                    Err(error) => {
                        tracing::warn!(%error, bitrate, "编码器拒绝动态码率，继续原码率并限制发送节奏")
                    }
                }
                if let Some(value) = datagram.as_mut() {
                    value
                        .bitrate
                        .store(bitrate.saturating_mul(3) / 2 + 1_000_000, Ordering::Relaxed);
                }
            }
            if !worker_udp.load(Ordering::Acquire) && datagram.take().is_some() {
                source.request_keyframe()?;
                collecting = false;
                info!("实时媒体回退 TCP，下一帧请求 IDR");
            }
            if worker_fallback.load(Ordering::Acquire) && worker_udp.load(Ordering::Acquire) {
                std::thread::sleep(Duration::from_millis(1));
                continue;
            }
            if report_at.elapsed() >= Duration::from_secs(5) {
                info!(backend = %source.description(), fps = produced as f64 / report_at.elapsed().as_secs_f64(), "扩展屏采集/编码统计");
                report_at = std::time::Instant::now();
                produced = 0;
            }
            if datagram
                .as_ref()
                .is_some_and(|value| value.frames.capacity() == 0)
            {
                std::thread::sleep(Duration::from_millis(1));
                continue;
            }
            if !worker_udp.load(Ordering::Acquire) && !collecting {
                match requested.try_recv() {
                    Ok(()) => collecting = true,
                    Err(mpsc::error::TryRecvError::Disconnected) => break,
                    Err(mpsc::error::TryRecvError::Empty) => {
                        std::thread::sleep(Duration::from_millis(1));
                        continue;
                    }
                }
            }
            if let Some(frame) = source.next_frame(INPUT_POLL)? {
                if frame.track == MediaTrack::Video {
                    produced += 1;
                }
                if let Some(value) = datagram.as_mut() {
                    if value.frames.try_send(frame).is_err() {
                        tracing::warn!("UDP 发送工作线程已停止，请求同步回退 TCP");
                        worker_fallback.store(true, Ordering::Release);
                        worker_notify.notify_one();
                    }
                } else if frames.try_send(frame).is_err() {
                    break;
                }
                collecting = false;
            } else {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        Ok(())
    });
    let (mut reader, mut writer) = stream.split();
    let (acks, mut feedback) = mpsc::channel(32);
    let rate = std::sync::Mutex::new(ferry_core::MediaBitrate::default());
    let read = async {
        loop {
            let message = timeout(crate::display_stream::MEDIA_TIMEOUT, reader.recv())
                .await
                .map_err(|_| Error::Timeout("扩展屏接收端响应"))??;
            match message {
                Some(Message::Pong { nonce }) => {
                    if acks.send(nonce).await.is_err() {
                        return Ok(());
                    }
                }
                Some(Message::DisplayInput(event)) => enqueue_input(&inputs, event)?,
                Some(Message::DisplayRealtimeFeedback {
                    frames,
                    lost,
                    recovered,
                    queue_ms,
                    keyframe,
                }) if udp_enabled => {
                    let adjusted = rate
                        .lock()
                        .expect("media bitrate poisoned")
                        .observe(frames, lost, recovered, queue_ms);
                    let mut control = controls.lock().expect("media control poisoned");
                    control.bitrate = adjusted.or(control.bitrate);
                    control.keyframe |= keyframe;
                }
                Some(Message::DisplayRealtimeFallback) if using_udp.load(Ordering::Acquire) => {
                    fallback.store(true, Ordering::Release);
                    transport_notify.notify_one();
                }
                Some(Message::Bye(_)) | None => return Ok(()),
                _ => return Err(Error::Protocol("扩展屏接收端发送了意外的媒体消息")),
            }
        }
    };
    let write = async {
        let mut ping = interval(Duration::from_millis(100));
        ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut nonce = 0u64;
        let mut requested = false;
        let mut credits: std::collections::VecDeque<(u64, std::time::Instant)> = Default::default();
        let mut probes: std::collections::VecDeque<(u64, std::time::Instant)> = Default::default();
        let mut baseline = Duration::from_millis(100);
        loop {
            if !using_udp.load(Ordering::Acquire) && !requested && credits.len() < MAX_IN_FLIGHT {
                if requests.send(()).await.is_err() {
                    return Ok(());
                }
                requested = true;
            }
            tokio::select! {
                _ = transport_notify.notified() => {
                    if using_udp.load(Ordering::Acquire) && fallback.load(Ordering::Acquire) {
                        // Send the boundary before allowing the worker to produce TCP bytes.
                        send_message(&mut writer, Message::DisplayRealtimeCommit { id: key.display_session, udp: false }).await?;
                        writer.set_network_class(ferry_platform::NetworkClass::Video);
                        using_udp.store(false, Ordering::Release);
                    }
                }
                frame = video.recv() => {
                    let Some(frame) = frame else { return Ok(()); };
                    requested = false;
                    let is_video = frame.track == MediaTrack::Video;
                    let sent = std::time::Instant::now();
                    send_display_unit(&mut writer, from_platform(frame)).await?;
                    if is_video {
                        nonce = nonce.wrapping_add(1);
                        send_message(&mut writer, Message::Ping { nonce }).await?;
                        credits.push_back((nonce, sent));
                    }
                }
                Some(nonce) = feedback.recv() => {
                    if let Some(at) = credits.iter().position(|&(id, _)| id == nonce) { credits.remove(at); }
                    if let Some(at) = probes.iter().position(|&(id, _)| id == nonce) {
                        let (_, sent) = probes.remove(at).expect("probe exists");
                        let rtt = sent.elapsed();
                        baseline = baseline.min(rtt);
                        if using_udp.load(Ordering::Acquire) {
                            let adjusted = rate.lock().expect("media bitrate poisoned").observe_delay(rtt.as_millis().min(u128::from(u32::MAX)) as u32, baseline.as_millis().min(u128::from(u32::MAX)) as u32);
                            if let Some(bitrate) = adjusted {
                                controls.lock().expect("media control poisoned").bitrate = Some(bitrate);
                                tracing::info!(rtt_ms = rtt.as_secs_f64()*1000.0, bitrate, "媒体响应突增，优先降低画质保护输入响应");
                            }
                        }
                    }
                }
                _ = ping.tick() => {
                    if credits.front().is_some_and(|(_,at)| at.elapsed() >= crate::display_stream::MEDIA_TIMEOUT) {
                        return Err(Error::Timeout("扩展屏解码消费确认"));
                    }
                    nonce = nonce.wrapping_add(1);
                    send_message(&mut writer, Message::Ping { nonce }).await?;
                    if probes.len() >= 32 { probes.pop_front(); }
                    probes.push_back((nonce, std::time::Instant::now()));
                }
            }
        }
    };
    let mut joined = false;
    let result = tokio::select! {
        result = &mut worker => { joined = true; result.map_err(worker_error)? },
        _ = cancel.changed() => Ok(()), result = read => result, result = write => result,
    };
    stop.stop();
    drop(video);
    drop(requests);
    if !joined {
        worker.await.map_err(worker_error)??;
    }
    result
}

enum ViewerWork {
    Frame(std::time::Instant, MediaFrame),
    // A fence follows its compressed frame on the wire and on the worker queue.
    // ACK after the decoder consumes the unit, not after enqueue. This also
    // covers present(false): a buffered decoder may need another input frame.
    Fence(u64),
}

#[allow(clippy::too_many_arguments)]
async fn run_viewer<S: AsyncRead + AsyncWrite + Unpin>(
    stream: Framed<S>,
    audio: bool,
    id: u64,
    peer_addr: SocketAddr,
    udp_enabled: bool,
    backend: Arc<dyn DisplayBackend>,
    mut cancel: watch::Receiver<bool>,
    first_frame: watch::Sender<bool>,
    options: ferry_types::DisplayOptions,
) -> Result<()> {
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    let mut stream = stream;
    let datagram =
        negotiate_viewer(&mut stream, id, peer_addr, udp_enabled, &backend, audio).await?;
    let using_udp = Arc::new(AtomicBool::new(datagram.is_some()));
    let read_udp = using_udp.clone();
    let recv_udp = using_udp.clone();
    let worker_cancel = cancel.clone();
    let stop = WorkerStop::new();
    let stopped = stop.0.clone();
    let receiver_stopped = stopped.clone();
    let queue_age = Arc::new(AtomicU32::new(0));
    let worker_age = queue_age.clone();
    let udp_queue = Arc::new(std::sync::Mutex::new(std::collections::VecDeque::<
        ViewerWork,
    >::new()));
    let worker_queue = udp_queue.clone();
    let fallback_queue = udp_queue.clone();
    let (reports, mut report_rx) = mpsc::channel::<Message>(8);
    let (tx, mut frames) = mpsc::channel::<ViewerWork>(1);
    let (acks, mut acknowledged) = mpsc::channel(32);
    let immediate_acks = acks.clone();
    let inputs = input_queue();
    let worker_inputs = inputs.clone();
    let notify = Arc::new(tokio::sync::Notify::new());
    let worker_notify = notify.clone();
    let mut worker = tokio::task::spawn_blocking(move || -> Result<()> {
        let mut viewer = backend.viewer_with_options(audio, options)?;
        info!(backend = %viewer.description(), "扩展屏接收媒体路径");
        let mut report_at = std::time::Instant::now();
        let mut count = 0u64;
        let mut render_samples = Vec::new();
        let mut queue_samples = Vec::new();
        while !*worker_cancel.borrow() && !stopped.load(Ordering::Acquire) {
            // 先处理本轮本机输入，不能排在解码/呈现之后。
            for _ in 0..128 {
                let Some(event) = viewer.input()? else {
                    break;
                };
                enqueue_input(&worker_inputs, event)?;
                worker_notify.notify_one();
            }
            let udp_work = worker_queue
                .lock()
                .expect("media queue poisoned")
                .pop_front();
            let work = udp_work.or_else(|| frames.try_recv().ok());
            match work {
                Some(ViewerWork::Frame(queued, frame)) => {
                    let start = std::time::Instant::now();
                    let age = queued.elapsed().as_secs_f64() * 1000.0;
                    worker_age.fetch_max(age.min(u32::MAX as f64) as u32, Ordering::Relaxed);
                    if viewer.present(frame)? {
                        if !*first_frame.borrow() {
                            first_frame.send_replace(true);
                        }
                        count += 1;
                        render_samples.push(start.elapsed().as_secs_f64() * 1000.0);
                        queue_samples.push(age);
                    }
                }
                Some(ViewerWork::Fence(nonce)) => {
                    if acks.try_send(nonce).is_err() {
                        return Err(Error::Protocol("扩展屏消费确认积压"));
                    }
                }
                None => std::thread::sleep(Duration::from_millis(1)),
            }
            if report_at.elapsed() >= Duration::from_secs(5) {
                info!(fps = count as f64 / report_at.elapsed().as_secs_f64(),
                    queue_p95_ms = percentile95(&mut queue_samples),
                    decode_present_p95_ms = percentile95(&mut render_samples),
                    backend = %viewer.description(), "扩展屏解码/呈现提交统计");
                count = 0;
                report_at = std::time::Instant::now();
                render_samples.clear();
                queue_samples.clear();
            }
            for _ in 0..128 {
                let Some(event) = viewer.input()? else {
                    break;
                };
                enqueue_input(&worker_inputs, event)?;
                worker_notify.notify_one();
            }
        }
        Ok(())
    });
    let mut udp_receiver = datagram.map(|mut datagram| {
        tokio::task::spawn_blocking(move || -> Result<()> {
            let mut recovery = ferry_core::MediaRecovery::default();
            let mut last_stats = datagram.stats();
            let mut last_lost = last_stats.lost;
            let mut last_frame = std::time::Instant::now();
            let mut received_frame = false;
            let mut report_at = std::time::Instant::now();
            let mut local_lost = 0u32;
            let mut keyframe_requested_at = std::time::Instant::now() - Duration::from_secs(1);
            while !receiver_stopped.load(Ordering::Acquire) && recv_udp.load(Ordering::Acquire) {
                let frame = match datagram.receive(Duration::from_millis(5)) {
                    Ok(frame) => frame,
                    Err(error) => {
                        tracing::warn!(%error, "UDP 接收失效，申请 TCP 回退");
                        let _ = reports.try_send(Message::DisplayRealtimeFallback);
                        break;
                    }
                };
                let stats = datagram.stats();
                if !recv_udp.load(Ordering::Acquire) {
                    break;
                }
                if stats.lost != last_lost {
                    recovery.lost();
                    last_lost = stats.lost;
                    udp_queue.lock().expect("media queue poisoned").clear();
                }
                if let Some(frame) = frame {
                    received_frame = true;
                    last_frame = std::time::Instant::now();
                    if frame.track == MediaTrack::Audio && !audio {
                        return Err(Error::Protocol("未协商扩展屏声音"));
                    }
                    let mut queue = udp_queue.lock().expect("media queue poisoned");
                    if queue.len() >= 2 {
                        queue.clear();
                        recovery.lost();
                        local_lost = local_lost.saturating_add(1);
                    }
                    if frame.track == MediaTrack::Audio || recovery.accept(&frame.bytes) {
                        queue.push_back(ViewerWork::Frame(std::time::Instant::now(), frame));
                    }
                }
                // Match Moonlight's immediate loss callback: recovery must not
                // wait for the slower bitrate/statistics reporting interval.
                if recovery.waiting()
                    && keyframe_requested_at.elapsed() >= Duration::from_millis(100)
                {
                    let _ = reports.try_send(Message::DisplayRealtimeFeedback {
                        frames: 0,
                        lost: 0,
                        recovered: 0,
                        queue_ms: 0,
                        keyframe: true,
                    });
                    keyframe_requested_at = std::time::Instant::now();
                }
                // 虚拟屏与编码器初始化可能超过 1.5 秒。首帧之前使用启动时限，
                // 避免正常启动被误判成网络失败；运行后才用短时限。
                if last_frame.elapsed() >= Duration::from_secs(if received_frame { 3 } else { 10 })
                {
                    tracing::warn!(received_frame, "UDP 媒体无完整帧超过时限，申请 TCP 回退");
                    let _ = reports.try_send(Message::DisplayRealtimeFallback);
                    break;
                }
                if report_at.elapsed() >= Duration::from_millis(100) {
                    let delta = |now: u64, before: u64| {
                        now.saturating_sub(before).min(u32::MAX as u64) as u32
                    };
                    let feedback = Message::DisplayRealtimeFeedback {
                        frames: delta(stats.frames, last_stats.frames),
                        lost: delta(stats.lost, last_stats.lost).saturating_add(local_lost),
                        recovered: delta(stats.recovered, last_stats.recovered),
                        queue_ms: queue_age.swap(0, Ordering::Relaxed).min(u16::MAX as u32) as u16,
                        keyframe: recovery.waiting(),
                    };
                    let _ = reports.try_send(feedback);
                    tracing::debug!(
                        ?stats,
                        waiting_idr = recovery.waiting(),
                        "Moonlight RTP/FEC 统计"
                    );
                    last_stats = stats;
                    local_lost = 0;
                    report_at = std::time::Instant::now();
                }
            }
            Ok(())
        })
    });
    let (mut reader, mut writer) = stream.split();
    let read = async {
        let mut assembler = DisplayAssembler::default();
        let mut assembling = false;
        let mut deadline = tokio::time::Instant::now() + crate::display_stream::MEDIA_TIMEOUT;
        loop {
            let message = tokio::time::timeout_at(deadline, reader.recv())
                .await
                .map_err(|_| Error::Timeout("扩展屏媒体读取"))??;
            let work = match message {
                Some(Message::DisplayRealtimeCommit {
                    id: peer_id,
                    udp: false,
                }) if peer_id == id && read_udp.load(Ordering::Acquire) => {
                    read_udp.store(false, Ordering::Release);
                    fallback_queue.lock().expect("media queue poisoned").clear();
                    deadline = tokio::time::Instant::now() + crate::display_stream::MEDIA_TIMEOUT;
                    info!("UDP 媒体已同步回退 TCP");
                    continue;
                }
                Some(Message::DisplayPacket(_)) if read_udp.load(Ordering::Acquire) => {
                    return Err(Error::Protocol("UDP 会话收到未声明的 TCP 媒体"));
                }
                Some(Message::DisplayPacket(packet)) => {
                    let unit = assembler.push(packet).map_err(ferry_net::Error::from)?;
                    assembling = unit.is_none();
                    let Some(unit) = unit else {
                        continue;
                    };
                    if unit.track == DisplayTrack::Audio && !audio {
                        return Err(Error::Protocol("未协商扩展屏声音"));
                    }
                    ViewerWork::Frame(std::time::Instant::now(), to_platform(unit))
                }
                Some(Message::Ping { nonce })
                    if !assembling && read_udp.load(Ordering::Acquire) =>
                {
                    immediate_acks
                        .send(nonce)
                        .await
                        .map_err(|_| Error::Protocol("扩展屏即时回应线程结束"))?;
                    deadline = tokio::time::Instant::now() + crate::display_stream::MEDIA_TIMEOUT;
                    continue;
                }
                Some(Message::Ping { nonce }) if !assembling => ViewerWork::Fence(nonce),
                Some(Message::Bye(_)) | None if !assembling => return Ok(()),
                _ => return Err(Error::Protocol("意外的扩展屏媒体消息")),
            };
            timeout(crate::display_stream::MEDIA_TIMEOUT, tx.send(work))
                .await
                .map_err(|_| Error::Timeout("扩展屏解码消费"))?
                .map_err(|_| Error::Protocol("扩展屏呈现线程结束"))?;
            if !assembling {
                deadline = tokio::time::Instant::now() + crate::display_stream::MEDIA_TIMEOUT;
            }
        }
    };
    let write = async {
        loop {
            tokio::select! {
                Some(message) = report_rx.recv() => send_message(&mut writer, message).await?,
                Some(nonce) = acknowledged.recv() => send_message(&mut writer, Message::Pong { nonce }).await?,
                _ = notify.notified() => {
                    let mut full_batch = true;
                    for _ in 0..64 {
                        let event = inputs.lock().expect("input queue poisoned").pop();
                        let Some(event) = event else { full_batch=false; break; };
                        send_message(&mut writer, Message::DisplayInput(event)).await?;
                    }
                    if full_batch { notify.notify_one(); tokio::task::yield_now().await; }
                }
            }
        }
    };
    let mut joined = false;
    let result = tokio::select! {
        result = &mut worker => { joined=true; result.map_err(worker_error)? },
        _ = cancel.changed() => Ok(()), result = read => result, result = write => result,
    };
    stop.stop();
    drop(tx);
    if let Some(receiver) = udp_receiver.take() {
        receiver.await.map_err(worker_error)??;
    }
    if !joined {
        worker.await.map_err(worker_error)??;
    }
    result
}

/// Also stops the blocking worker if the enclosing async task is dropped.
struct WorkerStop(Arc<std::sync::atomic::AtomicBool>);

impl WorkerStop {
    fn new() -> Self {
        Self(Arc::new(std::sync::atomic::AtomicBool::new(false)))
    }
    fn stop(&self) {
        self.0.store(true, std::sync::atomic::Ordering::Release);
    }
}

impl Drop for WorkerStop {
    fn drop(&mut self) {
        self.stop();
    }
}

fn percentile95(samples: &mut [f64]) -> f64 {
    samples.sort_by(f64::total_cmp);
    samples
        .get(samples.len().saturating_sub(1) * 95 / 100)
        .copied()
        .unwrap_or(0.0)
}

fn worker_error(error: tokio::task::JoinError) -> Error {
    Error::Io(std::io::Error::other(error))
}

fn from_platform(frame: MediaFrame) -> DisplayAccessUnit {
    DisplayAccessUnit {
        track: match frame.track {
            MediaTrack::Video => DisplayTrack::Video,
            MediaTrack::Audio => DisplayTrack::Audio,
        },
        timestamp_us: frame.timestamp_us,
        bytes: frame.bytes,
    }
}

fn to_platform(frame: DisplayAccessUnit) -> MediaFrame {
    MediaFrame {
        track: match frame.track {
            DisplayTrack::Video => MediaTrack::Video,
            DisplayTrack::Audio => MediaTrack::Audio,
        },
        timestamp_us: frame.timestamp_us,
        bytes: frame.bytes,
    }
}
