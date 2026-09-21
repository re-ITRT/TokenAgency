//! 运行时装配：把 [`GatewayDeps`] 与几个接缝装成一个可服务的网关。**唯一的装配入口**。
//!
//! ## 为什么装配只能有一个入口
//!
//! 两个地方各自拼一遍依赖，就会出现两套"什么算已装配好"的答案，而且漂移时
//! 没有任何东西会红（[docs/13 §4.5 L21] 的教训：含糊的接口让两个实现者造出不同的系统）。
//! 所以 `src-tauri` 不自己 new 管线，只调 [`build_runtime`]。
//!
//! ## [`LiveRouter`]：路由必须跟着配置换代，而且**一次请求只能用一个代次**
//!
//! `ta-core` 的 `RoutingIndex` / `PolicyEngineImpl` / `ChannelSelectorImpl` 都是**由某一版
//! 配置构造出来的**。这里有两层问题，必须一起解决：
//!
//! 1. **陈旧**：配置热更新（`ArcSwap`）之后继续用旧 index，会与
//!    `pipeline::resolved_channel`（读最新配置）分叉，表现为
//!    「通道 X 不在配置里（路由与配置不一致）」这种 TA-9001。
//! 2. **同一次求值里混代**：把 `decide()` 与 `select()` 当成两次独立调用，
//!    配置换代恰好落在两者之间时，候选集来自第 N 代、选择却在第 N+1 代的 index 上求值
//!    → 第 N 代才有的通道被跳过 → `select` 返回 `None` → 报「无可用通道」。
//!    窗口很窄，但症状与第 1 条一样是"改配置 = 偶发失败"。
//!
//! 所以这里按 **`Arc` 指针**判断"配置换代"（用 `Arc::ptr_eq` 且缓存里持着旧 `Arc`，
//! 所以不存在"旧分配被释放后地址复用导致假新鲜"的问题），换代即重建 index/engine/selector；
//! 并且**唯一的正式求值入口是 [`RoutingPlanner::plan`]** —— 它在**一把锁**里同时跑
//! engine 与 selector，并把这次用的那版 `Arc<ConfigDoc>` 一起回传，
//! 让调用方（`pipeline::prepare`）不需要、也不允许自己再 load 一次。
//!
//! **粘性表（sticky）跨换代保留**：粘性对用户是 12 小时级的可见语义，
//! 不能因为改了一次配置就清零 —— 所以它不是 `Router::new` 内部造的那一份，
//! 而是由本模块持有、每次重建都注入同一份。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use ta_core::domain::ConfigDoc;
use ta_core::pricing::PricingEngine;
use ta_core::routing::{
    ChannelSelector, ChannelSelectorImpl, EngineOptions, PolicyEngine, PolicyEngineImpl,
    RouteContext, RouteDecision, RouteHints, RoutingIndex, Selection, SelectorOptions,
};
use ta_core::sticky::{MemoryStickyResolver, StickyResolver};

use crate::dispatch::{DialerOptions, KernelDialer, LanePorts, OutboundDialer};
use crate::ingress::{self, IngressOptions, IngressState, KernelProbe};
use crate::passthrough::{FullPassthrough, PassthroughPolicy};
use crate::pipeline::{AttemptSink, Pipeline, PipelineDeps, ResponseMeter, SecretResolver};
use crate::trace::LoggingSink;
use crate::GatewayDeps;

// ================================================================ 装配入参

/// 装配运行时需要的几个接缝。
///
/// 它们不在 [`GatewayDeps`] 里，因为它们的实现者不是"网关的共享依赖"而是
/// **每个进程只装配一次的部件**（内核探针、凭据解析器、尝试接收器）。
pub struct RuntimeParts {
    /// 透传策略。**生产恒为 [`FullPassthrough`]**：同透传是本产品的核心承诺，
    /// 而 I3 是硬不变量 —— 这个字段存在的唯一理由是让测试注入替身，
    /// 所以**不提供 `with_passthrough` 之类的便利构造**（逃生口会被顺手用掉）。
    pub passthrough: Arc<dyn PassthroughPolicy>,
    /// 凭据解析器（`"${secret:name}"` / `"${env:VAR}"` → 明文）。入站本地密钥与通道凭据
    /// 共用同一个，避免同一套引用语法两份实现。
    pub secrets: Arc<dyn SecretResolver>,
    /// 内核状态探针（`/healthz` 与 TA-3001 的判据）。
    pub kernel: Arc<dyn KernelProbe>,
    /// 尝试记录接收器。
    pub sink: Arc<dyn AttemptSink>,
    /// `None` = 计量还没接上（G4 在途）。**不提供空实现**：静默地"接了但没数据"
    /// 比没接更糟（[docs/13 §4.5 L10]）。
    pub meter: Option<Arc<dyn ResponseMeter>>,
    pub ingress: IngressOptions,
}

