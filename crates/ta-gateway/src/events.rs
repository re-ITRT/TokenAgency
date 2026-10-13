//! 事件总线（Rust → 前端）。**工作包 G4 负责实现**。
//!
//! 性能要求：**环形缓冲 + 200ms 批量合并**，最多 50 条/次 ——
//! 一条请求一次 IPC 会打爆 WebView 的消息队列。

use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use ta_core::types::{Money, RequestSummary};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum TaEvent {
    /// 批量数组，不是单条。Tauri 事件名同样是 `request:new`
    #[serde(rename = "request:new")]
    RequestNew { items: Vec<RequestSummary> },
    #[serde(rename = "usage:tick")]
    UsageTick { today_tokens: u64, today_cost: Money },
    #[serde(rename = "channel:state")]
    ChannelState { channel_id: String, state: String, until_ms: Option<i64> },
    #[serde(rename = "lane:health")]
    LaneHealth { lane_id: String, node: Option<String>, latency_ms: Option<u32>, ok: bool },
    #[serde(rename = "kernel:status")]
    KernelStatus { state: String, version: Option<String> },
    #[serde(rename = "config:changed")]
    ConfigChanged { source: String, applied: bool },
    #[serde(rename = "subscription:updated")]
    SubscriptionUpdated { id: String, ok: bool, node_count: usize, error: Option<String> },
    #[serde(rename = "log:append")]
    LogAppend { items: Vec<serde_json::Value> },
    #[serde(rename = "toast")]
    Toast { level: String, title: String, body: String, dedupe_key: String },
    #[serde(rename = "tray:refresh")]
    TrayRefresh { payload: serde_json::Value },
}

impl TaEvent {
    /// 事件名，直接用于 `app.emit(name, payload)`。
    /// **必须与  逐一对应** —— 前端按名字订阅，写错就静默收不到。
    pub fn name(&self) -> &'static str {
        match self {
            TaEvent::RequestNew { .. } => "request:new",
            TaEvent::UsageTick { .. } => "usage:tick",
            TaEvent::ChannelState { .. } => "channel:state",
            TaEvent::LaneHealth { .. } => "lane:health",
            TaEvent::KernelStatus { .. } => "kernel:status",
            TaEvent::ConfigChanged { .. } => "config:changed",
            TaEvent::SubscriptionUpdated { .. } => "subscription:updated",
            TaEvent::LogAppend { .. } => "log:append",
            TaEvent::Toast { .. } => "toast",
            TaEvent::TrayRefresh { .. } => "tray:refresh",
        }
    }
}

pub trait EventBus: Send + Sync {
    fn publish(&self, e: TaEvent);
    fn subscribe(&self) -> tokio::sync::broadcast::Receiver<TaEvent>;
}

// ================================================================ 批量合并实现

/// 批量合并的参数（默认值来自 / 本模块头注释）。
#[derive(Debug, Clone, Copy)]
pub struct BatchingConfig {
    /// 环形缓冲容量（**在途**事件上限；满了丢最老的并在下次 flush 时记一条 warn）
    pub capacity: usize,
    /// 合并窗口（毫秒）
    pub flush_ms: u64,
    /// 一次 IPC 最多带多少条"数组型"事件（`request:new` / `log:append`）
    pub batch_max: usize,
}

impl Default for BatchingConfig {
    fn default() -> Self {
        Self { capacity: 1024, flush_ms: 200, batch_max: 50 }
    }
}

#[derive(Debug, Default)]
struct Pending {
    /// 环形缓冲：满了从头部丢（**最近的更重要**）
    items: std::collections::VecDeque<TaEvent>,
    /// 被丢掉的事件数（诊断用；下次 flush 时记一条 warn）
    dropped: u64,
}

/// 生产 `EventBus`：**环形缓冲 + 200ms 批量合并，最多 [`BatchingConfig::batch_max`] 条/次**。
///
/// 为什么必须有这一层：一条请求一次 IPC 会打爆 WebView 的消息队列（模块头注释）。
///
/// ## 合并规则（只合并"数组型"事件）
/// - `RequestNew { items }` / `LogAppend { items }`：把多次 `publish` 的 `items` **拼起来**，
///   一次发最多 `batch_max` 条 —— 这正是那两个变体带 `Vec` 的原因；
/// - 其余事件（`Toast` / `KernelStatus` / `ChannelState` / `UsageTick` …）**原样逐条发**：
///   它们是**状态跃迁**，不是快照 —— 把中间态合并掉会丢信息（"什么时候变的"就没了）。
///   要做"只留最新"的合并，必须逐个变体想清语义，不能一刀切。
///
/// ## 生命周期
/// [`BatchingEventBus::spawn`] 会起一个后台任务每 `flush_ms` 清一次缓冲；它持 `Weak`，
/// bus 被丢掉后任务自己退出（**不会**留下一个永远 sleep 的任务）。
pub struct BatchingEventBus {
    tx: tokio::sync::broadcast::Sender<TaEvent>,
    pending: Mutex<Pending>,
    flush_ms: u64,
    batch_max: usize,
    capacity: usize,
}

