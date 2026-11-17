//! Windows 自带的 H.264 MFT。编码和解码在各自的媒体线程上运行。

use std::mem::ManuallyDrop;
use std::ptr::null_mut;

use ferry_platform::{Error, Result};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
    CoUninitialize,
};
use windows::core::{GUID, Interface};

use super::gpu::{EncoderInput, Gpu};
use super::{FPS, HEIGHT, WIDTH, os};
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;

pub(super) enum DecodedFrame {
    Texture(ID3D11Texture2D, u32),
    Memory {
        stride: usize,
        height: usize,
        bytes: Vec<u8>,
    },
}

pub(super) struct MediaFoundation {
    timer: bool,
}

impl MediaFoundation {
    pub fn new() -> Result<Self> {
        // SAFETY: 初始化当前媒体线程的 COM，退出时在同一线程撤销。
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED)
                .ok()
                .map_err(|e| os("CoInitializeEx", e))?;
            if let Err(error) = MFStartup(MF_VERSION, MFSTARTUP_LITE) {
                CoUninitialize();
                return Err(os("MFStartup", error));
            }
        }
        // A coarse Windows sleep can turn a requested 1–2 ms media poll into 15.6 ms.
        // Keep the timer request scoped to a running media pipeline, including when
        // its settings window is occluded or closed.
        use windows::Win32::System::Threading::*;
        // SAFETY: change only this process's QoS and pair our timer request in Drop.
        let timer = unsafe {
            let state = PROCESS_POWER_THROTTLING_STATE {
                Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
                ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED
                    | PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION,
                StateMask: 0,
            };
            let _ = SetProcessInformation(
                GetCurrentProcess(),
                ProcessPowerThrottling,
                (&state as *const PROCESS_POWER_THROTTLING_STATE).cast(),
                size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
            );
            windows::Win32::Media::timeBeginPeriod(1) == 0
        };
        Ok(Self { timer })
    }
}

impl Drop for MediaFoundation {
    fn drop(&mut self) {
        // SAFETY: 同一个线程初始化和关闭 Media Foundation 及 COM。
        unsafe {
            if self.timer {
                let _ = windows::Win32::Media::timeEndPeriod(1);
            }
            let _ = MFShutdown();
            CoUninitialize();
        }
    }
}

pub(super) struct Codec {
    transform: IMFTransform,
    encoder: bool,
    refresh: u32,
    stride: usize,
    storage_height: usize,
    events: Option<IMFMediaEventGenerator>,
    need_input: u32,
    have_output: u32,
    pending: u32,
    activation: Option<IMFActivate>,
    name: String,
    gpu: Option<Gpu>,
    input: Option<EncoderInput>,
}

// SAFETY: 本对象在创建它的独立媒体工作线程内构造、使用和销毁，
// trait 的 Send 约束允许所有权交给工作线程，MFT 在多线程 COM 单元中创建。
unsafe impl Send for Codec {}

impl Codec {
    pub fn request_keyframe(&mut self) -> Result<()> {
        use windows::Win32::System::Variant::VARIANT;
        // SAFETY: codec control is used only by its owner media worker.
        unsafe {
            let control = self
                .transform
                .cast::<ICodecAPI>()
                .map_err(|e| os("获取编码控制", e))?;
            control
                .SetValue(&CODECAPI_AVEncVideoForceKeyFrame, &VARIANT::from(1_u32))
                .map_err(|e| os("请求关键帧", e))
        }
    }
    pub fn set_bitrate(&mut self, bits_per_second: u32) -> Result<()> {
        use windows::Win32::System::Variant::VARIANT;
        // SAFETY: no decoder or other thread accesses this encoder instance.
        unsafe {
            let control = self
                .transform
                .cast::<ICodecAPI>()
                .map_err(|e| os("获取码率控制", e))?;
            control
                .SetValue(
                    &CODECAPI_AVEncCommonMeanBitRate,
                    &VARIANT::from(bits_per_second),
                )
                .map_err(|e| os("调整编码码率", e))
        }
    }
    pub fn encoder(refresh: u32) -> Result<Self> {
        for activation in hardware_encoders()? {
            // SAFETY: activation objects originate from the system's hardware MFT enumeration.
            let result = unsafe { activation.ActivateObject::<IMFTransform>() };
            let Ok(transform) = result else { continue };
            let name = activation_name(&activation);
            let vendor = if name.contains("NVIDIA") {
                Some(0x10de)
            } else if name.contains("Intel") {
                Some(0x8086)
            } else if name.contains("AMD") {
                Some(0x1002)
            } else {
                None
            };
            let gpu = Gpu::new(vendor).ok();
            match Self::configure(transform, true, refresh, Some(activation), name, gpu) {
                Ok(codec) => {
                    tracing::info!(encoder = %codec.name, "扩展屏启用硬件 H.264 编码");
                    return Ok(codec);
                }
                Err(error) => tracing::warn!(%error, "硬件编码器不可用，尝试下一候选"),
            }
        }
        tracing::warn!("未能启用硬件编码，扩展屏回退到系统软件 H.264");
        Self::new(true, refresh, None)
    }

