//! 主线程事件循环：`EventLoop` 的 Windows 实现。
//!
//! 主线程运行消息循环。其他线程的任务投递给一个只收消息的隐藏窗口：
//! 托盘菜单弹出时的模态循环同样会分发窗口消息，任务不会丢。
//!
//! 后台是 Windows 子系统程序，自己没有控制台，双击或登录时启动不会闪出黑色窗口。
//! 命令行用法要输出时用 [`attach_console`] 接一个。

pub(crate) mod hotkeys;
pub(crate) mod latency;
mod shell;

pub use shell::launch_settings_as_user;

use std::cell::RefCell;
use std::collections::HashMap;

use ferry_platform::{Dispatcher, Error, EventLoop, MainTask, Result};
use windows::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_TYPE_UNKNOWN,
    GetFileType, OPEN_EXISTING,
};
use windows::Win32::System::Console::{
    ATTACH_PARENT_PROCESS, AllocConsole, AttachConsole, GetStdHandle, STD_ERROR_HANDLE, STD_HANDLE,
    STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, SetStdHandle,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, RegisterHotKey,
    UnregisterHotKey,
};
use windows::Win32::UI::Shell::{SEE_MASK_NOASYNC, SHELLEXECUTEINFOW, ShellExecuteExW};
use windows::Win32::UI::WindowsAndMessaging::{
    ASFW_ANY, AllowSetForegroundWindow, CreateWindowExW, DefWindowProcW, DispatchMessageW,
    GetMessageW, HWND_MESSAGE, MSG, PostMessageW, RegisterClassW, TranslateMessage,
    WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_HOTKEY, WNDCLASSW,
};
use windows::core::w;

/// 用户主动选择管理员模式，启动成功后由新后台请求旧后台退出。
pub fn launch_elevated(
    executable: &std::path::Path,
    data_dir: Option<&std::path::Path>,
) -> Result<()> {
    use windows::core::{HSTRING, PCWSTR};
    let executable = HSTRING::from(executable.as_os_str());
    // Windows 文件路径不能含双引号；末尾反斜杠在关闭引号前需转义。
    let parameters = HSTRING::from(match data_dir {
        Some(directory) => {
            let directory = directory.as_os_str().to_string_lossy();
            let trailing = directory.chars().rev().take_while(|&c| c == '\\').count();
            format!(
                "elevate --data-dir \"{directory}{}\"",
                "\\".repeat(trailing)
            )
        }
        None => "elevate".to_owned(),
    });
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOASYNC,
        lpVerb: w!("runas"),
        lpFile: PCWSTR(executable.as_ptr()),
        lpParameters: PCWSTR(parameters.as_ptr()),
        nShow: 1,
        ..Default::default()
    };
    // SAFETY: 所有字符串在同步调用返回前有效；runas 通过 Windows 的 UAC 征得用户同意。
    unsafe { ShellExecuteExW(&mut info) }.map_err(|e| os("请求管理员后台", e))
}

/// 投递任务用的消息。`lparam` 是 `Box<MainTask>` 的裸指针。
const WM_TASK: u32 = WM_APP + 1;

type HotkeyHandler = Box<dyn Fn()>;

thread_local! {
    // 注册、替换与消息处理都在创建主线程窗口的线程执行。
    static HOTKEYS: RefCell<HashMap<(isize, i32), HotkeyHandler>> = RefCell::new(HashMap::new());
}

/// 由应用配置的 Windows 全局快捷键。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WinHotkey {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub virtual_key: u16,
}

impl WinHotkey {
    fn modifiers(self) -> HOT_KEY_MODIFIERS {
        let mut flags = MOD_NOREPEAT;
        if self.ctrl {
            flags |= MOD_CONTROL;
        }
        if self.alt {
            flags |= MOD_ALT;
        }
        if self.shift {
            flags |= MOD_SHIFT;
        }
        flags
    }
}

pub struct WinEventLoop {
    hwnd: HWND,
    _hotkey_hook: windows::Win32::UI::WindowsAndMessaging::HHOOK,
}

/// 同一事件循环上的快捷键注册器。只在主线程使用。
#[derive(Clone, Copy)]
pub struct WinHotkeys(HWND);

impl WinEventLoop {
    /// 只能在主线程上调用：窗口属于创建它的线程，任务也在这个线程上执行。
    pub fn new() -> Result<Self> {
        let class = w!("ScreenFerryMainThread");
        // SAFETY: 参数都是有效的常量或刚取得的句柄；窗口过程只处理本模块投递的消息。
        let hwnd = unsafe {
            let instance = GetModuleHandleW(None).map_err(|e| os("GetModuleHandleW", e))?;
            let wc = WNDCLASSW {
                lpfnWndProc: Some(window_proc),
                hInstance: instance.into(),
                lpszClassName: class,
                ..Default::default()
            };
            RegisterClassW(&wc);
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                class,
                w!(""),
                WINDOW_STYLE(0),
                0,
                0,
                0,
                0,
                Some(HWND_MESSAGE),
                None,
                Some(instance.into()),
                None,
            )
            .map_err(|e| os("CreateWindowExW", e))?
        };
        let hook = hotkeys::install().map_err(|e| os("SetWindowsHookExW(shortcuts)", e))?;
        Ok(Self {
            hwnd,
            _hotkey_hook: hook,
        })
    }

    pub fn hotkeys(&self) -> WinHotkeys {
        WinHotkeys(self.hwnd)
    }
}

