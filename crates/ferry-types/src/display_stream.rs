//! 扩展屏的用户操作和本机快捷键偏好。

use crate::{Rect, Side};
use serde::{Deserialize, Serialize};

/// Windows 的主显示器属于桌面来源，或属于接收电脑显示的虚拟屏。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisplayPrimary {
    #[default]
    Source,
    Viewer,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DisplayOptions {
    pub source_screen: Option<u32>,
    pub viewer_screen: Option<u32>,
    /// 接收屏在桌面来源实体屏的哪一侧。
    pub side: Side,
    pub primary: DisplayPrimary,
    #[serde(skip_serializing_if = "crate::EdgeAlignment::is_default")]
    pub alignment: crate::EdgeAlignment,
}

impl Default for DisplayOptions {
    fn default() -> Self {
        Self {
            source_screen: None,
            viewer_screen: None,
            side: Side::Right,
            primary: DisplayPrimary::Source,
            alignment: Default::default(),
        }
    }
}

/// 数字对应 Windows 的 DISPLAYn 设备名，不沿用枚举顺序。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisplayScreen {
    pub id: u32,
    pub bounds: Rect,
    pub primary: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisplayPlan {
    pub id: u32,
    pub name: String,
    pub peer: String,
    /// 相对于保存本方案的电脑；不随主控/被控角色变化。
    pub local_source: bool,
    pub options: DisplayOptions,
    pub audio: bool,
    pub shortcut: Option<u8>,
}

impl DisplayPlan {
    pub fn valid(&self) -> bool {
        (1..=1_000_000).contains(&self.id)
            && !self.name.trim().is_empty()
            && self.name.chars().count() <= 64
            && self.peer.len() == 64
            && self.peer.bytes().all(|b| b.is_ascii_hexdigit())
            && self.options.alignment == self.options.alignment.clamped()
            && self.shortcut.is_none_or(|key| key.is_ascii_digit())
    }
}

/// 主控和被控是现有键鼠会话的角色，任一端发出命令的含义一致。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisplayAction {
    ExtendController,
    ExtendControlled,
    Stop,
    TogglePause,
}

impl DisplayAction {
    pub const ALL: [Self; 4] = [
        Self::ExtendController,
        Self::ExtendControlled,
        Self::Stop,
        Self::TogglePause,
    ];

    /// `true` 时本机创建虚拟屏，`false` 时本机显示对方的画面。
    pub const fn local_source(self, controller: bool) -> Option<bool> {
        match self {
            Self::ExtendController => Some(controller),
            Self::ExtendControlled => Some(!controller),
            Self::Stop | Self::TogglePause => None,
        }
    }
}

/// 四个扩展屏快捷键使用 Ctrl + Alt + Shift；空值关闭对应的快捷键。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DisplayShortcuts {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extend_controller: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extend_controlled: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub toggle_pause: Option<u8>,
}

impl DisplayShortcuts {
    pub const fn key(self, action: DisplayAction) -> Option<u8> {
        match action {
            DisplayAction::ExtendController => self.extend_controller,
            DisplayAction::ExtendControlled => self.extend_controlled,
            DisplayAction::Stop => self.stop,
            DisplayAction::TogglePause => self.toggle_pause,
        }
    }

    pub fn valid(self) -> bool {
        let keys = [
            self.extend_controller,
            self.extend_controlled,
            self.stop,
            self.toggle_pause,
        ];
        keys.iter().enumerate().all(|(i, key)| {
            key.is_none_or(|key| key.is_ascii_digit() && !keys[..i].contains(&Some(key)))
        })
    }
}