    pub fn decoder() -> Result<Self> {
        Self::new(false, FPS, Gpu::new(None).ok())
    }

    pub fn decoder_for_window(window: windows::Win32::Foundation::HWND) -> Result<Self> {
        let gpu = Gpu::for_window(window)
            .or_else(|error| {
                tracing::warn!(%error, "接收屏 GPU 不可用，尝试默认解码适配器");
                Gpu::new(None)
            })
            .ok();
        Self::new(false, FPS, gpu)
    }

    fn new(encoder: bool, refresh: u32, gpu: Option<Gpu>) -> Result<Self> {
        // SAFETY: 当前线程已初始化 COM 和 MF；所有参数指向本函数持有的接口。
        let transform: IMFTransform = unsafe {
            CoCreateInstance(
                if encoder {
                    &CMSH264EncoderMFT
                } else {
                    &CMSH264DecoderMFT
                },
                None,
                CLSCTX_INPROC_SERVER,
            )
        }
        .map_err(|e| os("创建 H.264 编解码器", e))?;
        Self::configure(
            transform,
            encoder,
            refresh,
            None,
            "Microsoft H.264".into(),
            gpu,
        )
    }

    fn configure(
        transform: IMFTransform,
        encoder: bool,
        refresh: u32,
        activation: Option<IMFActivate>,
        name: String,
        mut gpu: Option<Gpu>,
    ) -> Result<Self> {
        // SAFETY: attributes belong to the transform owned by this media worker.
        let events = unsafe {
            let attributes = transform
                .GetAttributes()
                .map_err(|e| os("读取编解码属性", e))?;
            let _ = attributes.SetUINT32(&MF_LOW_LATENCY, 1);
            if attributes.GetUINT32(&MF_SA_D3D11_AWARE).unwrap_or(0) == 0 {
                gpu = None;
            }
            if attributes.GetUINT32(&MF_TRANSFORM_ASYNC).unwrap_or(0) != 0 {
                attributes
                    .SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1)
                    .map_err(|e| os("解锁异步硬件编码器", e))?;
                Some(
                    transform
                        .cast::<IMFMediaEventGenerator>()
                        .map_err(|e| os("硬件编码事件", e))?,
                )
            } else {
                None
            }
        };
        if let Some(gpu) = &gpu {
            // SAFETY: the manager remains alive for the transform's entire lifetime.
            unsafe {
                transform.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, gpu.manager.as_raw() as usize)
            }
            .map_err(|e| os("编解码器绑定 GPU", e))?;
        }
        let input = if encoder {
            gpu.as_ref().map(EncoderInput::new).transpose()?
        } else {
            None
        };
        let mut codec = Self {
            transform,
            encoder,
            refresh,
            stride: WIDTH,
            storage_height: HEIGHT,
            events,
            need_input: 0,
            have_output: 0,
            pending: 0,
            activation,
            name,
            gpu,
            input,
        };
        // SAFETY: MFT 输入和输出各有一条，索引均为 0。
        unsafe {
            if let Ok(control) = codec.transform.cast::<ICodecAPI>() {
                use windows::Win32::System::Variant::VARIANT;
                let _ = control.SetValue(&CODECAPI_AVLowLatencyMode, &VARIANT::from(true));
                if encoder {
                    let _ = control.SetValue(&CODECAPI_AVEncCommonRealTime, &VARIANT::from(true));
                    let _ = control.SetValue(
                        &CODECAPI_AVEncMPVDefaultBPictureCount,
                        &VARIANT::from(0_u32),
                    );
                }
            }
            if encoder {
                let output = video_type(&MFVideoFormat_H264, refresh)?;
                output
                    .SetUINT32(&MF_MT_AVG_BITRATE, 18_000_000)
                    .map_err(|e| os("设置编码码率", e))?;
                output
                    .SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_Base.0 as u32)
                    .map_err(|e| os("设置 H.264 profile", e))?;
                codec
                    .transform
                    .SetOutputType(0, &output, 0)
                    .map_err(|e| os("设置 H.264 输出", e))?;
                let input = video_type(&MFVideoFormat_NV12, refresh)?;
                codec
                    .transform
                    .SetInputType(0, &input, 0)
                    .map_err(|e| os("设置 NV12 输入", e))?;
            } else {
                let input = MFCreateMediaType().map_err(|e| os("创建 H.264 输入", e))?;
                input
                    .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
                    .map_err(|e| os("设置媒体类型", e))?;
                input
                    .SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)
                    .map_err(|e| os("设置 H.264 类型", e))?;
                codec
                    .transform
                    .SetInputType(0, &input, 0)
                    .map_err(|e| os("设置解码输入", e))?;
                codec.select_decoder_output()?;
            }
            if let Ok(control) = codec.transform.cast::<ICodecAPI>() {
                use windows::Win32::System::Variant::VARIANT;
                let _ = control.SetValue(&CODECAPI_AVLowLatencyMode, &VARIANT::from(true));
                if encoder {
                    let _ = control.SetValue(&CODECAPI_AVEncCommonLowLatency, &VARIANT::from(true));
                    let _ = control.SetValue(
                        &CODECAPI_AVEncCommonRateControlMode,
                        &VARIANT::from(eAVEncCommonRateControlMode_CBR.0 as u32),
                    );
                    let _ = control
                        .SetValue(&CODECAPI_AVEncCommonQualityVsSpeed, &VARIANT::from(0_u32));
                    let _ = control.SetValue(
                        &CODECAPI_AVEncMPVDefaultBPictureCount,
                        &VARIANT::from(0_u32),
                    );
                }
            }
            codec
                .transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)
                .map_err(|e| os("启动编解码", e))?;
            codec
                .transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)
                .map_err(|e| os("启动编解码流", e))?;
        }
        Ok(codec)
    }

    pub fn can_input(&mut self) -> Result<bool> {
        self.pump_events()?;
        Ok(self.events.is_none() || self.need_input > 0 && self.pending < 2)
    }

    pub fn gpu(&self) -> Option<Gpu> {
        self.gpu.clone()
    }
    pub fn encode_texture(
        &mut self,
        texture: &ID3D11Texture2D,
        timestamp_us: u64,
    ) -> Result<Option<(u64, Vec<u8>)>> {
        if self.can_input()? {
            let sample = self
                .input
                .as_mut()
                .ok_or_else(|| Error::Failed("编码器没有 GPU 输入".into()))?
                .texture_sample(texture, timestamp_us, self.refresh)?;
            self.submit(&sample)?;
        }
        self.poll_bytes()
    }
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn encode_bgra(
        &mut self,
        bytes: &[u8],
        timestamp_us: u64,
    ) -> Result<Option<(u64, Vec<u8>)>> {
        if self.can_input()? {
            let sample = self
                .input
                .as_mut()
                .ok_or_else(|| Error::Failed("编码器没有 GPU 输入".into()))?
                .sample(bytes, timestamp_us, self.refresh)?;
            self.submit(&sample)?;
        }
        self.poll_bytes()
    }

    fn submit(&mut self, sample: &IMFSample) -> Result<()> {
        // SAFETY: the sample remains owned by the transform until encoding/decoding completes.
        unsafe { self.transform.ProcessInput(0, sample, 0) }.map_err(|e| os("提交 H.264 帧", e))?;
        if self.events.is_some() {
            self.need_input = self.need_input.saturating_sub(1);
            self.pending += 1;
        }
        Ok(())
    }

    fn pump_events(&mut self) -> Result<()> {
        let Some(events) = &self.events else {
            return Ok(());
        };
        // SAFETY: nonblocking event polling is performed only on the owning media worker.
        unsafe {
            for _ in 0..32 {
                let event = match events.GetEvent(MF_EVENT_FLAG_NO_WAIT) {
                    Ok(event) => event,
                    Err(e) if e.code() == MF_E_NO_EVENTS_AVAILABLE => break,
                    Err(e) => return Err(os("读取硬件编码事件", e)),
                };
                event
                    .GetStatus()
                    .and_then(|status| status.ok())
                    .map_err(|e| os("硬件编码事件状态", e))?;
                match event.GetType().map_err(|e| os("硬件编码事件类型", e))? as i32 {
                    n if n == METransformNeedInput.0 => {
                        self.need_input = self.need_input.saturating_add(1)
                    }
                    n if n == METransformHaveOutput.0 => {
                        self.have_output = self.have_output.saturating_add(1)
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }

    fn select_decoder_output(&mut self) -> Result<()> {
        // SAFETY: 在媒体线程对有效的解码器枚举它支持的输出类型。
        unsafe {
            for index in 0..32 {
                let Ok(media_type) = self.transform.GetOutputAvailableType(0, index) else {
                    break;
                };
                if media_type.GetGUID(&MF_MT_SUBTYPE).ok() != Some(MFVideoFormat_NV12) {
                    continue;
                }
                if let Ok(size) = media_type.GetUINT64(&MF_MT_FRAME_SIZE) {
                    let height = size as u32;
                    if size >> 32 != WIDTH as u64
                        || ![HEIGHT as u32, (HEIGHT as u32).next_multiple_of(16)].contains(&height)
                    {
                        return Err(Error::Failed(format!(
                            "扩展屏视频分辨率与协商不符（{}×{height}）",
                            size >> 32
                        )));
                    }
                    self.storage_height = height as usize;
                }
                self.stride = media_type
                    .GetUINT32(&MF_MT_DEFAULT_STRIDE)
                    .ok()
                    .map(|stride| stride as usize)
                    .filter(|stride| (WIDTH..=4096).contains(stride))
                    .unwrap_or(WIDTH);
                self.transform
                    .SetOutputType(0, &media_type, 0)
                    .map_err(|e| os("设置 NV12 解码输出", e))?;
                return Ok(());
            }
        }
        Err(Error::Failed("H.264 解码器不支持 NV12 输出".into()))
    }

    fn select_encoder_output(&mut self) -> Result<()> {
        // SAFETY: a hardware MFT can announce its final sequence-header/media type
        // after the first input. Negotiate that type before pulling its output again.
        unsafe {
            for index in 0..32 {
                let Ok(media_type) = self.transform.GetOutputAvailableType(0, index) else {
                    break;
                };
                if media_type.GetGUID(&MF_MT_SUBTYPE).ok() != Some(MFVideoFormat_H264) {
                    continue;
                }
                self.transform
                    .SetOutputType(0, &media_type, 0)
                    .map_err(|e| os("更新硬件 H.264 输出类型", e))?;
                return Ok(());
            }
        }
        Err(Error::Failed(
            "硬件编码器未提供更新后的 H.264 输出类型".into(),
        ))
    }

    pub fn process(&mut self, bytes: &[u8], timestamp_us: u64) -> Result<Option<(usize, Vec<u8>)>> {
        if !self.can_input()? {
            return self
                .poll_bytes()
                .map(|output| output.map(|(_, bytes)| (self.stride, bytes)));
        }
        self.push_bytes(bytes, timestamp_us)?;
        self.poll_bytes()
            .map(|output| output.map(|(_, bytes)| (self.stride, bytes)))
    }

    fn push_bytes(&mut self, bytes: &[u8], timestamp_us: u64) -> Result<()> {
        // SAFETY: sample buffer 初始化后随 sample 持有，MFT 不跨线程调用。
        unsafe {
            let sample = sample_with_buffer(bytes.len())?;
            let buffer = sample
                .GetBufferByIndex(0)
                .map_err(|e| os("取得媒体缓冲", e))?;
            let mut raw = null_mut();
            buffer
                .Lock(&mut raw, None, None)
                .map_err(|e| os("锁定媒体输入", e))?;
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), raw, bytes.len());
            buffer.Unlock().map_err(|e| os("解锁媒体输入", e))?;
            buffer
                .SetCurrentLength(bytes.len() as u32)
                .map_err(|e| os("设置媒体输入长度", e))?;
            sample
                .SetSampleTime(timestamp_us.saturating_mul(10).min(i64::MAX as u64) as i64)
                .map_err(|e| os("设置媒体时间", e))?;
            sample
                .SetSampleDuration(10_000_000 / i64::from(self.refresh))
                .map_err(|e| os("设置帧时长", e))?;
            self.submit(&sample)?;
        }
        Ok(())
    }

    pub fn poll_bytes(&mut self) -> Result<Option<(u64, Vec<u8>)>> {
        let Some(sample) = self.poll_sample()? else {
            return Ok(None);
        };
        // SAFETY: output sample is owned here and remains locked only while copying.
        let (timestamp, mut bytes) =
            unsafe { read_sample_bytes(&sample) }.map_err(|e| os("读取 H.264 输出", e))?;
        let start = self.stride * self.storage_height;
        let end = start + self.stride * (HEIGHT / 2);
        if !self.encoder && self.storage_height > HEIGHT && bytes.len() >= end {
            bytes.copy_within(start..end, self.stride * HEIGHT);
            bytes.truncate(self.stride * HEIGHT * 3 / 2);
        }
        Ok(Some((timestamp, bytes)))
    }

    pub fn decode(&mut self, bytes: &[u8], timestamp_us: u64) -> Result<Option<DecodedFrame>> {
        self.push_bytes(bytes, timestamp_us)?;
        let Some(sample) = self.poll_sample()? else {
            return Ok(None);
        };
        // SAFETY: decoded texture stays referenced until the GPU has queued its presentation.
        unsafe {
            let buffer = sample
                .GetBufferByIndex(0)
                .map_err(|e| os("取得解码缓冲", e))?;
            if let Ok(dxgi) = buffer.cast::<IMFDXGIBuffer>() {
                let mut raw = null_mut();
                dxgi.GetResource(&ID3D11Texture2D::IID, &mut raw)
                    .map_err(|e| os("取得解码纹理", e))?;
                if raw.is_null() {
                    return Err(Error::Failed("解码器返回空纹理".into()));
                }
                let texture = ID3D11Texture2D::from_raw(raw);
                let slice = dxgi
                    .GetSubresourceIndex()
                    .map_err(|e| os("解码纹理索引", e))?;
                Ok(Some(DecodedFrame::Texture(texture, slice)))
            } else {
                let (_, bytes) = read_sample_bytes(&sample).map_err(|e| os("读取解码帧", e))?;
                Ok(Some(DecodedFrame::Memory {
                    stride: self.stride,
                    height: self.storage_height,
                    bytes,
                }))
            }
        }
    }

    fn poll_sample(&mut self) -> Result<Option<IMFSample>> {
        self.pump_events()?;
        if self.events.is_some() && self.have_output == 0 {
            return Ok(None);
        }
        for _ in 0..2 {
            if self.events.is_some() {
                self.pump_events()?;
                if self.have_output == 0 {
                    return Ok(None);
                }
                // One ProcessOutput call consumes exactly one HaveOutput event,
                // including calls that report a media-type change.
                self.have_output -= 1;
            }
            match self.output_sample() {
                Err(error) if !self.encoder && error.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                    self.select_decoder_output()?
                }
                Err(error) if self.encoder && error.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                    self.select_encoder_output()?
                }
                Err(error) if error.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(None),
                Err(error) => return Err(os("取得 H.264 输出", error)),
                Ok(bytes) => {
                    if self.events.is_some() {
                        self.pending = self.pending.saturating_sub(1);
                    }
                    return Ok(bytes);
                }
            }
        }
        Ok(None)
    }

    fn output_sample(&self) -> windows::core::Result<Option<IMFSample>> {
        // SAFETY: MFT fills one output descriptor; both optional COM interfaces are dropped after the call.
        unsafe {
            let info = self.transform.GetOutputStreamInfo(0)?;
            let sample = if info.dwFlags
                & ((MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0)
                    as u32)
                == 0
            {
                let sample = MFCreateSample()?;
                sample.AddBuffer(&MFCreateMemoryBuffer(info.cbSize.max(1))?)?;
                Some(sample)
            } else {
                None
            };
            let mut output = MFT_OUTPUT_DATA_BUFFER {
                pSample: ManuallyDrop::new(sample),
                ..Default::default()
            };
            let result = self
                .transform
                .ProcessOutput(0, std::slice::from_mut(&mut output), &mut 0);
            let sample = ManuallyDrop::take(&mut output.pSample);
            ManuallyDrop::drop(&mut output.pEvents);
            result?;
            Ok(sample)
        }
    }
}

