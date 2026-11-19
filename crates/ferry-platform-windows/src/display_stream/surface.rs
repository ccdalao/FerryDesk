//! 只在明确识别到 Virtual Display Driver 时操作虚拟显示器。

use crate::hid_from_windows;
use ferry_core::Pressed;
use ferry_types::{
    DisplayOptions, DisplayPrimary, DisplayScreen, InputEvent, KeyAction, MouseButton, Point,
};
mod topology;
use std::collections::VecDeque;
use std::ffi::c_void;
use std::ptr::null_mut;
use std::sync::atomic::{AtomicBool, Ordering};
use topology::Topology;

use ferry_platform::{Displays, Error, Result};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, POINTL, WPARAM};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::StationsAndDesktops::{
    CloseDesktop, DESKTOP_READOBJECTS, GetUserObjectInformationW, OpenInputDesktop, UOI_NAME,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, GetCapture, VK_LBUTTON, VK_MBUTTON, VK_RBUTTON,
};
use windows::Win32::UI::Input::{GetCurrentInputMessageSource, IMO_HARDWARE, INPUT_MESSAGE_SOURCE};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

use super::codec::DecodedFrame;
use super::gpu::{Gpu, Presenter};
use super::{FPS, HEIGHT, WIDTH, os};
use crate::display::{WinDisplays, last_error};

static MONITOR_LEASE: AtomicBool = AtomicBool::new(false);

struct MonitorLease;

impl MonitorLease {
    fn acquire() -> Result<Self> {
        for _ in 0..40 {
            if MONITOR_LEASE
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return Ok(Self);
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        Err(Error::Failed("上一轮虚拟屏会话仍在结束".into()))
    }
}

impl Drop for MonitorLease {
    fn drop(&mut self) {
        MONITOR_LEASE.store(false, Ordering::Release);
    }
}

pub(super) struct VirtualMonitor {
    name: [u16; 32],
    position: POINTL,
    refresh: u32,
    restore: Option<DEVMODEW>,
    owned: bool,
    topology: Topology,
    _lease: MonitorLease,
}

impl VirtualMonitor {
    pub fn attach_with_options(options: DisplayOptions) -> Result<Self> {
        let lease = MonitorLease::acquire()?;
        let device = virtual_adapter()?;
        let owned = super::driver::owns_adapter()?;
        if !owned && device.StateFlags.contains(DISPLAY_DEVICE_PRIMARY_DEVICE) {
            return Err(Error::Failed("不能接管其他应用的主显示器".into()));
        }
        if !owned && options.primary == DisplayPrimary::Viewer {
            return Err(Error::Failed(
                "将扩展屏设为主屏需要 ScreenFerry 自己安装的虚拟显示设备".into(),
            ));
        }
        let source = PCWSTR(device.DeviceName.as_ptr());
        // Our adapter is session-owned. An attached output without a live lease
        // is a stale session, not a display layout that should be restored later.
        if owned
            && device
                .StateFlags
                .contains(DISPLAY_DEVICE_ATTACHED_TO_DESKTOP)
        {
            detach_installed_monitor()?;
        }
        let current = display_mode(source, ENUM_CURRENT_SETTINGS, EDS_RAWMODE);
        let topology = Topology::capture(&device.DeviceName)?;
        let attached = !owned
            && device
                .StateFlags
                .contains(DISPLAY_DEVICE_ATTACHED_TO_DESKTOP);
        let previous = attached.then_some(current).flatten();
        if attached && previous.is_none() {
            return Err(Error::Failed("无法读取虚拟屏的当前模式".into()));
        }
        let mut best = None;
        for index in 0..512 {
            let Some(mode) = display_mode(
                source,
                ENUM_DISPLAY_SETTINGS_MODE(index),
                ENUM_DISPLAY_SETTINGS_FLAGS(0),
            ) else {
                break;
            };
            if mode.dmPelsWidth == WIDTH as u32
                && mode.dmPelsHeight == HEIGHT as u32
                && (60..=FPS).contains(&mode.dmDisplayFrequency)
                && best
                    .as_ref()
                    .is_none_or(|best: &DEVMODEW| best.dmDisplayFrequency < mode.dmDisplayFrequency)
            {
                best = Some(mode);
            }
        }
        let mut mode = best
            .ok_or_else(|| Error::Failed("虚拟显示驱动未提供 1920×1080 60–100 Hz 模式".into()))?;
        mode.dmFields |= DM_POSITION | DM_PELSWIDTH | DM_PELSHEIGHT | DM_DISPLAYFREQUENCY;
        let mut monitor = Self {
            name: device.DeviceName,
            position: POINTL::default(),
            refresh: mode.dmDisplayFrequency,
            restore: previous,
            owned,
            topology,
            _lease: lease,
        };
        monitor.position = monitor
            .topology
            .apply(&monitor.name, mode, options, owned)?;
        if !display_mode(
            PCWSTR(monitor.name.as_ptr()),
            ENUM_CURRENT_SETTINGS,
            ENUM_DISPLAY_SETTINGS_FLAGS(0),
        )
        .is_some_and(|active| {
            active.dmPelsWidth == WIDTH as u32 && active.dmPelsHeight == HEIGHT as u32
        }) {
            return Err(Error::Failed("Windows 未激活扩展屏".into()));
        }
        Ok(monitor)
    }

