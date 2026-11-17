//! 通过已经核验的扩展屏媒体连接收发完整的编码单元。

use std::time::Duration;

use ferry_net::Framed;
use ferry_proto::limits::MAX_CHUNK_LEN;
use ferry_proto::{DisplayAccessUnit, DisplayAssembler, DisplayPacket, MAX_ACCESS_UNIT, Message};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::time::timeout;

use crate::{Error, Result};

// 单帧有界、发送前才采集，短暂热点抖动不再被误判为会话结束。
pub(crate) const MEDIA_TIMEOUT: Duration = Duration::from_secs(5);

/// 调用方在任何传输错误后关闭媒体连接，避免继续发送不完整的编码单元。
pub async fn send_display_unit<S: AsyncWrite + Unpin>(
    stream: &mut Framed<S>,
    unit: DisplayAccessUnit,
) -> Result<()> {
    if unit.bytes.is_empty() || unit.bytes.len() > MAX_ACCESS_UNIT {
        return Err(Error::Protocol("扩展屏编码单元长度无效"));
    }
    let send = async {
        let mut chunks = unit.bytes.chunks(MAX_CHUNK_LEN).peekable();
        while let Some(bytes) = chunks.next() {
            stream
                .send(&Message::DisplayPacket(DisplayPacket {
                    track: unit.track,
                    timestamp_us: unit.timestamp_us,
                    end: chunks.peek().is_none(),
                    bytes: bytes.to_vec(),
                }))
                .await?;
        }
        Ok(())
    };
    timeout(MEDIA_TIMEOUT, send)
        .await
        .map_err(|_| Error::Timeout("发送扩展屏帧"))?
}

/// 接收一个编码单元或会话结束。空闲的来源可用心跳维持连接。
/// 接到第一块后启用独立的帧时限，持续到达的残缺帧也会超时。
pub async fn receive_display_unit<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut Framed<S>,
) -> Result<Option<DisplayAccessUnit>> {
    let first = loop {
        let message = timeout(MEDIA_TIMEOUT, stream.recv())
            .await
            .map_err(|_| Error::Timeout("接收扩展屏帧"))??;
        match message {
            Some(Message::DisplayPacket(packet)) => break packet,
            Some(Message::Ping { nonce }) => {
                timeout(MEDIA_TIMEOUT, stream.send(&Message::Pong { nonce }))
                    .await
                    .map_err(|_| Error::Timeout("回应扩展屏心跳"))??;
            }
            Some(Message::Bye(_)) | None => return Ok(None),
            _ => return Err(Error::Protocol("意外的扩展屏媒体消息")),
        }
    };
    let assemble = async {
        let mut assembler = DisplayAssembler::default();
        let mut packet = first;
        loop {
            if let Some(unit) = assembler.push(packet).map_err(ferry_net::Error::from)? {
                return Ok(Some(unit));
            }
            packet = match stream.recv().await? {
                Some(Message::DisplayPacket(packet)) => packet,
                _ => return Err(Error::Protocol("扩展屏帧在分块中途结束")),
            };
        }
    };
    timeout(MEDIA_TIMEOUT, assemble)
        .await
        .map_err(|_| Error::Timeout("重组扩展屏帧"))?
}