unsafe fn read_sample_bytes(sample: &IMFSample) -> windows::core::Result<(u64, Vec<u8>)> {
    // SAFETY: buffer is locked only for this copy and always unlocked before returning.
    unsafe {
        let timestamp = sample.GetSampleTime().unwrap_or(0).max(0) as u64 / 10;
        let buffer = sample.ConvertToContiguousBuffer()?;
        let mut data = null_mut();
        let mut size = 0;
        buffer.Lock(&mut data, None, Some(&mut size))?;
        let bytes = if size == 0 {
            Vec::new()
        } else {
            std::slice::from_raw_parts(data, size as usize).to_vec()
        };
        buffer.Unlock()?;
        Ok((timestamp, bytes))
    }
}

impl Drop for Codec {
    fn drop(&mut self) {
        // SAFETY: shut down only our activated hardware instance, on its owning worker.
        unsafe {
            let _ = self
                .transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0);
            if let Some(activation) = &self.activation {
                let _ = activation.ShutdownObject();
            }
        }
    }
}

fn hardware_encoders() -> Result<Vec<IMFActivate>> {
    // SAFETY: MFTEnumEx allocates an array of owned COM references with CoTaskMemAlloc.
    unsafe {
        let input = MFT_REGISTER_TYPE_INFO {
            guidMajorType: MFMediaType_Video,
            guidSubtype: MFVideoFormat_NV12,
        };
        let output = MFT_REGISTER_TYPE_INFO {
            guidMajorType: MFMediaType_Video,
            guidSubtype: MFVideoFormat_H264,
        };
        let mut array = null_mut();
        let mut count = 0;
        MFTEnumEx(
            MFT_CATEGORY_VIDEO_ENCODER,
            MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER,
            Some(&input),
            Some(&output),
            &mut array,
            &mut count,
        )
        .map_err(|e| os("枚举硬件 H.264 编码器", e))?;
        if array.is_null() {
            return Ok(Vec::new());
        }
        let result = std::slice::from_raw_parts_mut(array, count as usize)
            .iter_mut()
            .filter_map(Option::take)
            .collect();
        CoTaskMemFree(Some(array.cast()));
        Ok(result)
    }
}

