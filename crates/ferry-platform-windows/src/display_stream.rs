//! Windows 扩展屏的捕获、编解码和接收窗口。

mod alignment;
mod apollo;
mod audio;
mod codec;
mod cursor;
mod desktop;
mod driver;
mod encoder;
mod gpu;
pub(crate) mod surface;
mod transport;

use crate::WinInject;
use ferry_core::Pressed;
use ferry_platform::InputInject;
use ferry_types::{InputEvent, Point};
use std::collections::VecDeque;
use std::time::{Duration, Instant};

use ferry_platform::{
    DisplayBackend, DisplayDatagram, DisplaySource, DisplayViewer, Error, MediaFrame, MediaTrack,
    Result,
};

use audio::{Loopback, Playback};
use codec::{Codec, MediaFoundation};
use encoder::Encoder;
use surface::{Capture, Viewer, VirtualMonitor, normal_desktop};

const WIDTH: usize = 1920;
const HEIGHT: usize = 1080;
const FPS: u32 = 100;

pub use driver::{install_display_driver, remove_display_driver};

pub struct WinDisplayBackend;

impl WinDisplayBackend {
    /// 在安装/诊断阶段验证系统编解码可用，无需打开屏幕窗口。
    pub fn check_codec() -> Result<()> {
        let _runtime = MediaFoundation::new()?;
        let mut encoder = Codec::encoder(FPS)?;
        let mut decoder = Codec::decoder()?;
        let mut frame = vec![128; WIDTH * HEIGHT * 3 / 2];
        let mut output_packets = 0;
        for n in 0..300 {
            frame[..WIDTH * HEIGHT].fill(32 + n as u8);
            if let Some((_, packet)) = encoder.process(&frame, n * 10_000)? {
                output_packets += 1;
                if let Some((stride, decoded)) = decoder.process(&packet, n * 10_000)?
                    && stride >= WIDTH
                    && decoded.len() >= WIDTH * HEIGHT * 3 / 2
                {
                    return Ok(());
                }
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        Err(Error::Failed(format!(
            "H.264 解码器没有产出测试帧，编码器输出 {output_packets} 帧"
        )))
    }
}

impl DisplayBackend for WinDisplayBackend {
    fn datagram(&self, _audio: bool) -> Result<Option<Box<dyn DisplayDatagram>>> {
        Ok(Some(Box::new(transport::ApolloDatagram::new()?)))
    }
    fn screens(&self) -> Result<Vec<ferry_types::DisplayScreen>> {
        surface::physical_screens()
    }
    fn recover(&self) -> Result<()> {
        surface::recover_monitor()
    }
    fn source(&self, _peer: [u8; 32], audio: bool) -> Result<Box<dyn DisplaySource>> {
        self.source_with_options(_peer, audio, Default::default())
    }
    fn source_with_options(
        &self,
        _peer: [u8; 32],
        audio: bool,
        options: ferry_types::DisplayOptions,
    ) -> Result<Box<dyn DisplaySource>> {
        let monitor = VirtualMonitor::attach_with_options(options)?;
        let calibration = monitor.calibration(options)?;
        let capture = Capture::new(&monitor)?;
        let runtime = MediaFoundation::new()?;
        let encoder = Encoder::new(monitor.refresh())?;
        let mut desktop_error = None;
        let desktop = encoder.gpu().and_then(|gpu| {
            match desktop::DesktopCapture::new(monitor.name(), &gpu) {
                Ok(capture) => Some(capture),
                Err(error) => {
                    tracing::warn!(%error, "DXGI 不可用，回退到 GDI 捕获");
                    desktop_error = Some(error.to_string());
                    None
                }
            }
        });
        let audio = audio.then(Loopback::new).transpose()?;
        let now = Instant::now();
        Ok(Box::new(Source {
            capture,
            _calibration: calibration,
            encoder,
            desktop,
            desktop_error,
            desktop_retry_at: now + Duration::from_millis(250),
            audio,
            audio_sent: false,
            input: WinInject::new(),
            pressed: Pressed::new(),
            pointer: Point::default(),
            _runtime: runtime,
            next: now,
            start: now,
            interval: Duration::from_micros(1_000_000 / u64::from(monitor.refresh())),
            _monitor: monitor,
            produced: 0,
            captured: 0,
            max_age_us: 0,
            capture_us: 0,
            submit_us: 0,
            sleep_us: 0,
            polls: 0,
            encoder_retry_at: None,
            encoder_recoveries: VecDeque::new(),
            bitrate: 18_000_000,
        }))
    }

    fn viewer(&self, audio: bool) -> Result<Box<dyn DisplayViewer>> {
        self.viewer_with_options(audio, Default::default())
    }
    fn viewer_with_options(
        &self,
        audio: bool,
        options: ferry_types::DisplayOptions,
    ) -> Result<Box<dyn DisplayViewer>> {
        let runtime = MediaFoundation::new()?;
        let mut viewer = Viewer::with_options(options)?;
        let decoder = Codec::decoder_for_window(viewer.window())?;
        if let Some(gpu) = decoder.gpu() {
            viewer.enable_gpu(&gpu)?;
        }
        let audio = audio.then(Playback::new).transpose()?;
        Ok(Box::new(Receiver {
            viewer,
            decoder,
            audio,
            _runtime: runtime,
        }))
    }
}

struct Source {
    capture: Capture,
    _calibration: Option<alignment::CursorAlignment>,
    encoder: Encoder,
    desktop: Option<desktop::DesktopCapture>,
    desktop_error: Option<String>,
    desktop_retry_at: Instant,
    audio: Option<Loopback>,
    audio_sent: bool,
    input: WinInject,
    pressed: Pressed,
    pointer: Point,
    _runtime: MediaFoundation,
    next: Instant,
    start: Instant,
    interval: Duration,
    _monitor: VirtualMonitor,
    produced: u64,
    captured: u64,
    max_age_us: u64,
    capture_us: u64,
    submit_us: u64,
    sleep_us: u64,
    polls: u64,
    encoder_retry_at: Option<Instant>,
    encoder_recoveries: VecDeque<Instant>,
    bitrate: u32,
}

impl Source {
    fn recover_encoder(&mut self, error: Error) -> Result<Option<MediaFrame>> {
        if !self.encoder.can_recover(&error) {
            return Err(error);
        }
        let now = Instant::now();
        self.encoder_recoveries
            .retain(|at| now.duration_since(*at) < Duration::from_secs(30));
        if self.encoder_recoveries.len() >= 3 {
            return Err(Error::Failed(format!(
                "硬件编码器在 30 秒内反复失效，已达到恢复次数上限并停止扩展屏：{error}"
            )));
        }
        self.encoder_recoveries.push_back(now);
        let attempt = self.encoder_recoveries.len();
        tracing::warn!(%error, attempt, "硬件编码暂时失效，保留扩展屏并重建编码器；下一帧从 IDR 恢复");
        self.desktop = None;
        self.encoder.discard();
        // Defer the retry rather than sleeping here: the media worker must keep
        // draining input and cancellation during a transient GPU stall.
        let retry_at = now + Duration::from_millis(250 * attempt as u64);
        self.encoder_retry_at = Some(retry_at);
        self.next = retry_at;
        Ok(None)
    }

    fn retry_encoder(&mut self, now: Instant) -> Result<bool> {
        let Some(retry_at) = self.encoder_retry_at else {
            return Ok(true);
        };
        if now < retry_at {
            return Ok(false);
        }
        self.encoder = Encoder::new(self._monitor.refresh())
            .map_err(|error| Error::Failed(format!("硬件编码器重建失败，已停止扩展屏：{error}")))?;
        self.encoder.set_bitrate(self.bitrate)?;
        // DXGI must be rebuilt against the replacement encoder's new GPU/device.
        // Start via the existing GDI fallback until its output is ready again.
        self.desktop_retry_at = now;
        self.encoder_retry_at = None;
        self.next = Instant::now();
        tracing::info!(encoder = %self.encoder.name(), "硬件编码器已重建，重新发送独立解码关键帧");
        Ok(true)
    }

    fn video(&mut self, timestamp_us: u64, bytes: Vec<u8>) -> MediaFrame {
        self.produced += 1;
        self.max_age_us = self
            .max_age_us
            .max((self.start.elapsed().as_micros() as u64).saturating_sub(timestamp_us));
        MediaFrame {
            track: MediaTrack::Video,
            timestamp_us,
            bytes,
        }
    }
}

impl DisplaySource for Source {
    fn request_keyframe(&mut self) -> Result<()> {
        self.encoder.request_keyframe()
    }
    fn set_bitrate(&mut self, bits_per_second: u32) -> Result<()> {
        self.encoder.set_bitrate(bits_per_second)?;
        self.bitrate = bits_per_second;
        Ok(())
    }
    fn capture_age(&self, timestamp_us: u64) -> Option<Duration> {
        Some(
            self.start
                .elapsed()
                .saturating_sub(Duration::from_micros(timestamp_us)),
        )
    }
    fn description(&self) -> String {
        format!(
            "{} capture / {} / {} / {}Hz / captured={} produced={} max_age_ms={:.1} capture_avg_ms={:.2} submit_avg_ms={:.2} sleep_avg_ms={:.2} fallback={:?}",
            if self.desktop.is_some() {
                "DXGI"
            } else {
                "GDI"
            },
            self.encoder.name(),
            if self.encoder.gpu().is_some() {
                "GPU NV12"
            } else {
                "CPU NV12"
            },
            self._monitor.refresh(),
            self.captured,
            self.produced,
            self.max_age_us as f64 / 1000.0,
            self.capture_us as f64 / self.captured.max(1) as f64 / 1000.0,
            self.submit_us as f64 / self.captured.max(1) as f64 / 1000.0,
            self.sleep_us as f64 / self.polls.max(1) as f64 / 1000.0,
            self.desktop_error
        )
    }
    fn input(&mut self, event: InputEvent) -> Result<()> {
        if !normal_desktop() {
            return Ok(());
        }
        // This media path injects into the interactive desktop directly; the
        // separately authenticated SYSTEM input session is not used here.
        self.input.check_available()?;
        let offset = self._monitor.current_position()?;
        let position = |point: Point| -> Point {
            Point::new(
                offset.x + point.x.clamp(0, WIDTH as i32 - 1),
                offset.y + point.y.clamp(0, HEIGHT as i32 - 1),
            )
        };
        let event = match event {
            InputEvent::PointerMotion { position: point } => {
                self.pointer = position(point);
                InputEvent::PointerMotion {
                    position: self.pointer,
                }
            }
            InputEvent::Button {
                position: point,
                button,
                pressed,
            } => {
                self.pointer = position(point);
                self.input.inject(&InputEvent::PointerMotion {
                    position: self.pointer,
                })?;
                InputEvent::Button {
                    position: self.pointer,
                    button,
                    pressed,
                }
            }
            InputEvent::Wheel {
                position: point,
                dx,
                dy,
            } => {
                self.pointer = position(point);
                self.input.inject(&InputEvent::PointerMotion {
                    position: self.pointer,
                })?;
                InputEvent::Wheel {
                    position: self.pointer,
                    dx,
                    dy,
                }
            }
            event => event,
        };
        self.input.inject(&event)?;
        self.pressed.track(&event);
        Ok(())
    }

    fn next_frame(&mut self, wait: Duration) -> Result<Option<MediaFrame>> {
        let left = self.next.saturating_duration_since(Instant::now());
        let sleep_start = Instant::now();
        std::thread::sleep(left.min(wait).min(Duration::from_millis(2)));
        self.sleep_us += sleep_start.elapsed().as_micros() as u64;
        self.polls += 1;
        let now = Instant::now();
        if !normal_desktop() {
            std::thread::sleep(wait.min(Duration::from_millis(30)));
            return Ok(None);
        }
        if !self.retry_encoder(now)? {
            return Ok(None);
        }
        // Rebuilding a device/encoder may take hundreds of milliseconds. Its
        // startup time must not become the new frame's capture timestamp.
        let now = Instant::now();
        if let Some((timestamp_us, bytes)) = self.encoder.poll_bytes()? {
            return Ok(Some(self.video(timestamp_us, bytes)));
        }
        if !self.audio_sent
            && let Some(audio) = &mut self.audio
            && let Some(bytes) = audio.next()?
        {
            self.audio_sent = true;
            return Ok(Some(MediaFrame {
                track: MediaTrack::Audio,
                timestamp_us: now.duration_since(self.start).as_micros() as u64,
                bytes,
            }));
        }
        self.audio_sent = false;
        if now < self.next || !self.encoder.can_input()? {
            return Ok(None);
        }
        if self
            .desktop
            .as_ref()
            .is_some_and(|capture| capture.needs_recreate)
        {
            self.desktop = None;
            self.desktop_retry_at = now;
        }
        if self.desktop.is_none() && now >= self.desktop_retry_at {
            self.desktop_retry_at = now + Duration::from_millis(250);
            if let Some(gpu) = self.encoder.gpu() {
                match desktop::DesktopCapture::new(self._monitor.name(), &gpu) {
                    Ok(capture) => {
                        self.desktop = Some(capture);
                        self.desktop_error = None;
                        tracing::info!("虚拟输出已就绪，恢复 DXGI 捕获");
                    }
                    Err(error) => {
                        let error = error.to_string();
                        if self.desktop_error.as_ref() != Some(&error) {
                            tracing::warn!(%error, "显示输出切换中，暂用 GDI 并重试 DXGI");
                        }
                        self.desktop_error = Some(error);
                    }
                }
            }
        }
        self.next = now + self.interval;
        self.captured += 1;
        let timestamp_us = now.duration_since(self.start).as_micros() as u64;
        let packet = if let Some(desktop) = &mut self.desktop {
            let capture_at = Instant::now();
            let mut frame = desktop.frame()?;
            if frame.is_none()
                && !desktop.needs_recreate
                && self.start.elapsed() >= Duration::from_millis(250)
            {
                // DXGI reports changes, so an unchanged desktop after reattachment
                // can have no first update. One GDI snapshot initializes the cache.
                desktop.seed(self.capture.desktop_bgra()?)?;
                frame = desktop.frame()?;
                tracing::debug!("使用一次初始桌面快照启动 DXGI 捕获");
            }
            self.capture_us += capture_at.elapsed().as_micros() as u64;
            let Some(frame) = frame else { return Ok(None) };
            let submit_at = Instant::now();
            let result = if desktop.bridge {
                self.encoder.encode_bgra(desktop.pixels()?, timestamp_us)
            } else {
                self.encoder.encode_texture(&frame, timestamp_us)
            };
            self.submit_us += submit_at.elapsed().as_micros() as u64;
            result
        } else if self.encoder.gpu().is_some() {
            let capture_at = Instant::now();
            let pixels = self.capture.bgra()?;
            self.capture_us += capture_at.elapsed().as_micros() as u64;
            let submit_at = Instant::now();
            let result = self.encoder.encode_bgra(pixels, timestamp_us);
            self.submit_us += submit_at.elapsed().as_micros() as u64;
            result
        } else {
            self.encoder.encode_nv12(self.capture.nv12()?, timestamp_us)
        };
        match packet {
            Ok(packet) => Ok(packet.map(|(timestamp_us, bytes)| self.video(timestamp_us, bytes))),
            Err(error) => self.recover_encoder(error),
        }
    }
}

impl Drop for Source {
    fn drop(&mut self) {
        for event in self.pressed.release_all(self.pointer) {
            let _ = self.input.inject(&event);
        }
    }
}

struct Receiver {
    viewer: Viewer,
    decoder: Codec,
    audio: Option<Playback>,
    _runtime: MediaFoundation,
}

impl DisplayViewer for Receiver {
    fn description(&self) -> String {
        format!(
            "{} / {} / {}",
            self.decoder.name(),
            if self.decoder.gpu().is_some() {
                "D3D11 decode and presentation"
            } else {
                "CPU decode and presentation"
            },
            self.viewer.description()
        )
    }
    fn input(&mut self) -> Result<Option<InputEvent>> {
        self.viewer.next_input()
    }
    fn present(&mut self, frame: MediaFrame) -> Result<bool> {
        self.viewer.events()?;
        if frame.track == MediaTrack::Audio {
            if let Some(audio) = &mut self.audio {
                audio.write(&frame.bytes)?;
            }
            return Ok(false);
        }
        if let Some(decoded) = self.decoder.decode(&frame.bytes, frame.timestamp_us)? {
            return self.viewer.present_decoded(decoded);
        }
        Ok(false)
    }
}

fn os(call: &'static str, error: windows::core::Error) -> Error {
    Error::Os {
        call,
        detail: error.to_string(),
    }
}
