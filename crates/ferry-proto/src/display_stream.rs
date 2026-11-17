//! 扩展屏的控制消息和独立媒体连接上的分块。

use crate::error::{DecodeError, EncodeError};
use crate::limits::MAX_CHUNK_LEN;
use crate::wire::{Body, Reader, Writer};
use ferry_types::{DisplayOptions, DisplayPrimary, DisplayScreen, Rect, Side};

/// 防止连续的媒体分块把接收端内存无限撑大。H.264 关键帧可以超过单条消息的限制。
pub const MAX_ACCESS_UNIT: usize = 2 * 1024 * 1024;

/// 方向以启动扩展屏的设备为参照。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DisplayDirection {
    /// 本机扩展桌面，对方提供物理屏幕。
    ExtendLocal,
    /// 对方扩展桌面，本机提供物理屏幕。
    ExtendPeer,
}

impl DisplayDirection {
    /// 把发起方的方向换成接收方视角。
    pub const fn reversed(self) -> Self {
        match self {
            Self::ExtendLocal => Self::ExtendPeer,
            Self::ExtendPeer => Self::ExtendLocal,
        }
    }
}

/// 在已认证的 ScreenFerry 会话内交换。发起方为每次启动分配新的 `id`。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DisplayControl {
    RulerStart {
        id: u64,
        screen: Option<u32>,
        horizontal: bool,
    },
    RulerPoints {
        id: u64,
        start: u16,
        end: u16,
    },
    RulerStop {
        id: u64,
    },
    StartConfigured {
        id: u64,
        direction: DisplayDirection,
        audio: bool,
        options: DisplayOptions,
    },
    Screens {
        screens: Vec<DisplayScreen>,
        side: Side,
    },
    Start {
        id: u64,
        direction: DisplayDirection,
        audio: bool,
    },
    Ready {
        id: u64,
    },
    Stop {
        id: u64,
    },
    Unavailable {
        id: u64,
    },
    /// 接收端已呈现第一帧。
    FirstFrame {
        id: u64,
    },
}

impl DisplayControl {
    pub fn requires_contact(&self) -> bool {
        matches!(self, Self::StartConfigured { options, .. } if options.alignment.contact.is_some())
    }
    pub const fn requires_ruler(&self) -> bool {
        matches!(
            self,
            Self::RulerStart { .. } | Self::RulerPoints { .. } | Self::RulerStop { .. }
        )
    }
    pub const fn requires_plans(&self) -> bool {
        matches!(self, Self::StartConfigured { .. } | Self::Screens { .. })
    }
    pub fn requires_alignment(&self) -> bool {
        matches!(self, Self::StartConfigured { options, .. } if !options.alignment.is_default())
    }
    pub const fn id(&self) -> u64 {
        match *self {
            Self::Start { id, .. }
            | Self::RulerStart { id, .. }
            | Self::RulerPoints { id, .. }
            | Self::RulerStop { id }
            | Self::StartConfigured { id, .. }
            | Self::Ready { id }
            | Self::Stop { id }
            | Self::Unavailable { id }
            | Self::FirstFrame { id } => id,
            Self::Screens { .. } => 0,
        }
    }
}

