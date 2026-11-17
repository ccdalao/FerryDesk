//! 实时媒体的参考链恢复与热点码率反馈；不涉及 IO 或平台状态。

pub struct MediaRecovery {
    waiting: bool,
}
impl Default for MediaRecovery {
    fn default() -> Self {
        Self { waiting: true }
    }
}
impl MediaRecovery {
    pub fn lost(&mut self) {
        self.waiting = true;
    }
    pub fn waiting(&self) -> bool {
        self.waiting
    }
    /// 丢帧后只从 Annex B H.264 的 IDR 恢复，不把依赖帧送入解码器。
    pub fn accept(&mut self, bytes: &[u8]) -> bool {
        if self.waiting
            && bytes
                .windows(4)
                .any(|nal| nal[..3] == [0, 0, 1] && nal[3] & 31 == 5)
        {
            self.waiting = false;
        }
        !self.waiting
    }
}
pub struct MediaBitrate {
    current: u32,
    stable: u8,
    bad: u8,
    delay_bad: u8,
    cooldown: u8,
}
impl Default for MediaBitrate {
    fn default() -> Self {
        Self {
            current: 24_000_000,
            stable: 0,
            bad: 0,
            delay_bad: 0,
            cooldown: 0,
        }
    }
}
impl MediaBitrate {
    pub fn current(&self) -> u32 {
        self.current
    }
    /// 每 100ms 一次反馈。成功的 FEC 恢复不是丢帧，也不能把单次呈现抖动当成持续拥塞。
    pub fn observe(
        &mut self,
        frames: u32,
        lost: u32,
        _recovered: u32,
        queue_ms: u16,
    ) -> Option<u32> {
        self.cooldown = self.cooldown.saturating_sub(1);
        if lost > 0 || (frames > 0 && queue_ms >= 100) {
            self.bad = self.bad.saturating_add(1);
            self.stable = 0;
            if self.bad >= 3 {
                return self.reduce();
            }
        } else if frames > 0 {
            self.bad = 0;
            self.stable += 1;
            if self.stable >= 20 && self.delay_bad == 0 {
                let before = self.current;
                self.current = (self.current + (self.current / 4).max(2_000_000)).min(40_000_000);
                self.stable = 0;
                return (before != self.current).then_some(self.current);
            }
        }
        None
    }
    /// 只使用 UDP 控制通道即时回应测得的 RTT，不把解码消费确认当网络 RTT。
    pub fn observe_delay(&mut self, rtt_ms: u32, baseline_ms: u32) -> Option<u32> {
        if rtt_ms > (baseline_ms.saturating_mul(3) + 25).max(60) {
            self.delay_bad = self.delay_bad.saturating_add(1);
            self.stable = 0;
            if self.delay_bad >= 2 {
                return self.reduce();
            }
        } else {
            self.delay_bad = 0;
        }
        None
    }
    fn reduce(&mut self) -> Option<u32> {
        if self.cooldown > 0 {
            return None;
        }
        let before = self.current;
        self.current = (self.current * 4 / 5).max(8_000_000);
        self.cooldown = 10;
        self.bad = 0;
        self.stable = 0;
        (before != self.current).then_some(self.current)
    }
}
