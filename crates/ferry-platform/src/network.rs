//! 对已经连接的 TCP 流申请操作系统网络优先级；失败不改变认证或连接策略。
use crate::Result;
use std::net::TcpStream;
use std::sync::Arc;

/// 交互控制最高，实时视频次之，剪贴板和文件使用额外数据优先级。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NetworkClass {
    Control,
    Video,
    Data,
}
pub trait NetworkFlow: Send + Sync {
    fn set_class(&self, class: NetworkClass) -> Result<()>;
}
pub trait NetworkPriority: Send + Sync {
    fn tcp(&self, stream: &TcpStream, class: NetworkClass) -> Result<Arc<dyn NetworkFlow>>;
}
