//! 在输入被抓取时仍优先识别本机快捷键；回调只投递主线程消息。

use super::WinHotkey;
use std::collections::BTreeMap;
use std::sync::Mutex;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, HHOOK, KBDLLHOOKSTRUCT, LLKHF_UP, PostMessageW, SetWindowsHookExW,
    WH_KEYBOARD_LL, WM_HOTKEY,
};

#[derive(Default)]
struct State {
    keys: BTreeMap<(isize, i32), WinHotkey>,
    modifiers: u16,
    consumed: BTreeMap<u16, ()>,
}
static STATE: Mutex<Option<State>> = Mutex::new(None);

pub(super) fn register(hwnd: HWND, id: i32, key: WinHotkey) {
    STATE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_default()
        .keys
        .insert((hwnd.0 as isize, id), key);
}
pub(super) fn unregister(hwnd: HWND, id: i32) {
    if let Some(state) = STATE.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        state.keys.remove(&(hwnd.0 as isize, id));
    }
}
pub(super) fn install() -> windows::core::Result<HHOOK> {
    // SAFETY: 进程内钩子运行于本线程的消息循环，钩子在整个事件循环期间存活。
    unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook), None, 0) }
}
unsafe extern "system" fn hook(code: i32, w: WPARAM, l: LPARAM) -> LRESULT {
    if code == 0 {
        // SAFETY: WH_KEYBOARD_LL 在 HC_ACTION 时提供有效的结构。
        if intercept(unsafe { &*(l.0 as *const KBDLLHOOKSTRUCT) }) {
            return LRESULT(1);
        }
    }
    // SAFETY: 未处理的消息继续传递。
    unsafe { CallNextHookEx(None, code, w, l) }
}
pub(crate) fn intercept(info: &KBDLLHOOKSTRUCT) -> bool {
    let mut guard = STATE.lock().unwrap_or_else(|e| e.into_inner());
    let state = guard.get_or_insert_default();
    let vk = info.vkCode as u16;
    let up = info.flags.contains(LLKHF_UP);
    let bit = match vk {
        0xA2 => 1,
        0xA3 => 2,
        0xA4 => 4,
        0xA5 => 8,
        0xA0 => 16,
        0xA1 => 32,
        0x5B => 64,
        0x5C => 128,
        _ => 0,
    };
    if bit != 0 {
        if up {
            state.modifiers &= !bit;
        } else {
            state.modifiers |= bit;
        }
        return false;
    }
    if state.consumed.contains_key(&vk) {
        if up {
            state.consumed.remove(&vk);
        }
        return true;
    }
    if up {
        return false;
    }
    let matched = state
        .keys
        .iter()
        .find(|(_, key)| {
            key.virtual_key == vk
                && key.ctrl == (state.modifiers & 3 != 0)
                && key.alt == (state.modifiers & 12 != 0)
                && key.shift == (state.modifiers & 48 != 0)
                && state.modifiers & 192 == 0
        })
        .map(|(&slot, _)| slot);
    if let Some((hwnd, id)) = matched {
        // SAFETY: 注册器保留事件循环窗口，注销同时从该表移除。
        if unsafe {
            PostMessageW(
                Some(HWND(hwnd as *mut _)),
                WM_HOTKEY,
                WPARAM(id as usize),
                LPARAM(0),
            )
        }
        .is_ok()
        {
            state.consumed.insert(vk, ());
            return true;
        }
    }
    false
}