    pub const fn refresh(&self) -> u32 {
        self.refresh
    }

    pub fn calibration(
        &self,
        options: DisplayOptions,
    ) -> Result<Option<super::alignment::CursorAlignment>> {
        if options.alignment.is_default() {
            return Ok(None);
        }
        let screens = self.topology.screens();
        let anchor = match options.source_screen {
            Some(id) => screens.iter().find(|s| s.id == id),
            None => screens.iter().find(|s| s.primary).or(screens.first()),
        }
        .ok_or_else(|| Error::Failed("对齐方案的来源屏幕已断开".into()))?;
        let original = ferry_core::display_extension_bounds(
            anchor.bounds,
            WIDTH as u32,
            HEIGHT as u32,
            options.side,
        )
        .ok_or_else(|| Error::Failed("对齐范围超出桌面".into()))?;
        let local = ferry_types::Rect::new(
            anchor.bounds.x + self.position.x - original.x,
            anchor.bounds.y + self.position.y - original.y,
            anchor.bounds.width,
            anchor.bounds.height,
        );
        let remote = ferry_types::Rect::new(
            self.position.x,
            self.position.y,
            WIDTH as u32,
            HEIGHT as u32,
        );
        super::alignment::CursorAlignment::start(local, remote, options.side, options.alignment)
    }
    pub const fn name(&self) -> &[u16; 32] {
        &self.name
    }

