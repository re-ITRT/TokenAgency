//! 事件总线（Rust → 前端）。**工作包 G4 负责实现**。
//!
//! 性能要求：**环形缓冲 + 200ms 批量合并**，最多 50 条/次 —— 
//! 一条请求一次 IPC 会打爆 WebView 的消息队列。

use serde::{Deserialize, Serialize};

use ta_core::types::{Money, RequestSummary};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TaEvent {
    /// 批量数组，不是单条
    RequestNew { items: Vec<RequestSummary> },
    UsageTick { today_tokens: u64, today_cost: Money },
    ChannelState { channel_id: String, state: String, until_ms: Option<i64> },
    LaneHealth { lane_id: String, node: Option<String>, latency_ms: Option<u32>, ok: bool },
    KernelStatus { state: String, version: Option<String> },
    ConfigChanged { source: String, applied: bool },
    SubscriptionUpdated { id: String, ok: bool, node_count: usize, error: Option<String> },
    Toast { level: String, title: String, body: String, dedupe_key: String },
}

pub trait EventBus: Send + Sync {
    fn publish(&self, e: TaEvent);
    fn subscribe(&self) -> tokio::sync::broadcast::Receiver<TaEvent>;
}
