//! Apollo/Moonlight 兼容的 Windows UDP 媒体适配器。

use std::ffi::{CStr, CString, c_char, c_void};
use std::net::SocketAddr;
use std::ptr::NonNull;
use std::time::Duration;

use ferry_platform::{
    DisplayDatagram, DisplayTransportStats, Error, MediaFrame, MediaTrack, Result,
};

#[repr(C)]
#[derive(Default)]
struct NativeStats {
    sent: u64,
    received: u64,
    invalid: u64,
    recovered: u64,
    lost: u64,
    frames: u64,
    qos: u64,
}

unsafe extern "C" {
    fn sf_apollo_media_create(local_port: u16, mtu: u16, fec_percent: u8) -> *mut c_void;
    fn sf_apollo_media_local_port(media: *const c_void) -> u16;
    fn sf_apollo_media_set_peer(
        media: *mut c_void,
        ipv4: *const c_char,
        port: u16,
        token: u64,
    ) -> i32;
    fn sf_apollo_media_send_frame(
        media: *mut c_void,
        data: *const u8,
        length: u32,
        frame_index: u32,
        timestamp_us: u64,
        track: u8,
    ) -> i32;
    fn sf_apollo_media_receive_frame(
        media: *mut c_void,
        out: *mut u8,
        capacity: u32,
        length: *mut u32,
        frame_index: *mut u32,
        timestamp_us: *mut u64,
        track: *mut u8,
        wait_ms: u32,
    ) -> i32;
    fn sf_apollo_media_destroy(media: *mut c_void);
    fn sf_apollo_media_error() -> *const c_char;
    fn sf_apollo_media_set_key(media: *mut c_void, key: *const u8) -> i32;
    fn sf_apollo_media_probe(media: *mut c_void, source: i32, wait_ms: u32) -> i32;
    fn sf_apollo_media_pacing(media: *mut c_void, bitrate: u32);
    fn sf_apollo_media_stats_read(media: *mut c_void, stats: *mut NativeStats);
}

pub(super) struct ApolloDatagram {
    handle: NonNull<c_void>,
    next_frame: [u32; 2],
    buffer: Vec<u8>,
}

// SAFETY: the native object owns one UDP socket and is called only from the
// single blocking media worker that owns this value.
unsafe impl Send for ApolloDatagram {}

impl ApolloDatagram {
    pub(super) fn new() -> Result<Self> {
        // SAFETY: the C adapter owns the returned handle and returns null on failure.
        let handle = unsafe { sf_apollo_media_create(0, 1200, 20) };
        let handle = NonNull::new(handle).ok_or_else(last_error)?;
        Ok(Self {
            handle,
            next_frame: [1, 1],
            buffer: vec![0; ferry_proto::MAX_ACCESS_UNIT],
        })
    }
}

impl DisplayDatagram for ApolloDatagram {
    fn set_key(&mut self, key: [u8; 32]) -> Result<()> {
        // SAFETY: the native call copies the 32-byte session key synchronously.
        if unsafe { sf_apollo_media_set_key(self.handle.as_ptr(), key.as_ptr()) } == 0 {
            Err(last_error())
        } else {
            Ok(())
        }
    }
    fn probe(&mut self, source: bool, wait: Duration) -> Result<bool> {
        // SAFETY: this worker exclusively owns the handle during the bounded probe.
        Ok(unsafe {
            sf_apollo_media_probe(
                self.handle.as_ptr(),
                i32::from(source),
                wait.as_millis().min(1000) as u32,
            )
        } == 1)
    }
    fn pacing(&mut self, bitrate_bps: u32) {
        // SAFETY: only changes this owned media sender's rate budget.
        unsafe { sf_apollo_media_pacing(self.handle.as_ptr(), bitrate_bps) };
    }
    fn stats(&mut self) -> DisplayTransportStats {
        let mut stats = NativeStats::default();
        // SAFETY: stats is a valid output matching the native repr(C) struct.
        unsafe { sf_apollo_media_stats_read(self.handle.as_ptr(), &mut stats) };
        DisplayTransportStats {
            sent: stats.sent,
            received: stats.received,
            invalid: stats.invalid,
            recovered: stats.recovered,
            lost: stats.lost,
            frames: stats.frames,
            qos: stats.qos,
        }
    }
    fn local_port(&self) -> u16 {
        // SAFETY: handle remains valid until Drop and is only read here.
        unsafe { sf_apollo_media_local_port(self.handle.as_ptr()) }
    }

    fn set_peer(&mut self, peer: SocketAddr, token: u64) -> Result<()> {
        let SocketAddr::V4(peer) = peer else {
            return Err(Error::Unsupported("Apollo UDP 媒体暂只支持 IPv4"));
        };
        let address = CString::new(peer.ip().to_string())
            .map_err(|_| Error::Failed("UDP 对端地址无效".into()))?;
        // SAFETY: address is NUL terminated for the duration of the call.
        let ok = unsafe {
            sf_apollo_media_set_peer(self.handle.as_ptr(), address.as_ptr(), peer.port(), token)
        };
        if ok == 0 { Err(last_error()) } else { Ok(()) }
    }

    fn send(&mut self, frame: MediaFrame) -> Result<()> {
        let track = match frame.track {
            MediaTrack::Video => 0,
            MediaTrack::Audio => 1,
        };
        let track_index = usize::from(track);
        let frame_index = self.next_frame[track_index];
        self.next_frame[track_index] = self.next_frame[track_index].wrapping_add(1);
        // SAFETY: the byte slice remains alive for the synchronous C call.
        let sent = unsafe {
            sf_apollo_media_send_frame(
                self.handle.as_ptr(),
                frame.bytes.as_ptr(),
                frame.bytes.len() as u32,
                frame_index,
                frame.timestamp_us,
                track,
            )
        };
        if sent <= 0 { Err(last_error()) } else { Ok(()) }
    }

    fn receive(&mut self, wait: Duration) -> Result<Option<MediaFrame>> {
        let mut length = 0_u32;
        let mut frame_index = 0_u32;
        let mut timestamp_us = 0_u64;
        let mut track = 0_u8;
        // SAFETY: the output buffer is owned by this worker and bounded by the
        // protocol's access-unit limit. The C function writes only `length` bytes.
        let result = unsafe {
            sf_apollo_media_receive_frame(
                self.handle.as_ptr(),
                self.buffer.as_mut_ptr(),
                self.buffer.len() as u32,
                &mut length,
                &mut frame_index,
                &mut timestamp_us,
                &mut track,
                wait.as_millis().min(u32::MAX as u128) as u32,
            )
        };
        match result {
            0 => Ok(None),
            1 if length as usize <= self.buffer.len() && track <= 1 => Ok(Some(MediaFrame {
                track: if track == 0 {
                    MediaTrack::Video
                } else {
                    MediaTrack::Audio
                },
                timestamp_us,
                bytes: self.buffer[..length as usize].to_vec(),
            })),
            _ => Err(last_error()),
        }
    }
}

impl Drop for ApolloDatagram {
    fn drop(&mut self) {
        // SAFETY: this is the sole owner and the destructor is called once.
        unsafe { sf_apollo_media_destroy(self.handle.as_ptr()) };
    }
}

fn last_error() -> Error {
    // SAFETY: the C adapter returns a thread-local NUL-terminated diagnostic.
    let message = unsafe { sf_apollo_media_error() };
    if message.is_null() {
        return Error::Failed("Apollo UDP 媒体失败".into());
    }
    Error::Failed(
        unsafe { CStr::from_ptr(message) }
            .to_string_lossy()
            .into_owned(),
    )
}
