//! 把小尺寸光标图像上传后在 GPU 合成，避免为光标读回整张桌面。

use super::gpu::Gpu;
use super::{HEIGHT, WIDTH, os};
use ferry_platform::{Error, Result};
use std::ptr::null_mut;
use windows::Win32::Foundation::POINTL;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Dxgi::{IDXGIDevice, IDXGISurface};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::Interface;

pub(super) struct Cursor {
    context: ID2D1DeviceContext,
    target: ID2D1Bitmap1,
    bitmap: Option<ID2D1Bitmap1>,
    shape: HCURSOR,
    hotspot: (u32, u32),
    size: (u32, u32),
}

// SAFETY: the cursor renderer stays on its owning media thread.
unsafe impl Send for Cursor {}

impl Cursor {
    pub fn new(gpu: &Gpu, texture: &ID3D11Texture2D) -> Result<Self> {
        // SAFETY: D2D and the target share the capture device and its protected context.
        unsafe {
            let factory: ID2D1Factory1 = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)
                .map_err(|e| os("光标 D2D factory", e))?;
            let dxgi: IDXGIDevice = gpu.device.cast().map_err(|e| os("光标 DXGI 设备", e))?;
            let device = factory
                .CreateDevice(&dxgi)
                .map_err(|e| os("光标 D2D 设备", e))?;
            let context = device
                .CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)
                .map_err(|e| os("光标 D2D 上下文", e))?;
            let surface: IDXGISurface = texture.cast().map_err(|e| os("光标合成表面", e))?;
            let target = context
                .CreateBitmapFromDxgiSurface(
                    &surface,
                    Some(&D2D1_BITMAP_PROPERTIES1 {
                        pixelFormat: D2D1_PIXEL_FORMAT {
                            format: DXGI_FORMAT_B8G8R8A8_UNORM,
                            alphaMode: D2D1_ALPHA_MODE_IGNORE,
                        },
                        dpiX: 96.0,
                        dpiY: 96.0,
                        bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET | D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
                        ..Default::default()
                    }),
                )
                .map_err(|e| os("光标合成目标", e))?;
            context.SetTarget(&target);
            Ok(Self {
                context,
                target,
                bitmap: None,
                shape: HCURSOR::default(),
                hotspot: (0, 0),
                size: (0, 0),
            })
        }
    }

    pub fn draw(&mut self, origin: POINTL) -> Result<()> {
        // SAFETY: query the cursor without changing it. Upload only when its shape changes.
        unsafe {
            let mut cursor = CURSORINFO {
                cbSize: size_of::<CURSORINFO>() as u32,
                ..Default::default()
            };
            if GetCursorInfo(&mut cursor).is_err() || cursor.flags != CURSOR_SHOWING {
                return Ok(());
            }
            let x = cursor.ptScreenPos.x - origin.x;
            let y = cursor.ptScreenPos.y - origin.y;
            if !(0..WIDTH as i32).contains(&x) || !(0..HEIGHT as i32).contains(&y) {
                return Ok(());
            }
            if self.bitmap.is_none() || self.shape != cursor.hCursor {
                let raster = match raster(cursor.hCursor) {
                    Ok(raster) => raster,
                    Err(error) => {
                        // Cursor handles may change while querying their bitmap
                        // (animated cursors or a desktop switch). A transient
                        // missing shape must not terminate the video session.
                        tracing::debug!(%error, "光标形状切换中，等待下一帧重试");
                        return Ok(());
                    }
                };
                let Raster {
                    pixels,
                    size: (width, height),
                    hotspot,
                } = raster;
                let bitmap = self
                    .context
                    .CreateBitmap(
                        D2D_SIZE_U { width, height },
                        Some(pixels.as_ptr().cast()),
                        width * 4,
                        &D2D1_BITMAP_PROPERTIES1 {
                            pixelFormat: D2D1_PIXEL_FORMAT {
                                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                            },
                            dpiX: 96.0,
                            dpiY: 96.0,
                            ..Default::default()
                        },
                    )
                    .map_err(|e| os("上传光标纹理", e))?;
                self.bitmap = Some(bitmap);
                self.shape = cursor.hCursor;
                self.size = (width, height);
                self.hotspot = hotspot;
            }
            let left = x as f32 - self.hotspot.0 as f32;
            let top = y as f32 - self.hotspot.1 as f32;
            let rect = D2D_RECT_F {
                left,
                top,
                right: left + self.size.0 as f32,
                bottom: top + self.size.1 as f32,
            };
            self.context.SetTarget(&self.target);
            self.context.BeginDraw();
            let bitmap = self
                .bitmap
                .as_ref()
                .ok_or_else(|| Error::Failed("光标纹理为空".into()))?;
            self.context.DrawBitmap(
                bitmap,
                Some(&rect),
                1.0,
                D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR,
                None,
                None,
            );
            self.context
                .EndDraw(None, None)
                .map_err(|e| os("合成 GPU 光标", e))?;
        }
        Ok(())
    }
}