    pub fn current_position(&self) -> Result<POINTL> {
        let mode = display_mode(
            PCWSTR(self.name.as_ptr()),
            ENUM_CURRENT_SETTINGS,
            EDS_RAWMODE,
        )
        .ok_or_else(|| Error::Failed("扩展屏已断开".into()))?;
        // SAFETY: the current mode returned by Windows contains its position.
        Ok(unsafe { mode.Anonymous1.Anonymous2.dmPosition })
    }
}

impl Drop for VirtualMonitor {
    fn drop(&mut self) {
        let mut result = Ok(());
        for _ in 0..3 {
            result = self
                .topology
                .restore(&self.name, self.restore.as_ref(), self.owned);
            if result.is_ok() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        if let Err(error) = result {
            tracing::error!(%error, "回收本次虚拟屏失败");
        } else {
            tracing::info!("本次虚拟屏已回收");
        }
        restore_physical_pointer();
    }
}

/// The same target is excluded from ordinary keyboard-sharing geometry and
/// viewer selection. The hardware ID alone is insufficient for recovery ownership.
pub(crate) fn managed_monitor_name() -> Result<Option<[u16; 32]>> {
    if !super::driver::owns_adapter()? {
        return Ok(None);
    }
    Ok(virtual_adapter().ok().map(|device| device.DeviceName))
}

pub(super) fn recover_monitor() -> Result<()> {
    let _lease = MonitorLease::acquire()?;
    if !super::driver::owns_adapter()? {
        return Ok(());
    }
    if let Ok(device) = virtual_adapter()
        && (device
            .StateFlags
            .contains(DISPLAY_DEVICE_ATTACHED_TO_DESKTOP)
            || Topology::needs_recovery()?)
    {
        detach_installed_monitor()?;
        restore_physical_pointer();
        tracing::info!("已回收上次异常退出残留的虚拟屏");
    }
    Ok(())
}

fn restore_physical_pointer() {
    // SAFETY: move only a pointer which is outside every remaining shared screen.
    unsafe {
        let mut at = POINT::default();
        if GetCursorPos(&mut at).is_err() {
            return;
        }
        let Ok(screens) = WinDisplays::new().displays() else {
            return;
        };
        if screens
            .iter()
            .any(|screen| screen.bounds.contains(Point::new(at.x, at.y)))
        {
            return;
        }
        if let Some((screen, target)) = screens
            .iter()
            .map(|screen| {
                let left = screen.bounds.x;
                let top = screen.bounds.y;
                let right = left + screen.bounds.width as i32 - 1;
                let bottom = top + screen.bounds.height as i32 - 1;
                let target = POINT {
                    x: at.x.clamp(left, right),
                    y: at.y.clamp(top, bottom),
                };
                let dx = i64::from(target.x - at.x);
                let dy = i64::from(target.y - at.y);
                (screen, target, dx * dx + dy * dy)
            })
            .min_by_key(|(_, _, distance)| *distance)
            .map(|(screen, target, _)| (screen, target))
        {
            tracing::debug!(
                from_x = at.x,
                from_y = at.y,
                to_x = target.x,
                to_y = target.y,
                screen = ?screen.id,
                "扩展屏回收后将光标放回最近实体屏边界"
            );
            let _ = SetCursorPos(target.x, target.y);
        }
    }
}

fn virtual_adapter() -> Result<DISPLAY_DEVICEW> {
    let mut selected = None;
    for index in 0..64 {
        let mut device = DISPLAY_DEVICEW {
            cb: size_of::<DISPLAY_DEVICEW>() as u32,
            ..Default::default()
        };
        // SAFETY: device is a writable DISPLAY_DEVICEW with its size set.
        if !unsafe { EnumDisplayDevicesW(PCWSTR::null(), index, &mut device, 0) }.as_bool() {
            break;
        }
        let end = device
            .DeviceID
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(device.DeviceID.len());
        let id = String::from_utf16_lossy(&device.DeviceID[..end]).to_ascii_uppercase();
        let length = device
            .DeviceString
            .iter()
            .position(|&ch| ch == 0)
            .unwrap_or(device.DeviceString.len());
        let description = String::from_utf16_lossy(&device.DeviceString[..length]);
        if id
            .split('\\')
            .any(|part| matches!(part, "MTTVDD" | "SCREENFERRYDISPLAY"))
            || description == "Virtual Display Driver"
        {
            if selected.is_some() {
                return Err(Error::Failed(
                    "发现多个 Virtual Display Driver 显示目标".into(),
                ));
            }
            selected = Some(device);
        }
    }
    selected.ok_or_else(|| Error::Failed("找不到 Virtual Display Driver 虚拟显示设备".into()))
}

pub(super) fn detach_installed_monitor() -> Result<()> {
    let device = virtual_adapter()?;
    Topology::recover(&device.DeviceName)
}

pub(super) fn physical_screens() -> Result<Vec<DisplayScreen>> {
    let excluded = virtual_adapter().map(|d| d.DeviceName).unwrap_or([0; 32]);
    Ok(Topology::capture(&excluded)?.screens())
}

fn display_mode(
    name: PCWSTR,
    index: ENUM_DISPLAY_SETTINGS_MODE,
    flags: ENUM_DISPLAY_SETTINGS_FLAGS,
) -> Option<DEVMODEW> {
    let mut mode = DEVMODEW {
        dmSize: size_of::<DEVMODEW>() as u16,
        ..Default::default()
    };
    // SAFETY: name is null terminated and mode has the required size.
    unsafe { EnumDisplaySettingsExW(name, index, &mut mode, flags) }
        .as_bool()
        .then_some(mode)
}

fn bitmap() -> BITMAPINFO {
    BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: WIDTH as i32,
            biHeight: -(HEIGHT as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    }
}

pub(super) struct Capture {
    name: [u16; 32],
    position: POINTL,
    screen: HDC,
    memory: HDC,
    dib: HBITMAP,
    original: HGDIOBJ,
    pixels: *mut u8,
    nv12: Vec<u8>,
}

// SAFETY: Handles and pixel pointer are exclusively owned by the capture object.
// The engine constructs, uses, and releases it on its media worker.
unsafe impl Send for Capture {}

impl Capture {
    pub fn new(monitor: &VirtualMonitor) -> Result<Self> {
        let mut capture = Self {
            name: monitor.name,
            position: monitor.position,
            screen: HDC::default(),
            memory: HDC::default(),
            dib: HBITMAP::default(),
            original: HGDIOBJ::default(),
            pixels: null_mut(),
            nv12: vec![0; WIDTH * HEIGHT * 3 / 2],
        };
        // SAFETY: All handles are retained until Drop, the bitmap has the stated size and pixel layout.
        unsafe {
            capture.screen = GetDC(None);
            if capture.screen.is_invalid() {
                return Err(last_error("GetDC"));
            }
            capture.memory = CreateCompatibleDC(Some(capture.screen));
            if capture.memory.is_invalid() {
                return Err(last_error("CreateCompatibleDC"));
            }
            let mut data = null_mut();
            capture.dib = CreateDIBSection(
                Some(capture.screen),
                &bitmap(),
                DIB_RGB_COLORS,
                &mut data,
                None,
                0,
            )
            .map_err(|e| os("CreateDIBSection", e))?;
            capture.pixels = data.cast();
            capture.original = SelectObject(capture.memory, capture.dib.into());
            if capture.original.is_invalid() || capture.pixels.is_null() {
                return Err(last_error("SelectObject"));
            }
        }
        Ok(capture)
    }

