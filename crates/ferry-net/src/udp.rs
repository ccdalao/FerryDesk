//! 已认证媒体会话使用的 UDP 套接字。
//!
//! 本模块只负责局域网地址绑定和异步 datagram 收发。设备身份、临时会话参数的
//! 协商和媒体包格式由上层完成；构造此对象前应已经通过 ScreenFerry 的 TLS
//! 连接验证对端。连接后的 UDP 套接字只接收指定的对端地址，避免把其他主机的
//! datagram 混入媒体重组器。

use std::net::SocketAddr;

use tokio::net::UdpSocket;

use crate::{Error, Result};

/// 由已认证的 ScreenFerry 会话临时生成的媒体连接参数。
///
/// `bind_addr` 应使用到对端的实际局域网出口地址（端口为 0 时由系统分配），
/// 而不是代理或 TUN 地址。`session_id` 只用于把 UDP 端点绑定到当前会话；
/// 它不是认证凭据，媒体包的认证标签由上层协议负责。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UdpMediaConfig {
    pub bind_addr: SocketAddr,
    pub peer_addr: SocketAddr,
    pub session_id: u64,
}

impl UdpMediaConfig {
    pub const fn new(bind_addr: SocketAddr, peer_addr: SocketAddr, session_id: u64) -> Self {
        Self {
            bind_addr,
            peer_addr,
            session_id,
        }
    }
}

/// 一个已连接的 UDP 媒体端点。
pub struct UdpMediaSocket {
    socket: UdpSocket,
    peer_addr: SocketAddr,
    session_id: u64,
}

impl UdpMediaSocket {
    /// 在指定本地接口绑定并连接到已认证的对端。
    pub async fn bind(config: UdpMediaConfig) -> Result<Self> {
        if config.bind_addr.is_ipv4() != config.peer_addr.is_ipv4() {
            return Err(Error::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "UDP 本地地址与对端地址的 IP 版本不一致",
            )));
        }
        let socket = UdpSocket::bind(config.bind_addr).await?;
        socket.connect(config.peer_addr).await?;
        Ok(Self {
            socket,
            peer_addr: config.peer_addr,
            session_id: config.session_id,
        })
    }

    pub fn local_addr(&self) -> Result<SocketAddr> {
        Ok(self.socket.local_addr()?)
    }

    pub const fn peer_addr(&self) -> SocketAddr {
        self.peer_addr
    }

    pub const fn session_id(&self) -> u64 {
        self.session_id
    }

    /// 发送一个已经由上层编码和认证的媒体 datagram。
    pub async fn send(&self, datagram: &[u8]) -> Result<usize> {
        Ok(self.socket.send(datagram).await?)
    }

    /// 接收来自已连接对端的一个 datagram。
    pub async fn recv(&self, buffer: &mut [u8]) -> Result<usize> {
        Ok(self.socket.recv(buffer).await?)
    }

    /// 暴露可读写的 socket 句柄给平台层设置 DSCP/qWAVE 等可选策略。
    ///
    /// 此方法不修改系统网络配置；调用方只能在创建媒体会话后针对该端点设置选项。
    pub fn socket(&self) -> &UdpSocket {
        &self.socket
    }
}
