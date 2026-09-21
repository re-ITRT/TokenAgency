//! 事件总线（Rust → 前端）。**工作包 G4 负责实现**。
//!
//! 性能要求：**环形缓冲 + 200ms 批量合并**，最多 50 条/次 ——
//! 一条请求一次 IPC 会打爆 WebView 的消息队列。

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
    /// **必须与 `docs/10-interface-spec.md` §2.3 逐一对应** —— 前端按名字订阅，写错就静默收不到。
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
