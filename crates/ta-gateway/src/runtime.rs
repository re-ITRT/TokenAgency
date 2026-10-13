//! 运行时装配：把 [`GatewayDeps`] 与几个接缝装成一个可服务的网关。**唯一的装配入口**。
//!
//! ## 为什么装配只能有一个入口
//!
//! 两个地方各自拼一遍依赖，就会出现两套"什么算已装配好"的答案，而且漂移时
//! 没有任何东西会红（教训：含糊的接口让两个实现者造出不同的系统）。
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
use ta_core::routing::{
    ChannelSelector, EngineOptions, PolicyEngine, RouteContext, RouteDecision, RouteHints, Router,
    RouterOptions, Selection, SelectorOptions,
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
    /// API key 验票接缝（多 key 鉴权）。
    ///
    /// 默认是 `NoApiKeys`（一把 key 都没有 = 不做多 key 鉴权），所以既有装配不用改；
    /// 应用层装它：`RuntimeParts::new(secrets, kernel).with_api_keys(...)`。
    pub api_keys: Arc<dyn crate::ingress::ApiKeyVerifier>,
    /// 内核状态探针（`/healthz` 与 TA-3001 的判据）。
    pub kernel: Arc<dyn KernelProbe>,
    /// 尝试记录接收器。
    pub sink: Arc<dyn AttemptSink>,
    /// `None` = 计量还没接上（G4 在途）。**不提供空实现**：静默地"接了但没数据"
    /// 比没接更糟。
    pub meter: Option<Arc<dyn ResponseMeter>>,
    pub ingress: IngressOptions,
}

impl RuntimeParts {
    /// 除 `secrets` / `kernel` 外都用默认件：全透传 + 日志级 attempt sink + 未接计量。
    pub fn new(secrets: Arc<dyn SecretResolver>, kernel: Arc<dyn KernelProbe>) -> Self {
        Self {
            passthrough: FullPassthrough::shared(),
            secrets,
            api_keys: Arc::new(crate::ingress::NoApiKeys),
            kernel,
            sink: Arc::new(LoggingSink::new()),
            meter: None,
            ingress: IngressOptions::default(),
        }
    }

    /// 装上 API key 验票器。装配点就是这里，别在别处再包一层。
    pub fn with_api_keys(mut self, api_keys: Arc<dyn crate::ingress::ApiKeyVerifier>) -> Self {
        self.api_keys = api_keys;
        self
    }

    pub fn with_sink(mut self, sink: Arc<dyn AttemptSink>) -> Self {
        self.sink = sink;
        self
    }

    pub fn with_meter(mut self, meter: Arc<dyn ResponseMeter>) -> Self {
        self.meter = Some(meter);
        self
    }

    /// 接上**生产计量器**（G4 ④）：响应 usage → 额度账本 / 价格快照 / `usage` 表。
    ///
    /// 为什么不给空实现、也不默认接上：`meter: None` 的语义是"**计量没接线**"，
    /// 此时成本中心永远是 0 —— 静默地"接了但没数据"比没接更糟，
    /// 所以由装配方显式调这一句，而不是让 [`build_runtime`] 替它猜。
    ///
    /// **必须在 tokio 运行时里装配**：落库是 `spawn` 出去的。
    pub fn with_usage_meter(mut self, deps: &Arc<GatewayDeps>) -> Self {
        self.meter = Some(Arc::new(crate::meter::UsageMeter::from_deps(deps)));
        self
    }

    pub fn with_ingress_options(mut self, ingress: IngressOptions) -> Self {
        self.ingress = ingress;
        self
    }

    /// 接上 trace 表（①）：sink 变成"**日志 + trace 表**"两份
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
    /// 跨配置换代共享的粘性表（见模块头注释）。
    sticky: Arc<dyn StickyResolver>,
    /// 各线路的 token 用量来源（平均粘性的依据）。
    ///
    /// 放在门面上而不是 HTTP 管线里：**只有这里同时看得见"用量来源"与"选路"**，
    /// 于是"每请求取一次快照"只有一处实现。`None` = 没装配 → 退化为按 priority。
    /// `BalanceSource` 的实现方自负缓存责任（每请求调一次 `used_tokens`）。
    balance: Option<Arc<dyn ta_core::routing::BalanceSource>>,
    engine_options: EngineOptions,
    selector_options: SelectorOptions,
    /// 单调代次号。**不用地址当前值**：旧分配释放后地址可能复用，
    /// 那样"两个不同代次会报同一个 generation"，诊断与断言都会 flake。
    next_generation: AtomicU64,
    current: Mutex<Current>,
}

