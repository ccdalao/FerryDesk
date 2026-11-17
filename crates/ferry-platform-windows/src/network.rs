//! qWAVE TCP 流优先级；保留 socket 的克隆直到流登记撤下。
use ferry_platform::{Error, NetworkClass, NetworkFlow, NetworkPriority, Result};
use std::{
    net::TcpStream,
    os::windows::io::AsRawSocket,
    sync::{Arc, Mutex},
};
pub struct WinNetworkPriority;
struct Flow {
    native: Mutex<usize>,
    _socket: TcpStream,
}
unsafe extern "C" {
    fn sf_network_priority_new(socket: usize, class: u32) -> *mut std::ffi::c_void;
    fn sf_network_priority_set(flow: *mut std::ffi::c_void, class: u32) -> i32;
    fn sf_network_priority_free(flow: *mut std::ffi::c_void);
}
fn kind(class: NetworkClass) -> u32 {
    match class {
        NetworkClass::Control => 5,
        NetworkClass::Video => 3,
        NetworkClass::Data => 2,
    }
}
impl NetworkPriority for WinNetworkPriority {
    fn tcp(&self, stream: &TcpStream, class: NetworkClass) -> Result<Arc<dyn NetworkFlow>> {
        let socket = stream
            .try_clone()
            .map_err(|e| Error::Failed(e.to_string()))?;
        // SAFETY: 克隆的 socket 在 Flow 的本机登记释放前一直存活。
        let native =
            unsafe { sf_network_priority_new(socket.as_raw_socket() as usize, kind(class)) };
        if native.is_null() {
            return Err(Error::Os {
                call: "qWAVE TCP priority",
                detail: windows::core::Error::from_thread().to_string(),
            });
        }
        tracing::info!(?class,peer=?socket.peer_addr().ok(),"TCP 网络优先级已登记");
        Ok(Arc::new(Flow {
            native: Mutex::new(native as usize),
            _socket: socket,
        }))
    }
}
impl NetworkFlow for Flow {
    fn set_class(&self, class: NetworkClass) -> Result<()> {
        let native = self.native.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: 登记句柄由本对象拥有，所有操作在同一把锁下串行。
        if unsafe { sf_network_priority_set(*native as *mut _, kind(class)) } == 0 {
            return Err(Error::Os {
                call: "qWAVE TCP priority",
                detail: windows::core::Error::from_thread().to_string(),
            });
        }
        Ok(())
    }
}
impl Drop for Flow {
    fn drop(&mut self) {
        let native = self.native.get_mut().unwrap_or_else(|e| e.into_inner());
        // SAFETY: 尚未关闭 _socket，句柄只在这里释放一次。
        unsafe { sf_network_priority_free(*native as *mut _) };
    }
}
