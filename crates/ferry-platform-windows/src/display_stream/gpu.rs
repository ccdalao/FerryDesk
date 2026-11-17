//! D3D11 视频处理、MF 设备管理与低排队交换链。对象只在媒体线程使用。

use super::{FPS, HEIGHT, WIDTH, os};
use ferry_platform::{Error, Result};
use std::mem::ManuallyDrop;
use windows::Win32::Foundation::{DXGI_STATUS_OCCLUDED, HMODULE, HWND, RECT};
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::Graphics::Gdi::{MONITOR_DEFAULTTONEAREST, MonitorFromWindow};
use windows::Win32::Media::MediaFoundation::*;
use windows::core::Interface;

#[derive(Clone)]
pub(super) struct Gpu {
    pub device: ID3D11Device,
    pub context: ID3D11DeviceContext,
    pub video: ID3D11VideoDevice,
    pub video_context: ID3D11VideoContext,
    pub manager: IMFDXGIDeviceManager,
}

// SAFETY: all use stays on the media worker. MF's internal threads use the
// protected immediate context through the DXGI device manager.
unsafe impl Send for Gpu {}

impl Gpu {
    pub fn new(vendor: Option<u32>) -> Result<Self> {
        // SAFETY: own each returned COM reference; no device or context escapes its owner.
        unsafe {
            let factory: IDXGIFactory1 = CreateDXGIFactory1().map_err(|e| os("DXGI factory", e))?;
            let mut selected = None;
            for n in 0..32 {
                let Ok(adapter) = factory.EnumAdapters1(n) else {
                    break;
                };
                let desc = adapter.GetDesc1().map_err(|e| os("GPU 信息", e))?;
                if desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 == 0
                    && vendor.is_none_or(|id| desc.VendorId == id)
                {
                    selected = Some(adapter);
                    break;
                }
            }
            let adapter = selected.ok_or_else(|| Error::Failed("找不到硬件图形适配器".into()))?;
            Self::from_adapter(&adapter)
        }
    }

    pub fn for_window(window: HWND) -> Result<Self> {
        // SAFETY: inspect the receiver's monitor and select its exact adapter.
        // Keeping decode and presentation on the display GPU avoids a DWM copy
        // between the integrated and discrete GPUs on hybrid laptops.
        unsafe {
            let monitor = MonitorFromWindow(window, MONITOR_DEFAULTTONEAREST);
            let factory: IDXGIFactory1 = CreateDXGIFactory1().map_err(|e| os("DXGI factory", e))?;
            for n in 0..32 {
                let Ok(adapter) = factory.EnumAdapters1(n) else {
                    break;
                };
                for m in 0..32 {
                    let Ok(output) = adapter.EnumOutputs(m) else {
                        break;
                    };
                    if output.GetDesc().map_err(|e| os("接收屏幕 GPU", e))?.Monitor == monitor {
                        return Self::from_adapter(&adapter);
                    }
                }
            }
        }
        Err(Error::Failed("找不到接收屏幕所在的图形适配器".into()))
    }