/// 与某一版配置绑定的路由器。
///
/// **装配实现只有一处**：`ta_core::routing::Router::with_sticky`（B1 收口）。
/// 这里曾经自己拼一遍 index / engine / selector —— 那是两份**手工同步**的装配，
/// 任何一侧加了新部件两边都不会红，而"策略模拟器 = 真实决策"这个论断会**静默变成假话**
/// 。现在 `LiveRouter` 只负责"换代即重建 + 一把锁内同代求值"，
/// 装配细节全部交给 ta-core。
struct Current {
    /// 缓存这一版的 `Arc`：`Arc::ptr_eq` 判定比"指针值"安全（旧分配不会被复用）
    doc: Arc<ConfigDoc>,
    /// ta-core 装配出来的路由器（唯一装配点）
    router: Router,
    generation: u64,
}

impl LiveRouter {
    pub fn new(
        cfg: Arc<arc_swap::ArcSwap<ConfigDoc>>,
        balance: Option<Arc<dyn ta_core::routing::BalanceSource>>,
        sticky: Arc<dyn StickyResolver>,
    ) -> Self {
        let engine_options = EngineOptions::default();
        let selector_options = SelectorOptions::default();
        let doc = cfg.load_full();
        let current = Mutex::new(Self::bind(&doc, 1, &sticky, &engine_options, &selector_options));
        Self {
            cfg,
            balance,
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
        sticky: &Arc<dyn StickyResolver>,
        engine_options: &EngineOptions,
        selector_options: &SelectorOptions,
    ) -> Current {
        // `opts.clock` 在**注入外部 sticky** 的这条路径上不参与装配（那份解析器自带时钟，
        // 见 `RouterOptions::clock` 的说明）；这里给系统时钟只是为了填满字段。
        let opts = RouterOptions {
            engine: engine_options.clone(),
            selector: selector_options.clone(),
            clock: Arc::new(ta_core::clock::SystemClock),
        };
        let router = Router::with_sticky(doc, opts, sticky.clone());
        Current { doc: doc.clone(), router, generation }
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
        // **一把锁内跑完** decide + select：配置换代不可能插进两者之间。
        // `Router::route` 就是 ta-core 的"完整一次路由"，也保证两者用同一个 router 实例。
        let guard = self.current();
        // 用量快照在这里取**一次**：`used_tokens` 自带 TTL 缓存，
        // 所以这是"每请求拷一份内存 map"，不是"每请求查一次库"。
        // 没装配 → 直接用调用方给的 ctx（`balance` 为空）→ 选路退化为按 priority
        // （**不是**"都没用过"）。
        let (decision, selection) = match &self.balance {
            None => guard.router.route(hints, ctx),
            Some(src) => {
                let mut filled = ctx.clone();
                filled.balance = src.used_tokens();
                guard.router.route(hints, &filled)
            }
        };
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
        self.current().router.decide(hints, ctx)
    }
}

impl ChannelSelector for LiveRouter {
    fn select(
        &self,
        decision: &RouteDecision,
        hints: &RouteHints,
        ctx: &RouteContext,
    ) -> Option<Selection> {
        self.current().router.selector.select(decision, hints, ctx)
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
    let router = Arc::new(LiveRouter::new(deps.config.clone(), deps.balance.clone(), sticky));

    let planner: Arc<dyn RoutingPlanner> = router.clone();
    let pipeline = Arc::new(Pipeline::new(Arc::new(PipelineDeps {
        cfg: deps.config.clone(),
        clock: deps.clock.clone(),
        planner,
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
        .with_gate(gate.clone())
        .with_api_keys(parts.api_keys),
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
    use ta_core::domain::{
        ChannelConfig, ChannelGroup, Credential, Lane, LaneKind, Model, Provider, Selector,
        StickyConfig,
    };
    use ta_core::error::Result;
    use ta_core::pricing::{PricingEngine, ResolvedPrice};
    use ta_core::routing::{BalanceSource, ChannelRuntimeState};
    use ta_core::types::{
        ChannelGroupId, ChannelId, CredentialId, LaneId, Money, Protocol, RequestSummary,
        TokenUsage, UsageRecord,
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

    // 这里曾有 `TwoTierPricing`（"ch-a 贵、ch-b 便宜"的替身），唯一用途是逼出
    // "`cheapest` 选择器到底有没有拿到定价引擎"。选路策略收敛为平均粘性之后
    // 选路不再读价格，那个替身与它的用例一起删除。
    // 定价引擎本身仍在：它服务的是**成本计量**（`meter.rs`），与选路无关。

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
        // 线路在**服务商**上（`channel.lane` 已删除）
        doc.providers.push(
            Provider::new("p-a", "A", Protocol::Openai, "https://a.example/v1")
                .with_lane("lane-jp"),
        );
        doc.credentials.push(Credential::new("c-a", "p-a", "A", "${secret:a}"));
        doc.channels.push(ChannelConfig::new("ch-a", MODEL, "p-a", "c-a", "upstream-a"));
        for i in 0..extra {
            let suffix = (b'b' + i as u8) as char;
            let provider = format!("p-{suffix}");
            let credential = format!("c-{suffix}");
            let channel = format!("ch-{suffix}");
            doc.providers.push(
                Provider::new(
                    provider.as_str(),
                    channel.as_str(),
                    Protocol::Openai,
                    format!("https://{suffix}.example/v1"),
                )
                .with_lane("lane-tokyo"),
            );
            doc.credentials.push(Credential::new(
                credential.as_str(),
                provider.as_str(),
                channel.as_str(),
                "${secret:a}",
            ));
            doc.channels.push(ChannelConfig::new(
                channel.as_str(),
                MODEL,
                provider.as_str(),
                credential.as_str(),
                format!("upstream-{suffix}"),
            ));
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
            balance: None,
        })
    }

    /// 固定用量的 [`ta_core::routing::BalanceSource`] 替身。
    ///
    /// 刻意**不含缓存逻辑**：这一层只验"门面有没有把快照喂进选路"，
    /// TTL 缓存的正确性是 `BalanceSource` 实现方自己的测试（`src-tauri`）。
    pub(crate) struct StaticBalance(pub Vec<(ChannelId, i64)>);

    impl StaticBalance {
        pub(crate) fn new<I, S>(pairs: I) -> Self
        where
            I: IntoIterator<Item = (S, i64)>,
            S: Into<ChannelId>,
        {
            Self(pairs.into_iter().map(|(id, used)| (id.into(), used)).collect())
        }
    }

    impl ta_core::routing::BalanceSource for StaticBalance {
        fn used_tokens(&self) -> std::collections::BTreeMap<ChannelId, i64> {
            self.0.iter().cloned().collect()
        }
    }

    pub(crate) fn test_parts() -> RuntimeParts {
        RuntimeParts::new(
            Arc::new(StaticSecrets::new([("${secret:a}", "sk-test-secret")])),
            Arc::new(FixedKernel(KernelState::Ready)),
        )
    }

    /// **记录型** trace 表替身：只关心"有没有真的被写"，不关心 SQL。
    ///
    /// 为什么需要它：`with_trace_store` 原来**零测试**，而它又从未被生产调用 ——
    /// 于是"生产没接 trace 表"这个缺口既没有测试覆盖，也没有任何东西会红。
    /// 这个替身让"接线"可以被**行为**验证（而不是扫源码字符串）。
    #[derive(Default)]
    pub(crate) struct RecordingTraceStore {
        pub(crate) written: std::sync::Mutex<Vec<String>>,
    }

    impl RecordingTraceStore {
        pub(crate) fn count(&self) -> usize {
            self.written.lock().expect("trace 替身锁").len()
        }
    }

    #[async_trait]
    impl ta_store::TraceStore for RecordingTraceStore {
        async fn write(&self, t: ta_store::TraceRecord) -> Result<()> {
            self.written.lock().expect("trace 替身锁").push(t.request_id.as_str().to_string());
            Ok(())
        }
        async fn query(&self, _q: &ta_store::LogQuery) -> Result<ta_store::LogPage> {
            unimplemented!("本替身只验写入")
        }
        async fn stats(&self, _q: &ta_store::LogQuery) -> Result<ta_store::LogStats> {
            unimplemented!("本替身只验写入")
        }
        async fn get(&self, _id: &str) -> Result<Option<ta_store::TraceDetail>> {
            Ok(None)
        }
        async fn set_mode(&self, _m: ta_core::types::LogMode) -> Result<()> {
            Ok(())
        }
        async fn set_retention(&self, _r: ta_core::types::Retention) -> Result<()> {
            Ok(())
        }
        async fn storage_stats(&self) -> Result<ta_store::StorageStats> {
            unimplemented!("本替身只验写入")
        }
        async fn clear(&self, _m: ta_store::ClearMode) -> Result<u64> {
            Ok(0)
        }
        async fn gc(&self, _now_ms: i64) -> Result<u64> {
            Ok(0)
        }
    }

    /// `RequestTraceSummary` 的**最小**构造：只给测试真正关心的两个字段，其余取合理默认。
    ///
    /// 不实现 `Default`（`Protocol` 等字段没有"中性的错值"，硬给一个反而会掩盖漏填），
    /// 所以这里显式写全，只是把"与本次断言无关"的值集中在一处。
    pub(crate) fn trace_summary(request_id: &str) -> crate::pipeline::RequestTraceSummary {
        crate::pipeline::RequestTraceSummary {
            request_id: request_id.to_string(),
            ts_ms: 1_700_000_000_000,
            model: Some(MODEL.to_string()),
            upstream_model: String::new(),
            protocol_in: Protocol::Openai,
            protocol_out: Protocol::Openai,
            stream: false,
            chosen_channel: None,
            lane: None,
            header_diff: None,
            status: 200,
            total_ms: 12,
            ttfb_ms: None,
            error: None,
        }
    }

    /// 一次尝试记录的最小构造（与 [`trace_summary`] 同一个理由）。
    pub(crate) fn attempt_record(
        request_id: &str,
        channel: &str,
    ) -> crate::pipeline::AttemptRecord {
        crate::pipeline::AttemptRecord {
            request_id: request_id.to_string(),
            channel_id: ChannelId::new(channel),
            credential_id: CredentialId::new("cred-1"),
            lane_id: None,
            attempt: 1,
            status: Some(200),
            error_code: None,
            latency_ms: 12,
            bytes_sent: false,
        }
    }

    /// `with_trace_store` 真的把 trace 表串进了 sink。
    ///
    /// 这条测试的**理由**（不是"为了覆盖率"）：`with_trace_store` 从未被生产装配调用，
    /// 却也没有任何测试碰它 —— 那意味着"轨迹写不进库"这件事
    /// 既不会编译报错、也不会测试变红，只会在用户发现"日志页查不到请求轨迹"时才暴露
    /// （/G4 的原始缺口形态）。
    ///
    /// 所以分两层钉住：
    /// 1. **行为**：装上之后 `sink` 真的把 `TraceRecord` 交给 store（下面这段）；
    /// 2. **接线**：`src-tauri/src/gateway.rs` 的生产装配必须调用它
    ///    （见 `the_gateway_assembly_actually_wires_the_api_key_verifier` 旁的
    ///    `the_gateway_assembly_actually_wires_the_trace_store` —— 那条在应用层）。
    #[tokio::test]
    async fn with_trace_store_really_writes_records_to_the_store() {
        let store = Arc::new(RecordingTraceStore::default());
        let parts = test_parts().with_trace_store(store.clone());

        // 一个请求走完：先有一次尝试，再收尾。
        parts.sink.on_attempt(attempt_record("req-trace-1", "ch-a")).await;
        parts.sink.on_request_complete(trace_summary("req-trace-1")).await;

        assert_eq!(
            store.count(),
            1,
            "with_trace_store 之后，请求收尾必须真的写进 trace 表（写不进去 = 前端日志页与 ta trace 都查不到轨迹）"
        );
        assert_eq!(
            store.written.lock().expect("trace 替身锁").as_slice(),
            ["req-trace-1"],
            "写进去的必须是这个 request_id"
        );
    }

    /// 对照组：**不**调 `with_trace_store` 时，日志 sink 仍然工作、但 trace 表一条都不写。
    ///
    /// 这条是"上面那条测试真的在验接线"的反证：如果没有它，
    /// 上面那条测试可能因为"默认 sink 也会写"而假绿。
    #[tokio::test]
    async fn without_trace_store_nothing_reaches_the_trace_table() {
        let store = Arc::new(RecordingTraceStore::default());
        let parts = test_parts(); // ← 故意不接

        parts.sink.on_attempt(attempt_record("req-trace-2", "ch-a")).await;
        parts.sink.on_request_complete(trace_summary("req-trace-2")).await;

        assert_eq!(store.count(), 0, "没接 trace 表就不该有任何记录");
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

    /// 生产计量器的装配入口：`with_usage_meter` 之后装配里**真的有**计量器。
    ///
    /// 这条挡的是"忘了装配"这类**静默**故障：`meter: None` 不会报任何错，
    /// 只会让成本中心永远 0。计量本身对不对由
    /// `meter::tests` 与 `pipeline::tests::production_meter_*` 证明。
    #[tokio::test]
    async fn usage_meter_is_wired_through_runtime_parts() {
        let deps = test_deps(cfg_with_channels(0), Arc::new(NoDialer));
        let parts = test_parts().with_usage_meter(&deps);
        assert!(parts.meter.is_some(), "with_usage_meter 必须真的接上计量器");
        // 装配也必须照常成功（计量器不会给装配引入新的前置条件）
        let _runtime = build_runtime(&deps, parts);
    }

    /// B1：`LiveRouter` 与 ta-core 的 `Router::with_sticky` **逐字段一致**。
    ///
    /// 为什么必须机器证明：`LiveRouter` 曾经自己拼一遍 index / engine / selector。
    /// 只要存在两份装配，"策略模拟器 = 真实决策"就可能悄悄变成假话
    /// 。现在调用方与实现合一，这条测试把一致性钉成可回归的事实。
    ///
    /// 覆盖四种形态：粘性命中、粘性未命中（priority）、`cheapest`（定价）、无候选模型。
    /// 它们各自逼出**一类参数级漂移**，例如"选择器根本没接上粘性表"（② 会选 ch-a）、
    /// "没把定价引擎传进选择器"（⑤ 会退化成 ch-a）。
    ///
    /// 它挡不住的是**结构性**漂移（有人把同一段装配再抄一遍，而当前配置下两份恰好同结果）——
    /// 那由 [`runtime_has_no_second_router_assembly`] 的源码守卫负责。
    ///
    /// 两边各用一份**独立但同构**的粘性表：共享一份会让"谁先被调用"改变对方的输入
    /// （先调的那次会写下粘性），比较就不再是"同一串请求序列下的行为"。
    ///
    /// 比较用 `Debug` 字符串：`RouteDecision` / `Selection` 只 derive 了 `Debug`（没有
    /// `PartialEq`），而 `Debug` 会打印**全部字段**（含 Vec 的顺序与内容）—— 字符串相等
    /// ⇔ 字段相等。手写字段断言反而会在 ta-core 新增字段时**静默漏掉**它。
    #[tokio::test]
    async fn live_router_matches_ta_core_router_field_by_field() {
        const NOW: i64 = 1_700_000_000_000;
        let mut cfg = cfg_with_channels(1);
        cfg.channel_groups.push(ChannelGroup {
            id: ChannelGroupId::new("grp"),
            model: MODEL.into(),
            members: vec![ChannelId::new("ch-a"), ChannelId::new("ch-b")],
            selector: Selector::BalancedSticky,
            sticky: StickyConfig {
                enabled: true,
                ttl_h: 12,
                key: vec!["session_id".into()],
                extra: Default::default(),
            },
            release_on: vec![],
            extra: Default::default(),
        });
        let doc = Arc::new(cfg.clone());
        // 两份**独立但同构**的粘性表：同一起点、各自演进。共享一份会让"谁先被调用"改变
        // 对方的输入（先调的那次会写下粘性），比较就不再是"同一串请求序列下的行为"。
        let clock = Arc::new(ManualClock::new(NOW));
        let sticky_live: Arc<dyn StickyResolver> =
            Arc::new(MemoryStickyResolver::with_clock(clock.clone()));
        let sticky_ref: Arc<dyn StickyResolver> =
            Arc::new(MemoryStickyResolver::with_clock(clock.clone()));
        let live =
            LiveRouter::new(Arc::new(arc_swap::ArcSwap::from(doc.clone())), None, sticky_live);
        let reference = <Router>::with_sticky(
            &cfg,
            RouterOptions {
                engine: EngineOptions::default(),
                selector: SelectorOptions::default(),
                clock: Arc::new(ta_core::clock::SystemClock),
            },
            sticky_ref,
        );

        let both_free = RouteContext { now_ms: NOW, ..Default::default() };
        // ① 预热：ch-a 耗尽 → 候选只剩 ch-b，粘性因此写成 `sess-1 → ch-b`
        let mut ch_a_down = RouteContext { now_ms: NOW, ..Default::default() };
        ch_a_down.channel_state.insert(
            ChannelId::new("ch-a"),
            ChannelRuntimeState { enabled: false, ..Default::default() },
        );
        let hints = RouteHints {
            model: Some(MODEL.into()),
            session_id: Some("sess-1".into()),
            ..Default::default()
        };
        let warm_live = live.plan(&hints, &ch_a_down);
        let warm_ref = reference.route(&hints, &ch_a_down);
        assert_eq!(
            warm_live.decision.candidates,
            vec![ChannelId::new("ch-b")],
            "预热时 ch-a 应当被淘汰，只剩 ch-b"
        );
        assert_eq!(
            warm_ref.1.as_ref().map(|s| s.channel.clone()),
            Some(ChannelId::new("ch-b")),
            "预热必须真的写下粘性"
        );

        // ② 两个候选都可用：粘性必须压过 priority，**两边都要**选 ch-b
        let plan = live.plan(&hints, &both_free);
        let (decision, selection) = reference.route(&hints, &both_free);
        assert_eq!(
            selection.as_ref().map(|s| (s.channel.clone(), s.sticky_hit)),
            Some((ChannelId::new("ch-b"), true)),
            "命中粘性才选 ch-b —— 这条同时证明本测试有区分力"
        );
        assert_eq!(
            plan.selection.as_ref().map(|s| (s.channel.clone(), s.sticky_hit)),
            Some((ChannelId::new("ch-b"), true)),
            "LiveRouter 也必须命中粘性语义（优先级顺序会选 ch-a）"
        );
        assert_eq!(
            format!("{:?}", plan.decision),
            format!("{decision:?}"),
            "decide 必须逐字段一致（粘性命中形态）"
        );
        assert_eq!(
            format!("{:?}", plan.selection),
            format!("{selection:?}"),
            "select 必须逐字段一致（粘性命中形态）"
        );

        // ③ 另一个会话：粘性未命中 → 按 priority（顺序敏感）
        let fresh = RouteHints {
            model: Some(MODEL.into()),
            session_id: Some("sess-2".into()),
            ..Default::default()
        };
        let plan_fresh = live.plan(&fresh, &both_free);
        let (decision_fresh, selection_fresh) = reference.route(&fresh, &both_free);
        assert_eq!(
            selection_fresh.as_ref().map(|s| (s.channel.clone(), s.sticky_hit)),
            Some((ChannelId::new("ch-a"), false)),
            "未命中粘性时按 priority 选 ch-a —— 与 ② 的 ch-b 相反，这就是区分力所在"
        );
        assert_eq!(
            format!("{:?}", plan_fresh.decision),
            format!("{decision_fresh:?}"),
            "decide 必须逐字段一致（粘性未命中形态）"
        );
        assert_eq!(
            format!("{:?}", plan_fresh.selection),
            format!("{selection_fresh:?}"),
            "select 必须逐字段一致（粘性未命中形态）"
        );

        // ④ 全模型无候选：淘汰清单（含理由）也必须一致
        let missing = RouteHints { model: Some("no-such-model".into()), ..Default::default() };
        let plan_missing = live.plan(&missing, &both_free);
        let (decision_missing, selection_missing) = reference.route(&missing, &both_free);
        assert_eq!(
            format!("{:?}", plan_missing.decision),
            format!("{decision_missing:?}"),
            "decide 必须逐字段一致（无候选形态）"
        );
        assert_eq!(
            format!("{:?}", plan_missing.selection),
            format!("{selection_missing:?}"),
            "select 必须逐字段一致（无候选形态）"
        );

        // ⑤ 平均粘性形态：专盯"用量快照有没有真的传进选择器"这一维。
        //    这是  之后唯一会改变选路结果的运行态输入。
        //    用 `ch-a` 已经用掉 5000 token、`ch-b` 为 0 → 未命中粘性时必须选 `ch-b`；
        //    少了这条，门面"取快照"那一步就没人盯着了。
        //
        //    两边必须用**同一份**配置（`doc` 带上面那个 `grp` 组）：
        //    没有组就没有会话 key，选择器会走"无 key → 档内随机"那条路，
        //    两边随机源不同自然会分叉 —— 那是夹具的错，不是装配的错。
        let bal_live = LiveRouter::new(
            Arc::new(arc_swap::ArcSwap::from(doc.clone())),
            Some(Arc::new(StaticBalance::new([("ch-a", 5000), ("ch-b", 0)]))),
            Arc::new(MemoryStickyResolver::with_clock(clock.clone())),
        );
        let bal_hints = RouteHints {
            model: Some(MODEL.into()),
            session_id: Some("bal-fresh".into()),
            ..Default::default()
        };
        let bal_plan = bal_live.plan(&bal_hints, &both_free);
        assert_eq!(
            bal_plan.selection.as_ref().map(|s| s.channel.clone()),
            Some(ChannelId::new("ch-b")),
            "用量少的那条必须被选中（门面没取快照就会退化回 ch-a）"
        );
        // 与 ta-core 参照路由器逐字段一致：两边喂**同一份**用量快照。
        let mut bal_ctx = RouteContext { now_ms: NOW, ..Default::default() };
        bal_ctx.balance = StaticBalance::new([("ch-a", 5000), ("ch-b", 0)]).used_tokens();
        let bal_ref = <Router>::with_sticky(
            &cfg,
            RouterOptions {
                engine: EngineOptions::default(),
                selector: SelectorOptions::default(),
                clock: Arc::new(ta_core::clock::SystemClock),
            },
            Arc::new(MemoryStickyResolver::with_clock(clock.clone())),
        );
        let (decision_bal, selection_bal) = bal_ref.route(&bal_hints, &bal_ctx);
        assert_eq!(
            format!("{:?}", bal_plan.decision),
            format!("{decision_bal:?}"),
            "decide 必须逐字段一致（平均粘性形态）"
        );
        assert_eq!(
            format!("{:?}", bal_plan.selection),
            format!("{selection_bal:?}"),
            "select 必须逐字段一致（平均粘性形态）"
        );
    }

    /// 装配**只能有一处**：生产的 `runtime.rs` 不许再自己拼 index / engine / selector。
    ///
    /// 这是 B1 的机械保证。行为等价测试挡不住"有人把同一段装配又抄一遍，然后**只改一处**"
    /// —— 那正是 描述的静默分叉。所以这里直接扫自己的源码。
    ///
    /// **只扫 `#[cfg(test)]` 之前的生产段**：本文件下面那些测试自己就要
    /// `Router::with_sticky`（参照路由器），把测试段也算进来，守卫会被测试代码喂饱而失效
    /// （这正是它第一版被实操证伪的地方：把 `bind` 改成 `Router::new` 后它依然绿）。
    ///
    /// 探针字符串是**拼出来**的：否则这条断言自己的字面量就会命中自己。
    #[test]
    fn runtime_has_no_second_router_assembly() {
        let full = include_str!("runtime.rs");
        let src = full.split("#[cfg(test)]").next().unwrap_or(full);
        assert!(
            src.contains("Router::with_sticky"),
            "生产装配必须走 ta-core 的 Router::with_sticky（B1 的唯一装配点）"
        );
        for needle in [
            concat!("Routing", "Index::build"),
            concat!("ChannelSelector", "Impl::new"),
            concat!("PolicyEngine", "Impl::with_options"),
            // 自造粘性表：配置每次换代都会换一份新的 → 用户看到的"会话固定"会莫名清零。
            // 探针带上前导分隔符：否则 `LiveRouter::new(` 会误判（被守的对象自己就长这样）。
            concat!(" ", "Router::", "new("),
            concat!("::Router::", "new("),
        ] {
            assert!(
                !src.contains(needle),
                "生产运行时里出现了第二份路由装配：{needle}（请改用 Router::with_sticky）"
            );
        }
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
        // 客户端发的是 Anthropic 的 `/v1/messages`，而夹具里的上游是
        // `protocol: openai` → **跨协议**，所以真正打出去的是 OpenAI 的端点。
        //
        // 这条断言原来写的是 `…/v1/messages`，等于把"协议不匹配"当成了正确行为
        //（用户第 325 轮的"选 Anthropic 访问不了"就是这么来的）。
        assert_eq!(urls, vec!["https://a.example/v1/chat/completions".to_string()]);
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