    pub fn nv12(&mut self) -> Result<&[u8]> {
        self.bgra()?;
        // SAFETY: pixels belongs to our DIB, already synchronized by bgra().
        let rgb = unsafe { std::slice::from_raw_parts(self.pixels, WIDTH * HEIGHT * 4) };
        rgb_to_nv12(rgb, &mut self.nv12);
        Ok(&self.nv12)
    }

    pub fn bgra(&mut self) -> Result<&[u8]> {
        self.snapshot(true)
    }

    pub fn desktop_bgra(&mut self) -> Result<&[u8]> {
        self.snapshot(false)
    }

    fn snapshot(&mut self, draw_cursor: bool) -> Result<&[u8]> {
        let mode = display_mode(
            PCWSTR(self.name.as_ptr()),
            ENUM_CURRENT_SETTINGS,
            ENUM_DISPLAY_SETTINGS_FLAGS(0),
        )
        .ok_or_else(|| Error::Failed("扩展屏配置已更改".into()))?;
        if mode.dmPelsWidth != WIDTH as u32 || mode.dmPelsHeight != HEIGHT as u32 {
            return Err(Error::Failed("扩展屏分辨率已更改".into()));
        }
        // SAFETY: EnumDisplaySettingsExW returns the current position in the display mode union.
        self.position = unsafe { mode.Anonymous1.Anonymous2.dmPosition };
        // SAFETY: Copy only the selected virtual output into our own bitmap. GdiFlush synchronizes writes to its memory.
        unsafe {
            BitBlt(
                self.memory,
                0,
                0,
                WIDTH as i32,
                HEIGHT as i32,
                Some(self.screen),
                self.position.x,
                self.position.y,
                SRCCOPY,
            )
            .map_err(|e| os("BitBlt", e))?;
            let mut cursor = CURSORINFO {
                cbSize: size_of::<CURSORINFO>() as u32,
                ..Default::default()
            };
            if draw_cursor
                && GetCursorInfo(&mut cursor).is_ok()
                && cursor.flags == CURSOR_SHOWING
                && !cursor.hCursor.is_invalid()
            {
                let x = cursor.ptScreenPos.x - self.position.x;
                let y = cursor.ptScreenPos.y - self.position.y;
                if (0..WIDTH as i32).contains(&x) && (0..HEIGHT as i32).contains(&y) {
                    let mut icon = ICONINFO::default();
                    if GetIconInfo(HICON(cursor.hCursor.0), &mut icon).is_ok() {
                        let _ = DrawIconEx(
                            self.memory,
                            x - icon.xHotspot as i32,
                            y - icon.yHotspot as i32,
                            HICON(cursor.hCursor.0),
                            0,
                            0,
                            0,
                            None,
                            DI_NORMAL,
                        );
                        if !icon.hbmMask.is_invalid() {
                            let _ = DeleteObject(icon.hbmMask.into());
                        }
                        if !icon.hbmColor.is_invalid() {
                            let _ = DeleteObject(icon.hbmColor.into());
                        }
                    }
                }
            }
            let _ = GdiFlush();
            Ok(std::slice::from_raw_parts(self.pixels, WIDTH * HEIGHT * 4))
        }
    }
}

pub(super) fn normal_desktop() -> bool {
    use windows::Win32::Foundation::HANDLE;
    // SAFETY: Read only the name of the current input desktop and release the handle immediately.
    unsafe {
        let Ok(desktop) = OpenInputDesktop(Default::default(), false, DESKTOP_READOBJECTS) else {
            return false;
        };
        let mut name = [0_u16; 64];
        let ok = GetUserObjectInformationW(
            HANDLE(desktop.0),
            UOI_NAME,
            Some(name.as_mut_ptr().cast()),
            size_of_val(&name) as u32,
            None,
        )
        .is_ok();
        let _ = CloseDesktop(desktop);
        ok && String::from_utf16_lossy(
            &name[..name.iter().position(|&ch| ch == 0).unwrap_or(name.len())],
        )
        .eq_ignore_ascii_case("Default")
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        // SAFETY: Release only the GDI handles created by this object.
        unsafe {
            if !self.original.is_invalid() {
                SelectObject(self.memory, self.original);
            }
            if !self.dib.is_invalid() {
                let _ = DeleteObject(self.dib.into());
            }
            if !self.memory.is_invalid() {
                let _ = DeleteDC(self.memory);
            }
            if !self.screen.is_invalid() {
                ReleaseDC(None, self.screen);
            }
        }
    }
}

pub(super) struct Viewer {
    window: HWND,
    rgb: Vec<u8>,
    shown: bool,
    events: VecDeque<InputEvent>,
    pressed: Pressed,
    pointer: Point,
    presenter: Option<Presenter>,
    size: (u32, u32),
    hardware_input_messages: u64,
    suppressed_input_messages: u64,
}

// SAFETY: The engine constructs, uses, and destroys the window on the same media worker.
unsafe impl Send for Viewer {}

impl Viewer {
    pub fn window(&self) -> HWND {
        self.window
    }

