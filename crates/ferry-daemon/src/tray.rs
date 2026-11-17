//! 托盘图标：显示是否连着另一台电脑，菜单里可以暂停或继续共享、打开设置、退出；
//! 新版本下载好后多一项“重启并更新”。
//! 图标只有两种：已连接（光球正穿过屏幕边框）和未连接（光球停在屏幕里、变淡）；
//! 暂停、等待连接、寻找服务端、出了问题都算未连接，具体情况看菜单第一行。
//! macOS 按菜单栏的习惯，点图标弹出菜单；Windows 按任务栏的习惯，左键直接打开设置界面，右键弹出菜单。
//!
//! 托盘对象只能在主线程上创建和修改，所以放在主线程的线程局部变量里；
//! 状态变化时由引擎所在的线程经 [`Dispatcher`] 把更新交给主线程。

use std::cell::RefCell;

use ferry_engine::resident::{Command, DisplayAction, Handle, Summary};
use ferry_platform::Dispatcher;
use tokio::sync::watch;
use tracing::warn;
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{Icon, TrayIcon};

const TOGGLE: &str = "toggle";
const OPEN: &str = "open";
const UPDATE: &str = "update";
const QUIT: &str = "quit";
const EXTEND_CONTROLLER: &str = "extend-controller";
const EXTEND_CONTROLLED: &str = "extend-controlled";
const STOP_EXTENSION: &str = "stop-extension";
#[cfg(target_os = "windows")]
const ELEVATE: &str = "elevate";

/// 菜单栏用模板图：只看透明度，系统按明暗模式自动着色。
#[cfg(target_os = "macos")]
mod art {
    use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

    pub(super) const CONNECTED: &[u8] =
        include_bytes!("../../../assets/icons/tray-connected@2x.png");
    pub(super) const DISCONNECTED: &[u8] =
        include_bytes!("../../../assets/icons/tray-disconnected@2x.png");

    pub(super) fn builder(icon: Icon) -> TrayIconBuilder {
        TrayIconBuilder::new().with_icon_templated(icon)
    }

    pub(super) fn set(tray: &TrayIcon, icon: Icon) -> tray_icon::Result<()> {
        tray.set_icon_templated(Some(icon))
    }
}

/// 任务栏可能是深色也可能是浅色，用两种底色上都看得清的彩色图标；未连接时是灰色。
#[cfg(target_os = "windows")]
mod art {
    use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

    pub(super) const CONNECTED: &[u8] =
        include_bytes!("../../../assets/icons/tray-win-connected.png");
    pub(super) const DISCONNECTED: &[u8] =
        include_bytes!("../../../assets/icons/tray-win-disconnected.png");

    pub(super) fn builder(icon: Icon) -> TrayIconBuilder {
        // 左键留给“打开设置界面”，菜单只在右键时弹出。
        TrayIconBuilder::new()
            .with_icon(icon)
            .with_menu_on_left_click(false)
    }

    pub(super) fn set(tray: &TrayIcon, icon: Icon) -> tray_icon::Result<()> {
        tray.set_icon(Some(icon))
    }
}

struct Tray {
    icon: TrayIcon,
    menu: Menu,
    status: MenuItem,
    /// “重启并更新”和它下面的分隔线，有新版本时才插进菜单。
    update: MenuItem,
    update_separator: PredefinedMenuItem,
    update_shown: bool,
    toggle: MenuItem,
    open: MenuItem,
    quit: MenuItem,
    display: Submenu,
    plans: Submenu,
    extend_controller: MenuItem,
    extend_controlled: MenuItem,
    stop_extension: MenuItem,
    pause_extension: MenuItem,
    #[cfg(target_os = "windows")]
    elevate: MenuItem,
    connected: Icon,
    disconnected: Icon,
    shown: Option<Summary>,
}

thread_local! {
    static TRAY: RefCell<Option<Tray>> = const { RefCell::new(None) };
    #[cfg(target_os = "windows")]
    static DISPLAY_KEYS: RefCell<Option<DisplayKeys>> = const { RefCell::new(None) };
}

#[cfg(target_os = "windows")]
struct DisplayKeys {
    hotkeys: ferry_platform_windows::WinHotkeys,
    handle: Handle,
    desired: Vec<(i32, u8, Command)>,
    requested: Vec<(i32, u8, Command)>,
    retry_at: std::time::Instant,
}

