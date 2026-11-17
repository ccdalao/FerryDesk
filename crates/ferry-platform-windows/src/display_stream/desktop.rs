//! 只捕获本次虚拟显示输出的 GPU 桌面帧。

use super::cursor::Cursor;
use super::gpu::Gpu;
use super::{HEIGHT, WIDTH, os};
use ferry_platform::{Error, Result};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::core::Interface;

pub(super) struct DesktopCapture {
    gpu: Gpu,
    duplication: IDXGIOutputDuplication,
    output: IDXGIOutput1,
    pub needs_recreate: bool,
    image: ID3D11Texture2D,
    has_image: bool,
    cursor_separate: bool,
    pub bridge: bool,
    staging: Option<ID3D11Texture2D>,
    pixels: Vec<u8>,
    composited: ID3D11Texture2D,
    cursor: Cursor,
    origin: windows::Win32::Foundation::POINTL,
}

// SAFETY: created and used only on the media worker owning the D3D11 context.
unsafe impl Send for DesktopCapture {}

impl DesktopCapture {
    pub fn new(name: &[u16; 32], gpu: &Gpu) -> Result<Self> {
        // SAFETY: duplicate only the selected virtual output using its owning GPU.
        unsafe {
            let factory: IDXGIFactory1 =
                CreateDXGIFactory1().map_err(|e| os("DXGI 捕获 factory", e))?;
            for n in 0..32 {
                let Ok(adapter) = factory.EnumAdapters1(n) else {
                    break;
                };
                for m in 0..32 {
                    let Ok(output) = adapter.EnumOutputs(m) else {
                        break;
                    };
                    let desc = output.GetDesc().map_err(|e| os("DXGI 输出信息", e))?;
                    if desc
                        .DeviceName
                        .iter()
                        .take_while(|&&ch| ch != 0)
                        .ne(name.iter().take_while(|&&ch| ch != 0))
                    {
                        continue;
                    }
                    let output: IDXGIOutput1 =
                        output.cast().map_err(|e| os("DXGI 输出捕获接口", e))?;
                    let (gpu, duplication, bridge) = match output.DuplicateOutput(&gpu.device) {
                        Ok(duplication) => (gpu.clone(), duplication, false),
                        Err(_) => {
                            // A vendor can expose several adapters. Duplication
                            // requires the exact adapter owning this output.
                            let owner = Gpu::from_adapter(&adapter)?;
                            let duplication = output
                                .DuplicateOutput(&owner.device)
                                .map_err(|e| os("DXGI 捕获虚拟屏", e))?;
                            (owner, duplication, true)
                        }
                    };
                    let image =
                        gpu.texture(DXGI_FORMAT_B8G8R8A8_UNORM, WIDTH as u32, HEIGHT as u32)?;
                    let composited =
                        gpu.texture(DXGI_FORMAT_B8G8R8A8_UNORM, WIDTH as u32, HEIGHT as u32)?;
                    let cursor = Cursor::new(&gpu, &composited)?;
                    return Ok(Self {
                        gpu,
                        duplication,
                        output,
                        image,
                        has_image: false,
                        cursor_separate: true,
                        needs_recreate: false,
                        bridge,
                        staging: None,
                        pixels: vec![0; WIDTH * HEIGHT * 4],
                        composited,
                        cursor,
                        origin: windows::Win32::Foundation::POINTL {
                            x: desc.DesktopCoordinates.left,
                            y: desc.DesktopCoordinates.top,
                        },
                    });
                }
            }
        }
        Err(Error::Failed("DXGI 未找到本次虚拟显示输出".into()))
    }

    pub fn pixels(&mut self) -> Result<&[u8]> {
        // SAFETY: readback is needed only when capture and encode use different GPUs.
        // Rows are copied using DXGI's reported pitch; Map/Unmap are paired.
        unsafe {
            if self.staging.is_none() {
                let mut desc = D3D11_TEXTURE2D_DESC::default();
                self.image.GetDesc(&mut desc);
                desc.Usage = D3D11_USAGE_STAGING;
                desc.BindFlags = 0;
                desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
                self.gpu
                    .device
                    .CreateTexture2D(&desc, None, Some(&mut self.staging))
                    .map_err(|e| os("跨 GPU 捕获缓冲", e))?;
            }
            let staging = self
                .staging
                .as_ref()
                .ok_or_else(|| Error::Failed("跨 GPU 缓冲为空".into()))?;
            self.gpu.context.CopyResource(staging, &self.composited);
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            self.gpu
                .context
                .Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
                .map_err(|e| os("读取跨 GPU 捕获帧", e))?;
            if mapped.RowPitch < WIDTH as u32 * 4 || mapped.pData.is_null() {
                self.gpu.context.Unmap(staging, 0);
                return Err(Error::Failed("跨 GPU 捕获步幅无效".into()));
            }
            for y in 0..HEIGHT {
                std::ptr::copy_nonoverlapping(
                    (mapped.pData as *const u8).add(y * mapped.RowPitch as usize),
                    self.pixels.as_mut_ptr().add(y * WIDTH * 4),
                    WIDTH * 4,
                );
            }
            self.gpu.context.Unmap(staging, 0);
        }
        Ok(&self.pixels)
    }