    pub fn from_adapter(adapter: &IDXGIAdapter1) -> Result<Self> {
        // SAFETY: retain device/context references created for this exact adapter.
        unsafe {
            let mut device = None;
            let mut context = None;
            D3D11CreateDevice(
                adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
            .map_err(|e| os("创建 D3D11 视频设备", e))?;
            let device = device.ok_or_else(|| Error::Failed("D3D11 没有返回设备".into()))?;
            let context = context.ok_or_else(|| Error::Failed("D3D11 没有返回上下文".into()))?;
            let protected: ID3D11Multithread =
                context.cast().map_err(|e| os("D3D11 线程保护", e))?;
            let _ = protected.SetMultithreadProtected(true);
            let mut token = 0;
            let mut manager = None;
            MFCreateDXGIDeviceManager(&mut token, &mut manager)
                .map_err(|e| os("MF DXGI manager", e))?;
            let manager =
                manager.ok_or_else(|| Error::Failed("MF 没有返回 DXGI manager".into()))?;
            manager
                .ResetDevice(&device, token)
                .map_err(|e| os("MF 设置 GPU", e))?;
            Ok(Self {
                video: device.cast().map_err(|e| os("D3D11 视频处理设备", e))?,
                video_context: context.cast().map_err(|e| os("D3D11 视频处理上下文", e))?,
                device,
                context,
                manager,
            })
        }
    }

    pub fn texture(&self, format: DXGI_FORMAT, width: u32, height: u32) -> Result<ID3D11Texture2D> {
        // SAFETY: dimensions and bind flags describe a single owned GPU texture.
        unsafe {
            let mut texture = None;
            self.device
                .CreateTexture2D(
                    &D3D11_TEXTURE2D_DESC {
                        Width: width,
                        Height: height,
                        MipLevels: 1,
                        ArraySize: 1,
                        Format: format,
                        SampleDesc: DXGI_SAMPLE_DESC {
                            Count: 1,
                            Quality: 0,
                        },
                        Usage: D3D11_USAGE_DEFAULT,
                        BindFlags: (D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE).0 as u32,
                        ..Default::default()
                    },
                    None,
                    Some(&mut texture),
                )
                .map_err(|e| os("创建视频纹理", e))?;
            texture.ok_or_else(|| Error::Failed("D3D11 没有返回视频纹理".into()))
        }
    }
}

pub(super) struct VideoProcessor {
    gpu: Gpu,
    enumeration: ID3D11VideoProcessorEnumerator,
    processor: ID3D11VideoProcessor,
}

unsafe impl Send for VideoProcessor {}

impl VideoProcessor {
    /// Create a processor with an explicit SDR color contract.
    ///
    /// Windows otherwise lets the video processor infer the NV12 range and
    /// matrix from driver defaults. Those defaults differ between adapters;
    /// on some systems the decoded desktop is expanded twice and appears
    /// clipped/overexposed. ScreenFerry always produces BT.709 studio-range
    /// NV12 and presents full-range sRGB/BGRA.
    pub fn new(gpu: &Gpu, width: u32, height: u32, nv12_output: bool) -> Result<Self> {
        // SAFETY: processor, enumeration and context refer to the same D3D11 device.
        unsafe {
            let enumeration = gpu
                .video
                .CreateVideoProcessorEnumerator(&D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
                    InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
                    InputFrameRate: DXGI_RATIONAL {
                        Numerator: FPS,
                        Denominator: 1,
                    },
                    InputWidth: WIDTH as u32,
                    InputHeight: HEIGHT as u32,
                    OutputFrameRate: DXGI_RATIONAL {
                        Numerator: FPS,
                        Denominator: 1,
                    },
                    OutputWidth: width,
                    OutputHeight: height,
                    Usage: D3D11_VIDEO_USAGE_OPTIMAL_SPEED,
                })
                .map_err(|e| os("GPU 视频处理枚举", e))?;
            let processor = gpu
                .video
                .CreateVideoProcessor(&enumeration, 0)
                .map_err(|e| os("GPU 视频处理器", e))?;
            gpu.video_context
                .VideoProcessorSetStreamAutoProcessingMode(&processor, 0, false);
            gpu.video_context.VideoProcessorSetStreamFrameFormat(
                &processor,
                0,
                D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            );
            let rgb_full = D3D11_VIDEO_PROCESSOR_COLOR_SPACE { _bitfield: 0 };
            // Matrix=BT.709 and nominal range 16..235. The RGB range bit is
            // ignored for YCbCr input/output, but explicit values avoid driver
            // defaults on older adapters.
            let ycbcr_studio_709 = D3D11_VIDEO_PROCESSOR_COLOR_SPACE {
                _bitfield: (1 << 2) | (1 << 4),
            };
            let input_space = if nv12_output {
                rgb_full
            } else {
                ycbcr_studio_709
            };
            let output_space = if nv12_output {
                ycbcr_studio_709
            } else {
                rgb_full
            };
            gpu.video_context
                .VideoProcessorSetStreamColorSpace(&processor, 0, &input_space);
            gpu.video_context
                .VideoProcessorSetOutputColorSpace(&processor, &output_space);
            gpu.video_context.VideoProcessorSetStreamSourceRect(
                &processor,
                0,
                true,
                Some(&RECT {
                    left: 0,
                    top: 0,
                    right: WIDTH as i32,
                    bottom: HEIGHT as i32,
                }),
            );
            let rect = RECT {
                left: 0,
                top: 0,
                right: width as i32,
                bottom: height as i32,
            };
            gpu.video_context
                .VideoProcessorSetStreamDestRect(&processor, 0, true, Some(&rect));
            gpu.video_context
                .VideoProcessorSetOutputTargetRect(&processor, true, Some(&rect));
            Ok(Self {
                gpu: gpu.clone(),
                enumeration,
                processor,
            })
        }
    }

