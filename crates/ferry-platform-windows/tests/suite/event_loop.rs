use ferry_platform::{EventLoop, InputCapture};
use ferry_platform_windows::{WinCapture, WinEventLoop, WinHotkey};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

#[test]
#[ignore = "发送真实组合键并抓取光标，需在本机交互桌面显式执行"]
fn configured_shortcut_fires_while_the_capture_hook_grabs_keyboard() {
    if std::env::var_os("SCREENFERRY_HOTKEY_CHILD").is_none() {
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "event_loop::configured_shortcut_fires_while_the_capture_hook_grabs_keyboard",
                "--ignored",
                "--nocapture",
            ])
            .env("SCREENFERRY_HOTKEY_CHILD", "1")
            .status()
            .unwrap();
        assert!(result.success());
        return;
    }
    let event_loop = WinEventLoop::new().unwrap();
    let fired = Arc::new(AtomicBool::new(false));
    let flag = fired.clone();
    event_loop
        .hotkeys()
        .register_hotkey(
            42,
            WinHotkey {
                ctrl: true,
                alt: true,
                shift: true,
                virtual_key: b'9' as u16,
            },
            move || {
                flag.store(true, Ordering::SeqCst);
            },
        )
        .unwrap();
    let mut capture = WinCapture::new();
    let mut original = windows::Win32::Foundation::POINT::default();
    // SAFETY: 只读当前光标位置，退出测试前恢复。
    unsafe { windows::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut original) }.unwrap();
    capture.start(Box::new(|_| {})).unwrap();
    capture.grab().unwrap();
    std::thread::spawn(move || {
        use windows::Win32::UI::Input::KeyboardAndMouse::*;
        std::thread::sleep(Duration::from_millis(100));
        for up in [false, true] {
            let keys = if up {
                [0x39, 0xA0, 0xA4, 0xA2]
            } else {
                [0xA2, 0xA4, 0xA0, 0x39]
            };
            for vk in keys {
                let input = INPUT {
                    r#type: INPUT_KEYBOARD,
                    Anonymous: INPUT_0 {
                        ki: KEYBDINPUT {
                            wVk: VIRTUAL_KEY(vk),
                            dwFlags: if up {
                                KEYEVENTF_KEYUP
                            } else {
                                KEYBD_EVENT_FLAGS(0)
                            },
                            ..Default::default()
                        },
                    },
                };
                // SAFETY: 固定大小 INPUT 数组，只发送上述测试组合键，并完整松开。
                assert_eq!(unsafe { SendInput(&[input], size_of::<INPUT>() as i32) }, 1);
                std::thread::sleep(Duration::from_millis(15));
            }
        }
        std::thread::sleep(Duration::from_millis(100));
        capture
            .release(ferry_types::Point::new(original.x, original.y))
            .unwrap();
        drop(capture);
        std::process::exit(if fired.load(Ordering::SeqCst) { 0 } else { 1 });
    });
    event_loop.run();
}