    pub fn frame(&mut self) -> Result<Option<ID3D11Texture2D>> {
        // SAFETY: every successful acquire is paired with ReleaseFrame, including failures.
        unsafe {
            let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
            let mut resource = None;
            match self
                .duplication
                .AcquireNextFrame(0, &mut info, &mut resource)
            {
                Ok(()) => {
                    let copied = (|| -> Result<()> {
                        let resource =
                            resource.ok_or_else(|| Error::Failed("DXGI 捕获返回空资源".into()))?;
                        let texture: ID3D11Texture2D =
                            resource.cast().map_err(|e| os("DXGI 桌面纹理", e))?;
                        let mut desc = D3D11_TEXTURE2D_DESC::default();
                        texture.GetDesc(&mut desc);
                        if desc.Width != WIDTH as u32 || desc.Height != HEIGHT as u32 {
                            return Err(Error::Failed("DXGI 捕获分辨率已更改".into()));
                        }
                        self.gpu.context.CopyResource(&self.image, &texture);
                        // PointerPosition is only an update when DXGI reports a
                        // mouse timestamp. Desktop-only frames otherwise carry
                        // a zero/default visibility flag: applying it makes the
                        // streamed cursor blink between mouse and desktop updates.
                        if info.LastMouseUpdateTime != 0 {
                            self.cursor_separate = info.PointerPosition.Visible.as_bool();
                        }
                        self.has_image = true;
                        Ok(())
                    })();
                    let released = self.duplication.ReleaseFrame();
                    copied?;
                    if let Err(error) = released {
                        if transient_capture_error(error.code()) {
                            self.invalidate();
                            return Ok(None);
                        }
                        return Err(os("释放 DXGI 捕获帧", error));
                    }
                }
                Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => {}
                Err(e) if transient_capture_error(e.code()) => {
                    // Re-enumerate through a fresh factory after display changes.
                    // Reusing this output can repeatedly duplicate an obsolete target.
                    self.invalidate();
                    return Ok(None);
                }
                Err(e) => return Err(os("DXGI 捕获桌面帧", e)),
            }
        }
        if self.has_image {
            // SAFETY: read the current virtual output position without changing its layout.
            let desc = match unsafe { self.output.GetDesc() } {
                Ok(desc) => desc,
                Err(error) if transient_capture_error(error.code()) => {
                    self.invalidate();
                    return Ok(None);
                }
                Err(error) => return Err(os("更新 DXGI 显示位置", error)),
            };
            self.origin.x = desc.DesktopCoordinates.left;
            self.origin.y = desc.DesktopCoordinates.top;
            // SAFETY: clear the previous cursor before drawing this frame's cursor.
            unsafe {
                self.gpu.context.CopyResource(&self.composited, &self.image);
            }
            if self.cursor_separate {
                self.cursor.draw(self.origin)?;
            }
            Ok(Some(self.composited.clone()))
        } else {
            Ok(None)
        }
    }
    fn invalidate(&mut self) {
        self.needs_recreate = true;
        self.has_image = false;
    }
    /// A newly attached, unchanged desktop may not produce a duplication update.
    /// Seed its initial image once; subsequent updates still come from DXGI.
    pub fn seed(&mut self, pixels: &[u8]) -> Result<()> {
        if pixels.len() != WIDTH * HEIGHT * 4 {
            return Err(Error::Failed("DXGI 初始桌面图像大小无效".into()));
        }
        // SAFETY: the source contains all BGRA rows required by the owned texture.
        unsafe {
            self.gpu.context.UpdateSubresource(
                &self.image,
                0,
                None,
                pixels.as_ptr().cast(),
                (WIDTH * 4) as u32,
                0,
            );
        }
        self.has_image = true;
        self.cursor_separate = true;
        Ok(())
    }
}

fn transient_capture_error(code: windows::core::HRESULT) -> bool {
    // These errors occur during mode/desktop/session transitions. Recreate the
    // duplication interface through a fresh factory, as Apollo/Sunshine do.
    code == DXGI_ERROR_ACCESS_LOST
        || code == DXGI_ERROR_ACCESS_DENIED
        || code == DXGI_ERROR_DEVICE_REMOVED
        || code == DXGI_ERROR_DEVICE_RESET
        || code == DXGI_ERROR_SESSION_DISCONNECTED
        || code == windows::Win32::Foundation::E_ACCESSDENIED
        || code == windows::core::HRESULT(windows::Win32::Foundation::WAIT_ABANDONED.0 as i32)
}
