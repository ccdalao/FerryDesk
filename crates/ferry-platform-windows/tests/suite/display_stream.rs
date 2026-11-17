mod transport;

#[test]
#[ignore = "真实 Win32 输入回归，会临时占屏并移动光标约半秒，随后恢复"]
fn stationary_viewer_does_not_overwrite_source_pointer() {
    use ferry_platform::DisplayBackend;
    use ferry_platform_windows::WinDisplayBackend;
    use ferry_types::{InputEvent, Point};
    use std::time::Duration;
    use windows::Win32::Foundation::{HWND, POINT, RECT};
    use windows::Win32::UI::Input::{GetCurrentInputMessageSource, INPUT_MESSAGE_SOURCE};
    use windows::Win32::UI::WindowsAndMessaging::*;
    use windows::core::w;

    struct Restore(POINT);
    impl Drop for Restore {
        fn drop(&mut self) {
            // SAFETY: restore the pointer position saved by this explicit desktop test.
            unsafe {
                let _ = SetCursorPos(self.0.x, self.0.y);
            }
        }
    }
    let mut original = POINT::default();
    // SAFETY: original is a valid writable POINT.
    unsafe { GetCursorPos(&mut original) }.unwrap();
    let _restore = Restore(original);
    let mut viewer = WinDisplayBackend.viewer(false).unwrap();
    // SAFETY: find and inspect only the receiver owned by this test thread.
    let window = unsafe {
        let window = FindWindowW(w!("ScreenFerryDisplayViewer"), w!("ScreenFerry 扩展屏")).unwrap();
        assert_eq!(
            GetWindowThreadProcessId(window, None),
            windows::Win32::System::Threading::GetCurrentThreadId()
        );
        let mut bounds = RECT::default();
        GetWindowRect(window, &mut bounds).unwrap();
        SetCursorPos(
            (bounds.left + bounds.right) / 2,
            (bounds.top + bounds.bottom) / 2,
        )
        .unwrap();
        window
    };
    // Clear messages caused by creating/showing the window and the explicit warp.
    // The bound is essential because the broken implementation manufactures more
    // WM_MOUSEMOVE notifications on every input() pump, even without hardware input.
    for _ in 0..20 {
        let _ = viewer.input().unwrap();
        std::thread::sleep(Duration::from_millis(5));
    }
    // Model a source-side physical mouse moving away from the receiver's parked
    // position. Any receiver motion below becomes DisplayInput and overwrites it.
    let native_source_position = Point::new(1517, 733);
    let mut source_position = native_source_position;
    let mut stationary_motions = 0;
    // Reproduce CursorHider's one-pixel topmost window competing with the
    // receiver. Its periodic keep-alive changes hit testing without hardware
    // motion, then Viewer::events puts the video back above it.
    struct Window(HWND);
    impl Drop for Window {
        fn drop(&mut self) {
            // SAFETY: this test creates and owns the competing window.
            unsafe {
                let _ = DestroyWindow(self.0);
            }
        }
    }
    let mut parked = POINT::default();
    // SAFETY: create a one-pixel owned helper without activating it.
    let competitor = unsafe {
        GetCursorPos(&mut parked).unwrap();
        Window(
            CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                w!("STATIC"),
                w!(""),
                WS_POPUP,
                parked.x,
                parked.y,
                1,
                1,
                None,
                None,
                None,
                None,
            )
            .unwrap(),
        )
    };
    let mut printed_provenance = false;
    for _ in 0..40 {
        // SAFETY: change only the z-order of this test's one-pixel helper.
        unsafe {
            SetWindowPos(
                competitor.0,
                Some(HWND_TOPMOST),
                parked.x,
                parked.y,
                1,
                1,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            )
            .unwrap();
        }
        // SAFETY: observe queued notification provenance without removing it.
        unsafe {
            let mut message = MSG::default();
            if PeekMessageW(
                &mut message,
                Some(window),
                WM_MOUSEMOVE,
                WM_MOUSEMOVE,
                PM_NOREMOVE,
            )
            .as_bool()
                && !printed_provenance
            {
                printed_provenance = true;
                let mut provenance = INPUT_MESSAGE_SOURCE::default();
                let queried = GetCurrentInputMessageSource(&mut provenance);
                println!(
                    "stationary queued mousemove provenance={provenance:?}, query={queried:?}, pt={:?}",
                    message.pt
                );
            }
        }
        if let Some(InputEvent::PointerMotion { position }) = viewer.input().unwrap() {
            source_position = position;
            stationary_motions += 1;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    println!(
        "stationary receiver motions={stationary_motions}, source native={native_source_position:?}, source after forwarding={source_position:?}"
    );
    // SAFETY: test only observes the visibility of its own receiver.
    println!("viewer visible={}", unsafe {
        IsWindowVisible(window).as_bool()
    });
    assert_eq!(
        stationary_motions, 0,
        "没有物理输入时，接收窗口的层级刷新不得制造回传鼠标移动"
    );
    assert_eq!(
        source_position, native_source_position,
        "接收端静止坐标不得覆盖来源电脑的物理鼠标位置"
    );
}

#[test]
#[ignore = "真实 Win32 输入回归，会短暂占屏并测试系统定位及注入移动，随后恢复光标"]
fn synthetic_viewer_motion_is_not_forwarded() {
    use ferry_platform::{DisplayBackend, DisplayViewer, InputInject};
    use ferry_platform_windows::{WinDisplayBackend, WinInject};
    use ferry_types::{InputEvent, Point};
    use std::time::Duration;
    use windows::Win32::Foundation::{POINT, RECT};
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_MOVE, MOUSEINPUT, SendInput,
    };
    use windows::Win32::UI::WindowsAndMessaging::*;
    use windows::core::w;

    struct Restore(POINT);
    impl Drop for Restore {
        fn drop(&mut self) {
            // SAFETY: restore the pointer saved before this explicit test.
            unsafe {
                let _ = SetCursorPos(self.0.x, self.0.y);
            }
        }
    }
    fn assert_no_forwarded_input(viewer: &mut dyn DisplayViewer, origin: &str) {
        let mut forwarded = Vec::new();
        for _ in 0..20 {
            if let Some(event) = viewer.input().unwrap() {
                forwarded.push(event);
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(
            forwarded.is_empty(),
            "{origin} 不得作为接收电脑的物理输入回传，实际 {forwarded:?}"
        );
    }
    let mut original = POINT::default();
    // SAFETY: original is a valid writable POINT.
    unsafe { GetCursorPos(&mut original) }.unwrap();
    let _restore = Restore(original);
    let mut viewer = WinDisplayBackend.viewer(false).unwrap();
    let mut bounds = RECT::default();
    // SAFETY: inspect only the receiver owned by this test thread.
    let window = unsafe {
        let window = FindWindowW(w!("ScreenFerryDisplayViewer"), w!("ScreenFerry 扩展屏")).unwrap();
        assert_eq!(
            GetWindowThreadProcessId(window, None),
            windows::Win32::System::Threading::GetCurrentThreadId()
        );
        GetWindowRect(window, &mut bounds).unwrap();
        window
    };
    assert_no_forwarded_input(viewer.as_mut(), "创建接收窗口");
    let center = Point::new(
        (bounds.left + bounds.right) / 2,
        (bounds.top + bounds.bottom) / 2,
    );
    // SAFETY: move within the test's own fullscreen receiver, without clicks.
    unsafe { SetCursorPos(center.x - 50, center.y - 50) }.unwrap();
    assert_no_forwarded_input(viewer.as_mut(), "SetCursorPos");
    // Untagged injection must be rejected by Windows' origin classification.
    let injection = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: 17,
                dy: 11,
                dwFlags: MOUSEEVENTF_MOVE,
                ..Default::default()
            },
        },
    };
    // SAFETY: inject a single relative motion over the test receiver, no buttons.
    assert_eq!(
        unsafe { SendInput(&[injection], size_of::<INPUT>() as i32) },
        1
    );
    assert_no_forwarded_input(viewer.as_mut(), "未标记 SendInput");
    // Production injection carries the application's marker. This also covers
    // UIAccess processes whose SendInput can be reported with HARDWARE origin.
    WinInject::new()
        .inject(&InputEvent::PointerMotion { position: center })
        .unwrap();
    assert_no_forwarded_input(viewer.as_mut(), "程序标记 SendInput");
    // SAFETY: post a synthetic position only to the window owned by this test.
    unsafe {
        PostMessageW(
            Some(window),
            WM_MOUSEMOVE,
            windows::Win32::Foundation::WPARAM(0),
            windows::Win32::Foundation::LPARAM(123 | (234 << 16)),
        )
    }
    .unwrap();
    assert_no_forwarded_input(viewer.as_mut(), "PostMessage WM_MOUSEMOVE");
}

