//! 扩展桌面的物理接缝校准。钩子只做坐标计算与本机定位，不访问网络或解码器。
use ferry_platform::{Error, Result};
use ferry_types::{EdgeAlignment, Point, Rect, Side};
use std::cell::RefCell;
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::WindowsAndMessaging::*;

struct State {
    local: Rect,
    remote: Rect,
    side: Side,
    alignment: EdgeAlignment,
    last: Point,
    warp: Option<Point>,
}
thread_local! { static STATE: RefCell<Option<State>> = const { RefCell::new(None) }; }
pub(super) struct CursorAlignment {
    id: u32,
    thread: Option<JoinHandle<()>>,
}
impl CursorAlignment {
    pub fn start(
        local: Rect,
        remote: Rect,
        side: Side,
        alignment: EdgeAlignment,
    ) -> Result<Option<Self>> {
        if alignment.is_default() {
            return Ok(None);
        }
        let (ready, receiver) = mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name("ferry-alignment".into())
            .spawn(move || {
                // SAFETY: 本线程拥有钩子和消息队列，停止时注销钩子。
                unsafe {
                    let mut msg = MSG::default();
                    let _ = PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE);
                    let mut point = windows::Win32::Foundation::POINT::default();
                    let _ = GetCursorPos(&mut point);
                    STATE.with(|s| {
                        *s.borrow_mut() = Some(State {
                            local,
                            remote,
                            side,
                            alignment,
                            last: Point::new(point.x, point.y),
                            warp: None,
                        })
                    });
                    let hook = match SetWindowsHookExW(WH_MOUSE_LL, Some(hook), None, 0) {
                        Ok(hook) => hook,
                        Err(e) => {
                            let _ = ready.send(Err(Error::Failed(e.to_string())));
                            return;
                        }
                    };
                    let _ = ready.send(Ok(GetCurrentThreadId()));
                    while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                        DispatchMessageW(&msg);
                    }
                    let _ = UnhookWindowsHookEx(hook);
                }
            })
            .map_err(|e| Error::Failed(e.to_string()))?;
        let id = receiver
            .recv()
            .map_err(|e| Error::Failed(e.to_string()))??;
        Ok(Some(Self {
            id,
            thread: Some(thread),
        }))
    }
}
impl Drop for CursorAlignment {
    fn drop(&mut self) {
        // SAFETY: id 来自本对象拥有的工作线程，队列在启动时已创建。
        let _ = unsafe { PostThreadMessageW(self.id, WM_QUIT, WPARAM(0), LPARAM(0)) };
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
unsafe extern "system" fn hook(code: i32, w: WPARAM, l: LPARAM) -> LRESULT {
    if code == 0 && w.0 == WM_MOUSEMOVE as usize {
        // SAFETY: WH_MOUSE_LL 在 HC_ACTION 时提供有效的结构。
        let info = unsafe { &*(l.0 as *const MSLLHOOKSTRUCT) };
        let at = Point::new(info.pt.x, info.pt.y);
        let target = STATE.with(|cell| {
            let mut state = cell.borrow_mut();
            let s = state.as_mut()?;
            if s.warp.take() == Some(at) {
                s.last = at;
                return None;
            }
            // 接收窗口回传的是虚拟屏自身坐标，不能再变换一次。
            if info.flags & 1 != 0 {
                s.last = at;
                return None;
            }
            let mapped = ferry_core::calibrated_display_crossing(
                s.local,
                s.remote,
                s.side,
                s.alignment,
                s.last,
                at,
            );
            s.last = mapped;
            if mapped != at {
                s.warp = Some(mapped);
                Some(mapped)
            } else {
                None
            }
        });
        if let Some(target) = target {
            // SAFETY: 只定位本机光标；下一次定位通知由 warp 精确认领，避免递归换算。
            if unsafe { SetCursorPos(target.x, target.y) }.is_ok() {
                return LRESULT(1);
            }
        }
    }
    // SAFETY: 继续传递没有处理的消息。
    unsafe { CallNextHookEx(None, code, w, l) }
}
