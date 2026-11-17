//! 管理员后台经当前交互用户的资源管理器打开普通权限的设置界面。

use std::path::Path;

use ferry_platform::{Error, Result};
use windows::Win32::System::Com::{
    CLSCTX_LOCAL_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
    CoUninitialize, IDispatch, IServiceProvider,
};
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Shell::{
    IShellBrowser, IShellDispatch2, IShellFolderViewDual, IShellWindows, SID_STopLevelBrowser,
    SVGIO_BACKGROUND, SWC_DESKTOP, SWFO_NEEDDISPATCH, ShellWindows,
};
use windows::core::{BSTR, Interface};

use super::os;

/// 仅打开指定程序；不经命令解释器，也不从管理员父进程继承管理员令牌。
/// Explorer 不可用时返回错误，避免意外启动管理员设置界面。
pub fn launch_settings_as_user(executable: &Path) -> Result<()> {
    let executable = executable.to_owned();
    // A dedicated STA also works when called from the resident's runtime thread.
    std::thread::Builder::new()
        .name("ferry-user-shell".into())
        .spawn(move || launch(&executable))
        .map_err(|error| Error::Os {
            call: "创建用户 Shell 线程",
            detail: error.to_string(),
        })?
        .join()
        .map_err(|_| Error::Os {
            call: "用户 Shell 线程",
            detail: "线程意外结束".into(),
        })?
}

struct Apartment;

impl Drop for Apartment {
    fn drop(&mut self) {
        // SAFETY: this guard exists only after successful initialization on this thread.
        unsafe { CoUninitialize() };
    }
}

fn launch(executable: &Path) -> Result<()> {
    // SAFETY: all COM objects stay in this STA; the guard balances initialization
    // after the interfaces have been released. Every pointer refers to a live
    // VARIANT/output slot. ShellExecute receives the path as a BSTR, not a command.
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED)
            .ok()
            .map_err(|error| os("初始化用户 Shell", error))?;
        let _apartment = Apartment;
        let result = (|| -> windows::core::Result<()> {
            let windows: IShellWindows =
                CoCreateInstance(&ShellWindows, None, CLSCTX_LOCAL_SERVER)?;
            let empty = VARIANT::default();
            let mut hwnd = 0;
            let desktop =
                windows.FindWindowSW(&empty, &empty, SWC_DESKTOP, &mut hwnd, SWFO_NEEDDISPATCH)?;
            let provider: IServiceProvider = desktop.cast()?;
            let browser: IShellBrowser = provider.QueryService(&SID_STopLevelBrowser)?;
            let view = browser.QueryActiveShellView()?;
            let background: IDispatch = view.GetItemObject(SVGIO_BACKGROUND)?;
            let folder: IShellFolderViewDual = background.cast()?;
            let shell: IShellDispatch2 = folder.Application()?.cast()?;
            let file = BSTR::from(executable.as_os_str().to_string_lossy().as_ref());
            let directory = VARIANT::from(BSTR::from(
                executable
                    .parent()
                    .unwrap_or(Path::new(""))
                    .as_os_str()
                    .to_string_lossy()
                    .as_ref(),
            ));
            shell.ShellExecute(
                &file,
                &empty,
                &directory,
                &VARIANT::from("open"),
                &VARIANT::from(1_i32),
            )
        })();
        result.map_err(|error| os("通过资源管理器打开设置", error))
    }
}
