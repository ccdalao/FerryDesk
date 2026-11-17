//! Apollo v0.4.6 的原生 D3D11 NVENC；C ABI 只在本媒体线程内使用。

use super::gpu::{Gpu, VideoProcessor};
use super::{HEIGHT, WIDTH};
use ferry_platform::{Error, Result};
use std::ffi::{CStr, c_char, c_void};
use std::ptr::NonNull;
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::core::Interface;

unsafe extern "C" {
    fn sf_apollo_nvenc_create(
        device: *mut c_void,
        width: u32,
        height: u32,
        refresh: u32,
        bitrate: u32,
        callback: Option<unsafe extern "C" fn(i32, *const c_char)>,
    ) -> *mut c_void;
    fn sf_apollo_nvenc_texture(handle: *mut c_void) -> *mut c_void;
    fn sf_apollo_nvenc_encode(handle: *mut c_void, data: *mut *const u8, size: *mut usize) -> i32;
    fn sf_apollo_nvenc_error() -> *const c_char;
    fn sf_apollo_nvenc_destroy(handle: *mut c_void);
    fn sf_apollo_nvenc_request_idr(handle: *mut c_void);
    fn sf_apollo_nvenc_bitrate(handle: *mut c_void, bitrate: u32) -> i32;
}

pub(super) struct ApolloEncoder {
    handle: NonNull<c_void>,
    gpu: Gpu,
    processor: VideoProcessor,
    input: ID3D11Texture2D,
    upload: ID3D11Texture2D,
}

// SAFETY: the engine constructs, uses and destroys the C++ encoder on one
// media worker. No borrowed pointers or resources escape to another thread.
unsafe impl Send for ApolloEncoder {}

impl ApolloEncoder {
    pub fn request_keyframe(&mut self) {
        // SAFETY: this worker exclusively owns the native encoder handle.
        unsafe { sf_apollo_nvenc_request_idr(self.handle.as_ptr()) };
    }
    pub fn set_bitrate(&mut self, bits_per_second: u32) -> Result<()> {
        // SAFETY: the bitrate reconfiguration keeps the owned input surface.
        if unsafe { sf_apollo_nvenc_bitrate(self.handle.as_ptr(), bits_per_second) } == 1 {
            Ok(())
        } else {
            Err(last_error())
        }
    }
    pub fn new(refresh: u32) -> Result<Self> {
        let gpu = Gpu::new(Some(0x10de))?;
        let processor = VideoProcessor::new(&gpu, WIDTH as u32, HEIGHT as u32, true)?;
        let upload = gpu.texture(DXGI_FORMAT_B8G8R8A8_UNORM, WIDTH as u32, HEIGHT as u32)?;
        // SAFETY: C++ takes its own device reference; all scalar dimensions are fixed
        // and the logger function pointer has static lifetime. C++ catches exceptions.
        let handle = unsafe {
            sf_apollo_nvenc_create(
                gpu.device.as_raw(),
                WIDTH as u32,
                HEIGHT as u32,
                refresh,
                18_000_000,
                Some(log),
            )
        };
        let handle = NonNull::new(handle).ok_or_else(last_error)?;
        // SAFETY: this function returns one owned COM reference, adopted exactly once.
        let texture = unsafe { sf_apollo_nvenc_texture(handle.as_ptr()) };
        if texture.is_null() {
            // SAFETY: release the successfully constructed C++ object on failure.
            unsafe {
                sf_apollo_nvenc_destroy(handle.as_ptr());
            }
            return Err(Error::Failed("Apollo NVENC 未返回输入纹理".into()));
        }
        let input = unsafe { ID3D11Texture2D::from_raw(texture) };
        tracing::info!("扩展屏启用 Apollo v0.4.6 原生 D3D11 NVENC");
        Ok(Self {
            handle,
            gpu,
            processor,
            input,
            upload,
        })
    }

    pub fn gpu(&self) -> Gpu {
        self.gpu.clone()
    }

    pub fn encode_bgra(&mut self, bgra: &[u8], timestamp: u64) -> Result<Option<(u64, Vec<u8>)>> {
        if bgra.len() != WIDTH * HEIGHT * 4 {
            return Err(Error::Failed("Apollo BGRA 输入尺寸无效".into()));
        }
        // SAFETY: upload reads a complete BGRA frame with the stated pitch.
        unsafe {
            self.gpu.context.UpdateSubresource(
                &self.upload,
                0,
                None,
                bgra.as_ptr().cast(),
                WIDTH as u32 * 4,
                0,
            );
        }
        self.encode_texture(&self.upload.clone(), timestamp)
    }

    pub fn encode_texture(
        &mut self,
        texture: &ID3D11Texture2D,
        timestamp: u64,
    ) -> Result<Option<(u64, Vec<u8>)>> {
        self.processor.blit(texture, 0, &self.input)?;
        // SAFETY: submit conversion before NVENC's separate queue reads its input.
        unsafe {
            self.gpu.context.Flush();
        }
        let mut data = std::ptr::null();
        let mut size = 0;
        // SAFETY: valid exclusive handle; C++ output lives until the next encode/destroy.
        if unsafe { sf_apollo_nvenc_encode(self.handle.as_ptr(), &mut data, &mut size) } == 0 {
            return Err(last_error());
        }
        if data.is_null() || size == 0 || size > ferry_proto::MAX_ACCESS_UNIT {
            return Err(Error::Failed("Apollo NVENC 输出长度无效".into()));
        }
        // SAFETY: validate output size and copy before the C++ frame can be replaced.
        let bytes = unsafe { std::slice::from_raw_parts(data, size) }.to_vec();
        Ok(Some((timestamp, bytes)))
    }
}

impl Drop for ApolloEncoder {
    fn drop(&mut self) {
        // SAFETY: this handle is owned and destroyed exactly once on its media worker.
        unsafe {
            sf_apollo_nvenc_destroy(self.handle.as_ptr());
        }
    }
}

fn last_error() -> Error {
    // SAFETY: C++ returns its thread-local, NUL-terminated error string; copy it now.
    let pointer = unsafe { sf_apollo_nvenc_error() };
    let message = if pointer.is_null() {
        "未知 NVENC 错误".into()
    } else {
        unsafe { CStr::from_ptr(pointer) }
            .to_string_lossy()
            .into_owned()
    };
    Error::Failed(format!("Apollo NVENC：{message}"))
}

unsafe extern "C" fn log(level: i32, message: *const c_char) {
    if message.is_null() {
        return;
    }
    // SAFETY: the C++ log line holds its NUL-terminated string for this callback.
    let message = unsafe { CStr::from_ptr(message) }.to_string_lossy();
    // Never unwind across the C ABI even if a logging subscriber fails.
    let _ = std::panic::catch_unwind(|| match level {
        0 => tracing::debug!("Apollo core: {message}"),
        1 => tracing::info!("Apollo core: {message}"),
        2 => tracing::warn!("Apollo core: {message}"),
        _ => tracing::error!("Apollo core: {message}"),
    });
}
