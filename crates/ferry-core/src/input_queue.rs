//! 有界的实时输入队列：连续绝对移动只保留最新位置，不跨越按键或点击。

use std::collections::VecDeque;

use ferry_types::InputEvent;

/// 鼠标报告率不应决定延迟或占满按键队列。
pub struct InputQueue {
    events: VecDeque<InputEvent>,
    capacity: usize,
}

impl InputQueue {
    pub fn new(capacity: usize) -> Self {
        Self {
            events: VecDeque::new(),
            capacity,
        }
    }

    /// 只合并相邻的绝对移动；返回 false 表示不可丢弃的事件已经占满队列。
    pub fn push(&mut self, event: InputEvent) -> bool {
        if matches!(event, InputEvent::PointerMotion { .. })
            && let Some(last @ InputEvent::PointerMotion { .. }) = self.events.back_mut()
        {
            *last = event;
            return true;
        }
        if self.events.len() == self.capacity {
            return false;
        }
        self.events.push_back(event);
        true
    }

    pub fn pop(&mut self) -> Option<InputEvent> {
        self.events.pop_front()
    }
}
