//! 帧读写：在任意字节流上收发协议消息。

use ferry_proto::{Channel, Codec, Message};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::error::{Error, Result};

/// 每次从流中至少预留这么多字节再读。
const READ_CHUNK: usize = 4 * 1024;

/// 一条连接上的消息收发。读写缓冲区复用，稳定运行时不再分配内存。
pub struct Framed<S> {
    priority: Option<std::sync::Arc<dyn ferry_platform::NetworkFlow>>,
    io: S,
    codec: Codec,
    read_buf: Vec<u8>,
    write_buf: Vec<u8>,
}

impl<S> Framed<S> {
    pub fn new(io: S, channel: Channel) -> Self {
        Self {
            priority: None,
            io,
            codec: Codec::new(channel),
            read_buf: Vec::with_capacity(READ_CHUNK),
            write_buf: Vec::with_capacity(READ_CHUNK),
        }
    }

    pub(crate) fn with_priority(
        mut self,
        priority: Option<std::sync::Arc<dyn ferry_platform::NetworkFlow>>,
    ) -> Self {
        self.priority = priority;
        self
    }

    pub fn get_ref(&self) -> &S {
        &self.io
    }

    /// 本连接的类型，决定允许哪些消息、单帧多大。
    pub fn channel(&self) -> Channel {
        self.codec.channel()
    }

    /// 接受方收到 `Hello` 后才知道连接类型，由握手流程切换过去。
    pub(crate) fn set_channel(&mut self, channel: Channel) {
        self.set_network_class(crate::priority::class(channel));
        self.codec = Codec::new(channel);
    }

    /// 设置本方向的发送优先级，系统不支持时连接继续。
    pub fn set_network_class(&self, class: ferry_platform::NetworkClass) {
        if let Some(flow) = &self.priority
            && let Err(error) = flow.set_class(class)
        {
            tracing::warn!(?class,%error,"TCP 网络优先级调整未生效");
        }
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin> Framed<S> {
    /// 分离媒体连接的读写，保留握手后预读的帧；输入回传不再等待下一帧视频。
    /// 调用前的发送必须已经完成，不能在部分写入后继续使用连接。
    pub fn split(
        self,
    ) -> (
        Framed<tokio::io::ReadHalf<S>>,
        Framed<tokio::io::WriteHalf<S>>,
    ) {
        let channel = self.channel();
        let (read, write) = tokio::io::split(self.io);
        (
            Framed {
                priority: self.priority.clone(),
                io: read,
                codec: self.codec,
                read_buf: self.read_buf,
                write_buf: Vec::new(),
            },
            Framed {
                priority: self.priority,
                io: write,
                codec: Codec::new(channel),
                read_buf: Vec::new(),
                write_buf: self.write_buf,
            },
        )
    }
}

impl<S: AsyncWrite + Unpin> Framed<S> {
    /// 发送一条消息。
    pub async fn send(&mut self, message: &Message) -> Result<()> {
        self.write_buf.clear();
        self.codec.encode(message, &mut self.write_buf)?;
        self.io.write_all(&self.write_buf).await?;
        self.io.flush().await?;
        Ok(())
    }
}

impl<S: AsyncRead + Unpin> Framed<S> {
    /// 已到达的连续绝对移动只取最新位置；点击、按键、进入/离开、挑战均为不可跨越的屏障。
    /// 已签署动作不合并：安全输入代理要求证明序号连续，合并必须在发送端签署之前完成。
    pub async fn recv_latest_pointer(&mut self) -> Result<Option<Message>> {
        fn motion(message: &Message) -> u8 {
            use ferry_types::InputEvent;
            match message {
                Message::Input(InputEvent::PointerMotion { .. }) => 1,
                _ => 0,
            }
        }
        let Some(mut latest) = self.recv().await? else {
            return Ok(None);
        };
        let kind = motion(&latest);
        if kind != 0 {
            while let Some((next, used)) = self.codec.decode(&self.read_buf)? {
                if motion(&next) != kind {
                    break;
                }
                self.read_buf.drain(..used);
                latest = next;
            }
        }
        Ok(Some(latest))
    }
    /// 接收一条消息。对方在帧边界上正常关闭连接时返回 `None`。
    ///
    /// 可以放在 `select!` 中：被取消时已读到的数据保留在缓冲区里，不会丢失。
    pub async fn recv(&mut self) -> Result<Option<Message>> {
        loop {
            if let Some((message, used)) = self.codec.decode(&self.read_buf)? {
                self.read_buf.drain(..used);
                return Ok(Some(message));
            }
            self.read_buf.reserve(READ_CHUNK);
            if self.io.read_buf(&mut self.read_buf).await? == 0 {
                return if self.read_buf.is_empty() {
                    Ok(None)
                } else {
                    Err(Error::TruncatedFrame)
                };
            }
        }
    }
}
