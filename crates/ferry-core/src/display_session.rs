//! 单个扩展屏会话的生命周期。传输和 Windows 资源由调用方管理。

use std::time::{Duration, Instant};

const START_TIMEOUT: Duration = Duration::from_secs(10);

/// 校准物理接缝，只在越过接缝时变换沿边坐标，视频仍保持固定分辨率。
pub fn calibrated_display_crossing(
    local: ferry_types::Rect,
    remote: ferry_types::Rect,
    side: ferry_types::Side,
    alignment: ferry_types::EdgeAlignment,
    previous: ferry_types::Point,
    position: ferry_types::Point,
) -> ferry_types::Point {
    use ferry_types::{Point, Side};
    let a = alignment.clamped();
    if !local.contains(previous) && !remote.contains(previous) {
        return position;
    }
    let vertical = matches!(side, Side::Left | Side::Right);
    let (local_edge, remote_edge) = crate::alignment::aligned_edges(local, remote, vertical, a);
    let (base, len, contact, span, rb, rl) = if vertical {
        (
            local.y,
            local.height,
            local_edge.y,
            local_edge.height,
            remote_edge.y,
            remote_edge.height,
        )
    } else {
        (
            local.x,
            local.width,
            local_edge.x,
            local_edge.width,
            remote_edge.x,
            remote_edge.width,
        )
    };
    let (contact, span, rb, rl) = (
        i64::from(contact),
        i64::from(span),
        i64::from(rb),
        i64::from(rl),
    );
    let across = |p: Point| match side {
        Side::Right => p.x >= remote.x,
        Side::Left => p.x < local.x,
        Side::Bottom => p.y >= remote.y,
        Side::Top => p.y < local.y,
    };
    if across(previous) == across(position) {
        return position;
    }
    let along = i64::from(if vertical { position.y } else { position.x });
    let mapped = if across(position) {
        if along < contact || along >= contact + span {
            return match side {
                Side::Right => Point::new(local.right() - 1, position.y),
                Side::Left => Point::new(local.x, position.y),
                Side::Bottom => Point::new(position.x, local.bottom() - 1),
                Side::Top => Point::new(position.x, local.y),
            };
        }
        rb + (along - contact) * rl / span
    } else {
        let result = contact + (along - rb) * span / rl.max(1);
        if along < rb
            || along >= rb + rl
            || result < i64::from(base)
            || result >= i64::from(base) + i64::from(len)
        {
            return match side {
                Side::Right => Point::new(remote.x, position.y),
                Side::Left => Point::new(remote.right() - 1, position.y),
                Side::Bottom => Point::new(position.x, remote.y),
                Side::Top => Point::new(position.x, remote.bottom() - 1),
            };
        }
        result
    };
    let mapped = mapped.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32;
    if vertical {
        Point::new(position.x, mapped)
    } else {
        Point::new(mapped, position.y)
    }
}

/// 共用的窗口/鼠标桌面边界；返回值与锚屏贴合，不预留隐藏间隙。
pub fn display_extension_bounds(
    anchor: ferry_types::Rect,
    width: u32,
    height: u32,
    side: ferry_types::Side,
) -> Option<ferry_types::Rect> {
    use ferry_types::{Rect, Side};
    let (x, y) = match side {
        Side::Left => (i64::from(anchor.x) - i64::from(width), i64::from(anchor.y)),
        Side::Right => (
            i64::from(anchor.x) + i64::from(anchor.width),
            i64::from(anchor.y),
        ),
        Side::Top => (i64::from(anchor.x), i64::from(anchor.y) - i64::from(height)),
        Side::Bottom => (
            i64::from(anchor.x),
            i64::from(anchor.y) + i64::from(anchor.height),
        ),
    };
    if width == 0 || height == 0 {
        return None;
    }
    Some(Rect::new(
        i32::try_from(x).ok()?,
        i32::try_from(y).ok()?,
        width,
        height,
    ))
}

/// 保存配对设备、输入会话和扩展屏请求三者的绑定。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DisplaySessionKey {
    pub fingerprint: [u8; 32],
    pub input_session: u64,
    pub display_session: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DisplaySession {
    pub key: DisplaySessionKey,
    pub local_source: bool,
    pub audio: bool,
    deadline: Option<Instant>,
    media_claimed: bool,
}

impl DisplaySession {
    pub fn starting(key: DisplaySessionKey, local_source: bool, audio: bool, now: Instant) -> Self {
        Self {
            key,
            local_source,
            audio,
            deadline: Some(now + START_TIMEOUT),
            media_claimed: false,
        }
    }

    /// 每次启动只认领一个媒体连接。换向或重连时由调用方创建新的会话对象。
    pub fn claim_media(&mut self, key: DisplaySessionKey, now: Instant) -> bool {
        if self.key != key || self.media_claimed || self.expired(now) {
            return false;
        }
        self.media_claimed = true;
        true
    }

    /// 收到首帧（接收端）或确认对端收到首帧（发送端）才算开始。
    pub fn first_frame(&mut self, key: DisplaySessionKey, now: Instant) -> bool {
        if self.key != key || !self.media_claimed || self.expired(now) || self.deadline.is_none() {
            return false;
        }
        self.deadline = None;
        true
    }

    pub fn expired(&self, now: Instant) -> bool {
        self.deadline.is_some_and(|deadline| now >= deadline)
    }

    pub const fn is_running(&self) -> bool {
        self.deadline.is_none()
    }

    pub const fn media_claimed(&self) -> bool {
        self.media_claimed
    }
}