#[test]
#[ignore = "人工正向验证，会占屏最多30秒，需要用本机实体鼠标移动并按住左键拖动后松开"]
fn physical_viewer_motion_and_drag_are_forwarded() {
    use ferry_platform::DisplayBackend;
    use ferry_platform_windows::WinDisplayBackend;
    use ferry_types::{InputEvent, MouseButton};
    use std::cell::Cell;
    use std::time::{Duration, Instant};
    use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
    use windows::Win32::Graphics::Gdi::*;
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
    use windows::Win32::UI::Input::{
        GetCurrentInputMessageSource, IMO_HARDWARE, INPUT_MESSAGE_SOURCE,
    };
    use windows::Win32::UI::WindowsAndMessaging::*;
    use windows::core::w;

    struct Restore {
        pointer: POINT,
        foreground: HWND,
    }
    impl Drop for Restore {
        fn drop(&mut self) {
            // SAFETY: restore only the cursor and foreground observed before
            // this explicit desktop test, after its receiver has been destroyed.
            unsafe {
                let _ = SetCursorPos(self.pointer.x, self.pointer.y);
                if IsWindow(Some(self.foreground)).as_bool() {
                    let _ = SetForegroundWindow(self.foreground);
                }
            }
        }
    }
    struct TestFont(HFONT);
    impl Drop for TestFont {
        fn drop(&mut self) {
            // SAFETY: this test creates and owns the font; each paint restores
            // the previous selected font before releasing its DC.
            unsafe {
                let _ = DeleteObject(self.0.into());
            }
        }
    }
    thread_local! {
        static ORIGINAL_PROC: Cell<isize> = const { Cell::new(0) };
        static RAW_HARDWARE_MOVES: Cell<usize> = const { Cell::new(0) };
        static RAW_SYSTEM_MOVES: Cell<usize> = const { Cell::new(0) };
    }
    unsafe extern "system" fn input_probe(
        window: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        // SAFETY: this subclass runs on the test receiver's owning thread. It
        // observes the message before calling the saved procedure exactly once.
        unsafe {
            if matches!(message, WM_MOUSEFIRST..=WM_MOUSELAST) {
                let mut provenance = INPUT_MESSAGE_SOURCE::default();
                let queried = GetCurrentInputMessageSource(&mut provenance);
                let log = if message == WM_MOUSEMOVE {
                    let counts = if provenance.originId == IMO_HARDWARE {
                        &RAW_HARDWARE_MOVES
                    } else {
                        &RAW_SYSTEM_MOVES
                    };
                    counts.with(|count| {
                        let n = count.get();
                        count.set(n + 1);
                        n < 8
                    })
                } else {
                    true
                };
                if log {
                    println!(
                        "RAW msg=0x{message:04x} wparam=0x{:x} MK_LBUTTON={} async_left={} source={provenance:?} source_ok={} extra=0x{:x} client=({}, {})",
                        wparam.0,
                        wparam.0 & 1 != 0,
                        GetAsyncKeyState(i32::from(VK_LBUTTON.0)) < 0,
                        queried.is_ok(),
                        GetMessageExtraInfo().0,
                        lparam.0 as i16,
                        (lparam.0 >> 16) as i16
                    );
                }
            }
            let old = ORIGINAL_PROC.with(Cell::get);
            assert_ne!(old, 0);
            // SAFETY: old is the window procedure returned by SetWindowLongPtrW
            // for this same window immediately before installing this callback.
            let procedure: WNDPROC = Some(std::mem::transmute::<
                isize,
                unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT,
            >(old));
            let result = CallWindowProcW(procedure, window, message, wparam, lparam);
            if matches!(message, WM_MOUSEACTIVATE | WM_SETFOCUS | WM_KILLFOCUS) {
                println!(
                    "RAW activation msg=0x{message:04x} result={} foreground={:?}",
                    result.0,
                    GetForegroundWindow()
                );
            }
            result
        }
    }
    struct Probe(HWND, isize);
    impl Drop for Probe {
        fn drop(&mut self) {
            // SAFETY: restore the receiver's original procedure before Viewer
            // destroys its window. Both operations happen on the creating thread.
            unsafe {
                SetWindowLongPtrW(self.0, GWLP_WNDPROC, self.1);
            }
            ORIGINAL_PROC.with(|procedure| procedure.set(0));
        }
    }
    fn paint(window: HWND, font: HFONT, lines: &[String], pointer: Option<ferry_types::Point>) {
        // SAFETY: window belongs to this test thread. The DC, selected objects,
        // and temporary brush are restored/released before returning.
        unsafe {
            let dc = GetDC(Some(window));
            assert!(!dc.is_invalid());
            let mut client = RECT::default();
            GetClientRect(window, &mut client).unwrap();
            let brush = CreateSolidBrush(COLORREF(0x0029_2013));
            FillRect(dc, &client, brush);
            let _ = DeleteObject(brush.into());
            let previous = SelectObject(dc, font.into());
            SetTextColor(dc, COLORREF(0x00ff_eeee));
            SetBkMode(dc, TRANSPARENT);
            for (index, line) in lines.iter().enumerate() {
                let text: Vec<u16> = line.encode_utf16().collect();
                let _ = TextOutW(dc, 36, 36 + index as i32 * 48, &text);
            }
            if let Some(pointer) = pointer {
                let (x, y) = (
                    pointer.x * client.right / 1920,
                    pointer.y * client.bottom / 1080,
                );
                let old_pen = SelectObject(dc, GetStockObject(WHITE_PEN));
                let old_brush = SelectObject(dc, GetStockObject(DC_BRUSH));
                SetDCBrushColor(dc, COLORREF(0x0044_ddff));
                let _ = Ellipse(dc, x - 8, y - 8, x + 8, y + 8);
                SelectObject(dc, old_brush);
                SelectObject(dc, old_pen);
            }
            SelectObject(dc, previous);
            ReleaseDC(Some(window), dc);
        }
    }
    let mut original = POINT::default();
    // SAFETY: original is a valid writable POINT.
    unsafe { GetCursorPos(&mut original) }.unwrap();
    let _restore = Restore {
        pointer: original,
        // SAFETY: read-only capture of the current foreground window.
        foreground: unsafe { GetForegroundWindow() },
    };
    let mut viewer = WinDisplayBackend.viewer(false).unwrap();
    // SAFETY: locate only the receiver owned by this test thread, and create a
    // font which the local guard releases after all painting has completed.
    let (window, font) = unsafe {
        let window = FindWindowW(w!("ScreenFerryDisplayViewer"), w!("ScreenFerry 扩展屏")).unwrap();
        assert_eq!(
            GetWindowThreadProcessId(window, None),
            windows::Win32::System::Threading::GetCurrentThreadId()
        );
        let mut descriptor = LOGFONTW {
            lfHeight: -30,
            lfWeight: 600,
            ..Default::default()
        };
        let face: Vec<u16> = "Segoe UI".encode_utf16().collect();
        descriptor.lfFaceName[..face.len()].copy_from_slice(&face);
        let font = CreateFontIndirectW(&descriptor);
        assert!(!font.is_invalid());
        (window, TestFont(font))
    };
    // SAFETY: subclass only the receiver just verified to belong to this thread.
    let _probe = unsafe {
        let old = SetWindowLongPtrW(window, GWLP_WNDPROC, input_probe as *const () as isize);
        assert_ne!(old, 0);
        ORIGINAL_PROC.with(|procedure| procedure.set(old));
        RAW_HARDWARE_MOVES.with(|count| count.set(0));
        RAW_SYSTEM_MOVES.with(|count| count.set(0));
        Probe(window, old)
    };
    let mut positions = Vec::new();
    let mut down = false;
    let mut drag_motions = 0;
    let mut left_downs = 0;
    let mut left_ups = 0;
    let mut released = false;
    let started = Instant::now();
    let deadline = started + Duration::from_secs(30);
    let mut next_paint = started;
    println!("请使用本机实体鼠标移动，并按住左键拖动，再松开。测试最多等待30秒。");
    while Instant::now() < deadline {
        match viewer.input().unwrap() {
            Some(InputEvent::PointerMotion { position }) => {
                if positions.last() != Some(&position) {
                    positions.push(position);
                    if positions.len() <= 5 {
                        println!(
                            "hardware motion #{} position={position:?} left_down={down} elapsed_ms={}",
                            positions.len(),
                            started.elapsed().as_millis()
                        );
                    }
                    if down {
                        drag_motions += 1;
                    }
                }
            }
            Some(InputEvent::Button {
                button,
                pressed,
                position,
            }) => {
                println!(
                    "hardware button={button:?} pressed={pressed} position={position:?} elapsed_ms={}",
                    started.elapsed().as_millis()
                );
                if button == MouseButton::LEFT {
                    down = pressed;
                    left_downs += usize::from(pressed);
                    left_ups += usize::from(!pressed);
                    released |= !pressed && drag_motions > 0;
                }
            }
            _ => {}
        }
        if Instant::now() >= next_paint {
            paint(
                window,
                font.0,
                &[
                    "FerryDesk - physical mouse diagnostic".into(),
                    "1. Move the LOCAL physical mouse.".into(),
                    "2. HOLD LEFT, move while holding, then RELEASE LEFT.".into(),
                    "The yellow marker follows ACCEPTED hardware movement.".into(),
                    format!(
                        "Moves={}   Left DOWN={}   Drag moves={}   Left UP={}",
                        positions.len(),
                        left_downs,
                        drag_motions,
                        left_ups
                    ),
                    format!(
                        "LEFT held={}   Time remaining={}s",
                        down,
                        deadline.saturating_duration_since(Instant::now()).as_secs()
                    ),
                    "The test closes automatically after a successful drag.".into(),
                ],
                positions.last().copied(),
            );
            next_paint = Instant::now() + Duration::from_millis(100);
        }
        if positions.len() >= 3 && drag_motions > 0 && released {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    println!(
        "hardware summary motions={} left_downs={left_downs} drag_motions={drag_motions} left_ups={left_ups} released={released}",
        positions.len()
    );
    assert!(positions.len() >= 3, "未接收到真实硬件鼠标的连续移动");
    assert!(drag_motions > 0, "未接收到按住左键期间的真实移动");
    assert!(released, "未接收到拖动后的左键松开");
}

#[test]
#[ignore = "需要 Windows Media Foundation H.264 编解码组件"]
fn system_codec_can_round_trip_a_1080p100_frame() {
    ferry_platform_windows::WinDisplayBackend::check_codec().unwrap();
}

#[test]
#[ignore = "需要已安装驱动，会短暂把光标移入虚拟屏并恢复原位置"]
fn gpu_cursor_can_be_composited_after_virtual_display_input() {
    use ferry_platform::{DisplayBackend, MediaTrack};
    use ferry_platform_windows::WinDisplayBackend;
    use ferry_types::{InputEvent, Point};
    use std::time::{Duration, Instant};
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::WindowsAndMessaging::{GetCursorPos, SetCursorPos};
    struct Restore(POINT);
    impl Drop for Restore {
        fn drop(&mut self) {
            // SAFETY: restore the cursor position observed before this explicit test.
            unsafe {
                let _ = SetCursorPos(self.0.x, self.0.y);
            }
        }
    }
    let mut position = POINT::default();
    // SAFETY: position is a valid writable POINT.
    unsafe { GetCursorPos(&mut position) }.unwrap();
    let _restore = Restore(position);
    let mut source = WinDisplayBackend.source([0; 32], false).unwrap();
    source
        .input(InputEvent::PointerMotion {
            position: Point::new(64, 64),
        })
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        assert!(Instant::now() < deadline);
        if let Some(frame) = source.next_frame(Duration::from_millis(5)).unwrap()
            && frame.track == MediaTrack::Video
        {
            break;
        }
    }
}

#[test]
#[ignore = "需要已安装签名虚拟显示驱动，会短暂启用一块虚拟屏"]
fn installed_virtual_display_captures_video_and_detaches_after_stop() {
    use ferry_platform::{DisplayBackend, Displays, MediaTrack};
    use ferry_platform_windows::{WinDisplayBackend, WinDisplays};
    use std::time::{Duration, Instant};

    let displays = WinDisplays::new();
    let original: Vec<_> = displays
        .displays()
        .unwrap()
        .iter()
        .map(|d| d.bounds)
        .collect();
    let mut source = WinDisplayBackend.source([0; 32], false).unwrap();
    // Session output must not alter the geometry used by ordinary sharing.
    assert_eq!(
        displays
            .displays()
            .unwrap()
            .iter()
            .map(|d| d.bounds)
            .collect::<Vec<_>>(),
        original
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        assert!(
            Instant::now() < deadline,
            "虚拟屏没有产出编码视频帧：{}",
            source.description()
        );
        if let Some(frame) = source.next_frame(Duration::from_millis(20)).unwrap() {
            assert_eq!(frame.track, MediaTrack::Video);
            assert!(!frame.bytes.is_empty());
            break;
        }
    }
    drop(source);
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let current: Vec<_> = displays
            .displays()
            .unwrap()
            .iter()
            .map(|d| d.bounds)
            .collect();
        if current == original {
            break;
        }
        assert!(Instant::now() < deadline, "结束后虚拟屏没有移出桌面");
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[test]
#[ignore = "需要 ScreenFerry 安装的虚拟屏，会恢复残留显示并验证连续换向、光标及窗口位置"]
fn restarting_display_restores_physical_geometry_and_uses_one_cursor() {
    use ferry_platform::{DisplayBackend, Displays, MediaTrack};
    use ferry_platform_windows::{WinDisplayBackend, WinDisplays};
    use ferry_types::{InputEvent, Point};
    use std::time::{Duration, Instant};
    use windows::Win32::Foundation::{LPARAM, POINT, RECT, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::*;
    use windows::core::w;

    WinDisplayBackend.recover().unwrap();
    let physical = WinDisplays::new().displays().unwrap();
    // SAFETY: read current virtual desktop width; this test owns all created resources.
    let width = unsafe { GetSystemMetrics(SM_CXVIRTUALSCREEN) };
    for _ in 0..3 {
        let mut source = WinDisplayBackend.source([0; 32], false).unwrap();
        assert!(
            unsafe { GetSystemMetrics(SM_CXVIRTUALSCREEN) } > width,
            "没有激活真正的 Windows 虚拟屏"
        );
        assert_eq!(
            WinDisplays::new().displays().unwrap(),
            physical,
            "虚拟屏污染了普通键鼠布局"
        );
        let mut viewer = WinDisplayBackend.viewer(false).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            assert!(Instant::now() < deadline, "换向后没有呈现首帧");
            if let Some(frame) = source.next_frame(Duration::from_millis(5)).unwrap()
                && frame.track == MediaTrack::Video
                && viewer.present(frame).unwrap()
            {
                break;
            }
        }
        // SAFETY: find only the viewer created on this test thread, then inspect its
        // client cursor handling. No synthetic clicks or changes to other windows.
        unsafe {
            let window =
                FindWindowW(w!("ScreenFerryDisplayViewer"), w!("ScreenFerry 扩展屏")).unwrap();
            let mut bounds = RECT::default();
            GetWindowRect(window, &mut bounds).unwrap();
            let middle = Point::new(
                (bounds.left + bounds.right) / 2,
                (bounds.top + bounds.bottom) / 2,
            );
            assert!(
                physical.iter().any(|screen| screen.bounds.contains(middle)),
                "接收窗口落入不可见的虚拟屏"
            );
            assert_eq!(
                SendMessageW(
                    window,
                    WM_SETCURSOR,
                    Some(WPARAM(window.0 as usize)),
                    Some(LPARAM(HTCLIENT as isize))
                )
                .0,
                1
            );
            assert!(GetCursor().is_invalid(), "接收端重复绘制了本地光标");
        }
        drop(viewer);
        // Reverse-direction input can leave the source pointer on its virtual screen.
        source
            .input(InputEvent::PointerMotion {
                position: Point::new(64, 64),
            })
            .unwrap();
        drop(source);
        assert_eq!(
            unsafe { GetSystemMetrics(SM_CXVIRTUALSCREEN) },
            width,
            "取消后仍残留看不见的屏幕"
        );
        let mut pointer = POINT::default();
        unsafe { GetCursorPos(&mut pointer) }.unwrap();
        assert!(
            physical
                .iter()
                .any(|screen| screen.bounds.contains(Point::new(pointer.x, pointer.y))),
            "取消后光标留在虚拟屏"
        );
    }
}

#[test]
#[ignore = "需要已安装驱动；子进程异常退出后，验证遗留虚拟屏和光标恢复"]
fn abandoned_virtual_output_is_detached_without_a_connected_peer() {
    use ferry_platform::{DisplayBackend, Displays};
    use ferry_platform_windows::{WinDisplayBackend, WinDisplays};
    use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXVIRTUALSCREEN};

    const CHILD: &str = "SCREENFERRY_ABANDONED_DISPLAY_TEST_CHILD";
    if std::env::var_os(CHILD).is_some() {
        let _source = WinDisplayBackend
            .source_with_options(
                [0; 32],
                false,
                ferry_types::DisplayOptions {
                    side: ferry_types::Side::Left,
                    primary: ferry_types::DisplayPrimary::Viewer,
                    alignment: Default::default(),
                    ..Default::default()
                },
            )
            .unwrap();
        // Model process termination without running any Rust Drop implementations.
        std::process::exit(0);
    }
    struct Recover;
    impl Drop for Recover {
        fn drop(&mut self) {
            let _ = WinDisplayBackend.recover();
        }
    }
    WinDisplayBackend.recover().unwrap();
    let _recover = Recover;
    let physical = WinDisplays::new().displays().unwrap();
    // SAFETY: system metrics are read-only; only this explicit test creates an output.
    let width = unsafe { GetSystemMetrics(SM_CXVIRTUALSCREEN) };
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "display_stream::abandoned_virtual_output_is_detached_without_a_connected_peer",
            "--nocapture",
        ])
        .env(CHILD, "1")
        .status()
        .unwrap();
    assert!(status.success());
    assert!(
        unsafe { GetSystemMetrics(SM_CXVIRTUALSCREEN) } > width,
        "子进程退出后没有留下待回收的真实虚拟屏"
    );
    assert_eq!(WinDisplays::new().displays().unwrap().len(), physical.len());
    WinDisplayBackend.recover().unwrap();
    assert_eq!(
        WinDisplays::new().displays().unwrap(),
        physical,
        "异常退出后应恢复原主屏和坐标"
    );
    assert_eq!(
        unsafe { GetSystemMetrics(SM_CXVIRTUALSCREEN) },
        width,
        "未连接设备的空闲恢复应移除虚拟屏，禁止鼠标划到屏幕之外"
    );
}

