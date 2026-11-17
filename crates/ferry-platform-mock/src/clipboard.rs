//! 剪贴板：`Clipboard` 的测试实现。

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use ferry_platform::{Clipboard, Result};
use ferry_types::{ClipboardContent, ClipboardFormat};

/// 假的剪贴板：测试用 [`copy`](Self::copy) 模拟用户复制，用 [`content`](Self::content) 观察结果。
#[derive(Clone, Default)]
pub struct MockClipboard {
    state: Arc<Mutex<State>>,
}

#[derive(Default)]
struct State {
    content: ClipboardContent,
    count: u64,
    writes: usize,
    reads: usize,
    next_read_delay: Option<Duration>,
}

impl MockClipboard {
    pub fn new() -> Self {
        Self::default()
    }

    /// 模拟本机用户复制了一段内容。
    pub fn copy(&self, content: ClipboardContent) {
        let mut state = self.lock();
        state.content = content;
        state.count += 1;
    }

    /// 当前剪贴板内容。
    pub fn content(&self) -> ClipboardContent {
        self.lock().content.clone()
    }

    /// 被测代码写入剪贴板的次数。
    pub fn writes(&self) -> usize {
        self.lock().writes
    }

    /// 模拟其他程序延迟渲染剪贴板，或图片格式转换耗时。
    pub fn delay_next_read(&self, delay: Duration) {
        self.lock().next_read_delay = Some(delay);
    }

    /// 已开始的读取次数，包含仍在等待系统返回的读取。
    pub fn reads(&self) -> usize {
        self.lock().reads
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Clipboard for MockClipboard {
    fn formats(&self) -> &'static [ClipboardFormat] {
        &[
            ClipboardFormat::TEXT,
            ClipboardFormat::HTML,
            ClipboardFormat::RTF,
            ClipboardFormat::PNG,
        ]
    }

    fn change_count(&mut self) -> u64 {
        self.lock().count
    }

    fn read(&mut self) -> Result<ClipboardContent> {
        let (content, delay) = {
            let mut state = self.lock();
            state.reads += 1;
            (state.content.clone(), state.next_read_delay.take())
        };
        if let Some(delay) = delay {
            std::thread::sleep(delay);
        }
        Ok(content)
    }

    fn write(&mut self, content: &ClipboardContent) -> Result<u64> {
        let mut state = self.lock();
        state.content = content.clone();
        state.count += 1;
        state.writes += 1;
        Ok(state.count)
    }
}
