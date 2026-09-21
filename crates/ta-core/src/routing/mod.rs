//! 路由决策。**首个命中的策略即生效，不做叠加。**

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::domain::{FailoverPolicy, NoCandidateAction, Selector};
use crate::types::*;

/// 路由只需要读请求里很少的几个字段（**不改请求**）。
#[derive(Debug, Clone, Default)]
pub struct RouteHints {
    pub model: Option<String>,
    pub protocol: Option<Protocol>,
    pub has_image: bool,
    pub has_tools: bool,
    pub stream: bool,
    pub est_input_tokens: u64,
    /// 客户端身份（从 UA 粗解析），用于统计与策略匹配
    pub client: Option<ClientId>,
    pub ua: Option<String>,
    pub headers: BTreeMap<String, String>,
    /// 用于粘性派生的原始 body（首条 system + 首条 user 的哈希）
    pub body_hash: Option<String>,
    pub user_id: Option<String>,
    pub session_id: Option<String>,
    pub conversation_id: Option<String>,
}

/// 求值时需要的运行态上下文（账本 / 并发 / 时间 / 健康）。
#[derive(Debug, Clone, Default)]
pub struct RouteContext {
    pub now_ms: i64,
    /// 通道 → 是否可用（冷却 / 耗尽 / 禁用）
    pub channel_state: BTreeMap<ChannelId, ChannelRuntimeState>,
    /// 凭据 → 剩余比例（0.0–1.0，None = 未知）
    pub quota_remaining: BTreeMap<CredentialId, Option<f64>>,
    /// 通道 → EWMA 延迟
    pub latency_ms: BTreeMap<ChannelId, u32>,
    /// 凭据 → 当前并发
    pub concurrency: BTreeMap<CredentialId, u32>,
    /// lane → 健康
    pub lane_health: BTreeMap<LaneId, bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChannelRuntimeState {
    pub enabled: bool,
    pub cooling_until_ms: Option<i64>,
    pub exhausted: bool,
    pub invalid_credential: bool,
}

impl ChannelRuntimeState {
    pub fn is_available(&self, now_ms: i64) -> bool {
        self.enabled
            && !self.exhausted
            && !self.invalid_credential
            && self.cooling_until_ms.map(|t| t <= now_ms).unwrap_or(true)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EliminateReason {
    Disabled,
    Cooling,
    QuotaExhausted,
    InvalidCredential,
    TaggedOut,
    ExcludedByPolicy,
    NoLane,
    LaneDown,
    NotInGroup,
}

#[derive(Debug, Clone)]
pub struct RouteDecision {
    pub candidates: Vec<ChannelId>,
    pub selector: Selector,
    pub failover: FailoverPolicy,
    pub on_no_candidate: NoCandidateAction,
    pub error_code: Option<u16>,
    pub matched_policy: Option<PolicyId>,
    /// 强制锁定到某通道（策略 `pin_sticky`）
    pub pinned: Option<ChannelId>,
    /// 策略指定的全局线路覆盖
    pub lane_override: Option<LaneId>,
    pub eliminated: Vec<(ChannelId, EliminateReason)>,
}

impl Default for RouteDecision {
    fn default() -> Self {
        Self {
            candidates: vec![],
            selector: Selector::StickyThenPriority,
            failover: FailoverPolicy {
                on: vec![],
                max_attempts: 3,
                cooldown_s: 120,
                backoff: crate::domain::Backoff::Exponential,
                extra: Default::default(),
            },
            on_no_candidate: NoCandidateAction::Error,
            error_code: None,
            matched_policy: None,
            pinned: None,
            lane_override: None,
            eliminated: vec![],
        }
    }
}

/// 选择结果。
#[derive(Debug, Clone)]
pub struct Selection {
    pub channel: ChannelId,
    pub sticky_hit: bool,
    pub sticky_key: Option<StickyKey>,
    /// 有序的失败转移链（含首选）
    pub chain: Vec<ChannelId>,
}

pub trait PolicyEngine: Send + Sync {
    fn decide(&self, hints: &RouteHints, ctx: &RouteContext) -> RouteDecision;
}

pub trait ChannelSelector: Send + Sync {
    fn select(
        &self,
        decision: &RouteDecision,
        hints: &RouteHints,
        ctx: &RouteContext,
    ) -> Option<Selection>;
}

impl EliminateReason {
    /// 稳定的机器可读串（trace / 模拟器 / 日志都用它，**不要**依赖 `Debug` 的输出）。
    pub fn as_str(&self) -> &'static str {
        match self {
            EliminateReason::Disabled => "disabled",
            EliminateReason::Cooling => "cooling",
            EliminateReason::QuotaExhausted => "quota_exhausted",
            EliminateReason::InvalidCredential => "invalid_credential",
            EliminateReason::TaggedOut => "tagged_out",
            EliminateReason::ExcludedByPolicy => "excluded_by_policy",
            EliminateReason::NoLane => "no_lane",
            EliminateReason::LaneDown => "lane_down",
            EliminateReason::NotInGroup => "not_in_group",
        }
    }
}

// ---------------------------------------------------------------- 实现模块

pub mod engine;
pub mod index;
pub mod router;
pub mod selector;
pub mod simulate;

pub use engine::{EngineOptions, PolicyEngineImpl};
pub use index::RoutingIndex;
pub use router::{Router, RouterOptions};
pub use selector::{ChannelSelectorImpl, SelectorOptions};
pub use simulate::{simulate, simulate_input, simulate_with, SimulateInput, SimulateResult};