#[test]
#[ignore = "需要已安装驱动，会切换 8 次屏幕位置和主屏并恢复"]
fn display_plans_apply_all_edges_and_restore_the_original_primary() {
    use ferry_platform::DisplayBackend;
    use ferry_platform_windows::WinDisplayBackend;
    use ferry_types::{DisplayOptions, DisplayPrimary, InputEvent, Point, Side};
    use windows::Win32::Foundation::POINT;
    use windows::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MONITOR_DEFAULTTONULL, MONITORINFO, MonitorFromPoint,
    };
    use windows::Win32::UI::WindowsAndMessaging::{GetCursorPos, MONITORINFOF_PRIMARY};
    WinDisplayBackend.recover().unwrap();
    let original = WinDisplayBackend.screens().unwrap();
    let anchor = original.iter().find(|s| s.primary).unwrap();
    for primary in [DisplayPrimary::Source, DisplayPrimary::Viewer] {
        for side in [Side::Left, Side::Right, Side::Top, Side::Bottom] {
            println!("testing topology: primary={primary:?}, side={side:?}");
            let mut source = WinDisplayBackend
                .source_with_options(
                    [0; 32],
                    false,
                    DisplayOptions {
                        source_screen: Some(anchor.id),
                        side,
                        primary,
                        ..Default::default()
                    },
                )
                .unwrap();
            let extension =
                ferry_core::display_extension_bounds(anchor.bounds, 1920, 1080, side).unwrap();
            let offset = if primary == DisplayPrimary::Viewer {
                (extension.x, extension.y)
            } else {
                (anchor.bounds.x, anchor.bounds.y)
            };
            let screens = WinDisplayBackend.screens().unwrap();
            assert_eq!(
                screens.len(),
                original.len(),
                "虚拟主屏不应出现在实体屏幕清单中"
            );
            let physical = screens.iter().find(|s| s.id == anchor.id).unwrap();
            assert_eq!(physical.bounds.x, anchor.bounds.x - offset.0);
            assert_eq!(physical.bounds.y, anchor.bounds.y - offset.1);
            assert_eq!(physical.primary, primary == DisplayPrimary::Source);
            source
                .input(InputEvent::PointerMotion {
                    position: Point::new(64, 64),
                })
                .unwrap();
            let mut cursor = POINT::default();
            // SAFETY: inspect only the current pointer and its monitor's metadata.
            unsafe { GetCursorPos(&mut cursor) }.unwrap();
            assert_eq!(
                (cursor.x, cursor.y),
                (extension.x - offset.0 + 64, extension.y - offset.1 + 64)
            );
            let mut info = MONITORINFO {
                cbSize: size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            unsafe {
                assert!(
                    GetMonitorInfoW(MonitorFromPoint(cursor, MONITOR_DEFAULTTONULL), &mut info)
                        .as_bool()
                );
            }
            assert_eq!(
                info.dwFlags & MONITORINFOF_PRIMARY != 0,
                primary == DisplayPrimary::Viewer
            );
            drop(source);
            assert_eq!(
                WinDisplayBackend.screens().unwrap(),
                original,
                "取消方案没有恢复主屏和位置"
            );
        }
    }
}

