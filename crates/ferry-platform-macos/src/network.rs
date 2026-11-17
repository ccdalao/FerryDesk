//! macOS 的网络优先级适配入口。
use ferry_platform::{Error, NetworkClass, NetworkFlow, NetworkPriority, Result};
use std::{net::TcpStream, sync::Arc};
pub struct MacNetworkPriority;
impl NetworkPriority for MacNetworkPriority {
    fn tcp(&self, _stream: &TcpStream, _class: NetworkClass) -> Result<Arc<dyn NetworkFlow>> {
        Err(Error::Unsupported("macOS 网络 QoS 尚未启用"))
    }
}
