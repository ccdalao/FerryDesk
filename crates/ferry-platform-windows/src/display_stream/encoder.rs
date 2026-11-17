//! 优先使用 Apollo 原生编码，硬件不可用时选择系统硬件/软件编码。

use super::apollo::ApolloEncoder;
use super::codec::Codec;
use super::gpu::Gpu;
use ferry_platform::{Error, Result};
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;

pub(super) enum Encoder {
    Apollo(Box<ApolloEncoder>),
    System(Box<Codec>),
    Recovering,
}

impl Encoder {
    pub fn request_keyframe(&mut self) -> Result<()> {
        match self {
            Self::Apollo(encoder) => {
                encoder.request_keyframe();
                Ok(())
            }
            Self::System(codec) => codec.request_keyframe(),
            Self::Recovering => Ok(()),
        }
    }
    pub fn set_bitrate(&mut self, bits_per_second: u32) -> Result<()> {
        match self {
            Self::Apollo(encoder) => encoder.set_bitrate(bits_per_second),
            Self::System(codec) => codec.set_bitrate(bits_per_second),
            Self::Recovering => Ok(()),
        }
    }
    pub fn new(refresh: u32) -> Result<Self> {
        match ApolloEncoder::new(refresh) {
            Ok(encoder) => Ok(Self::Apollo(Box::new(encoder))),
            Err(error) => {
                tracing::info!(%error, "Apollo NVENC 不可用，尝试其他硬件编码器");
                Ok(Self::System(Box::new(Codec::encoder(refresh)?)))
            }
        }
    }
    pub fn name(&self) -> &str {
        match self {
            Self::Apollo(_) => "Apollo v0.4.6 native D3D11 NVENC",
            Self::System(codec) => codec.name(),
            Self::Recovering => "recovering encoder",
        }
    }
    pub fn gpu(&self) -> Option<Gpu> {
        match self {
            Self::Apollo(encoder) => Some(encoder.gpu()),
            Self::System(codec) => codec.gpu(),
            Self::Recovering => None,
        }
    }
    pub fn can_recover(&self, error: &Error) -> bool {
        match self {
            Self::Apollo(encoder) => match error {
                // Retry only completion/busy/device-loss failures. An invalid
                // parameter, input dimensions or output size remains fatal.
                Error::Failed(message) if message.starts_with("Apollo NVENC：") => {
                    message.contains("encode wait timeout")
                        || message.contains("NV_ENC_ERR_ENCODER_BUSY")
                        || message.contains("NV_ENC_ERR_LOCK_BUSY")
                        // SAFETY: read-only query of this encoder's owned device.
                        || unsafe { encoder.gpu().device.GetDeviceRemovedReason().is_err() }
                }
                Error::Os { .. } => {
                    // SAFETY: querying this encoder's owned device is read-only.
                    unsafe { encoder.gpu().device.GetDeviceRemovedReason().is_err() }
                }
                _ => false,
            },
            _ => false,
        }
    }
    pub fn discard(&mut self) {
        // Release the NVENC session before opening its replacement, including
        // on GPUs where the concurrent-session limit is already reached.
        let previous = std::mem::replace(self, Self::Recovering);
        drop(previous);
    }
    pub fn can_input(&mut self) -> Result<bool> {
        match self {
            Self::Apollo(_) => Ok(true),
            Self::System(codec) => codec.can_input(),
            Self::Recovering => Ok(false),
        }
    }
    pub fn poll_bytes(&mut self) -> Result<Option<(u64, Vec<u8>)>> {
        match self {
            Self::Apollo(_) => Ok(None),
            Self::System(codec) => codec.poll_bytes(),
            Self::Recovering => Ok(None),
        }
    }
    pub fn encode_bgra(&mut self, bytes: &[u8], timestamp: u64) -> Result<Option<(u64, Vec<u8>)>> {
        match self {
            Self::Apollo(encoder) => encoder.encode_bgra(bytes, timestamp),
            Self::System(codec) => codec.encode_bgra(bytes, timestamp),
            Self::Recovering => Err(Error::Failed("编码器正在恢复".into())),
        }
    }
    pub fn encode_texture(
        &mut self,
        texture: &ID3D11Texture2D,
        timestamp: u64,
    ) -> Result<Option<(u64, Vec<u8>)>> {
        match self {
            Self::Apollo(encoder) => encoder.encode_texture(texture, timestamp),
            Self::System(codec) => codec.encode_texture(texture, timestamp),
            Self::Recovering => Err(Error::Failed("编码器正在恢复".into())),
        }
    }
    pub fn encode_nv12(&mut self, bytes: &[u8], timestamp: u64) -> Result<Option<(u64, Vec<u8>)>> {
        match self {
            Self::System(codec) => codec
                .process(bytes, timestamp)
                .map(|out| out.map(|(_, data)| (timestamp, data))),
            Self::Apollo(_) => Err(Error::Failed("Apollo 原生编码要求 GPU 输入".into())),
            Self::Recovering => Err(Error::Failed("编码器正在恢复".into())),
        }
    }
}