#[test]
#[ignore = "实机性能诊断：短暂创建虚拟屏，采集两秒，不断言机器性能"]
fn measure_native_capture_throughput() {
    use ferry_platform::{DisplayBackend, MediaTrack};
    use ferry_platform_windows::WinDisplayBackend;
    use std::time::{Duration, Instant};
    let mut source = WinDisplayBackend.source([0; 32], false).unwrap();
    println!("backend: {}", source.description());
    let start = Instant::now();
    let mut frames = 0;
    let mut times = Vec::new();
    while start.elapsed() < Duration::from_secs(2) {
        let at = Instant::now();
        if let Some(frame) = source.next_frame(Duration::from_millis(5)).unwrap()
            && frame.track == MediaTrack::Video
        {
            frames += 1;
            times.push(at.elapsed().as_secs_f64() * 1000.0);
        }
    }
    times.sort_by(f64::total_cmp);
    println!("completed backend: {}", source.description());
    assert!(frames > 0, "硬件/软件编码器没有产生输出");
    println!(
        "native pipeline: frames={frames}, fps={:.1}, call_ms_p50={:.2}, call_ms_p95={:.2}",
        frames as f64 / start.elapsed().as_secs_f64(),
        times[times.len() / 2],
        times[times.len() * 95 / 100]
    );
}