impl Body for DisplayControl {
    fn encode(&self, w: &mut Writer<'_>) -> Result<(), EncodeError> {
        match *self {
            Self::RulerStart {
                id,
                screen,
                horizontal,
            } => {
                w.u8(8);
                w.u64(id);
                w.u32(screen.unwrap_or(u32::MAX));
                w.bool(horizontal);
            }
            Self::RulerPoints { id, start, end } => {
                if start > 1000 || end > 1000 {
                    return Err(EncodeError::LimitExceeded("实屏尺取点"));
                }
                w.u8(9);
                w.u64(id);
                w.u16(start);
                w.u16(end);
            }
            Self::RulerStop { id } => {
                w.u8(10);
                w.u64(id);
            }
            Self::StartConfigured {
                id,
                direction,
                audio,
                options,
            } => {
                let aligned = !options.alignment.is_default();
                if options.alignment != options.alignment.clamped() {
                    return Err(EncodeError::LimitExceeded("接缝校准范围"));
                }
                w.u8(if options.alignment.contact.is_some() {
                    11
                } else if aligned {
                    7
                } else {
                    5
                });
                w.u64(id);
                w.u8(u8::from(direction == DisplayDirection::ExtendPeer));
                w.bool(audio);
                w.u32(options.source_screen.unwrap_or(u32::MAX));
                w.u32(options.viewer_screen.unwrap_or(u32::MAX));
                options.side.encode(w)?;
                w.bool(options.primary == DisplayPrimary::Viewer);
                if aligned {
                    w.i32(options.alignment.offset);
                    w.u32(options.alignment.span);
                }
                if let Some(contact) = options.alignment.contact {
                    for point in contact.local.into_iter().chain(contact.remote) {
                        w.u16(point);
                    }
                }
            }
            Self::Screens { ref screens, side } => {
                if screens.len() > 16 {
                    return Err(EncodeError::LimitExceeded("扩展屏清单"));
                }
                w.u8(6);
                w.u64(0);
                side.encode(w)?;
                w.u8(screens.len() as u8);
                for screen in screens {
                    w.u32(screen.id);
                    screen.bounds.encode(w)?;
                    w.bool(screen.primary);
                }
            }
            Self::Start {
                id,
                direction,
                audio,
            } => {
                w.u8(0);
                w.u64(id);
                w.u8(match direction {
                    DisplayDirection::ExtendLocal => 0,
                    DisplayDirection::ExtendPeer => 1,
                });
                w.bool(audio);
            }
            Self::Ready { id } => {
                w.u8(1);
                w.u64(id);
            }
            Self::Stop { id } => {
                w.u8(2);
                w.u64(id);
            }
            Self::Unavailable { id } => {
                w.u8(3);
                w.u64(id);
            }
            Self::FirstFrame { id } => {
                w.u8(4);
                w.u64(id);
            }
        }
        Ok(())
    }