impl BatchingEventBus {
    /// 造 bus 并起后台冲刷任务（**必须在 tokio 运行时里调用**）。
    pub fn spawn(config: BatchingConfig) -> std::sync::Arc<Self> {
        let bus = std::sync::Arc::new(Self::new(config));
        let weak = std::sync::Arc::downgrade(&bus);
        let flush_ms = bus.flush_ms;
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(flush_ms)).await;
                // bus 没了就退出（否则这个任务会一直活着）
                let Some(bus) = weak.upgrade() else { return };
                bus.flush_once();
            }
        });
        bus
    }

    pub fn new(config: BatchingConfig) -> Self {
        let (tx, _rx) = tokio::sync::broadcast::channel(config.capacity.max(1));
        Self {
            tx,
            pending: Mutex::new(Pending::default()),
            flush_ms: config.flush_ms.max(1),
            batch_max: config.batch_max.max(1),
            capacity: config.capacity.max(1),
        }
    }

    /// 用默认参数造 bus 并起后台任务（生产装配最常用的一行）。
    pub fn spawn_default() -> std::sync::Arc<Self> {
        Self::spawn(BatchingConfig::default())
    }

    /// 被丢掉的事件数（缓冲溢出；诊断 / 测试用）。
    pub fn dropped(&self) -> u64 {
        self.lock().dropped
    }

    /// 在途事件数（测试 / 诊断用）。
    pub fn pending_len(&self) -> usize {
        self.lock().items.len()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Pending> {
        // 锁中毒不该让网关瘫痪
        match self.pending.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// 冲一次：把缓冲里的内容按合并规则发出去，返回**发出的 IPC 条数**。
    ///
    /// 公开是为了可测（DoD 要求"1000 条 → ≤ 若干批次"）：测试可以把它当"手动时钟"用，
    /// 不必依赖 200ms 的真实等待。
    pub fn flush_once(&self) -> usize {
        // 取走快照：锁只在这一段持有，之后发送不再持锁
        let (events, dropped) = {
            let mut guard = self.lock();
            let dropped = std::mem::take(&mut guard.dropped);
            let events: Vec<TaEvent> = guard.items.drain(..).collect();
            (events, dropped)
        };
        if dropped > 0 {
            tracing::warn!(removed = dropped, "事件总线缓冲溢出，最早的 {dropped} 条事件被丢弃");
        }
        if events.is_empty() {
            return 0;
        }

        // 数组型合并：RequestNew / LogAppend 各自拼，最多 batch_max 条
        let mut requests: Vec<RequestSummary> = Vec::new();
        let mut logs: Vec<serde_json::Value> = Vec::new();
        let mut others: Vec<TaEvent> = Vec::new();
        for event in events {
            match event {
                TaEvent::RequestNew { items } => requests.extend(items),
                TaEvent::LogAppend { items } => logs.extend(items),
                other => others.push(other),
            }
        }

        let mut sent = 0usize;
        for chunk in requests.chunks(self.batch_max) {
            if self.tx.send(TaEvent::RequestNew { items: chunk.to_vec() }).is_ok() {
                sent += 1;
            }
        }
        for chunk in logs.chunks(self.batch_max) {
            if self.tx.send(TaEvent::LogAppend { items: chunk.to_vec() }).is_ok() {
                sent += 1;
            }
        }
        for event in others {
            if self.tx.send(event).is_ok() {
                sent += 1;
            }
        }
        sent
    }
}

impl EventBus for BatchingEventBus {
    fn publish(&self, e: TaEvent) {
        let mut guard = self.lock();
        guard.items.push_back(e);
        while guard.items.len() > self.capacity {
            guard.items.pop_front();
            guard.dropped = guard.dropped.saturating_add(1);
        }
    }

    fn subscribe(&self) -> tokio::sync::broadcast::Receiver<TaEvent> {
        self.tx.subscribe()
    }
}

// ================================================================ 测试

#[cfg(test)]
mod tests {
    use super::*;
    use ta_core::types::{ChannelId, RequestId};

    fn summary(i: usize) -> RequestSummary {
        RequestSummary {
            request_id: RequestId::new(format!("req_{i}")),
            ts_ms: 1_700_000_000_000 + i as i64,
            model: "claude-sonnet-4-5".into(),
            upstream_model: "claude-sonnet-4-5-20250929".into(),
            channel_id: ChannelId::new("ch-a"),
            client: None,
            lane_id: None,
            node: None,
            status: 200,
            outcome: "ok".into(),
            latency_ms: 1,
            ttfb_ms: None,
            total_tokens: 0,
            cost: Money::new(0.0, "CNY"),
            attempts: 1,
            error_type: None,
        }
    }

    fn request_new(i: usize) -> TaEvent {
        TaEvent::RequestNew { items: vec![summary(i)] }
    }

    fn test_config(capacity: usize, batch_max: usize) -> BatchingConfig {
        BatchingConfig { capacity, flush_ms: 200, batch_max }
    }

    /// DoD（②）：**1000 条高频事件必须被合并成有界批次**。
    ///
    /// 断言的是**两个数**：IPC 次数（20）与条目总数（1000）。只断前者会让"丢事件"也通过。
    #[tokio::test]
    async fn a_thousand_events_are_coalesced_into_bounded_batches() {
        let bus = BatchingEventBus::new(test_config(4096, 50));
        let mut rx = bus.subscribe();

        for i in 0..1000 {
            bus.publish(request_new(i));
        }
        let ipc = bus.flush_once();
        assert_eq!(ipc, 20, "1000 条 / 每批 50 条 = 20 次 IPC，而不是 1000 次");

        let mut batches = 0usize;
        let mut items = 0usize;
        while let Ok(event) = rx.try_recv() {
            match event {
                TaEvent::RequestNew { items: batch } => {
                    assert!(batch.len() <= 50, "单次 IPC 最多 50 条，实际 {}", batch.len());
                    batches += 1;
                    items += batch.len();
                }
                other => panic!("不该出现别的类型：{other:?}"),
            }
        }
        assert_eq!(batches, 20);
        assert_eq!(items, 1000, "一条都不能少");
    }

    /// `log:append` 是另一种"数组型"事件，同样要合并（否则日志页会重演同一个问题）。
    #[tokio::test]
    async fn log_appends_are_coalesced_too() {
        let bus = BatchingEventBus::new(test_config(4096, 50));
        let mut rx = bus.subscribe();
        for i in 0..120 {
            bus.publish(TaEvent::LogAppend { items: vec![serde_json::json!({ "line": i })] });
        }
        assert_eq!(bus.flush_once(), 3, "120 / 50 = 3 批");
        let mut items = 0usize;
        while let Ok(event) = rx.try_recv() {
            if let TaEvent::LogAppend { items: batch } = event {
                items += batch.len();
            }
        }
        assert_eq!(items, 120);
    }

    /// 环形缓冲：满了**丢最老的**（最近的更重要），并且要能报出丢了多少。
    #[tokio::test]
    async fn the_ring_buffer_drops_the_oldest_and_reports_it() {
        let bus = BatchingEventBus::new(test_config(10, 50));
        let mut rx = bus.subscribe();
        for i in 0..15 {
            bus.publish(request_new(i));
        }
        assert_eq!(bus.pending_len(), 10, "在途事件不得超过缓冲上限");
        assert_eq!(bus.dropped(), 5, "丢掉的数量要能查到（否则静默丢事件）");

        bus.flush_once();
        let TaEvent::RequestNew { items } = rx.try_recv().expect("应当有一批") else {
            panic!("类型不对")
        };
        assert_eq!(items.len(), 10);
        assert_eq!(items.first().unwrap().request_id.as_str(), "req_5", "丢的必须是最老的");
        assert_eq!(items.last().unwrap().request_id.as_str(), "req_14");
    }

    /// 状态跃迁类事件**原样逐条发、顺序不变**：它们是"什么时候变的"，合并掉就丢信息。
    #[tokio::test]
    async fn state_events_are_forwarded_one_by_one_in_order() {
        let bus = BatchingEventBus::new(test_config(64, 50));
        let mut rx = bus.subscribe();
        bus.publish(TaEvent::Toast {
            level: "info".into(),
            title: "t".into(),
            body: "b".into(),
            dedupe_key: "k".into(),
        });
        bus.publish(TaEvent::KernelStatus { state: "ready".into(), version: Some("v1".into()) });
        assert_eq!(bus.flush_once(), 2);
        match rx.try_recv().expect("第一条") {
            TaEvent::Toast { title, .. } => assert_eq!(title, "t"),
            other => panic!("顺序错了：{other:?}"),
        }
        match rx.try_recv().expect("第二条") {
            TaEvent::KernelStatus { state, .. } => assert_eq!(state, "ready"),
            other => panic!("顺序错了：{other:?}"),
        }
    }

    /// 后台任务必须**真的按计时器冲刷**（否则 `flush_once` 测得再好，生产也不工作）。
    #[tokio::test]
    async fn the_spawned_bus_flushes_on_its_timer_without_an_explicit_call() {
        let bus =
            BatchingEventBus::spawn(BatchingConfig { capacity: 64, flush_ms: 30, batch_max: 50 });
        let mut rx = bus.subscribe();
        bus.publish(request_new(1));
        let event = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .expect("后台任务必须按时冲刷")
            .expect("应当收到事件");
        match event {
            TaEvent::RequestNew { items } => assert_eq!(items.len(), 1),
            other => panic!("{other:?}"),
        }
    }
}