#[cfg(target_os = "windows")]
pub(crate) fn display_hotkeys(hotkeys: ferry_platform_windows::WinHotkeys, handle: Handle) {
    DISPLAY_KEYS.with(|keys| {
        *keys.borrow_mut() = Some(DisplayKeys {
            hotkeys,
            handle,
            desired: Default::default(),
            requested: Default::default(),
            retry_at: std::time::Instant::now(),
        })
    });
}

#[cfg(target_os = "windows")]
fn update_display_hotkeys(next: ferry_types::DisplayShortcuts, plans: &[ferry_types::DisplayPlan]) {
    use ferry_platform_windows::WinHotkey;

    DISPLAY_KEYS.with(|state| {
        let mut state = state.borrow_mut();
        let Some(state) = state.as_mut() else {
            return;
        };
        let desired: Vec<_> = (1..)
            .zip(DisplayAction::ALL)
            .filter_map(|(id, action)| {
                next.key(action)
                    .map(|key| (id, key, Command::Display(action)))
            })
            .chain(plans.iter().enumerate().filter_map(|(index, plan)| {
                plan.shortcut
                    .map(|key| (100 + index as i32, key, Command::RunDisplayPlan(plan.id)))
            }))
            .collect();
        if state.desired == desired {
            return;
        }
        if state.requested == desired && std::time::Instant::now() < state.retry_at {
            return;
        }
        state.requested = desired.clone();
        state.retry_at = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut failed = false;
        for (id, _, _) in &state.desired {
            if let Err(err) = state.hotkeys.unregister_hotkey(*id) {
                warn!("注销扩展屏快捷键失败：{err}");
                failed = true;
            }
        }
        let mut registered = Vec::new();
        for &(id, key, command) in &desired {
            let handle = state.handle.clone();
            let key = WinHotkey {
                ctrl: true,
                alt: true,
                shift: true,
                virtual_key: u16::from(key),
            };
            if let Err(err) = state
                .hotkeys
                .register_hotkey(id, key, move || handle.send(command))
            {
                warn!("注册扩展屏快捷键失败：{err}");
                failed = true;
            } else {
                registered.push((id, key.virtual_key as u8, command));
            }
        }
        state.desired = registered;
        if failed {
            state.handle.send(Command::DisplayShortcutFailed);
        }
    });
}

/// 在主线程上创建托盘图标，菜单命令转给 `handle`。
pub(crate) fn install(
    handle: Handle,
    _data_dir: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let status = MenuItem::new("正在启动…", false, None);
    let toggle = MenuItem::with_id(TOGGLE, "暂停共享", true, None);
    let open = MenuItem::with_id(OPEN, "打开设置", true, None);
    let quit = MenuItem::with_id(QUIT, "退出 ScreenFerry", true, None);
    let update = MenuItem::with_id(UPDATE, "", true, None);
    let update_separator = PredefinedMenuItem::separator();
    let display = Submenu::new("扩展屏", false);
    let plans = Submenu::new("方案", false);
    let extend_controller =
        MenuItem::with_id(EXTEND_CONTROLLER, "用被控电脑屏幕扩展主控", false, None);
    let extend_controlled =
        MenuItem::with_id(EXTEND_CONTROLLED, "用主控电脑屏幕扩展被控", false, None);
    let pause_extension = MenuItem::with_id("pause-extension", "暂停扩展屏", false, None);
    let stop_extension = MenuItem::with_id(STOP_EXTENSION, "取消副屏拓展", false, None);
    display.append_items(&[
        &plans,
        &extend_controller,
        &extend_controlled,
        &pause_extension,
        &stop_extension,
    ])?;
    let menu = Menu::new();
    menu.append_items(&[
        &status,
        &PredefinedMenuItem::separator(),
        &toggle,
        &display,
        &open,
        &PredefinedMenuItem::separator(),
        &quit,
    ])?;
    #[cfg(target_os = "windows")]
    let elevate = {
        let item = MenuItem::with_id(ELEVATE, "以管理员权限重启后台", true, None);
        menu.insert(&item, 5)?;
        item
    };

    let connected = decode(art::CONNECTED)?;
    let disconnected = decode(art::DISCONNECTED)?;
    let icon = art::builder(disconnected.clone())
        .with_tooltip("ScreenFerry")
        .with_menu(Box::new(menu.clone()))
        .build()?;

    #[cfg(target_os = "windows")]
    {
        use tray_icon::{MouseButton, MouseButtonState, TrayIconEvent};

        let handle = handle.clone();
        TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                handle.send(Command::OpenSettings);
            }
        }));
    }

    #[cfg(target_os = "windows")]
    let data_dir = _data_dir.to_owned();
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| match event.id.0.as_str() {
        TOGGLE => {
            let sharing = handle.summary().borrow().sharing;
            handle.send(Command::SetSharing(!sharing));
        }
        OPEN => handle.send(Command::OpenSettings),
        EXTEND_CONTROLLER => handle.send(Command::Display(DisplayAction::ExtendController)),
        EXTEND_CONTROLLED => handle.send(Command::Display(DisplayAction::ExtendControlled)),
        "pause-extension" => handle.send(Command::Display(DisplayAction::TogglePause)),
        STOP_EXTENSION => handle.send(Command::Display(DisplayAction::Stop)),
        #[cfg(target_os = "windows")]
        ELEVATE => restart_elevated(data_dir.clone()),
        UPDATE => handle.send(Command::InstallUpdate),
        QUIT => handle.send(Command::Quit),
        key => {
            if let Some(id) = key
                .strip_prefix("display-plan-")
                .and_then(|id| id.parse().ok())
            {
                handle.send(Command::RunDisplayPlan(id));
            }
        }
    }));

    TRAY.with(|tray| {
        *tray.borrow_mut() = Some(Tray {
            icon,
            menu,
            status,
            update,
            update_separator,
            update_shown: false,
            toggle,
            open,
            quit,
            display,
            plans,
            extend_controller,
            extend_controlled,
            stop_extension,
            pause_extension,
            #[cfg(target_os = "windows")]
            elevate,
            connected,
            disconnected,
            shown: None,
        });
    });
    Ok(())
}