    pub fn description(&self) -> String {
        self.presenter
            .as_ref()
            .map(Presenter::description)
            .unwrap_or_else(|| "GDI presentation".into())
    }

    pub fn with_options(options: DisplayOptions) -> Result<Self> {
        let displays = physical_screens()?;
        let bounds = match options.viewer_screen {
            Some(id) => displays.iter().find(|d| d.id == id),
            None => displays.iter().find(|d| !d.primary).or(displays.first()),
        }
        .ok_or_else(|| Error::Failed("方案指定的接收屏幕已断开".into()))?
        .bounds;
        // SAFETY: register a window class and create a window belonging to the current thread.
        let window = unsafe {
            let instance = GetModuleHandleW(None).map_err(|e| os("GetModuleHandleW", e))?;
            let class = w!("ScreenFerryDisplayViewer");
            RegisterClassW(&WNDCLASSW {
                lpfnWndProc: Some(view_proc),
                hInstance: instance.into(),
                lpszClassName: class,
                ..Default::default()
            });
            CreateWindowExW(
                // NOACTIVATE keeps the local taskbar and keyboard focus out of
                // the media window while TOPMOST lets the streamed desktop
                // cover shell/taskbar and ordinary always-on-top lyric windows.
                WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
                class,
                w!("ScreenFerry 扩展屏"),
                WS_POPUP,
                bounds.x,
                bounds.y,
                bounds.width as i32,
                bounds.height as i32,
                None,
                None,
                Some(instance.into()),
                None,
            )
            .map_err(|e| os("创建扩展屏窗口", e))?
        };
        tracing::info!(
            x = bounds.x,
            y = bounds.y,
            width = bounds.width,
            height = bounds.height,
            "扩展屏接收窗口使用实体桌面"
        );
        Ok(Self {
            window,
            rgb: vec![0; WIDTH * HEIGHT * 4],
            shown: false,
            events: VecDeque::new(),
            pressed: Pressed::new(),
            pointer: Point::default(),
            presenter: None,
            size: (bounds.width, bounds.height),
            hardware_input_messages: 0,
            suppressed_input_messages: 0,
        })
    }

