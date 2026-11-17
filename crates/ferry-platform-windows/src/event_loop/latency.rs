//! 输入后台的低延迟提示。无线设置只属于本 WLAN 客户端，退出时释放，不改网卡或 UAC 策略。

use std::sync::{Once, mpsc};
use std::thread;
use std::time::Duration;

use windows::Win32::Foundation::HANDLE;
use windows::Win32::NetworkManagement::WiFi::{
    WLAN_INTERFACE_INFO_LIST, WlanCloseHandle, WlanEnumInterfaces, WlanFreeMemory, WlanOpenHandle,
    WlanSetInterface, wlan_interface_state_connected, wlan_intf_opcode_media_streaming_mode,
};
use windows::Win32::System::Threading::{
    GetCurrentProcess, PROCESS_POWER_THROTTLING_CURRENT_VERSION,
    PROCESS_POWER_THROTTLING_EXECUTION_SPEED, PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION,
    PROCESS_POWER_THROTTLING_STATE, ProcessPowerThrottling, SetProcessInformation,
};
use windows::core::{BOOL, GUID};

/// 跟随输入捕获/注入实例。网络系统调用在独立线程，不能阻塞输入运行时。
pub(crate) struct InputLatency {
    stop: Option<mpsc::Sender<()>>,
}

impl InputLatency {
    pub(crate) fn start() -> Self {
        static QOS: Once = Once::new();
        QOS.call_once(|| {
            let state = PROCESS_POWER_THROTTLING_STATE {
                Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
                ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED
                    | PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION,
                StateMask: 0,
            };
            // SAFETY: 仅为本进程明确关闭执行速度节流；不改系统电源计划或其他进程优先级。
            let _ = unsafe {
                SetProcessInformation(
                    GetCurrentProcess(),
                    ProcessPowerThrottling,
                    (&state as *const PROCESS_POWER_THROTTLING_STATE).cast(),
                    size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
                )
            };
        });
        let (stop, receiver) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("ferry-wifi-latency".into())
            .spawn(move || {
                let Some(mut wlan) = Wlan::open() else {
                    return; // 没有 WLAN 服务（例如有线电脑）不影响输入。
                };
                loop {
                    wlan.refresh();
                    // 断开连接会自动清除媒体流模式，重连后重新申请。不依赖设置窗口是否可见。
                    if receiver.recv_timeout(Duration::from_secs(5))
                        != Err(mpsc::RecvTimeoutError::Timeout)
                    {
                        break;
                    }
                }
            });
        Self {
            stop: worker.ok().map(|_| stop),
        }
    }
}

impl Drop for InputLatency {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}

struct Wlan {
    handle: HANDLE,
    interfaces: Vec<GUID>,
}

impl Wlan {
    fn open() -> Option<Self> {
        let mut negotiated = 0;
        let mut handle = HANDLE::default();
        // SAFETY: 输出指向完整本地变量；客户端句柄只在本线程使用，由 Drop 关闭。
        let status = unsafe { WlanOpenHandle(2, None, &mut negotiated, &mut handle) };
        if status != 0 {
            return None;
        }
        Some(Self {
            handle,
            interfaces: Vec::new(),
        })
    }

    fn refresh(&mut self) {
        let mut list: *mut WLAN_INTERFACE_INFO_LIST = std::ptr::null_mut();
        // SAFETY: 系统分配的列表仅在释放前读取；枚举失败时没有需要释放的输出。
        if unsafe { WlanEnumInterfaces(self.handle, None, &mut list) } != 0 || list.is_null() {
            return;
        }
        unsafe {
            let interfaces = std::slice::from_raw_parts(
                (*list).InterfaceInfo.as_ptr(),
                (*list).dwNumberOfItems as usize,
            );
            for interface in interfaces {
                if interface.isState == wlan_interface_state_connected
                    && self.streaming(&interface.InterfaceGuid, true)
                    && !self.interfaces.contains(&interface.InterfaceGuid)
                {
                    self.interfaces.push(interface.InterfaceGuid);
                }
            }
            WlanFreeMemory(list.cast());
        }
    }

    fn streaming(&self, interface: &GUID, enabled: bool) -> bool {
        let enabled = BOOL::from(enabled);
        // SAFETY: 操作只影响此 WLAN 客户端的媒体流请求，参数是正确大小的 BOOL。
        unsafe {
            WlanSetInterface(
                self.handle,
                interface,
                wlan_intf_opcode_media_streaming_mode,
                size_of::<BOOL>() as u32,
                (&enabled as *const BOOL).cast(),
                None,
            ) == 0
        }
    }
}

impl Drop for Wlan {
    fn drop(&mut self) {
        for interface in &self.interfaces {
            let _ = self.streaming(interface, false);
        }
        // SAFETY: 本线程独占的有效句柄，仅关闭一次。其他应用的媒体流请求保持有效。
        let _ = unsafe { WlanCloseHandle(self.handle, None) };
    }
}