    fn decode(r: &mut Reader<'_>) -> Result<Self, DecodeError> {
        let kind = r.u8()?;
        let id = r.u64()?;
        match kind {
            8 => {
                let screen = r.u32()?;
                Ok(Self::RulerStart {
                    id,
                    screen: (screen != u32::MAX).then_some(screen),
                    horizontal: r.bool()?,
                })
            }
            9 => {
                let start = r.u16()?;
                let end = r.u16()?;
                if start > 1000 || end > 1000 {
                    return Err(DecodeError::InvalidValue("实屏尺取点"));
                }
                Ok(Self::RulerPoints { id, start, end })
            }
            10 => Ok(Self::RulerStop { id }),
            5 | 7 | 11 => {
                let direction = match r.u8()? {
                    0 => DisplayDirection::ExtendLocal,
                    1 => DisplayDirection::ExtendPeer,
                    _ => return Err(DecodeError::InvalidValue("扩展屏方向")),
                };
                let audio = r.bool()?;
                let source = r.u32()?;
                let viewer = r.u32()?;
                Ok(Self::StartConfigured {
                    id,
                    direction,
                    audio,
                    options: DisplayOptions {
                        source_screen: (source != u32::MAX).then_some(source),
                        viewer_screen: (viewer != u32::MAX).then_some(viewer),
                        side: Side::decode(r)?,
                        primary: if r.bool()? {
                            DisplayPrimary::Viewer
                        } else {
                            DisplayPrimary::Source
                        },
                        alignment: if kind != 5 {
                            let alignment = ferry_types::EdgeAlignment {
                                offset: r.i32()?,
                                span: r.u32()?,
                                contact: if kind == 11 {
                                    Some(ferry_types::EdgeContact {
                                        local: [r.u16()?, r.u16()?],
                                        remote: [r.u16()?, r.u16()?],
                                    })
                                } else {
                                    None
                                },
                            };
                            if alignment != alignment.clamped() || alignment.is_default() {
                                return Err(DecodeError::InvalidValue("接缝校准范围"));
                            }
                            alignment
                        } else {
                            Default::default()
                        },
                    },
                })
            }
            6 => {
                let side = Side::decode(r)?;
                let len = r.u8()? as usize;
                if id != 0 || len > 16 {
                    return Err(DecodeError::InvalidValue("扩展屏清单"));
                }
                let mut screens = Vec::with_capacity(len);
                for _ in 0..len {
                    screens.push(DisplayScreen {
                        id: r.u32()?,
                        bounds: Rect::decode(r)?,
                        primary: r.bool()?,
                    });
                }
                Ok(Self::Screens { screens, side })
            }
            0 => {
                let direction = match r.u8()? {
                    0 => DisplayDirection::ExtendLocal,
                    1 => DisplayDirection::ExtendPeer,
                    _ => return Err(DecodeError::InvalidValue("扩展屏方向")),
                };
                Ok(Self::Start {
                    id,
                    direction,
                    audio: r.bool()?,
                })
            }
            1 => Ok(Self::Ready { id }),
            2 => Ok(Self::Stop { id }),
            3 => Ok(Self::Unavailable { id }),
            4 => Ok(Self::FirstFrame { id }),
            _ => Err(DecodeError::InvalidValue("扩展屏控制消息")),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DisplayTrack {
    Video,
    Audio,
}

/// 一个编码单元可跨多个分块，同一轨道的分块按顺序传输。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DisplayPacket {
    pub track: DisplayTrack,
    /// 相对媒体会话开始的时间，单位为微秒。
    pub timestamp_us: u64,
    pub end: bool,
    pub bytes: Vec<u8>,
}

/// 完整的编码视频帧或音频帧。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DisplayAccessUnit {
    pub track: DisplayTrack,
    pub timestamp_us: u64,
    pub bytes: Vec<u8>,
}

/// 当前发送方向只允许一个未完成编码单元，避免串轨或混入另一帧。
#[derive(Debug, Default)]
pub struct DisplayAssembler(Option<DisplayAccessUnit>);

impl DisplayAssembler {
    pub fn push(
        &mut self,
        packet: DisplayPacket,
    ) -> Result<Option<DisplayAccessUnit>, DecodeError> {
        if packet.bytes.is_empty() {
            return Err(DecodeError::InvalidValue("空的扩展屏媒体分块"));
        }
        let unit = self.0.get_or_insert_with(|| DisplayAccessUnit {
            track: packet.track,
            timestamp_us: packet.timestamp_us,
            bytes: Vec::new(),
        });
        if unit.track != packet.track || unit.timestamp_us != packet.timestamp_us {
            return Err(DecodeError::InvalidValue("扩展屏媒体分块次序"));
        }
        if unit.bytes.len().saturating_add(packet.bytes.len()) > MAX_ACCESS_UNIT {
            return Err(DecodeError::LimitExceeded("扩展屏编码单元长度"));
        }
        unit.bytes.extend(packet.bytes);
        Ok(if packet.end { self.0.take() } else { None })
    }
}

impl Body for DisplayPacket {
    fn encode(&self, w: &mut Writer<'_>) -> Result<(), EncodeError> {
        w.u8(match self.track {
            DisplayTrack::Video => 0,
            DisplayTrack::Audio => 1,
        });
        w.u64(self.timestamp_us);
        w.bool(self.end);
        w.bytes(&self.bytes, MAX_CHUNK_LEN, "扩展屏媒体分块长度")
    }

    fn decode(r: &mut Reader<'_>) -> Result<Self, DecodeError> {
        let track = match r.u8()? {
            0 => DisplayTrack::Video,
            1 => DisplayTrack::Audio,
            _ => return Err(DecodeError::InvalidValue("扩展屏媒体轨道")),
        };
        Ok(Self {
            track,
            timestamp_us: r.u64()?,
            end: r.bool()?,
            bytes: r.bytes(MAX_CHUNK_LEN, "扩展屏媒体分块长度")?,
        })
    }
}