    pub fn blit(
        &self,
        input: &ID3D11Texture2D,
        slice: u32,
        output: &ID3D11Texture2D,
    ) -> Result<()> {
        // SAFETY: views hold their textures until VideoProcessorBlt has queued GPU work.
        unsafe {
            let mut input_view = None;
            self.gpu
                .video
                .CreateVideoProcessorInputView(
                    input,
                    &self.enumeration,
                    &D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
                        ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
                        Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                            Texture2D: D3D11_TEX2D_VPIV {
                                MipSlice: 0,
                                ArraySlice: slice,
                            },
                        },
                        ..Default::default()
                    },
                    Some(&mut input_view),
                )
                .map_err(|e| os("GPU 视频输入视图", e))?;
            let mut output_view = None;
            self.gpu
                .video
                .CreateVideoProcessorOutputView(
                    output,
                    &self.enumeration,
                    &D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                        ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
                        ..Default::default()
                    },
                    Some(&mut output_view),
                )
                .map_err(|e| os("GPU 视频输出视图", e))?;
            let output_view =
                output_view.ok_or_else(|| Error::Failed("GPU 没有返回输出视图".into()))?;
            let mut stream = D3D11_VIDEO_PROCESSOR_STREAM {
                Enable: true.into(),
                pInputSurface: ManuallyDrop::new(input_view),
                ..Default::default()
            };
            let result = self.gpu.video_context.VideoProcessorBlt(
                &self.processor,
                &output_view,
                0,
                std::slice::from_ref(&stream),
            );
            ManuallyDrop::drop(&mut stream.pInputSurface);
            result.map_err(|e| os("GPU 视频颜色转换", e))
        }
    }
}

pub(super) struct EncoderInput {
    gpu: Gpu,
    processor: VideoProcessor,
    bgra: ID3D11Texture2D,
    nv12: Vec<ID3D11Texture2D>,
    next: usize,
}

unsafe impl Send for EncoderInput {}

impl EncoderInput {
    pub fn new(gpu: &Gpu) -> Result<Self> {
        Ok(Self {
            gpu: gpu.clone(),
            processor: VideoProcessor::new(gpu, WIDTH as u32, HEIGHT as u32, true)?,
            bgra: gpu.texture(DXGI_FORMAT_B8G8R8A8_UNORM, WIDTH as u32, HEIGHT as u32)?,
            nv12: (0..3)
                .map(|_| gpu.texture(DXGI_FORMAT_NV12, WIDTH as u32, HEIGHT as u32))
                .collect::<Result<_>>()?,
            next: 0,
        })
    }

    pub fn sample(&mut self, bgra: &[u8], timestamp_us: u64, refresh: u32) -> Result<IMFSample> {
        if bgra.len() != WIDTH * HEIGHT * 4 {
            return Err(Error::Failed("BGRA 帧尺寸无效".into()));
        }
        // SAFETY: upload reads the exact row-pitched buffer. At most two samples are
        // in flight, so rotating three output textures never overwrites an encoder input.
        unsafe {
            self.gpu.context.UpdateSubresource(
                &self.bgra,
                0,
                None,
                bgra.as_ptr().cast(),
                WIDTH as u32 * 4,
                0,
            );
        }
        self.texture_sample(&self.bgra.clone(), timestamp_us, refresh)
    }

    pub fn texture_sample(
        &mut self,
        bgra: &ID3D11Texture2D,
        timestamp_us: u64,
        refresh: u32,
    ) -> Result<IMFSample> {
        // SAFETY: pool ownership and the two-sample encoder limit protect these surfaces.
        unsafe {
            let texture = &self.nv12[self.next];
            self.next = (self.next + 1) % self.nv12.len();
            self.processor.blit(bgra, 0, texture)?;
            // The MFT uses its own context; submit conversion work immediately.
            self.gpu.context.Flush();
            let buffer = MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, texture, 0, false)
                .map_err(|e| os("MF GPU 输入缓冲", e))?;
            let sample = MFCreateSample().map_err(|e| os("MF GPU sample", e))?;
            sample
                .AddBuffer(&buffer)
                .map_err(|e| os("MF 附加 GPU 输入", e))?;
            sample
                .SetSampleTime(timestamp_us.saturating_mul(10).min(i64::MAX as u64) as i64)
                .map_err(|e| os("MF GPU 时间戳", e))?;
            sample
                .SetSampleDuration(10_000_000 / i64::from(refresh))
                .map_err(|e| os("MF GPU 帧时长", e))?;
            Ok(sample)
        }
    }
}

pub(super) struct Presenter {
    gpu: Gpu,
    swap: IDXGISwapChain1,
    processor: VideoProcessor,
    upload: Option<(usize, ID3D11Texture2D)>,
    tearing: bool,
    presented: u64,
    busy: u64,
    occluded: u64,
}