#[test]
#[ignore = "实机性能诊断：创建虚拟屏和接收窗口，测试采集到 GPU 呈现，不经过网络"]
fn measure_native_capture_decode_present_throughput() {
    use ferry_platform::{DisplayBackend, MediaTrack};
    use ferry_platform_windows::WinDisplayBackend;
    use std::time::{Duration, Instant};
    let mut source = WinDisplayBackend.source([0; 32], false).unwrap();
    let mut viewer = WinDisplayBackend.viewer(false).unwrap();
    let start = Instant::now();
    let mut frames = 0;
    let mut presented = 0;
    let mut age_ms = Vec::new();
    while start.elapsed() < Duration::from_secs(10) {
        if let Some(frame) = source.next_frame(Duration::from_millis(5)).unwrap()
            && frame.track == MediaTrack::Video
        {
            frames += 1;
            let timestamp = frame.timestamp_us;
            if viewer.present(frame).unwrap() {
                presented += 1;
                age_ms.push(source.capture_age(timestamp).unwrap().as_secs_f64() * 1000.0);
            }
            // Drain input just like the resident; this rendering benchmark does
            // not forward real user clicks into its synthetic source desktop.
            while viewer.input().unwrap().is_some() {}
        }
    }
    age_ms.sort_by(f64::total_cmp);
    println!("source: {}", source.description());
    println!("viewer: {}", viewer.description());
    println!("frames={frames}, presented={presented}");
    assert!(presented > 0, "接收端未呈现任何视频帧");
    println!(
        "local capture/decode/present: encoded={frames}, presented={presented}, fps={:.1}, age_p50_ms={:.2}, age_p95_ms={:.2}",
        presented as f64 / start.elapsed().as_secs_f64(),
        age_ms[age_ms.len() / 2],
        age_ms[age_ms.len() * 95 / 100]
    );
}

