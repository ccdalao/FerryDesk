//! 常驻引擎与本机媒体实现之间的接口。

use std::net::SocketAddr;
use std::time::Duration;

use crate::Result;
use ferry_types::{DisplayOptions, DisplayScreen, InputEvent};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaTrack {
    Video,
    Audio,
}

pub struct MediaFrame {
    pub track: MediaTrack,
    pub timestamp_us: u64,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct DisplayTransportStats {
    pub sent: u64,
    pub received: u64,
    pub invalid: u64,
    pub recovered: u64,
    pub lost: u64,
    pub frames: u64,
    pub qos: u64,
}

/// Apollo/Moonlight 兼容的实时媒体数据报接口。
///
/// 这是平台适配层的同步接口：Windows 实现把调用交给上游 FEC/UDP 核心，
/// 引擎在专用阻塞线程调用它。没有该能力的平台返回 `None`，继续使用 TCP 回退。
pub trait DisplayDatagram: Send {
    fn local_port(&self) -> u16;
    fn set_peer(&mut self, peer: SocketAddr, token: u64) -> Result<()>;
    fn set_key(&mut self, key: [u8; 32]) -> Result<()>;
    fn probe(&mut self, source: bool, wait: Duration) -> Result<bool>;
    fn pacing(&mut self, bitrate_bps: u32);
    fn stats(&mut self) -> DisplayTransportStats;
    fn send(&mut self, frame: MediaFrame) -> Result<()>;
    fn receive(&mut self, wait: Duration) -> Result<Option<MediaFrame>>;
}

/// 负责虚拟屏、画面和可选音频。对象被丢弃时释放本次会话的资源。
pub trait DisplaySource: Send {
    fn request_keyframe(&mut self) -> Result<()> {
        Ok(())
    }
    fn set_bitrate(&mut self, _bits_per_second: u32) -> Result<()> {
        Ok(())
    }
    /// 本机采集时间戳到当前时刻的耗时，不比较不同电脑的时钟。
    fn capture_age(&self, _timestamp_us: u64) -> Option<Duration> {
        None
    }
    fn description(&self) -> String {
        "platform display source".into()
    }
    /// 两次调用间隔允许任何线程及时结束媒体任务。
    fn next_frame(&mut self, wait: Duration) -> Result<Option<MediaFrame>>;
    fn input(&mut self, _event: InputEvent) -> Result<()> {
        Ok(())
    }
}

pub trait DisplayViewer: Send {
    fn description(&self) -> String {
        "platform display viewer".into()
    }
    /// 每次视频帧实际呈现后返回 `true`，首个 `true` 表示接收窗口已经就绪。
    fn present(&mut self, frame: MediaFrame) -> Result<bool>;
    fn input(&mut self) -> Result<Option<InputEvent>> {
        Ok(None)
    }
}

/// 构造在媒体线程执行。环境不具备所需设备时报告错误。
pub trait DisplayBackend: Send + Sync + 'static {
    fn datagram(&self, _audio: bool) -> Result<Option<Box<dyn DisplayDatagram>>> {
        Ok(None)
    }
    fn screens(&self) -> Result<Vec<DisplayScreen>> {
        Ok(Vec::new())
    }
    fn source_with_options(
        &self,
        peer: [u8; 32],
        audio: bool,
        _options: DisplayOptions,
    ) -> Result<Box<dyn DisplaySource>> {
        self.source(peer, audio)
    }
    fn viewer_with_options(
        &self,
        audio: bool,
        _options: DisplayOptions,
    ) -> Result<Box<dyn DisplayViewer>> {
        self.viewer(audio)
    }
    /// 启动时回收上次异常退出遗留的本软件资源；不得更改其他应用的显示器。
    fn recover(&self) -> Result<()> {
        Ok(())
    }
    fn source(&self, peer: [u8; 32], audio: bool) -> Result<Box<dyn DisplaySource>>;
    fn viewer(&self, audio: bool) -> Result<Box<dyn DisplayViewer>>;
}