fn activation_name(activation: &IMFActivate) -> String {
    // SAFETY: the allocated UTF-16 string is copied before releasing the MF allocation.
    unsafe {
        let mut text = windows::core::PWSTR::null();
        let mut size = 0;
        if activation
            .GetAllocatedString(&MFT_FRIENDLY_NAME_Attribute, &mut text, &mut size)
            .is_err()
        {
            return "Hardware H.264".into();
        }
        let result = String::from_utf16_lossy(std::slice::from_raw_parts(text.0, size as usize));
        CoTaskMemFree(Some(text.0.cast()));
        result
    }
}

fn sample_with_buffer(size: usize) -> Result<IMFSample> {
    // SAFETY: 创建 sample 与它拥有的等长内存缓冲。
    unsafe {
        let sample = MFCreateSample().map_err(|e| os("创建媒体 sample", e))?;
        let buffer = MFCreateMemoryBuffer(size as u32).map_err(|e| os("创建媒体缓冲", e))?;
        sample
            .AddBuffer(&buffer)
            .map_err(|e| os("附加媒体缓冲", e))?;
        Ok(sample)
    }
}

fn video_type(subtype: &GUID, refresh: u32) -> Result<IMFMediaType> {
    // SAFETY: 各属性的键、类型与 Media Foundation 定义一致。
    unsafe {
        let media_type = MFCreateMediaType().map_err(|e| os("创建视频类型", e))?;
        let set = || -> windows::core::Result<()> {
            media_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            media_type.SetGUID(&MF_MT_SUBTYPE, subtype)?;
            media_type.SetUINT64(&MF_MT_FRAME_SIZE, (WIDTH as u64) << 32 | HEIGHT as u64)?;
            media_type.SetUINT64(&MF_MT_FRAME_RATE, u64::from(refresh) << 32 | 1)?;
            media_type.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, 1_u64 << 32 | 1)?;
            media_type.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            Ok(())
        };
        set().map_err(|e| os("配置视频类型", e))?;
        Ok(media_type)
    }
}