    pub fn enable_gpu(&mut self, gpu: &Gpu) -> Result<()> {
        self.presenter = Some(Presenter::new(gpu, self.window, self.size.0, self.size.1)?);
        tracing::info!("扩展屏启用 GPU 解码与交换链呈现");
        Ok(())
    }

    pub fn present_decoded(&mut self, frame: DecodedFrame) -> Result<bool> {
        if !self.shown {
            // SAFETY: the media worker owns this window.
            unsafe {
                let _ = ShowWindow(self.window, SW_SHOWNOACTIVATE);
            }
            self.shown = true;
        }
        self.raise_above_shell();
        match (frame, &mut self.presenter) {
            (DecodedFrame::Texture(texture, slice), Some(presenter)) => {
                presenter.texture(&texture, slice)
            }
            (
                DecodedFrame::Memory {
                    stride,
                    height,
                    bytes,
                },
                Some(presenter),
            ) => presenter.nv12(stride, height, &bytes),
            (
                DecodedFrame::Memory {
                    stride,
                    height,
                    mut bytes,
                },
                None,
            ) => {
                let start = stride * height;
                let end = start + stride * HEIGHT / 2;
                if height > HEIGHT && bytes.len() >= end {
                    bytes.copy_within(start..end, stride * HEIGHT);
                }
                self.present_nv12(stride, &bytes)?;
                Ok(true)
            }
            _ => Err(Error::Failed("GPU 解码帧没有可用的呈现设备".into())),
        }
    }