impl WinHotkeys {
    /// 在主线程注册一个快捷键。若系统已有相同快捷键，返回错误。
    pub fn register_hotkey(
        &self,
        id: i32,
        key: WinHotkey,
        handler: impl Fn() + 'static,
    ) -> Result<()> {
        let slot = (self.0.0 as isize, id);
        HOTKEYS.with(|hotkeys| {
            let mut hotkeys = hotkeys.borrow_mut();
            if hotkeys.contains_key(&slot) {
                return Err(Error::Failed("快捷键编号已注册".into()));
            }
            // SAFETY: 主线程拥有 hwnd，键和修饰键都是传值参数。
            unsafe {
                RegisterHotKey(
                    Some(self.0),
                    id,
                    key.modifiers(),
                    u32::from(key.virtual_key),
                )
            }
            .map_err(|e| os("RegisterHotKey", e))?;
            hotkeys.insert(slot, Box::new(handler));
            hotkeys::register(self.0, id, key);
            Ok(())
        })
    }

    pub fn unregister_hotkey(&self, id: i32) -> Result<()> {
        let slot = (self.0.0 as isize, id);
        HOTKEYS.with(|hotkeys| {
            let mut hotkeys = hotkeys.borrow_mut();
            if hotkeys.contains_key(&slot) {
                // SAFETY: 主线程拥有 hwnd，登记编号由本窗口分配。
                unsafe { UnregisterHotKey(Some(self.0), id) }
                    .map_err(|e| os("UnregisterHotKey", e))?;
                hotkeys.remove(&slot);
                hotkeys::unregister(self.0, id);
            }
            Ok(())
        })
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_TASK {
        // SAFETY: 这个消息只由 dispatcher 投递，lparam 是 Box::into_raw 得到的指针，只取回一次。
        let task = unsafe { Box::from_raw(lparam.0 as *mut MainTask) };
        task();
        return LRESULT(0);
    }
    if msg == WM_HOTKEY
        && HOTKEYS.with(|hotkeys| {
            let handlers = hotkeys.borrow();
            let Some(handler) = handlers.get(&(hwnd.0 as isize, wparam.0 as i32)) else {
                return false;
            };
            handler();
            true
        })
    {
        return LRESULT(0);
    }
    // SAFETY: 其余消息交给系统默认处理。
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

impl EventLoop for WinEventLoop {
    fn dispatcher(&self) -> Dispatcher {
        // 窗口句柄本身不是 Send，以整数形式带到其他线程；PostMessageW 可以在任何线程调用。
        let hwnd = self.hwnd.0 as isize;
        Dispatcher::new(move |task| {
            let raw = Box::into_raw(Box::new(task));
            // SAFETY: 窗口在进程结束前一直存在；投递失败时收回指针，避免泄漏。
            let posted = unsafe {
                PostMessageW(
                    Some(HWND(hwnd as *mut _)),
                    WM_TASK,
                    WPARAM(0),
                    LPARAM(raw as isize),
                )
            };
            if posted.is_err() {
                // SAFETY: 投递失败，指针仍归这里所有。
                drop(unsafe { Box::from_raw(raw) });
            }
        })
    }

    /// Windows 上再次运行程序会启动新进程，它经本机通信请这一份打开设置界面，不走这里。
    fn on_reopen(&mut self, _handler: Box<dyn Fn()>) {}

    fn run(self) -> ! {
        let mut msg = MSG::default();
        // SAFETY: 标准的消息循环，msg 指向有效的缓冲区。
        unsafe {
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        std::process::exit(0)
    }
}

/// 给命令行用法接一个控制台，接上了返回 `true`。
///
/// `own` 为真时新开一个控制台窗口：`serve`、`connect` 要在终端里确认配对码、按 Ctrl+C 退出，
/// 和启动它的终端抢输入会乱。否则接到启动它的终端上（只输出），没有终端时什么也不做。
/// 已经重定向到文件或管道的标准句柄保持不变，其余的指向控制台。
pub fn attach_console(own: bool) -> bool {
    // SAFETY: 只给本进程接上控制台；失败时（父进程没有控制台等）什么也不改。
    let attached = unsafe {
        if own {
            AllocConsole()
        } else {
            AttachConsole(ATTACH_PARENT_PROCESS)
        }
    };
    if attached.is_err() {
        return false;
    }
    for (which, device) in [
        (STD_INPUT_HANDLE, w!("CONIN$")),
        (STD_OUTPUT_HANDLE, w!("CONOUT$")),
        (STD_ERROR_HANDLE, w!("CONOUT$")),
    ] {
        if !usable(which) {
            // SAFETY: 打开控制台设备，设为本进程的标准句柄，一直用到进程退出，不需要关闭。
            unsafe {
                if let Ok(handle) = CreateFileW(
                    device,
                    (GENERIC_READ | GENERIC_WRITE).0,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    None,
                    OPEN_EXISTING,
                    FILE_ATTRIBUTE_NORMAL,
                    None,
                ) {
                    let _ = SetStdHandle(which, handle);
                }
            }
        }
    }
    true
}

/// 这个标准句柄能用：已经指向控制台、文件或管道。
fn usable(which: STD_HANDLE) -> bool {
    // SAFETY: 只读本进程的标准句柄、查询句柄类型。
    unsafe { GetStdHandle(which) }
        .is_ok_and(|h| !h.is_invalid() && unsafe { GetFileType(h) } != FILE_TYPE_UNKNOWN)
}

/// 允许接下来启动的程序把自己的窗口放到最前面。
///
/// 从后台进程启动的窗口默认抢不到前台，只会在任务栏上闪一下。用户刚点过托盘时本进程有前台权限，
/// 在启动设置界面之前把这个权限让出去。
pub fn allow_foreground() {
    // SAFETY: 简单的系统调用；没有权限时失败，不影响什么。
    let _ = unsafe { AllowSetForegroundWindow(ASFW_ANY) };
}

fn os(call: &'static str, e: windows::core::Error) -> Error {
    Error::Os {
        call,
        detail: e.message(),
    }
}