impl RuntimeParts {
    /// 除 `secrets` / `kernel` 外都用默认件：全透传 + 日志级 attempt sink + 未接计量。
    pub fn new(secrets: Arc<dyn SecretResolver>, kernel: Arc<dyn KernelProbe>) -> Self {
        Self {
            passthrough: FullPassthrough::shared(),
            secrets,
            kernel,
            sink: Arc::new(LoggingSink::new()),
            meter: None,
            ingress: IngressOptions::default(),
        }
    }

    pub fn with_sink(mut self, sink: Arc<dyn AttemptSink>) -> Self {
        self.sink = sink;
        self
    }

    pub fn with_meter(mut self, meter: Arc<dyn ResponseMeter>) -> Self {
        self.meter = Some(meter);
        self
    }

    pub fn with_ingress_options(mut self, ingress: IngressOptions) -> Self {
        self.ingress = ingress;
        self
    }

    /// 接上 trace 表（[docs/12 §4 G4] ①）：sink 变成"**日志 + trace 表**"两份
    /// （已通过 [`RuntimeParts::with_sink`] 注入的 sink 也保留，排在前面）。
    ///
    /// 不接的话只写日志 —— 那样"拿一个 `request_id` 筛出完整轨迹"就只剩应用日志这一路，
    /// 前端日志页与 `ta trace` 都查不到。所以生产装配应当接上。
    pub fn with_trace_store(mut self, store: Arc<dyn ta_store::TraceStore>) -> Self {
        use crate::trace::{TeeAttemptSink, TraceStoreSink};
        let trace: Arc<dyn AttemptSink> = Arc::new(TraceStoreSink::new(store));
        // `self.sink` 默认就是日志 sink（`RuntimeParts::new`），所以组合后 = 日志 + trace 表
        self.sink = Arc::new(TeeAttemptSink::new(vec![self.sink, trace]));
        self
    }
}

// ================================================================ 路由计划器

/// 一次路由求值的**完整**结果。
///
/// 关键在 `cfg`：它就是这次 `decision` / `selection` 所依据的那一版配置。
/// 调用方必须用它去解析通道、算 lane、造请求 —— **不要再自己 `load_full()`**，
/// 否则就是把"同一代"这件事交回给运气（见模块头注释第 2 条）。
pub struct RoutePlan {
    pub cfg: Arc<ConfigDoc>,
    pub decision: RouteDecision,
    pub selection: Option<Selection>,
}

/// 路由计划器：**一次绑定、一次求值**。
pub trait RoutingPlanner: Send + Sync {
    /// 当前绑定的配置。给"只读快照"（`RouteContext`）用，拿不到就当空上下文。
    fn current_doc(&self) -> Arc<ConfigDoc>;

    /// 求值。返回的 `RoutePlan::cfg` 必须与 `decision` / `selection` 同代。
    fn plan(&self, hints: &RouteHints, ctx: &RouteContext) -> RoutePlan;
}

/// 把"策略 + 选择器"两个现成对象组合成一个计划器（固定夹具 / 测试用）。
///
/// 组合的意义不是省事，而是**把同代约束变成一个结构事实**：两个对象都只在这一个函数里
/// 被调用，`cfg` 只在这里取一次。
pub struct PolicySelectorPlanner {
    cfg: Arc<arc_swap::ArcSwap<ConfigDoc>>,
    policy: Arc<dyn PolicyEngine>,
    selector: Arc<dyn ChannelSelector>,
}

impl PolicySelectorPlanner {
    pub fn new(
        cfg: Arc<arc_swap::ArcSwap<ConfigDoc>>,
        policy: Arc<dyn PolicyEngine>,
        selector: Arc<dyn ChannelSelector>,
    ) -> Self {
        Self { cfg, policy, selector }
    }
}

