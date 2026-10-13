//! `ta-gateway` — 本地网关本体：入站、全透传、路由管线、分发、协议适配、trace。
//!
//! 契约：`docs/10-interface-spec.md` §1、`docs/11-internal-contracts.md` §1.7。
//!
//! 所有权划分（见 `docs/12-work-breakdown.md`）：
//! - **G1** `ingress` / `passthrough` / `admin`
//! - **G2** `pipeline` / `dispatch`
//! - **G3** `adapt`
//! - **G4** `trace` / `events` / `meter`

pub mod adapt;
pub mod admin;
pub mod dispatch;
pub mod events;
pub mod ingress;
pub mod meter;
pub mod passthrough;
pub mod pipeline;
pub mod probe;
pub mod runtime;
pub mod trace;

pub use probe::{read_body_bounded, ProbeKind, ProbeProviderRequest, ProbeResult, UpstreamProbe};
pub use runtime::{build_runtime, kernel_dialer, GatewayRuntime, LiveRouter, RuntimeParts};

/// 网关运行时共享状态。**所有子模块通过它取依赖，不各自持有全局变量。**
pub struct GatewayDeps {
    pub config: std::sync::Arc<arc_swap::ArcSwap<ta_core::domain::ConfigDoc>>,
    pub store: std::sync::Arc<dyn crate::traits::GatewayStore>,
    pub dialer: std::sync::Arc<dyn crate::traits::OutboundDialer>,
    pub events: std::sync::Arc<dyn crate::traits::EventBus>,
    pub clock: std::sync::Arc<dyn ta_core::clock::Clock>,
    pub pricing: std::sync::Arc<dyn ta_core::pricing::PricingEngine>,
    /// 各线路的 token 用量（平均粘性的"平均"依据，[docs/48 §4]）。
    ///
    /// `None` = 没装配 → 选路退化为按 `priority`（**不是**当成"都没用过"）。
    /// 实现方负责缓存（每个请求调一次 `used_tokens`），见 `ta_core::routing::BalanceSource`。
    pub balance: Option<std::sync::Arc<dyn ta_core::routing::BalanceSource>>,
}

/// 网关用到的 trait 汇总（实现分别落在 store / dispatch 等模块）。
pub mod traits {
    pub use crate::dispatch::OutboundDialer;
    pub use crate::events::EventBus;
    pub use crate::traits_store::GatewayStore;
}

pub mod traits_store;