unsafe impl Send for Presenter {}

impl Presenter {
    pub fn new(gpu: &Gpu, window: HWND, width: u32, height: u32) -> Result<Self> {
        // SAFETY: the media worker owns the window and this swap chain.
        unsafe {
            let factory: IDXGIFactory2 =
                CreateDXGIFactory1().map_err(|e| os("DXGI 呈现 factory", e))?;
            let tearing = factory.cast::<IDXGIFactory5>().ok().is_some_and(|factory| {
                let mut supported = windows::core::BOOL::default();
                factory
                    .CheckFeatureSupport(
                        DXGI_FEATURE_PRESENT_ALLOW_TEARING,
                        (&mut supported as *mut windows::core::BOOL).cast(),
                        size_of_val(&supported) as u32,
                    )
                    .is_ok()
                    && supported.as_bool()
            });
            let swap = factory
                .CreateSwapChainForHwnd(
                    &gpu.device,
                    window,
                    &DXGI_SWAP_CHAIN_DESC1 {
                        Width: width,
                        Height: height,
                        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                        SampleDesc: DXGI_SAMPLE_DESC {
                            Count: 1,
                            Quality: 0,
                        },
                        BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                        // As in Moonlight's D3D11 renderer, SyncInterval 0 replaces
                        // pending frames. Spare buffers prevent DWM starvation;
                        // limiting IDXGIDevice1 latency to one would force a wait
                        // for DWM even though presentation is nonblocking.
                        BufferCount: 5,
                        SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
                        Flags: if tearing {
                            DXGI_SWAP_CHAIN_FLAG_ALLOW_TEARING.0 as u32
                        } else {
                            0
                        },
                        ..Default::default()
                    },
                    None,
                    None,
                )
                .map_err(|e| os("创建低延迟交换链", e))?;
            let _ = factory.MakeWindowAssociation(window, DXGI_MWA_NO_ALT_ENTER);
            Ok(Self {
                gpu: gpu.clone(),
                swap,
                processor: VideoProcessor::new(gpu, width, height, false)?,
                upload: None,
                tearing,
                presented: 0,
                busy: 0,
                occluded: 0,
            })
        }
    }

    pub fn description(&self) -> String {
        format!(
            "flip-discard/5 buffers tearing={} presented={} busy={} occluded={}",
            self.tearing, self.presented, self.busy, self.occluded
        )
    }

    pub fn texture(&mut self, texture: &ID3D11Texture2D, slice: u32) -> Result<bool> {
        // SAFETY: back buffer and decoded texture belong to the same protected D3D11 device.
        unsafe {
            let output: ID3D11Texture2D = self
                .swap
                .GetBuffer(0)
                .map_err(|e| os("取得交换链缓冲", e))?;
            self.processor.blit(texture, slice, &output)?;
            let flags = DXGI_PRESENT_DO_NOT_WAIT
                | if self.tearing {
                    DXGI_PRESENT_ALLOW_TEARING
                } else {
                    DXGI_PRESENT(0)
                };
            let result = self.swap.Present(0, flags);
            if result == DXGI_ERROR_WAS_STILL_DRAWING {
                self.busy += 1;
                return Ok(false);
            }
            if result == DXGI_STATUS_OCCLUDED {
                self.occluded += 1;
                return Ok(false);
            }
            result.ok().map_err(|e| os("GPU 呈现视频帧", e))?;
            self.presented += 1;
            Ok(true)
        }
    }

    pub fn nv12(&mut self, stride: usize, storage_height: usize, bytes: &[u8]) -> Result<bool> {
        if stride < WIDTH || bytes.len() < stride * storage_height * 3 / 2 {
            return Err(Error::Failed("NV12 帧尺寸无效".into()));
        }
        if self
            .upload
            .as_ref()
            .is_none_or(|(height, _)| *height != storage_height)
        {
            self.upload = Some((
                storage_height,
                self.gpu
                    .texture(DXGI_FORMAT_NV12, WIDTH as u32, storage_height as u32)?,
            ));
        }
        let texture = self
            .upload
            .as_ref()
            .expect("upload was initialized")
            .1
            .clone();
        // SAFETY: the NV12 allocation contains both planes, with the same row pitch.
        unsafe {
            self.gpu.context.UpdateSubresource(
                &texture,
                0,
                None,
                bytes.as_ptr().cast(),
                stride as u32,
                0,
            )
        };
        self.texture(&texture, 0)
    }
}