impl RoutingPlanner for PolicySelectorPlanner {
    fn current_doc(&self) -> Arc<ConfigDoc> {
        self.cfg.load_full()
    }

    fn plan(&self, hints: &RouteHints, ctx: &RouteContext) -> RoutePlan {
        // 只在这里取一次配置：decide 与 select 必然同代
        let cfg = self.cfg.load_full();
        let decision = self.policy.decide(hints, ctx);
        let selection = self.selector.select(&decision, hints, ctx);
        RoutePlan { cfg, decision, selection }
    }
}

// ================================================================ LiveRouter

/// 跟随配置热重载的路由门面（生产环境唯一的 [`RoutingPlanner`]）。
pub struct LiveRouter {
    cfg: Arc<arc_swap::ArcSwap<ConfigDoc>>,
    pricing: Option<Arc<dyn PricingEngine>>,
    /// 跨配置换代共享的粘性表（见模块头注释）。
    sticky: Arc<dyn StickyResolver>,
    engine_options: EngineOptions,
    selector_options: SelectorOptions,
    /// 单调代次号。**不用地址当前值**：旧分配释放后地址可能复用，
    /// 那样"两个不同代次会报同一个 generation"，诊断与断言都会 flake。
    next_generation: AtomicU64,
    current: Mutex<Current>,
}

/// 与某一版配置绑定的 engine/selector。
struct Current {
    /// 缓存这一版的 `Arc`：`Arc::ptr_eq` 判定比"指针值"安全（旧分配不会被复用）
    doc: Arc<ConfigDoc>,
    engine: PolicyEngineImpl,
    selector: ChannelSelectorImpl,
    generation: u64,
}

impl LiveRouter {
    pub fn new(
        cfg: Arc<arc_swap::ArcSwap<ConfigDoc>>,
        pricing: Option<Arc<dyn PricingEngine>>,
        sticky: Arc<dyn StickyResolver>,
    ) -> Self {
        let engine_options = EngineOptions::default();
        let selector_options = SelectorOptions::default();
        let doc = cfg.load_full();
        let current = Mutex::new(Self::bind(
            &doc,
            1,
            pricing.clone(),
            &sticky,
            &engine_options,
            &selector_options,
        ));
        Self {
            cfg,
            pricing,
            sticky,
            engine_options,
            selector_options,
            next_generation: AtomicU64::new(1),
            current,
        }
    }

    fn bind(
        doc: &Arc<ConfigDoc>,
        generation: u64,
        pricing: Option<Arc<dyn PricingEngine>>,
        sticky: &Arc<dyn StickyResolver>,
        engine_options: &EngineOptions,
        selector_options: &SelectorOptions,
    ) -> Current {
        let index = Arc::new(RoutingIndex::build(doc));
        let mut selector = ChannelSelectorImpl::new(index.clone())
            .with_sticky(sticky.clone())
            .with_options(selector_options.clone());
        if let Some(pricing) = pricing {
            selector = selector.with_pricing(pricing);
        }
        let engine = PolicyEngineImpl::with_options(index, engine_options.clone());
        Current { doc: doc.clone(), engine, selector, generation }
    }

    /// 取"与当前配置同代"的 engine/selector；配置换代就地重建。
    fn current(&self) -> std::sync::MutexGuard<'_, Current> {
        let doc = self.cfg.load_full();
        // 锁中毒不该让网关瘫痪：取回内部值继续用
        let mut guard = match self.current.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if !Arc::ptr_eq(&guard.doc, &doc) {
            let generation = self.next_generation.fetch_add(1, Ordering::SeqCst) + 1;
            *guard = Self::bind(
                &doc,
                generation,
                self.pricing.clone(),
                &self.sticky,
                &self.engine_options,
                &self.selector_options,
            );
        }
        guard
    }

    /// 当前配置代次（trace / 诊断用；单调递增，可安全 `assert_ne!`）。
    pub fn generation(&self) -> u64 {
        self.current().generation
    }
}

impl RoutingPlanner for LiveRouter {
    fn current_doc(&self) -> Arc<ConfigDoc> {
        self.current().doc.clone()
    }

