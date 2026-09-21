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
pub mod runtime;
pub mod trace;

pub use runtime::{build_runtime, kernel_dialer, GatewayRuntime, LiveRouter, RuntimeParts};

/// 网关运行时共享状态。**所有子模块通过它取依赖，不各自持有全局变量。**
pub struct GatewayDeps {
    pub config: std::sync::Arc<arc_swap::ArcSwap<ta_core::domain::ConfigDoc>>,
    pub store: std::sync::Arc<dyn crate::traits::GatewayStore>,
    pub dialer: std::sync::Arc<dyn crate::traits::OutboundDialer>,
    pub events: std::sync::Arc<dyn crate::traits::EventBus>,
    pub clock: std::sync::Arc<dyn ta_core::clock::Clock>,
    pub pricing: std::sync::Arc<dyn ta_core::pricing::PricingEngine>,
    pub quota: std::sync::Arc<dyn ta_core::quota::QuotaLedger>,
}

/// 网关用到的 trait 汇总（实现分别落在 store / dispatch 等模块）。
pub mod traits {
    pub use crate::dispatch::OutboundDialer;
    pub use crate::events::EventBus;
    pub use crate::traits_store::GatewayStore;
}

pub mod traits_store;
