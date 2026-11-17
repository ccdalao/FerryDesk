//! 回录默认播放设备，以 48 kHz 双声道 PCM 发送和播放。

use std::ptr::null_mut;

use ferry_platform::Result;
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};

use super::os;

const BLOCK: usize = 4;
const RECENT_BYTES: usize = 48_000 * BLOCK / 50; // 20 ms of stereo PCM.
const CAPTURE_BATCH: usize = 32;
const FORMAT: WAVEFORMATEX = WAVEFORMATEX {
    wFormatTag: 1,
    nChannels: 2,
    nSamplesPerSec: 48_000,
    nAvgBytesPerSec: 48_000 * BLOCK as u32,
    nBlockAlign: BLOCK as u16,
    wBitsPerSample: 16,
    cbSize: 0,
};

fn client(loopback: bool) -> Result<IAudioClient> {
    // SAFETY: The media thread has initialized COM, and FORMAT is a valid PCM format.
    unsafe {
        let device: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_INPROC_SERVER)
                .map_err(|e| os("创建 WASAPI 设备枚举器", e))?;
        let device = device
            .GetDefaultAudioEndpoint(eRender, eConsole)
            .map_err(|e| os("获取默认声音设备", e))?;
        let client: IAudioClient = device
            .Activate(CLSCTX_INPROC_SERVER, None)
            .map_err(|e| os("启用 WASAPI", e))?;
        let mut flags = AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
            | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY
            | AUDCLNT_STREAMFLAGS_NOPERSIST;
        if loopback {
            flags |= AUDCLNT_STREAMFLAGS_LOOPBACK;
        }
        client
            .Initialize(AUDCLNT_SHAREMODE_SHARED, flags, 400_000, 0, &FORMAT, None)
            .map_err(|e| os("初始化扩展屏声音", e))?;
        Ok(client)
    }
}

pub(super) struct Loopback {
    capture: IAudioCaptureClient,
    client: IAudioClient,
}

// SAFETY: Both interfaces are used and released by the same MTA media worker.
unsafe impl Send for Loopback {}

impl Loopback {
    pub fn new() -> Result<Self> {
        let client = client(true)?;
        // SAFETY: The initialized client is used in shared loopback mode.
        unsafe {
            let capture = client.GetService().map_err(|e| os("打开 WASAPI 回录", e))?;
            client.Start().map_err(|e| os("启动声音回录", e))?;
            Ok(Self { capture, client })
        }
    }

    pub fn next(&mut self) -> Result<Option<Vec<u8>>> {
        // SAFETY: Every acquired packet is released on this media thread before
        // reading another one. Keep only a bounded, frame-aligned PCM tail.
        unsafe {
            let mut bytes = Vec::with_capacity(RECENT_BYTES);
            for _ in 0..CAPTURE_BATCH {
                if self
                    .capture
                    .GetNextPacketSize()
                    .map_err(|e| os("查询音频包", e))?
                    == 0
                {
                    return Ok((!bytes.is_empty()).then_some(bytes));
                }
                let (mut pointer, mut frames, mut flags) = (null_mut(), 0, 0);
                self.capture
                    .GetBuffer(&mut pointer, &mut frames, &mut flags, None, None)
                    .map_err(|e| os("读取音频包", e))?;
                let keep_frames = (frames as usize).min(RECENT_BYTES / BLOCK);
                let keep_bytes = keep_frames * BLOCK;
                let discard = (bytes.len() + keep_bytes).saturating_sub(RECENT_BYTES);
                bytes.drain(..discard);
                if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 || pointer.is_null() {
                    bytes.resize(bytes.len() + keep_bytes, 0);
                } else {
                    let skip = (frames as usize - keep_frames) * BLOCK;
                    bytes.extend_from_slice(std::slice::from_raw_parts(
                        pointer.add(skip),
                        keep_bytes,
                    ));
                }
                self.capture
                    .ReleaseBuffer(frames)
                    .map_err(|e| os("归还音频包", e))?;
            }
            // Under prolonged backpressure even the tail of this bounded batch
            // may still be old. Continue draining on the next poll instead of
            // transmitting intermediate backlog with a fresh stream timestamp.
            if self
                .capture
                .GetNextPacketSize()
                .map_err(|e| os("查询音频包", e))?
                != 0
            {
                return Ok(None);
            }
            Ok((!bytes.is_empty()).then_some(bytes))
        }
    }
}

impl Drop for Loopback {
    fn drop(&mut self) {
        // SAFETY: Stop the client started by this object on this thread.
        unsafe {
            let _ = self.client.Stop();
        }
    }
}

pub(super) struct Playback {
    render: IAudioRenderClient,
    client: IAudioClient,
    capacity: u32,
}

// SAFETY: Both interfaces are used and released by the same MTA media worker.
unsafe impl Send for Playback {}

impl Playback {
    pub fn new() -> Result<Self> {
        let client = client(false)?;
        // SAFETY: The initialized client is in shared rendering mode.
        unsafe {
            let render = client.GetService().map_err(|e| os("打开 WASAPI 播放", e))?;
            let capacity = client
                .GetBufferSize()
                .map_err(|e| os("读取播放缓冲区容量", e))?;
            client.Start().map_err(|e| os("启动扩展屏声音播放", e))?;
            Ok(Self {
                render,
                client,
                capacity,
            })
        }
    }

    pub fn write(&mut self, bytes: &[u8]) -> Result<()> {
        if !bytes.len().is_multiple_of(BLOCK) || bytes.len() > 48_000 * BLOCK / 4 {
            return Err(ferry_platform::Error::Failed(
                "扩展屏的音频包尺寸无效".into(),
            ));
        }
        // SAFETY: The PCM copy is at most the free space reported by WASAPI.
        unsafe {
            let padding = self
                .client
                .GetCurrentPadding()
                .map_err(|e| os("查询播放缓冲", e))?;
            let frames = ((bytes.len() / BLOCK) as u32).min(self.capacity.saturating_sub(padding));
            if frames == 0 {
                return Ok(());
            }
            let pointer = self
                .render
                .GetBuffer(frames)
                .map_err(|e| os("取得播放缓冲", e))?;
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), pointer, frames as usize * BLOCK);
            self.render
                .ReleaseBuffer(frames, 0)
                .map_err(|e| os("提交扩展屏音频", e))
        }
    }
}

impl Drop for Playback {
    fn drop(&mut self) {
        // SAFETY: Stop the client started by this object on this thread.
        unsafe {
            let _ = self.client.Stop();
        }
    }
}