    fn plan(&self, hints: &RouteHints, ctx: &RouteContext) -> RoutePlan {
        // **一把锁内跑完** decide + select：配置换代不可能插进两者之间
        let guard = self.current();
        let decision = guard.engine.decide(hints, ctx);
        let selection = guard.selector.select(&decision, hints, ctx);
        RoutePlan { cfg: guard.doc.clone(), decision, selection }
    }
}

/// 单独调用 `decide` / `select` 的旧入口。
///
/// **不要在新代码里分别调它们** —— 两次调用各自可能跨代（模块头注释第 2 条）。
/// 保留它们只是为了兼容 `ta_core::routing` 的既有接缝（例如模拟器/回放要单独拿
/// `RouteDecision`）；管线一律走 [`RoutingPlanner::plan`]。
impl PolicyEngine for LiveRouter {
    fn decide(&self, hints: &RouteHints, ctx: &RouteContext) -> RouteDecision {
        self.current().engine.decide(hints, ctx)
    }
}

impl ChannelSelector for LiveRouter {
    fn select(
        &self,
        decision: &RouteDecision,
        hints: &RouteHints,
        ctx: &RouteContext,
    ) -> Option<Selection> {
        self.current().selector.select(decision, hints, ctx)
    }
}

// ================================================================ 装配

/// 装配好的网关运行时。
pub struct GatewayRuntime {
    pub deps: Arc<GatewayDeps>,
    pub router: Arc<LiveRouter>,
    pub pipeline: Arc<Pipeline>,
    pub state: Arc<IngressState>,
    /// 暂停/恢复闸门（`app_pause_gateway` / `app_resume_gateway` 的接缝）。
    /// 与 `state.gate` 是**同一个 `Arc`** —— 不要在别处再造一个。
    pub gate: Arc<crate::ingress::GatewayGate>,
}

impl GatewayRuntime {
    /// 起网关服务。**只绑回环**（I1 由 `ingress::serve` 的 loopback 检查保证，
    /// 不是靠调用方自觉）。
    pub async fn serve(&self) -> ta_core::error::Result<()> {
        ingress::serve(self.state.clone()).await
    }
}

/// **唯一的装配入口**：`GatewayDeps` + [`RuntimeParts`] → 可服务的网关。
///
/// 不返回 `Result`：这里的每一步都是纯装配（没有 IO、没有配置校验）。
/// 配置是否合法由 `ta-core` 的 validator 在上游负责 —— 把校验藏进装配会让
/// "装配成功"这句话同时意味着两件事。
pub fn build_runtime(deps: &Arc<GatewayDeps>, parts: RuntimeParts) -> GatewayRuntime {
    let sticky: Arc<dyn StickyResolver> =
        Arc::new(MemoryStickyResolver::with_clock(deps.clock.clone()));
    let router = Arc::new(LiveRouter::new(deps.config.clone(), Some(deps.pricing.clone()), sticky));

    let planner: Arc<dyn RoutingPlanner> = router.clone();
    let pipeline = Arc::new(Pipeline::new(Arc::new(PipelineDeps {
        cfg: deps.config.clone(),
        clock: deps.clock.clone(),
        planner,
        quota: deps.quota.clone(),
        secrets: parts.secrets.clone(),
        dialer: deps.dialer.clone(),
        passthrough: parts.passthrough.clone(),
        sink: parts.sink,
        meter: parts.meter,
    })));

    let gate = Arc::new(crate::ingress::GatewayGate::new());
    let state = Arc::new(
        IngressState::new(
            deps.config.clone(),
            deps.clock.clone(),
            parts.passthrough,
            parts.secrets,
            pipeline.clone(),
            parts.kernel,
            parts.ingress,
        )
        .with_gate(gate.clone()),
    );

    GatewayRuntime { deps: deps.clone(), router, pipeline, state, gate }
}

/// 由内核 lane 端口表造出站拨号器。
///
/// 存在的理由：`KernelDialer` 的构造细节（每 lane 一个 client、关自动解压、
/// 不跟随重定向）是透传保真的一部分，不该让装配方去拼。
pub fn kernel_dialer(ports: Arc<dyn LanePorts>, options: DialerOptions) -> Arc<dyn OutboundDialer> {
    Arc::new(KernelDialer::new(ports, options))
}