struct Raster {
    pixels: Vec<u8>,
    size: (u32, u32),
    hotspot: (u32, u32),
}

fn raster(cursor: HCURSOR) -> Result<Raster> {
    // SAFETY: bitmap copies returned by GetIconInfo are deleted, and all temporary
    // GDI objects are unselected/released before returning, including error paths.
    unsafe {
        let mut icon = ICONINFO::default();
        GetIconInfo(HICON(cursor.0), &mut icon).map_err(|e| os("读取光标形状", e))?;
        let result = (|| {
            let mut bitmap = BITMAP::default();
            let color = !icon.hbmColor.is_invalid();
            if GetObjectW(
                if color {
                    icon.hbmColor.into()
                } else {
                    icon.hbmMask.into()
                },
                size_of::<BITMAP>() as i32,
                Some((&mut bitmap as *mut BITMAP).cast()),
            ) == 0
            {
                return Err(Error::Failed("无法读取光标位图尺寸".into()));
            }
            let width = bitmap.bmWidth.unsigned_abs();
            let height = bitmap.bmHeight.unsigned_abs() / if color { 1 } else { 2 };
            if width == 0 || height == 0 || width > 256 || height > 256 {
                return Err(Error::Failed("光标位图尺寸无效".into()));
            }
            let dc = CreateCompatibleDC(None);
            if dc.is_invalid() {
                return Err(Error::Failed("无法创建光标画布".into()));
            }
            let mut info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width as i32,
                    biHeight: -(height as i32),
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut raw = null_mut();
            let dib = match CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut raw, None, 0) {
                Ok(dib) => dib,
                Err(e) => {
                    let _ = DeleteDC(dc);
                    return Err(os("创建光标位图", e));
                }
            };
            if raw.is_null() || dib.is_invalid() {
                let _ = DeleteObject(dib.into());
                let _ = DeleteDC(dc);
                return Err(Error::Failed("创建光标位图失败".into()));
            }
            let old = SelectObject(dc, dib.into());
            let pixels = std::slice::from_raw_parts_mut(
                raw as *mut u8,
                width as usize * height as usize * 4,
            );
            pixels.fill(0);
            let drawn = DrawIconEx(
                dc,
                0,
                0,
                HICON(cursor.0),
                width as i32,
                height as i32,
                0,
                None,
                DI_NORMAL,
            );
            let _ = GdiFlush();
            let mut result = pixels.to_vec();
            SelectObject(dc, old);
            let _ = DeleteObject(dib.into());
            if result.chunks_exact(4).all(|p| p[3] == 0) {
                let mut mask = vec![0_u8; result.len()];
                if !icon.hbmMask.is_invalid()
                    && GetDIBits(
                        dc,
                        icon.hbmMask,
                        0,
                        height,
                        Some(mask.as_mut_ptr().cast()),
                        &mut info,
                        DIB_RGB_COLORS,
                    ) != 0
                {
                    for (pixel, mask) in result.chunks_exact_mut(4).zip(mask.chunks_exact(4)) {
                        if mask[0] == 0 || pixel[..3].iter().any(|&value| value != 0) {
                            pixel[3] = 255;
                        }
                    }
                }
            }
            let _ = DeleteDC(dc);
            drawn.map_err(|e| os("绘制光标形状", e))?;
            Ok(Raster {
                pixels: result,
                size: (width, height),
                hotspot: (icon.xHotspot, icon.yHotspot),
            })
        })();
        if !icon.hbmMask.is_invalid() {
            let _ = DeleteObject(icon.hbmMask.into());
        }
        if !icon.hbmColor.is_invalid() {
            let _ = DeleteObject(icon.hbmColor.into());
        }
        result
    }
}