/// 状态一变就请主线程更新托盘。在引擎所在的 tokio 运行时里运行。
pub(crate) async fn follow(mut summary: watch::Receiver<Summary>, dispatcher: Dispatcher) {
    loop {
        let current = summary.borrow_and_update().clone();
        dispatcher.run_on_main(move || update(&current));
        if summary.changed().await.is_err() {
            return;
        }
    }
}

/// 在主线程上调用。
fn update(summary: &Summary) {
    #[cfg(target_os = "windows")]
    update_display_hotkeys(summary.display_shortcuts, &summary.display_plans);
    TRAY.with(|tray| {
        let mut tray = tray.borrow_mut();
        let Some(tray) = tray.as_mut() else { return };
        if tray.shown.as_ref() == Some(summary) {
            return;
        }
        let lang = summary.lang;
        let status = describe(summary);
        tray.status.set_text(&status);
        tray.toggle.set_text(if summary.sharing {
            lang.pick("暂停共享", "Pause Sharing")
        } else {
            lang.pick("继续共享", "Resume Sharing")
        });
        tray.open.set_text(lang.pick("打开设置", "Open Settings"));
        #[cfg(target_os = "windows")]
        {
            let elevated = ferry_platform_windows::is_elevated();
            tray.elevate.set_enabled(!elevated);
            tray.elevate.set_text(if elevated {
                lang.pick(
                    "后台已具有管理员权限",
                    "Background Is Running as Administrator",
                )
            } else {
                lang.pick(
                    "以管理员权限重启后台",
                    "Restart Background as Administrator",
                )
            });
        }
        tray.display
            .set_text(lang.pick("扩展屏", "Display Extension"));
        tray.plans.set_text(lang.pick("方案", "Plans"));
        tray.plans.set_enabled(!summary.display_plans.is_empty());
        if tray
            .shown
            .as_ref()
            .is_none_or(|old| old.display_plans != summary.display_plans)
        {
            while tray.plans.remove_at(0).is_some() {}
            for plan in &summary.display_plans {
                let label = match plan.shortcut {
                    Some(key) => format!("{} · Ctrl+Alt+Shift+{}", plan.name, char::from(key)),
                    None => plan.name.clone(),
                };
                let item =
                    MenuItem::with_id(format!("display-plan-{}", plan.id), label, true, None);
                if let Err(error) = tray.plans.append(&item) {
                    warn!(%error, "添加扩展屏方案菜单失败");
                }
            }
        }
        tray.display.set_enabled(
            summary.display_available || summary.display_active.is_some() || summary.display_paused,
        );
        tray.extend_controller.set_text(lang.pick(
            "用被控电脑屏幕扩展主控",
            "Extend Controller onto Controlled PC",
        ));
        tray.extend_controlled.set_text(lang.pick(
            "用主控电脑屏幕扩展被控",
            "Extend Controlled PC onto Controller",
        ));
        tray.stop_extension
            .set_text(lang.pick("取消副屏拓展", "Stop Display Extension"));
        tray.extend_controller.set_enabled(
            summary.display_available
                && summary.display_active != Some(DisplayAction::ExtendController),
        );
        tray.extend_controlled.set_enabled(
            summary.display_available
                && summary.display_active != Some(DisplayAction::ExtendControlled),
        );
        tray.stop_extension
            .set_enabled(summary.display_active.is_some() || summary.display_paused);
        tray.pause_extension.set_text(if summary.display_paused {
            lang.pick("恢复扩展屏", "Resume Display Extension")
        } else {
            lang.pick("暂停扩展屏", "Pause Display Extension")
        });
        tray.pause_extension
            .set_enabled(summary.display_active.is_some() || summary.display_paused);
        tray.quit
            .set_text(lang.pick("退出 ScreenFerry", "Quit ScreenFerry"));
        show_update(tray, summary);
        let linked = summary.sharing && summary.problem.is_none() && summary.connected_to.is_some();
        let icon = if linked {
            &tray.connected
        } else {
            &tray.disconnected
        };
        let set = art::set(&tray.icon, icon.clone());
        if let Err(e) = set {
            warn!("更新托盘图标失败：{e}");
        }
        let _ = tray
            .icon
            .set_tooltip(Some(format!("ScreenFerry · {status}")));
        tray.shown = Some(summary.clone());
    });
}

