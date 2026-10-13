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
    /// 通道 → EWMA 延迟
    pub latency_ms: BTreeMap<ChannelId, u32>,
    /// 通道 → **时间窗内的 token 用量**（[docs/48 §3]）。平均粘性的"平均"就看它：
    /// 未命中粘性时挑这张表里数值最小的线路。
    ///
    /// 缺省（空表）= **不知道**，不是"用量为 0" —— 于是退化为按 `priority` 排。
    /// 这个区分是刻意的：把缺失当成 0 会让"没查到"变成"这条最闲"，凭空重定向流量。
    pub balance: BTreeMap<ChannelId, i64>,
    /// 凭据 → 当前并发
    pub concurrency: BTreeMap<CredentialId, u32>,
    /// lane → 健康
    pub lane_health: BTreeMap<LaneId, bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChannelRuntimeState {
    pub enabled: bool,
    pub cooling_until_ms: Option<i64>,
    pub invalid_credential: bool,
}

impl ChannelRuntimeState {
    pub fn is_available(&self, now_ms: i64) -> bool {
        self.enabled
            && !self.invalid_credential
            && self.cooling_until_ms.map(|t| t <= now_ms).unwrap_or(true)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EliminateReason {
    Disabled,
    Cooling,
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
    // 这里曾有 `lane_override: Option<LaneId>`（策略级全局线路覆盖）。
    //
    // [docs/55]（第 322 轮）整条删除：线路分配只剩**服务商级**一处（`Provider.lane`），
    // 再没有任何东西能产生"覆盖"这种决策，留着它就只是一段恒为 `None` 的死管线。
    pub eliminated: Vec<(ChannelId, EliminateReason)>,
}

impl Default for RouteDecision {
    fn default() -> Self {
        Self {
            candidates: vec![],
            selector: Selector::BalancedSticky,
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

/// 各线路的 token 用量来源（[docs/48 §4]）。
///
/// 为什么是个 trait 而不是直接塞进 `RouteContext`：用量在**库里**，而 `ta-core`
/// 不依赖任何存储实现。装配方（`src-tauri`）提供一个带 TTL 缓存的实现，
/// 网关每请求只是**读一份内存快照**。
///
/// **实现方的硬约束**：`used_tokens` 会被**每个请求**调用，它自己必须把结果缓存住
/// （见 `docs/48 §4`：TTL 默认 30s，过期由第一个请求触发刷新）。
/// 在这里每调用一次就查一次库等于把数据库打死，这正是用户明确要求避免的。
pub trait BalanceSource: Send + Sync {
    /// 时间窗内各线路的累计 token 用量。查不到就是空表 / 缺键 —— **不要**补 0 冒充已知。
    fn used_tokens(&self) -> BTreeMap<ChannelId, i64>;
}

impl EliminateReason {
    /// 稳定的机器可读串（trace / 模拟器 / 日志都用它，**不要**依赖 `Debug` 的输出）。
    pub fn as_str(&self) -> &'static str {
        match self {
            EliminateReason::Disabled => "disabled",
            EliminateReason::Cooling => "cooling",
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
