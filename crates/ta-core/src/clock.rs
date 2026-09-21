use serde::{Deserialize, Serialize};

/// 可注入时钟。**所有涉及"现在几点"的逻辑都必须走它**，否则没法写确定性测试。
pub trait Clock: Send + Sync {
    /// Unix 毫秒
    fn now_ms(&self) -> i64;
    /// 本地时区偏移（分钟），用于"今天/本周"这类按本地时间的聚合
    fn local_offset_minutes(&self) -> i32;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> i64 {
        chrono::Utc::now().timestamp_millis()
    }
    fn local_offset_minutes(&self) -> i32 {
        use chrono::Offset;
        chrono::Local::now().offset().fix().local_minus_utc() / 60
    }
}

/// 测试用固定时钟。
#[derive(Debug, Clone, Copy)]
pub struct FixedClock {
    pub ms: i64,
    pub offset_minutes: i32,
}

impl FixedClock {
    pub fn new(ms: i64) -> Self {
        Self { ms, offset_minutes: 480 }
    }
}

impl Clock for FixedClock {
    fn now_ms(&self) -> i64 {
        self.ms
    }
    fn local_offset_minutes(&self) -> i32 {
        self.offset_minutes
    }
}

/// 一个可推进的测试时钟（用于额度窗口滚动、冷却退出这类测试）。
#[derive(Debug)]
pub struct ManualClock {
    inner: std::sync::Mutex<FixedClock>,
}

impl ManualClock {
    pub fn new(ms: i64) -> Self {
        Self { inner: std::sync::Mutex::new(FixedClock::new(ms)) }
    }
    pub fn advance(&self, ms: i64) {
        let mut g = self.inner.lock().unwrap();
        g.ms += ms;
    }
}

impl Clock for ManualClock {
    fn now_ms(&self) -> i64 {
        self.inner.lock().unwrap().ms
    }
    fn local_offset_minutes(&self) -> i32 {
        self.inner.lock().unwrap().offset_minutes
    }
}

/// 时间范围（查询用）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimeRange {
    /// 显式区间（Unix 毫秒，左闭右开）
    Explicit { from_ms: i64, to_ms: i64 },
    Today,
    Last7d,
    Last30d,
    ThisMonth,
    All,
}