/// 有下载好的新版本时，在“退出”上面插一项“重启并更新到 …”，装好后去掉。
fn show_update(tray: &mut Tray, summary: &Summary) {
    match (&summary.update, tray.update_shown) {
        (Some(version), shown) => {
            tray.update.set_text(summary.lang.pick(
                format!("重启并更新到 {version}"),
                format!("Restart to Update to {version}"),
            ));
            if !shown {
                // 退出始终是最后一项，Windows 还含管理员模式入口。
                let at = tray.menu.items().len().saturating_sub(1);
                let inserted = tray
                    .menu
                    .insert_items(&[&tray.update, &tray.update_separator], at);
                match inserted {
                    Ok(()) => tray.update_shown = true,
                    Err(e) => warn!("托盘菜单加不上“重启并更新”：{e}"),
                }
            }
        }
        (None, true) => {
            let _ = tray.menu.remove(&tray.update);
            let _ = tray.menu.remove(&tray.update_separator);
            tray.update_shown = false;
        }
        (None, false) => {}
    }
}

#[cfg(target_os = "windows")]
fn restart_elevated(data_dir: std::path::PathBuf) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static PENDING: AtomicBool = AtomicBool::new(false);
    if PENDING.swap(true, Ordering::AcqRel) {
        return;
    }
    // UAC confirmation must not block the tray's message loop. The new process
    // asks the existing resident to exit only after elevation succeeds.
    if let Err(error) = std::thread::Builder::new()
        .name("ferry-elevate".into())
        .spawn(move || {
            let result = std::env::current_exe()
                .map_err(|error| error.to_string())
                .and_then(|exe| {
                    ferry_platform_windows::launch_elevated(&exe, Some(&data_dir))
                        .map_err(|error| error.to_string())
                });
            if let Err(error) = result {
                warn!("未切换到管理员后台，现有后台继续运行：{error}");
            }
            PENDING.store(false, Ordering::Release);
        })
    {
        PENDING.store(false, Ordering::Release);
        warn!("请求管理员后台失败：{error}");
    }
}

/// 菜单第一行的状态文字。
fn describe(summary: &Summary) -> String {
    if let Some(problem) = &summary.problem {
        let short: String = problem.chars().take(28).collect();
        let ellipsis = if problem.chars().count() > 28 {
            "…"
        } else {
            ""
        };
        return format!("⚠︎ {short}{ellipsis}");
    }
    let lang = summary.lang;
    match (&summary.connected_to, summary.sharing) {
        (_, false) => lang.pick("已暂停共享", "Sharing paused").to_owned(),
        (Some(peer), true) => lang.pick(format!("已连接 {peer}"), format!("Connected to {peer}")),
        (None, true) => lang
            .pick("共享中 · 等待连接", "Sharing · Waiting for a connection")
            .to_owned(),
    }
}

fn decode(png_bytes: &[u8]) -> Result<Icon, Box<dyn std::error::Error>> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(png_bytes));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::ALPHA);
    let mut reader = decoder.read_info()?;
    let mut rgba = vec![0; reader.output_buffer_size().ok_or("图标太大")?];
    let info = reader.next_frame(&mut rgba)?;
    rgba.truncate(info.buffer_size());
    Ok(Icon::from_rgba(rgba, info.width, info.height)?)
}