// ================================================================ 测试

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    use async_trait::async_trait;
    use bytes::Bytes;
    use ta_core::clock::ManualClock;
    use ta_core::domain::{ChannelConfig, Credential, Lane, LaneKind, Model, Provider};
    use ta_core::error::Result;
    use ta_core::pricing::ResolvedPrice;
    use ta_core::quota::{QuotaLedger, QuotaObservation, QuotaState, QuotaStateKind};
    use ta_core::types::{
        ChannelId, CredentialId, LaneId, Money, Protocol, RequestSummary, TokenUsage, UsageRecord,
    };

    use crate::dispatch::{HealthReport, StaticLanePorts, UpstreamRequest, UpstreamResponse};
    use crate::events::TaEvent;
    use crate::ingress::KernelState;
    use crate::pipeline::StaticSecrets;
    use crate::traits_store::GatewayStore;

    pub(crate) const MODEL: &str = "claude-sonnet-4-5";

    // ---------------------------------------------------------- mocks

    pub(crate) struct NoStore;

    #[async_trait]
    impl GatewayStore for NoStore {
        async fn record_usage(&self, _rec: UsageRecord) -> Result<()> {
            Ok(())
        }
        async fn recent_summaries(&self, _limit: usize) -> Result<Vec<RequestSummary>> {
            Ok(vec![])
        }
    }

    pub(crate) struct NoDialer;

    #[async_trait]
    impl OutboundDialer for NoDialer {
        async fn execute(&self, _lane: &LaneId, _req: UpstreamRequest) -> Result<UpstreamResponse> {
            Err(ta_core::error::TaError::AllChannelsFailed {
                message: "测试用的 NoDialer 不拨号".into(),
            })
        }
        async fn health(&self, lane: &LaneId) -> HealthReport {
            HealthReport { lane_id: lane.clone(), ok: true, latency_ms: None, error: None }
        }
    }

    /// 记录拨号调用的 dialer（装配测试里用来断言"确实尝试过"）。
    #[derive(Default)]
    pub(crate) struct RecordingDialer {
        pub(crate) urls: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl OutboundDialer for RecordingDialer {
        async fn execute(&self, _lane: &LaneId, req: UpstreamRequest) -> Result<UpstreamResponse> {
            match self.urls.lock() {
                Ok(mut guard) => guard.push(req.url.clone()),
                Err(poisoned) => poisoned.into_inner().push(req.url.clone()),
            }
            Ok(UpstreamResponse {
                status: http::StatusCode::OK,
                headers: http::HeaderMap::new(),
                body: crate::dispatch::byte_stream_from_bytes(Bytes::from_static(b"{}")),
            })
        }
        async fn health(&self, lane: &LaneId) -> HealthReport {
            HealthReport { lane_id: lane.clone(), ok: true, latency_ms: None, error: None }
        }
    }

    pub(crate) struct NoEvents;

    impl crate::events::EventBus for NoEvents {
        fn publish(&self, _e: TaEvent) {}
        fn subscribe(&self) -> tokio::sync::broadcast::Receiver<TaEvent> {
            let (tx, rx) = tokio::sync::broadcast::channel(1);
            drop(tx);
            rx
        }
    }

    pub(crate) struct NoPricing;

    impl PricingEngine for NoPricing {
        fn resolve(
            &self,
            _model: &str,
            _provider: &ta_core::types::ProviderId,
            _cred: &CredentialId,
        ) -> ResolvedPrice {
            ResolvedPrice::unpriced("CNY", 1_000_000.0)
        }
        fn cost(&self, _usage: &TokenUsage, price: &ResolvedPrice) -> Money {
            Money::new(0.0, price.currency.clone())
        }
    }

    pub(crate) struct NoQuota;

    impl QuotaLedger for NoQuota {
        fn state(&self, cred: &CredentialId) -> QuotaState {
            QuotaState {
                credential_id: cred.clone(),
                kind: QuotaStateKind::Unknown,
                ..Default::default()
            }
        }
        fn observe(&self, _cred: &CredentialId, _obs: QuotaObservation) {}
        fn mark_exhausted(&self, _cred: &CredentialId, _hint: Option<i64>) {}
        fn mark_invalid(&self, _cred: &CredentialId) {}
        fn roll_window(&self, _now_ms: i64) -> Vec<CredentialId> {
            vec![]
        }
        fn remaining_ratio(&self, _cred: &CredentialId) -> Option<f64> {
            None
        }
    }

    pub(crate) struct FixedKernel(pub(crate) KernelState);

    impl KernelProbe for FixedKernel {
        fn state(&self) -> KernelState {
            self.0
        }
    }

    // ---------------------------------------------------------- 夹具

    /// 一个模型挂 `extra` 个通道（第 2 个起是 `ch-b` / `ch-c`…），用于热更新测试。
    ///
    /// 线路必须**显式声明**：`EngineOptions::default().strict_lane_check == true`，
    /// 指向未声明线路的通道会被淘汰（`EliminateReason::NoLane`）—— 这正是 K4(d) 要的
    /// "响亮地失败"，所以夹具必须像真实配置一样声明线路，而不是让测试绕过它。
    pub(crate) fn cfg_with_channels(extra: usize) -> ConfigDoc {
        let mut doc = ConfigDoc::default();
        doc.models.push(Model::new(MODEL));
        doc.lanes.push(Lane::new("lane-jp", "日本", LaneKind::Direct));
        doc.lanes.push(Lane::new("lane-tokyo", "东京", LaneKind::Direct));
        doc.providers.push(Provider::new("p-a", "A", Protocol::Openai, "https://a.example/v1"));
        doc.credentials.push(Credential::new("c-a", "p-a", "A", "${secret:a}"));
        doc.channels.push(
            ChannelConfig::new("ch-a", MODEL, "p-a", "c-a", "upstream-a").with_lane("lane-jp"),
        );
        for i in 0..extra {
            let suffix = (b'b' + i as u8) as char;
            let provider = format!("p-{suffix}");
            let credential = format!("c-{suffix}");
            let channel = format!("ch-{suffix}");
            doc.providers.push(Provider::new(
                provider.as_str(),
                channel.as_str(),
                Protocol::Openai,
                format!("https://{suffix}.example/v1"),
            ));
            doc.credentials.push(Credential::new(
                credential.as_str(),
                provider.as_str(),
                channel.as_str(),
                "${secret:a}",
            ));
            doc.channels.push(
                ChannelConfig::new(
                    channel.as_str(),
                    MODEL,
                    provider.as_str(),
                    credential.as_str(),
                    format!("upstream-{suffix}"),
                )
                .with_lane("lane-tokyo"),
            );
        }
        doc
    }

    pub(crate) fn test_deps(cfg: ConfigDoc, dialer: Arc<dyn OutboundDialer>) -> Arc<GatewayDeps> {
        Arc::new(GatewayDeps {
            config: Arc::new(arc_swap::ArcSwap::from_pointee(cfg)),
            store: Arc::new(NoStore),
            dialer,
            events: Arc::new(NoEvents),
            clock: Arc::new(ManualClock::new(1_700_000_000_000)),
            pricing: Arc::new(NoPricing),
            quota: Arc::new(NoQuota),
        })
    }

    pub(crate) fn test_parts() -> RuntimeParts {
        RuntimeParts::new(
            Arc::new(StaticSecrets::new([("${secret:a}", "sk-test-secret")])),
            Arc::new(FixedKernel(KernelState::Ready)),
        )
    }

    // ---------------------------------------------------------- 装配

    #[tokio::test]
    async fn build_runtime_assembles_a_servable_gateway() {
        let deps = test_deps(cfg_with_channels(0), Arc::new(NoDialer));
        let runtime = build_runtime(&deps, test_parts());

        // 装配出来的 state 真的能服务：/healthz 报注入的内核状态
        let request = axum::http::Request::builder()
            .method("GET")
            .uri("/healthz")
            .body(axum::body::Body::empty())
            .expect("构造请求");
        let response = tower::ServiceExt::oneshot(ingress::router(runtime.state.clone()), request)
            .await
            .expect("路由必须能处理请求");
        assert_eq!(response.status(), http::StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), 4096).await.expect("响应体");
        let value: serde_json::Value = serde_json::from_slice(&bytes).expect("JSON");
        assert_eq!(value["status"], "ok");
        assert_eq!(value["kernel"], "ready");

        // 装配出来的那条管线**就是** state 用的那条（不是各造一条）。
        // 两个 `Arc` 指向不同具体类型（`Arc<Pipeline>` vs `Arc<dyn RequestPipeline>`），
        // 所以比数据指针的地址：相等 ⇔ 同一个对象。
        assert_eq!(
            Arc::as_ptr(&runtime.pipeline) as *const (),
            Arc::as_ptr(&runtime.state.pipeline) as *const (),
            "state 里的管线必须与 GatewayRuntime.pipeline 是同一个 Arc"
        );
        // 配置也是同一份 ArcSwap（管理 API 改配置，网关立刻看到）
        assert!(Arc::ptr_eq(&deps.config, &runtime.state.cfg));
    }

    // ---------------------------------------------------------- 热重载

    #[tokio::test]
    async fn routing_follows_config_generation() {
        let deps = test_deps(cfg_with_channels(0), Arc::new(NoDialer));
        let runtime = build_runtime(&deps, test_parts());

        let hints = RouteHints { model: Some(MODEL.into()), ..Default::default() };
        let ctx = RouteContext { now_ms: 1_700_000_000_000, ..Default::default() };

        let before = runtime.router.plan(&hints, &ctx);
        assert_eq!(before.decision.candidates, vec![ChannelId::new("ch-a")]);
        let generation_before = runtime.router.generation();

        // 配置热更新：同一个逻辑模型多挂一个通道（这正是用户最常做的操作）
        deps.config.store(Arc::new(cfg_with_channels(1)));

        let after = runtime.router.plan(&hints, &ctx);
        assert!(
            after.decision.candidates.contains(&ChannelId::new("ch-b")),
            "配置换代后路由必须重建 index，否则会与 resolved_channel 读到的配置分叉：{:?}",
            after.decision.candidates
        );
        assert!(
            Arc::ptr_eq(&after.cfg, &deps.config.load_full()),
            "plan 回传的 cfg 必须是它这次求值用的那一版"
        );
        assert_ne!(
            runtime.router.generation(),
            generation_before,
            "换代必须可观测（trace/诊断靠它区分路由用的是哪一版配置）"
        );

        // 再换回来也必须跟着走（可逆）
        deps.config.store(Arc::new(cfg_with_channels(0)));
        let back = runtime.router.plan(&hints, &ctx);
        assert_eq!(back.decision.candidates, vec![ChannelId::new("ch-a")]);
    }

    /// 装配出来的管线真的会把请求打到上游（不是只装配了个空壳）。
    #[tokio::test]
    async fn assembled_pipeline_reaches_the_dialer() {
        let dialer = Arc::new(RecordingDialer::default());
        let deps = test_deps(cfg_with_channels(0), dialer.clone());
        let runtime = build_runtime(&deps, test_parts());

        let body = br#"{"model":"claude-sonnet-4-5","messages":[{"role":"user","content":"hi"}]}"#;
        let request = axum::http::Request::builder()
            .method("POST")
            .uri("/v1/messages")
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(axum::body::Body::from(body.to_vec()))
            .expect("构造请求");
        let response = tower::ServiceExt::oneshot(ingress::router(runtime.state.clone()), request)
            .await
            .expect("路由必须能处理请求");
        assert_eq!(response.status(), http::StatusCode::OK);
        let urls = match dialer.urls.lock() {
            Ok(guard) => guard.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        };
        assert_eq!(urls, vec!["https://a.example/v1/messages".to_string()]);
    }

    /// `KernelDialer` 的构造细节不外包给装配方。
    #[test]
    fn kernel_dialer_helper_builds_the_real_dialer() {
        let dialer = kernel_dialer(Arc::new(StaticLanePorts::default()), DialerOptions::default());
        // 没有端口表的非 direct lane → TA-3002（证明造出来的确实是 KernelDialer，
        // 而不是某个总是成功的替身）
        let lane = LaneId::new("lane-jp");
        let outcome = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("测试运行时")
            .block_on(dialer.execute(
                &lane,
                UpstreamRequest {
                    method: "POST".into(),
                    url: "http://127.0.0.1:1/v1/messages".into(),
                    headers: http::HeaderMap::new(),
                    body: Bytes::from_static(b"{}"),
                    stream: false,
                    total_timeout_s: None,
                },
            ));
        let err = outcome.expect_err("没有端口必须报错");
        assert_eq!(err.code(), "TA-3002");
    }
}
