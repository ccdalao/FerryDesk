//! Windows 平台实现，基于 windows-rs 与 Win32 API。
//!
//! 进程会被设为“每显示器 DPI 感知 V2”，所有坐标都是物理像素。
//!
//! 仅在 Windows 上编译。层级：L2。
//! 扩展屏使用 DXGI/D3D11 和固定版本的 Apollo 原生 NVENC，C ABI 只在本平台实现中出现。

#![cfg(target_os = "windows")]

mod autostart;
mod capture;
mod clipboard;
mod display;
mod display_stream;
mod event_loop;
mod inject;
mod keymap;
mod overlay;
mod permission;
mod update;

pub use autostart::WinAutostart;
pub use capture::WinCapture;
pub use clipboard::{WinClipboard, text_from_windows, text_to_windows};
pub use display::WinDisplays;
pub use display_stream::{WinDisplayBackend, install_display_driver, remove_display_driver};
pub use event_loop::{
    WinEventLoop, WinHotkey, WinHotkeys, allow_foreground, attach_console, launch_elevated,
    launch_settings_as_user,
};
pub use inject::{
    WinInject, authorize_input_peer, install_input_service, is_elevated, is_input_peer_authorized,
    prepare_input_service, remove_input_service, revoke_input_peer, run_input_agent,
    run_input_service, stop_input_service,
};
pub use keymap::{WinKey, hid_from_windows, win_key};
pub use overlay::WinOverlay;
pub use permission::WinPermissions;
pub use update::{WinUpdater, parse_proxy_server};

mod network;
pub use network::WinNetworkPriority;
