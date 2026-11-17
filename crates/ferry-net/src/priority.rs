//! 平台优先级由应用注入，网络适配器保持平台无关。
use ferry_platform::{NetworkClass, NetworkFlow, NetworkPriority};
use ferry_proto::Channel;
use std::{
    net::TcpStream,
    sync::{Arc, OnceLock},
};
static POLICY: OnceLock<Arc<dyn NetworkPriority>> = OnceLock::new();
pub fn set_network_priority(policy: Arc<dyn NetworkPriority>) {
    let _ = POLICY.set(policy);
}
pub(crate) fn class(channel: Channel) -> NetworkClass {
    match channel {
        Channel::Data | Channel::File => NetworkClass::Data,
        // 媒体 TCP 也承载输入和控制；UDP 视频使用独立的 Video 优先级。
        _ => NetworkClass::Control,
    }
}
pub(crate) fn attach(stream: &TcpStream, channel: Channel) -> Option<Arc<dyn NetworkFlow>> {
    let policy = POLICY.get()?;
    match policy.tcp(stream, class(channel)) {
        Ok(flow) => Some(flow),
        Err(error) => {
            tracing::warn!(%error,?channel,"系统未授予网络优先级，连接继续");
            None
        }
    }
}