    pub fn events(&mut self) -> Result<()> {
        self.raise_above_shell();
        // SAFETY: Only this thread owns the window. Pump messages while rendering.
        unsafe {
            let mut message = MSG::default();
            // Bound each pump so a high-rate mouse cannot starve decode/present.
            // Leave excess messages in Windows until input() drains our queue;
            // queue pressure must not tear down an otherwise healthy display.
            for _ in 0..128 {
                if self.events.len() >= 240
                    || !PeekMessageW(&mut message, Some(self.window), 0, 0, PM_REMOVE).as_bool()
                {
                    break;
                }
                if message.message == WM_CLOSE {
                    return Err(Error::Cancelled);
                }
                if matches!(message.message, WM_KEYDOWN | WM_SYSKEYDOWN)
                    && message.wParam.0 == 0x1b
                    && GetAsyncKeyState(0x11) < 0
                    && GetAsyncKeyState(0x12) < 0
                {
                    return Err(Error::Cancelled);
                }
                self.input_message(&message)?;
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
            // WM_KILLFOCUS is sent directly to the window procedure, rather than queued.
            // The viewer is intentionally NOACTIVATE so the local shell stays
            // usable. Do not treat that expected state as focus loss while a
            // mouse button is captured for a drag.
            let mouse_captured = GetCapture() == self.window;
            let mouse_held = [VK_LBUTTON, VK_RBUTTON, VK_MBUTTON]
                .into_iter()
                .any(|button| GetAsyncKeyState(i32::from(button.0)) < 0);
            if GetForegroundWindow() != self.window && !mouse_captured && !mouse_held {
                self.events.extend(self.pressed.release_all(self.pointer));
            }
        }
        Ok(())
    }

    pub fn next_input(&mut self) -> Result<Option<InputEvent>> {
        self.events()?;
        Ok(self.events.pop_front())
    }

    fn input_message(&mut self, message: &MSG) -> Result<()> {
        // WM_MOUSEMOVE is also synthesized when a topmost window changes hit
        // testing. Forwarding a stationary receiver's system notifications
        // overwrites the source's independently moving hardware pointer.
        // Query immediately after PeekMessage, before dispatch changes context.
        // UIAccess SendInput can be labelled HARDWARE, so reject our tag too.
        if matches!(message.message, WM_MOUSEFIRST..=WM_MOUSELAST | WM_KEYFIRST..=WM_KEYLAST) {
            let mut source = INPUT_MESSAGE_SOURCE::default();
            // SAFETY: source is a valid output buffer, and PeekMessage selected
            // the current message on this window's owning thread.
            unsafe { GetCurrentInputMessageSource(&mut source) }
                .map_err(|e| os("GetCurrentInputMessageSource", e))?;
            // SAFETY: retrieves metadata for the same message on this thread.
            let physical = unsafe {
                source.originId == IMO_HARDWARE
                    && GetMessageExtraInfo().0 as usize != crate::inject::FERRY_INPUT_TAG
            };
            if !physical {
                self.suppressed_input_messages += 1;
                return Ok(());
            }
            self.hardware_input_messages += 1;
        }
        let event = match message.message {
            WM_MOUSEMOVE | WM_LBUTTONDOWN | WM_LBUTTONUP | WM_RBUTTONDOWN | WM_RBUTTONUP
            | WM_MBUTTONDOWN | WM_MBUTTONUP => {
                let x = message.lParam.0 as i16 as i32;
                let y = (message.lParam.0 >> 16) as i16 as i32;
                let mut rect = Default::default();
                // SAFETY: self.window belongs to this worker thread and rect is writable.
                unsafe { GetClientRect(self.window, &mut rect) }
                    .map_err(|e| os("GetClientRect", e))?;
                self.pointer = Point::new(
                    x * WIDTH as i32 / rect.right.max(1),
                    y * HEIGHT as i32 / rect.bottom.max(1),
                );
                let button = match message.message {
                    WM_LBUTTONDOWN | WM_LBUTTONUP => Some(MouseButton::LEFT),
                    WM_RBUTTONDOWN | WM_RBUTTONUP => Some(MouseButton::RIGHT),
                    WM_MBUTTONDOWN | WM_MBUTTONUP => Some(MouseButton::MIDDLE),
                    _ => None,
                };
                if let Some(button) = button {
                    Some(InputEvent::Button {
                        button,
                        pressed: matches!(
                            message.message,
                            WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN
                        ),
                        position: self.pointer,
                    })
                } else {
                    Some(InputEvent::PointerMotion {
                        position: self.pointer,
                    })
                }
            }
            WM_MOUSEWHEEL | WM_MOUSEHWHEEL => {
                let delta = (message.wParam.0 >> 16) as i16 as i32;
                Some(InputEvent::Wheel {
                    position: self.pointer,
                    dx: if message.message == WM_MOUSEHWHEEL {
                        delta
                    } else {
                        0
                    },
                    dy: if message.message == WM_MOUSEWHEEL {
                        delta
                    } else {
                        0
                    },
                })
            }
            WM_KEYDOWN | WM_KEYUP | WM_SYSKEYDOWN | WM_SYSKEYUP => {
                let scan = (message.lParam.0 >> 16) as u16 & 0xff;
                let extended = message.lParam.0 & (1 << 24) != 0;
                hid_from_windows(message.wParam.0 as u16, scan, extended).map(|code| {
                    InputEvent::Key {
                        code,
                        action: if matches!(message.message, WM_KEYUP | WM_SYSKEYUP) {
                            KeyAction::Up
                        } else if message.lParam.0 & (1 << 30) != 0 {
                            KeyAction::Repeat
                        } else {
                            KeyAction::Down
                        },
                    }
                })
            }
            WM_KILLFOCUS => {
                self.events.extend(self.pressed.release_all(self.pointer));
                None
            }
            _ => None,
        };
        if let Some(event) = event {
            self.pressed.track(&event);
            // Only adjacent motions can replace each other. Preserve every key,
            // wheel and button boundary and its own absolute click position.
            if matches!(event, InputEvent::PointerMotion { .. })
                && let Some(previous @ InputEvent::PointerMotion { .. }) = self.events.back_mut()
            {
                *previous = event;
            } else {
                self.events.push_back(event);
            }
        }
        Ok(())
    }

    pub fn present_nv12(&mut self, stride: usize, pixels: &[u8]) -> Result<()> {
        if stride < WIDTH || pixels.len() < stride * HEIGHT * 3 / 2 {
            return Err(Error::Failed("视频解码帧的尺寸不正确".into()));
        }
        nv12_to_rgb(pixels, stride, &mut self.rgb);
        // SAFETY: The source has the exact 32-bit top-down DIB layout. The DC is released before returning.
        unsafe {
            if !self.shown {
                let _ = ShowWindow(self.window, SW_SHOWNOACTIVATE);
                self.shown = true;
            }
            self.raise_above_shell();
            let mut rect = Default::default();
            GetClientRect(self.window, &mut rect).map_err(|e| os("GetClientRect", e))?;
            let dc = GetDC(Some(self.window));
            if dc.is_invalid() {
                return Err(last_error("GetDC"));
            }
            let output = StretchDIBits(
                dc,
                0,
                0,
                rect.right,
                rect.bottom,
                0,
                0,
                WIDTH as i32,
                HEIGHT as i32,
                Some(self.rgb.as_ptr().cast::<c_void>()),
                &bitmap(),
                DIB_RGB_COLORS,
                SRCCOPY,
            );
            ReleaseDC(Some(self.window), dc);
            if output <= 0 {
                return Err(last_error("StretchDIBits"));
            }
        }
        Ok(())
    }

    /// Shell surfaces and lyrics players can reassert their topmost z-order
    /// without activating their window. Reassert ours on every media/input
    /// tick so the viewer remains a true full-screen surface.
    fn raise_above_shell(&self) {
        // SAFETY: this media worker owns the window and only changes its z-order.
        unsafe {
            let _ = SetWindowPos(
                self.window,
                Some(HWND_TOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
        }
    }
}

unsafe extern "system" fn view_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // SAFETY: change the cursor only over this window's video client area.
    // The video already includes the source cursor; retaining the local one
    // creates two pointers. Outside this window Windows selects the usual cursor.
    unsafe {
        if msg == WM_SETCURSOR && lparam.0 as u16 == HTCLIENT as u16 {
            SetCursor(None);
            return LRESULT(1);
        }
        DefWindowProcW(hwnd, msg, wparam, lparam)
    }
}

impl Drop for Viewer {
    fn drop(&mut self) {
        tracing::info!(
            hardware_messages = self.hardware_input_messages,
            suppressed_messages = self.suppressed_input_messages,
            "扩展屏接收输入来源统计"
        );
        self.presenter = None;
        // SAFETY: The viewer releases its own window on the creating thread.
        unsafe {
            let _ = DestroyWindow(self.window);
            if let Ok(cursor) = LoadCursorW(None, IDC_ARROW) {
                SetCursor(Some(cursor));
            }
        }
    }
}

fn rgb_to_nv12(rgb: &[u8], dst: &mut [u8]) {
    let (luma, chroma) = dst.split_at_mut(WIDTH * HEIGHT);
    for (index, pixel) in rgb.chunks_exact(4).enumerate() {
        let (b, g, r) = (
            i32::from(pixel[0]),
            i32::from(pixel[1]),
            i32::from(pixel[2]),
        );
        // BT.709 limited-range coefficients. The GPU path uses the same
        // matrix/range contract; BT.601 here caused a visible color shift
        // when the decoder fell back to memory presentation.
        luma[index] = (((47 * r + 157 * g + 16 * b + 128) >> 8) + 16) as u8;
        let (x, y) = (index % WIDTH, index / WIDTH);
        if (x | y) & 1 == 0 {
            let uv = y / 2 * WIDTH + x;
            chroma[uv] = (((-26 * r - 87 * g + 112 * b + 128) >> 8) + 128) as u8;
            chroma[uv + 1] = (((112 * r - 102 * g - 10 * b + 128) >> 8) + 128) as u8;
        }
    }
}

fn nv12_to_rgb(nv12: &[u8], stride: usize, dst: &mut [u8]) {
    let (luma, chroma) = nv12.split_at(stride * HEIGHT);
    for (index, pixel) in dst.chunks_exact_mut(4).enumerate() {
        let (x, y) = (index % WIDTH, index / WIDTH);
        let uv = y / 2 * stride + (x & !1);
        let yy = i32::from(luma[y * stride + x]) - 16;
        let u = i32::from(chroma[uv]) - 128;
        let v = i32::from(chroma[uv + 1]) - 128;
        let value = |chroma: i32| ((298 * yy + chroma + 128) >> 8).clamp(0, 255) as u8;
        pixel.copy_from_slice(&[
            value(540 * u),
            value(-54 * u - 136 * v),
            value(459 * v),
            255,
        ]);
    }
}
