//! 测试网络优先级，不更改系统 socket 设置。
use ferry_platform::{NetworkClass, NetworkFlow, NetworkPriority, Result};
use std::{
    net::TcpStream,
    sync::{Arc, Mutex},
};
#[derive(Clone, Default)]
pub struct MockNetworkPriority(pub Arc<Mutex<Vec<NetworkClass>>>);
impl NetworkFlow for MockNetworkPriority {
    fn set_class(&self, class: NetworkClass) -> Result<()> {
        self.0.lock().unwrap().push(class);
        Ok(())
    }
}
impl NetworkPriority for MockNetworkPriority {
    fn tcp(&self, _stream: &TcpStream, class: NetworkClass) -> Result<Arc<dyn NetworkFlow>> {
        self.set_class(class)?;
        Ok(Arc::new(self.clone()))
    }
}