#[test]
#[ignore = "需要 Windows Media Foundation；短暂显示接收窗口，验证伪输入洪峰不被回传"]
fn viewer_rejects_posted_input_pressure_without_ending_the_session() {
    use ferry_platform::DisplayBackend;
    use ferry_platform_windows::WinDisplayBackend;
    use windows::Win32::Foundation::{LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::*;
    use windows::core::w;

    let mut viewer = WinDisplayBackend.viewer(false).unwrap();
    // SAFETY: post only to the hidden receiver owned by this test. It never
    // presents a frame or forwards these synthetic events to Windows SendInput.
    unsafe {
        let window = FindWindowW(w!("ScreenFerryDisplayViewer"), w!("ScreenFerry 扩展屏")).unwrap();
        assert_eq!(
            GetWindowThreadProcessId(window, None),
            windows::Win32::System::Threading::GetCurrentThreadId(),
            "只允许向本测试线程的隐藏窗口投递消息"
        );
        assert!(!IsWindowVisible(window).as_bool());
        for n in 0..400 {
            PostMessageW(
                Some(window),
                WM_MOUSEMOVE,
                WPARAM(0),
                LPARAM(64 | (64 << 16)),
            )
            .unwrap();
            PostMessageW(
                Some(window),
                if n % 2 == 0 {
                    WM_LBUTTONDOWN
                } else {
                    WM_LBUTTONUP
                },
                WPARAM(0),
                LPARAM(64 | (64 << 16)),
            )
            .unwrap();
        }
    }
    // Posted messages have no hardware provenance. Consume enough bounded
    // pumps for all 800 messages and verify none can manufacture remote clicks.
    for _ in 0..16 {
        assert!(
            viewer
                .input()
                .expect("伪输入洪峰不应结束扩展模式")
                .is_none(),
            "PostMessage 移动与点击不得冒充硬件操作"
        );
    }
    // The pump must remain responsive to a real session-control message after
    // rejecting the flood. It cannot stall forever with a full input queue.
    unsafe {
        let window = FindWindowW(w!("ScreenFerryDisplayViewer"), w!("ScreenFerry 扩展屏")).unwrap();
        PostMessageW(Some(window), WM_CLOSE, WPARAM(0), LPARAM(0)).unwrap();
    }
    assert!(matches!(
        viewer.input(),
        Err(ferry_platform::Error::Cancelled)
    ));
}
