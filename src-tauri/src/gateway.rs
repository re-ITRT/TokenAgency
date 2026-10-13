//! 网关装配：把 `src-tauri` 的运行时状态接到 `ta-gateway` 的**唯一装配入口**上。
//!
//! 依据 ⑤ 与：网关是启动编排里的一个阶段，
//! **装配只走 E5 的 `build_runtime`**，本文件不自己拼管线（：
//! 两条装配路径必然漂移，而且漂移时不报错）。
//!
//! # 本文件提供什么
//!
//! `ta-gateway` 的 `GatewayDeps` / `RuntimeParts` 里有 4 个接缝需要宿主实现，
//! 它们在这里落地（每一个都必须是**真的**，空实现会让功能静默失效 ——）：
//!
//! | 接缝 | 实现 | 为什么不能糊 |
//! |---|---|---|
//! | `GatewayStore` | [`UsageStoreGateway`] → `ta-store` 的用量存储 | 尝试链与请求摘要落库 |
//! | `SecretResolver` | [`SecretsAdapter`] → `ta-secrets` 的优先级链 | **入站本地密钥校验也用它**；空实现 = 所有请求 401 |
//! | `KernelProbe` | [`KernelProbeAdapter`] → `KernelSupervisor::state()`（同步） | `/healthz` 与 TA-3001 的判据 |
//! | `EventBus` | [`TauriEventBus`] → 前端 `emit` | 前端订阅不到就整条实时链路静默失效 |
//!
//! # 就绪判据：真的在监听
//!
//! [`GatewayBoot::wait_listening`] **不会**因为"`serve()` 被调用过"就返回成功 ——
//! 它真的去 TCP 连 `gateway.listen`。这是"网关在 8787 上对外可用"唯一诚实的证据；
//! 否则端口被占 / 绑定失败时我们会报绿，而客户端连不上。
//!
//! # 已登记的字段名（[14 §5]）
//! 本文件的日志只用白名单里的名字：`port` / `path` / `reason` / `kind` / `job` /
//! `request_id` / `code`。`listen` 不是白名单字段，所以监听地址写进 message。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tauri::{AppHandle, Emitter, Runtime};

use ta_core::error::{Result as TaResult, TaError};
use ta_core::pricing::PricingEngineImpl;
use ta_core::types::{RequestSummary, UsageRecord};
use ta_gateway::dispatch::DialerOptions;
use ta_gateway::events::{EventBus, TaEvent};
use ta_gateway::ingress::{ApiKeyVerifier, KernelProbe};
use ta_gateway::pipeline::SecretResolver;
use ta_gateway::traits_store::GatewayStore;
use ta_gateway::{build_runtime, kernel_dialer, GatewayDeps, GatewayRuntime, RuntimeParts};
use ta_kernel::process::KernelSupervisor;
use ta_kernel::{KernelState, PortMap};
use ta_secrets::composite::CompositeStore;
use ta_secrets::resolver::{resolve_spec, SecretString};
use ta_secrets::{EnvStore, SecretStore};
use ta_store::{SqliteUsageStore, UsageStore};

use crate::state::AppState;

// ================================================================ 接缝实现

/// `GatewayStore`：请求用量与最近摘要。
///
/// 放在 `src-tauri` 而不是 `ta-store`：`ta-store` 在依赖图里**低于** `ta-gateway`，
/// 让它去实现后者的 trait 就是反转依赖方向（主管已确认）。
/// （`ta-gateway/src/traits_store.rs` 开头那句"实现由 ta-store 提供适配"是过期注释。）
pub struct UsageStoreGateway {
    usage: Arc<SqliteUsageStore>,
}

impl UsageStoreGateway {
    pub fn new(usage: Arc<SqliteUsageStore>) -> Self {
        Self { usage }
    }
}

#[async_trait]
impl GatewayStore for UsageStoreGateway {
    async fn record_usage(&self, rec: UsageRecord) -> TaResult<()> {
        self.usage.record_usage(&rec).await
    }

    async fn recent_summaries(&self, limit: usize) -> TaResult<Vec<RequestSummary>> {
        // 类型宽度不同（trait 是 usize、store 是 u32）：夹到 u32 上限，别静默截断成 0
        let limit = limit.min(u32::MAX as usize) as u32;
        self.usage.recent_summaries(limit).await
    }
}

/// `BalanceSource`：**时间窗内各线路的 token 用量**，平均粘性的"平均"依据。
///
/// # 为什么必须缓存（用户明确要求"让数据库压力不是那么大"）
///
/// `used_tokens` 会被**每个请求**调用一次。直接查库就是"每请求一条 `GROUP BY`" ——
/// 这正是要避免的。所以这里持一份带 TTL 的快照：
///
/// * 未过期 → 直接返回内存里那份 map（**零 IO、不碰锁以外的任何东西**）；
/// * 过期 → **踢一次后台刷新**，本次仍旧返回旧快照；
/// * 刷新进行中 → 不重复踢（`refreshing` 门闩），于是**一个 TTL 周期最多查一次库**，
///   哪怕同时来 1000 个请求。
///
/// # 为什么是"后台刷新"而不是"当场查库"
///
/// `used_tokens` 是**同步**接口（`RouteContext` 要的是一份纯快照，见
/// `ta_core::routing::BalanceSource`），而 sqlx 是异步的。第一版我用
/// `block_in_place` + `block_on` 在调用点当场查 —— 那有两个问题：
///
/// 1. **单线程 runtime 上会 panic**（`block_in_place` 只允许多线程 runtime）。
///    生产是 Tauri 的运行时，把"能不能发请求"押在"运行时是不是多线程"上太脆；
/// 2. 它会把**请求本身**卡在一次 DB 查询上 —— 正是要避免的东西。
///
/// 现在改成"踢一脚就走"：`used_tokens` **永不阻塞**，代价是**冷启动的第一个 TTL 周期**
/// 里快照还是空的（选路先按 priority 走，刷新落地后自动开始按用量平均）。
/// 这个取舍是刻意的：宁可头几秒不退避，也不要每请求挂在数据库上。
///
/// 刷新失败（库错）时**保留旧快照**并记一条 warn：宁可短暂用稍旧的用量，
/// 也不要因为一次查询失败就把选路打回"按 priority"，那会让用户看到线路选择莫名变来变去。
///
/// # 时间窗
///
/// 滚动 24 小时。选它的理由：太短（比如 1 小时）会让"刚用得多"的线路很快又被判定为最闲，
/// 起不到拉平作用；太长（比如 30 天）则一条早期用得多的线路会**长期**被冷落，
/// 而用户真正想要的是"最近别老压着一条打"。24 小时是这两者之间的取舍，属于可调参数。
pub struct SqliteBalanceSource {
    usage: Arc<SqliteUsageStore>,
    clock: Arc<dyn ta_core::clock::Clock>,
    cache: std::sync::Arc<std::sync::Mutex<Cache>>,
}

#[derive(Default)]
struct Cache {
    snapshot: std::collections::BTreeMap<ta_core::types::ChannelId, i64>,
    /// 上一次**成功**刷新的时刻（`0` = 从没成功过 → 立刻该刷）。
    fetched_at_ms: i64,
    /// 刷新门闩：防止一个 TTL 周期内踢出多次查询（并发请求会同时看到"过期"）。
    refreshing: bool,
}

/// 快照的存活时间。30 秒 = "新会话最多晚 30 秒才开始避开刚变重的那条线路"。
const BALANCE_TTL_MS: i64 = 30_000;
/// 统计窗口：滚动 24 小时（见类型文档里对这个取舍的说明）。
const BALANCE_WINDOW_MS: i64 = 24 * 3_600_000;

impl SqliteBalanceSource {
    pub fn new(usage: Arc<SqliteUsageStore>, clock: Arc<dyn ta_core::clock::Clock>) -> Self {
        Self { usage, clock, cache: std::sync::Arc::new(std::sync::Mutex::new(Cache::default())) }
    }

    /// 拿锁（中毒也要能继续用：一次刷新 panic 不该让选路永久瘫掉）。
    fn lock(cache: &std::sync::Mutex<Cache>) -> std::sync::MutexGuard<'_, Cache> {
        match cache.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

impl ta_core::routing::BalanceSource for SqliteBalanceSource {
    fn used_tokens(&self) -> std::collections::BTreeMap<ta_core::types::ChannelId, i64> {
        let now_ms = self.clock.now_ms();
        let mut guard = Self::lock(&self.cache);

        let stale = now_ms.saturating_sub(guard.fetched_at_ms) >= BALANCE_TTL_MS;
        if stale && !guard.refreshing {
            guard.refreshing = true;
            let usage = self.usage.clone();
            let clock = self.clock.clone();
            let cache = self.cache.clone();
            // 踢一脚就走：**不 await、不阻塞调用方**。刷新落地后写回快照。
            tokio::spawn(async move {
                let since = clock.now_ms() - BALANCE_WINDOW_MS;
                let outcome = usage.channel_token_usage(since).await;
                let mut g = SqliteBalanceSource::lock(&cache);
                // 无论成败都要放开门闩，否则一次失败会把刷新永久卡死。
                g.refreshing = false;
                match outcome {
                    Ok(rows) => {
                        g.snapshot = rows
                            .into_iter()
                            .map(|(id, tokens)| (ta_core::types::ChannelId::new(id), tokens))
                            .collect();
                        g.fetched_at_ms = clock.now_ms();
                    }
                    Err(e) => {
                        // 保留旧快照与旧的 `fetched_at_ms`（于是下个请求会**再**试一次）。
                        tracing::warn!("线路用量快照刷新失败（沿用上一次的快照）：{e}");
                    }
                }
            });
        }

        guard.snapshot.clone()
    }
}

/// `SecretResolver`：`"${secret:name}"` / `"${env:VAR}"` → 明文。
///
/// **必须是真实现**：入站本地密钥校验也走它（E5 的 D4 修复后 `gateway.local_key`
/// 默认写的就是引用）。返回的明文只交给管线去拼请求头，**不进日志** ——
/// 日志里只能出现 [`ta_secrets::mask_secret`] 的结果。
pub struct SecretsAdapter {
    store: Arc<CompositeStore>,
    env: Arc<EnvStore>,
}

impl SecretsAdapter {
    /// 生产链：钥匙串 → 便携文件 → 环境变量。
    ///
    /// `with_defaults()` 失败（钥匙串不可用等）时**降级成"环境变量 + 便携文件"链并留日志**，
    /// 而不是退回空实现 —— 空实现会让每个 `${secret:…}` 都解析失败，
    /// 用户看到的是"我的 key 坏了"，与真实原因（钥匙串打不开）完全不同。
    pub fn with_defaults() -> Self {
        match CompositeStore::with_defaults() {
            Ok(store) => Self { store: Arc::new(store), env: Arc::new(EnvStore::new()) },
            Err(e) => {
                tracing::error!(
                    code = e.code(),
                    error = %e,
                    "密钥优先级链初始化失败，降级为环境变量与便携文件（${{secret:…}} 可能解析失败）"
                );
                let portable = ta_secrets::PortableStore::open(
                    ta_secrets::portable::default_path()
                        .unwrap_or_else(|_| std::path::PathBuf::from("secrets.enc")),
                );
                let chain = CompositeStore::portable_mode(portable);
                Self { store: Arc::new(chain), env: Arc::new(EnvStore::new()) }
            }
        }
    }

    /// 显式给一条链（测试用，避免碰真钥匙串）。
    pub fn with_store(store: CompositeStore, env: EnvStore) -> Self {
        Self { store: Arc::new(store), env: Arc::new(env) }
    }
}

#[async_trait]
impl SecretResolver for SecretsAdapter {
    async fn resolve(&self, spec: &str) -> TaResult<String> {
        let store: &dyn SecretStore = self.store.as_ref();
        let env: &dyn ta_secrets::resolver::EnvSource = self.env.as_ref();
        let value: SecretString = resolve_spec(spec, store, env).await?;
        // 明文只在这一处离开 SecretString，直接交给调用方拼请求头，绝不落到日志里
        Ok(value.into_string())
    }
}

/// `KernelProbe`：同步的内核状态探针（`/healthz` 与 TA-3001 的判据）。
///
/// `KernelSupervisor::state()` 是同步的，所以探针不需要锁也不用缓存 ——
/// 这很重要：**缓存会让"/healthz 说 ready、实际内核已经死了"** 成为可能。
pub struct KernelProbeAdapter {
    supervisor: Option<Arc<KernelSupervisor>>,
}

impl KernelProbeAdapter {
    pub fn new(supervisor: Option<Arc<KernelSupervisor>>) -> Self {
        Self { supervisor }
    }
}

/// `ta_kernel::KernelState`（5 态）→ `ta_gateway::ingress::KernelState`（6 态）。
///
/// 两个枚举**故意不共用**（一个描述 mihomo 进程、一个描述网关看到的内核可用性），
/// 所以这里显式映射；`Unknown` 只用于"根本没接内核"。
fn map_kernel_state(s: &KernelState) -> ta_gateway::ingress::KernelState {
    use ta_gateway::ingress::KernelState as G;
    match s {
        KernelState::Stopped => G::Stopped,
        KernelState::Starting => G::Starting,
        KernelState::Ready => G::Ready,
        KernelState::Restarting => G::Restarting,
        KernelState::Failed => G::Failed,
    }
}

impl KernelProbe for KernelProbeAdapter {
    fn state(&self) -> ta_gateway::ingress::KernelState {
        match &self.supervisor {
            Some(s) => map_kernel_state(&s.state()),
            // 没配内核：**诚实报 unknown**，不假装 ready（`UnknownKernel` 的语义）
            None => ta_gateway::ingress::KernelState::Unknown,
        }
    }
}

// ================================================================ 事件桥

/// `EventBus` → Tauri `emit`。
///
/// 事件名只有一个来源：[`TaEvent::name()`]（10 个名字由 `EVENT_NAMES`
/// 抄成常量并由测试钉住）。 的教训：`request_new` vs `request:new`
/// 这种错**编译全过、运行全静默**，所以名字必须有测试守着。
///
/// **载荷形状**：`request:new` 按 发**批量数组**；其余变体发
/// `TaEvent` 自身序列化后的对象。**已知缺口**：`usage:tick` 的
/// `{ today }` 形状与 `TaEvent::UsageTick` 的字段不完全一致（`ta-gateway`
/// 侧只有 `today_tokens` / `today_cost`），这需要和 E5 对齐后再改载荷，
/// 本文件不自己发明形状。
pub struct TauriEventBus<R: Runtime> {
    app: AppHandle<R>,
    tx: tokio::sync::broadcast::Sender<TaEvent>,
}

impl<R: Runtime> TauriEventBus<R> {
    pub fn new(app: AppHandle<R>) -> Self {
        // 容量给足：订阅者（诊断/看板）短暂落后不该丢事件
        let (tx, _) = tokio::sync::broadcast::channel(1024);
        Self { app, tx }
    }
}

impl<R: Runtime> EventBus for TauriEventBus<R> {
    fn publish(&self, e: TaEvent) {
        // 1) 广播给进程内订阅者（诊断包 / 看板的内部消费者）
        let _ = self.tx.send(e.clone());
        // 2) 发到前端。失败只留痕，绝不让它影响网关主链路
        let name = e.name();
        if let Err(err) = self.app.emit(name, event_payload(&e)) {
            // `tauri::Error` 没有 TA 码 → 包成 `TaError::Internal` 再取 `code()`。
            // **必须走 `code = e.code()` 这个字段形式**：把 "TA-9001" 直接写成字符串
            // 字面量既会被源码扫描器误当 message，也违背 [14 §6 C2] 的"按码检索"。
            let e = TaError::Internal(format!("事件发到前端失败：{name}：{err}"));
            tracing::warn!(code = e.code(), error = %e, "事件发到前端失败");
        }
    }

    fn subscribe(&self) -> tokio::sync::broadcast::Receiver<TaEvent> {
        self.tx.subscribe()
    }
}

/// `TaEvent` → 前端载荷。
///
/// 与 `TaEvent::name()` 配对使用：名字由枚举自己给，载荷在这里裁。
pub fn event_payload(e: &TaEvent) -> serde_json::Value {
    match e {
        // [10 §2.3] 明确要求**批量数组**（一次 IPC 发最多 50 条，避免打爆 WebView 队列）
        TaEvent::RequestNew { items } => serde_json::json!(items),
        other => serde_json::to_value(other).unwrap_or(serde_json::Value::Null),
    }
}

// ================================================================ 装配 + 启动

/// 网关装配入参的**唯一一处**：全透传 + 凭据解析器 + 内核探针 + **API key 验票器** + **trace 表**。
///
/// 为什么单独成函数：这里的接缝**漏一个都不会编译报错、也不会有任何东西变红** ——
/// 只会在真机上表现成"功能静默失效"。两个已知的失效形态：
///
/// * `with_api_keys` 少写一句 → 网关静默退化成 `NoApiKeys`（既不强制鉴权、也不认任何 api key），
///   真机上只表现为"访问密钥建了也挡不住人"；
/// * `with_trace_store` 少写一句 → 请求轨迹**一条都不落库**，真机上表现为
///   "前端日志页与 `ta trace` 查不到任何请求"（/G4）。
///   `with_trace_store` 自己的文档写着"生产装配应当接上"，而它此前**既没被调用、也没有测试** ——
///   正是那种"没人接"的原始缺口形态。
///
/// 抽出来之后，两条防漂移测试能直接对装配入参断言
/// （[`the_gateway_assembly_actually_wires_the_api_key_verifier`] /
/// [`the_gateway_assembly_actually_wires_the_trace_store`]）。
fn runtime_parts(state: &AppState) -> RuntimeParts {
    RuntimeParts::new(
        Arc::new(SecretsAdapter::with_defaults()),
        Arc::new(KernelProbeAdapter::new(state.kernel_supervisor())),
    )
    .with_api_keys(api_key_verifier(state))
    .with_trace_store(state.trace.clone())
}

/// 只造验票器（测试用它，避免碰真钥匙串 —— `SecretsAdapter::with_defaults()` 会开钥匙串）。
fn api_key_verifier(state: &AppState) -> Arc<dyn ApiKeyVerifier> {
    Arc::new(state.api_key_authenticator())
}

/// 装配好的网关 + 监听判据。
pub struct GatewayBoot {
    runtime: Arc<GatewayRuntime>,
    /// `gateway.listen`（如 `127.0.0.1:8787`）——就绪探测真的连它
    listen: String,
    started: AtomicBool,
}

impl GatewayBoot {
    pub fn listen(&self) -> &str {
        &self.listen
    }

    /// 暂停/恢复闸门（[10 §2.2] `app_pause_gateway` / `app_resume_gateway`）。
    ///
    /// 闸门来自**唯一装配路径**：`build_runtime` 把同一个 `Arc<GatewayGate>` 同时交给
    /// 运行时与 `IngressState`，所以本层只是把它取出来交给命令层 ——
    /// **不在 ingress 外面另包一层准入逻辑**（那会是第二条请求路径）。
    pub fn gate(&self) -> Arc<ta_gateway::ingress::GatewayGate> {
        self.runtime.gate.clone()
    }

    /// **唯一出站通道**（`GatewayDeps`：dialer / store / pricing / events）。
    ///
    /// 为什么必须能取出来（而不是"装配完就丢掉"）：命令层要发 HTTP 只能走
    /// `GatewayDeps.dialer` —— `src-tauri` 不依赖 `reqwest`，另开一条出站路径会**绕过线路策略**
    /// 。`app_status` 的"网关状态"也要靠 `AppState.gateway` 是否存在
    /// （`gateway_ready()`），否则状态会撒谎。
    ///
    /// 取回的是**同一个对象**：`assemble` 把 deps 交给 `build_runtime` 之后，运行时仍持有
    /// 那份 `Arc`（可用 `Arc::ptr_eq` 证明），所以这里不是"第二份依赖"。
    pub fn deps(&self) -> Arc<ta_gateway::GatewayDeps> {
        self.runtime.deps.clone()
    }

    /// 按 的"唯一装配入口"装配（不自己拼管线）。
    pub fn assemble(app: AppHandle<tauri::Wry>, state: &AppState, ports: PortMap) -> Self {
        let doc = state.doc();

        // 出站拨号器：**必须**用 E5 的助手（每 lane 一个 client、关自动解压、
        // 不跟随重定向 —— 这些是透传保真的一部分）
        //
        // 端口表用**可热更新**的 `SharedPorts`（不是裸 `PortMap` 的快照）：
        // 端口是运行期才分配的，而拨号器原来拿的是 boot 那一刻的冻结副本 ——
        // 于是 boot 之后新建的 lane（「自动」/ 订阅节点）查不到端口 → TA-3002，
        // 真机表现就是"只有直连能出网"。句柄同时交给 `AppState`，
        // 让每个写配置的命令都能把新端口推过来。
        let shared_ports = ta_gateway::dispatch::SharedPorts::new(ports);
        state.set_lane_ports(shared_ports.clone());
        let dialer = kernel_dialer(shared_ports, DialerOptions::default());

        let events: Arc<dyn EventBus> = Arc::new(TauriEventBus::new(app));

        let deps = Arc::new(GatewayDeps {
            config: state.doc.clone(),
            store: Arc::new(UsageStoreGateway::new(state.usage.clone())),
            dialer,
            events,
            clock: state.clock.clone(),
            pricing: Arc::new(PricingEngineImpl::new(&doc.pricing)),
            // 平均粘性的用量来源：带 30s TTL + 24h 窗口，
            // 于是"每请求取快照"不会变成"每请求查库"。
            balance: Some(Arc::new(SqliteBalanceSource::new(
                state.usage.clone(),
                state.clock.clone(),
            ))),
        });

        // `meter` 保持 None：G4 在途，**不塞空实现**（空实现 = "接了但没数据"，L10）
        //
        // 装配入参只有一处（[`runtime_parts`]）：它与"验票器接没接上"必须可断言 ——
        // 少写一句 `.with_api_keys(...)` 会**静默**退化成 `NoApiKeys`，
        // 在真机上的表现是"访问密钥建了也挡不住人"。
        let parts = runtime_parts(state);

        // **计量接线**（第 330 轮修）。`with_usage_meter` 早就写好了、
        // 文档也写着"生产装配应当显式调它"，但**从来没有人调用** ⇒ `meter: None`
        // ⇒ `usage_request` 表恒为空 ⇒ 成本中心永远是 0、日志详情里的
        // "Token 与成本"没有数据可显示。
        //
        // 上面那句"`meter` 保持 None：G4 在途"在这一轮不再成立：`UsageMeter` 是完整实现
        // （`ta-gateway::meter`）。不接的后果不是"少一个功能"，而是**永远没有数据**，
        // 且界面上看不出原因 —— 那正是 L10 要防的"静默失效"。
        //
        // 安全性：`from_deps` 只是构造；真正 `spawn` 落库发生在**请求时**
        // （那时在 tokio 运行时里），所以在这里接不会踩"没有运行时"那条告警。
        let parts = parts.with_usage_meter(&deps);

        let listen = doc.gateway.listen.clone();
        Self {
            runtime: Arc::new(build_runtime(&deps, parts)),
            listen,
            started: AtomicBool::new(false),
        }
    }

    /// 起服务（幂等）。返回 `false` = 之前已经起过了。
    ///
    /// 用 `tauri::async_runtime::spawn`：我们已经在 Tauri 的运行时里，
    /// 再自己造一个 runtime 会让绑定与关闭时序变复杂。
    pub fn spawn(&self) -> bool {
        if self.started.swap(true, Ordering::SeqCst) {
            return false;
        }
        let runtime = self.runtime.clone();
        let listen = self.listen.clone();
        tauri::async_runtime::spawn(async move {
            tracing::info!(port = %listen, "网关开始服务（只绑回环）");
            if let Err(e) = runtime.serve().await {
                tracing::error!(
                    code = e.code(),
                    error = %e,
                    "网关服务退出：{listen}（客户端将连不上）"
                );
            }
        });
        true
    }

    /// 等 `gateway.listen` **真的能连上**。
    ///
    /// 这是就绪的唯一诚实判据：`spawn()` 只是提交了一个任务，端口可能被占、
    /// 绑定可能失败。**"调用过启动函数"不是就绪。**
    pub async fn wait_listening(&self, timeout: Duration) -> Result<(), String> {
        wait_listening_at(&self.listen, timeout).await
    }
}

/// 编排需要的最小能力面。
///
/// 抽成 trait 是为了让**就绪路径可测**：`GatewayBoot` 是具体类型且装配需要
/// `AppHandle`，如果 `KernelBootDeps` 直接持有它，那"网关在监听 → 转绿"这条
/// 就只能靠真机跑 GUI 来验证。有了这个 trait，测试可以注入一个假的 starter
/// 来断言**编排层的语义**（绿必须等网关真的在监听），
/// 而"TCP 探测本身对不对"由 [`wait_listening_at`] 的真实 listener 测试覆盖 ——
/// 两个问题分开证，各自证得干净。
///
/// 用 `#[async_trait]`（而不是原生 `async fn`）：原生 `async fn` 的 trait
/// **不是 dyn-compatible**，而 `KernelBootDeps` 需要存 `Arc<dyn GatewayStarter>`。
#[async_trait]
pub trait GatewayStarter: Send + Sync {
    /// 起服务（幂等）。
    fn spawn(&self);
    /// 等端口真的可连。
    async fn wait_listening(&self, timeout: Duration) -> Result<(), String>;
}

#[async_trait]
impl GatewayStarter for GatewayBoot {
    fn spawn(&self) {
        GatewayBoot::spawn(self);
    }
    async fn wait_listening(&self, timeout: Duration) -> Result<(), String> {
        GatewayBoot::wait_listening(self, timeout).await
    }
}

/// **就绪探测本体**：反复 TCP 连 `listen`，超时返回可读原因。
///
/// 单独成自由函数（不是 `GatewayBoot` 的方法）纯粹为了可测：
/// 测试可以起一个真的 `TcpListener` 来证明它真的在"连"，
/// 而不是证明"函数被调用过"。
pub async fn wait_listening_at(listen: &str, timeout: Duration) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    loop {
        match tokio::net::TcpStream::connect(listen).await {
            Ok(_) => return Ok(()),
            Err(e) => {
                if Instant::now() >= deadline {
                    return Err(format!("{} 秒内没有在 {} 上监听：{e}", timeout.as_secs(), listen));
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
}

// ================================================================ 内核口径

/// 从 `AppState` 取内核 supervisor 的**具体类型**（探针要同步 `state()`）。
///
/// `AppState` 存的是 `Arc<dyn KernelController>`（只有异步 `status()`），
/// 而 `KernelProbe` 是同步接口。所以启动时要把 supervisor 的具体 Arc 也存一份。
impl AppState {
    pub fn kernel_supervisor(&self) -> Option<Arc<KernelSupervisor>> {
        self.kernel_supervisor.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn set_kernel_supervisor(&self, supervisor: Arc<KernelSupervisor>) {
        *self.kernel_supervisor.write().unwrap_or_else(|e| e.into_inner()) = Some(supervisor);
    }
}

/// 把 `TaError` 转成"可读原因"（编排与 UI 都要字符串）。
pub fn reason_of(e: &TaError) -> String {
    e.to_string()
}

// ================================================================ 测试夹具

/// **一个最小可用的 `GatewayDeps`**（每个部件只做"能被构造"这一件事）。
///
/// 为什么放在生产文件里（而不是某个 `mod tests` 里）：命令层的出站测试
/// （`commands/upstream.rs`）要真的把 deps 装进 `AppState.gateway`、再**经过真 dialer**
/// 对一个本机假上游发请求。夹具只有一份，才不会出现"接线测试用的 deps 与命令测试用的
/// deps 悄悄不一样"。
///
/// `dialer` 是**真的** `KernelDialer`（空的 lane→端口表）：`direct` lane 不走代理，
/// 所以它能打到 `127.0.0.1:<假上游端口>`；其余 lane 会如实报 `TA-3002`。
#[cfg(test)]
pub(crate) fn test_gateway_deps(state: &AppState) -> Arc<GatewayDeps> {
    /// 空的 `EventBus`：本夹具只证明"依赖能被构造"，不证明事件真的流起来。
    struct NoEvents;

    impl EventBus for NoEvents {
        fn publish(&self, _e: TaEvent) {}

        fn subscribe(&self) -> tokio::sync::broadcast::Receiver<TaEvent> {
            let (tx, rx) = tokio::sync::broadcast::channel(1);
            drop(tx);
            rx
        }
    }

    let doc = ta_core::domain::ConfigDoc::default();
    Arc::new(GatewayDeps {
        config: state.doc.clone(),
        store: Arc::new(UsageStoreGateway::new(state.usage.clone())),
        dialer: ta_gateway::kernel_dialer(
            Arc::new(ta_kernel::PortMap::default()),
            DialerOptions::default(),
        ),
        events: Arc::new(NoEvents),
        clock: state.clock.clone(),
        pricing: Arc::new(PricingEngineImpl::new(&doc.pricing)),
        // 这个夹具只测"网关起得来"，用量来源不需要真接上 → `None`
        // （选路会退化为按 priority，那对这条测试无关）。
        balance: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    // 快照那两条用例要**通过 trait 调** `used_tokens`（它定义在 trait 上）。
    use ta_core::routing::BalanceSource;

    /// 一个**真的库**：验票器要证明的就是"它查的是这张表"。
    async fn temp_state(tag: &str) -> AppState {
        let dir = std::env::temp_dir().join(format!("ta-gw-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("建临时目录");
        AppState::bootstrap(dir.join("db.sqlite")).await.expect("开库")
    }

    /// 的接线：**装上的是真 store，不是 `NoApiKeys`**。
    ///
    /// 两边都断言（反向 + 正向）：空库时 `any_enabled` 必须是假（否则老用户全 401）；
    /// 建一把之后必须是真，且明文能换回它的 id（`NoApiKeys` 的 `verify` 恒 `None`，
    /// 所以这条会在"忘了接"时红）。
    #[tokio::test]
    async fn the_api_key_verifier_is_wired_to_the_real_store() {
        let state = temp_state("api-key-wiring").await;
        let verifier = api_key_verifier(&state);
        assert!(!verifier.any_enabled().await.unwrap(), "空库 = 一把都没有 → 不强制鉴权");

        let created = state.api_keys.create("客户端", None, 1).await.unwrap();
        assert!(verifier.any_enabled().await.unwrap(), "装上的必须是真 store，不是 NoApiKeys");
        assert_eq!(
            verifier.verify(&created.plaintext).await.unwrap(),
            Some(created.key.id.clone()),
            "明文必须换回它自己的 key_id（NoApiKeys 恒 None，忘接线就会红）"
        );
        assert!(verifier.verify("ta-sk-别的").await.unwrap().is_none());
    }

    /// 第 330 轮真机 bug 的复现与修复：**配置说 meta，store 说 off ⇒ 一条都不落库**。
    ///
    /// 真机证据（用户 DB）：`store_setting` 里 `('trace.mode', '"off"')`，
    /// 而配置里是 `request_log.mode: meta` —— 两个真相源，没人同步。
    /// `TraceStore::write` 在 off 下**直接 return**，所以日志页永远空白，
    /// 而用户在界面上找不到原因。
    ///
    /// 这条测试把整个过程走一遍：先**制造**那个坏状态，再断言同步把它治好。
    #[tokio::test]
    async fn syncing_request_log_settings_recovers_from_a_stale_off_mode() {
        use ta_store::TraceStore;

        let state = temp_state("log-sync").await;

        // ① 制造坏状态：store 被设成 off（模拟"历史上某次被关掉、配置后来改成 meta"）
        state.trace.set_mode(ta_core::types::LogMode::Off).await.unwrap();
        assert!(!state.trace.mode().await.unwrap().enabled(), "先确认真的关上了");

        // ② 配置说 meta（默认就是 Meta）
        let doc = state.doc();
        assert_eq!(doc.request_log.mode, ta_core::types::LogMode::Meta);

        // ③ 同步 = 修复
        crate::commands::log::sync_request_log_settings(&state, &doc).await.unwrap();
        assert!(
            state.trace.mode().await.unwrap().enabled(),
            "同步之后 store 必须跟着配置走 —— 否则日志一条都不落库"
        );

        // ④ 真的能写进去了（不是只改了那个 setting 字段）
        let mut record = ta_store::TraceRecord::new(
            ta_core::types::RequestId::new("req_log_sync"),
            1_700_000_000_000,
            "m".to_string(),
        );
        record.status = 200;
        state.trace.write(record).await.unwrap();
        let page = state.trace.query(&ta_store::LogQuery::default()).await.unwrap();
        assert!(
            page.rows.iter().any(|r| r.request_id.as_str() == "req_log_sync"),
            "同步后日志必须真的落库"
        );
    }

    /// **装配入参必须真的接上验票器 / trace 表 / 计量器**。
    ///
    /// 上面的行为测试直接调 `api_key_verifier`，所以把 `assemble` 里那一句改回内联
    /// `RuntimeParts::new(...)` 它照样绿 —— 而"没人接"正是这个缺口的原始形态。
    /// 只扫 `#[cfg(test)]` 之前的生产段。
    #[test]
    fn the_gateway_assembly_actually_wires_the_api_key_verifier() {
        let full = include_str!("gateway.rs");
        let production = full.split("#[cfg(test)]").next().unwrap_or(full);
        assert!(
            production.contains("let parts = runtime_parts(state);"),
            "assemble 必须走 runtime_parts（唯一装配点）"
        );
        assert!(
            production.contains(".with_api_keys("),
            "装配入参必须装上 API key 验票器（docs/19 §3.3：少这一句 = 建了 key 也挡不住人）"
        );
        // 第二处同类缺口：`with_trace_store` 的文档写着"生产装配应当接上"，
        // 但它此前**从未被调用、也没有任何测试** ⇒ 请求轨迹一条都不落库，
        // 真机上只表现为"日志页查不到请求"，编译与测试全绿。
        // 这一句和上面那句是**同一个模式**，所以放在一起守。
        assert!(
            production.contains(".with_trace_store("),
            "装配入参必须接上 trace 表（[docs/14 §6 G2]：少这一句 = 请求轨迹一条都不落库，\
             前端日志页与 `ta trace` 都查不到；行为验证见 ta-gateway 的 \
             `with_trace_store_really_writes_records_to_the_store`）"
        );
        // 第 330 轮两处**同类**缺口（都是"功能代码写好、但没人调用"）：
        //
        // 1. `with_usage_meter` 从来没被调用 ⇒ `usage_request` 恒为空 ⇒
        //    成本中心永远是 0、日志详情里的"Token 与成本"没有数据；
        // 2. 配置里的 `request_log.*` 从来没被喂给 trace store ⇒ 用户改了日志设置
        //    看不出变化，而 store 里一条古老的 `trace.mode = "off"` 会让
        //    **一条日志都不落库**（真机现象）。
        //
        // 这一条守卫是**模式匹配**：这个仓库已经靠它抓到过两次同样的错
        // （`with_api_keys` / `with_trace_store`），所以第三、四次也按同一模式守。
        assert!(
            production.contains(".with_usage_meter("),
            "装配入参必须接上用量计量器（[docs/59]：少这一句 = 成本中心永远 0、\
             usage 表恒空，而界面上看不出原因）"
        );
        // 启动同步在 `lib.rs` 的 setup 里，所以扫那个文件的生产段。
        let lib = include_str!("lib.rs");
        let lib_production = lib.split("#[cfg(test)]").next().unwrap_or(lib);
        assert!(
            lib_production.contains("sync_request_log_settings"),
            "启动时必须把 `request_log.*` 同步进 trace store（[docs/59]：少这一句 = \
             配置说 meta、store 说 off，日志一条都不落库）"
        );
    }

    /// 的 10 个事件名，**逐字**与 `TaEvent::name()` 对齐。
    ///
    /// 这条测试存在的唯一理由： ——
    /// `request_new` vs `request:new` 这种错**编译全过、运行全静默**。
    const DOC_EVENT_NAMES: [&str; 10] = [
        "request:new",
        "usage:tick",
        "channel:state",
        "lane:health",
        "kernel:status",
        "config:changed",
        "subscription:updated",
        "log:append",
        "toast",
        "tray:refresh",
    ];

    #[test]
    fn every_event_name_matches_the_contract_exactly() {
        let events = [
            TaEvent::RequestNew { items: Vec::new() },
            TaEvent::UsageTick {
                today_tokens: 0,
                today_cost: ta_core::types::Money::new(0.0, "CNY"),
            },
            TaEvent::ChannelState {
                channel_id: "c".into(),
                state: "cooling".into(),
                until_ms: None,
            },
            TaEvent::LaneHealth { lane_id: "l".into(), node: None, latency_ms: None, ok: true },
            TaEvent::KernelStatus { state: "ready".into(), version: None },
            TaEvent::ConfigChanged { source: "db".into(), applied: true },
            TaEvent::SubscriptionUpdated { id: "s".into(), ok: true, node_count: 0, error: None },
            TaEvent::LogAppend { items: Vec::new() },
            TaEvent::Toast {
                level: "info".into(),
                title: "t".into(),
                body: "b".into(),
                dedupe_key: "k".into(),
            },
            TaEvent::TrayRefresh { payload: serde_json::Value::Null },
        ];

        let got: Vec<&str> = events.iter().map(|e| e.name()).collect();
        assert_eq!(got, DOC_EVENT_NAMES, "事件名必须与 [10 §2.3] 逐字一致");
    }

    #[test]
    fn request_new_payload_is_a_bare_array_not_a_tagged_object() {
        // [10 §2.3]：载荷就是数组。若发成 {"kind":"request:new","items":[…]}，
        // 前端 `.map()` 会直接崩。
        let payload = event_payload(&TaEvent::RequestNew { items: Vec::new() });
        assert!(payload.is_array(), "request:new 的载荷必须是数组：{payload}");
    }

    #[test]
    fn kernel_state_mapping_covers_all_five_variants() {
        use ta_gateway::ingress::KernelState as G;
        assert_eq!(map_kernel_state(&KernelState::Stopped), G::Stopped);
        assert_eq!(map_kernel_state(&KernelState::Starting), G::Starting);
        assert_eq!(map_kernel_state(&KernelState::Ready), G::Ready);
        assert_eq!(map_kernel_state(&KernelState::Restarting), G::Restarting);
        assert_eq!(map_kernel_state(&KernelState::Failed), G::Failed);
    }

    #[test]
    fn probe_without_a_kernel_reports_unknown_not_ready() {
        // 没配内核时**不能**报告 ready —— 那会让 /healthz 撒谎
        let probe = KernelProbeAdapter::new(None);
        assert_eq!(probe.state(), ta_gateway::ingress::KernelState::Unknown);
        assert_ne!(probe.state(), ta_gateway::ingress::KernelState::Ready);
    }

    #[test]
    fn reason_of_keeps_the_user_text_and_the_code_is_separate() {
        // `TaError::Display` **故意不含码**（[14 §6.2]：给用户看的文案不该出现内部编号），
        // 所以 `reason_of` 只保证"人话在"；"能按码检索"靠的是日志里的 `code` 字段。
        // 这条测试把两者的分工钉住 —— 谁要是想靠 `reason` 去搜码，会在这里看到为什么不行。
        let e = TaError::Kernel("起不来".into());
        let reason = reason_of(&e);
        assert!(reason.contains("起不来"), "人话必须在：{reason}");
        assert!(
            !reason.contains("TA-"),
            "Display 不该带码（带了就说明 [14 §6.2] 被改了）：{reason}"
        );
        assert!(e.code().starts_with("TA-"), "码要能单独取出来：{}", e.code());
    }

    /// 就绪探测**真的在连 TCP**：对着一个真 listener 必须成功。
    ///
    /// 这条测试的意义：证明"网关在监听"这个判据不是"函数被调用过"，
    /// 而是真的建立了连接。
    #[tokio::test]
    async fn wait_listening_succeeds_against_a_real_listener() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("起临时 listener");
        let addr = listener.local_addr().expect("取地址").to_string();

        let ok = wait_listening_at(&addr, Duration::from_secs(2)).await;
        assert!(ok.is_ok(), "真 listener 在听，探测必须成功：{ok:?}");
    }

    /// 反向：没人监听时必须**超时失败**，而不是"调用过就算过"。
    #[tokio::test]
    async fn wait_listening_times_out_when_nothing_is_listening() {
        // 拿一个当前空闲的端口，然后立刻放掉 —— 探测期间没人监听
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("占位");
        let addr = listener.local_addr().expect("取地址").to_string();
        drop(listener);

        let err = wait_listening_at(&addr, Duration::from_millis(200))
            .await
            .expect_err("没人监听就必须失败");
        assert!(err.contains("没有在"), "{err}");
        assert!(err.contains(&addr), "要点名地址：{err}");
    }

    // ---------------------------------------------------------- 用量快照缓存

    /// 往 `usage_request` 里真写一条用量（`total_tokens` 就是快照要 SUM 的那一列）。
    async fn insert_usage(state: &AppState, id: &str, ts: i64, channel: &str, tokens: i64) {
        sqlx::query(
            "INSERT INTO usage_request (request_id, ts, channel_id, total_tokens)
             VALUES (?, ?, ?, ?)",
        )
        .bind(id)
        .bind(ts)
        .bind(channel)
        .bind(tokens)
        .execute(state.usage.pool())
        .await
        .expect("插一条用量");
    }

    /// 等异步刷新落地（有界轮询，别用固定 sleep —— 那会 flake）。
    async fn wait_for_channel_tokens(
        src: &SqliteBalanceSource,
        channel: &str,
        want: i64,
    ) -> std::collections::BTreeMap<ta_core::types::ChannelId, i64> {
        let key = ta_core::types::ChannelId::new(channel);
        for _ in 0..400 {
            let snap = src.used_tokens();
            if snap.get(&key).copied() == Some(want) {
                return snap;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("等不到快照里 {channel} = {want}（最后一次：{:?}）", src.used_tokens());
    }

    ///：用量快照**一个 TTL 周期最多查一次库**，不是每请求查一次 ——
    /// 这正是用户要求的"让数据库压力不是那么大"。
    ///
    /// 验法不靠数 SQL 条数，而靠**行为证据**：刷新是异步的，所以
    /// ① 第一次调用必须**立刻**返回（拿到空表，说明它没有当场查库）；
    /// ② 刷新落地后才看得到数据；
    /// ③ 期间再往库里写一条，**立刻**再调用 —— 仍旧是旧快照（TTL 内没有重新查库）；
    /// ④ 把时钟推过 TTL 才会再刷一次，那时新用量才出现。
    ///
    /// ③ 是这条测试的核心：它是"缓存真的在挡查询"唯一可观察的证据。
    #[tokio::test]
    async fn balance_snapshot_is_cached_for_a_ttl_instead_of_querying_per_request() {
        let state = temp_state("balance-cache").await;
        let clock = Arc::new(ta_core::clock::ManualClock::new(1_700_000_000_000));
        let src = SqliteBalanceSource::new(state.usage.clone(), clock.clone());
        let key = ta_core::types::ChannelId::new("ch-a");

        insert_usage(&state, "r1", 1_700_000_000_000, "ch-a", 300).await;

        // ① 第一次调用不阻塞：此刻刷新还在后台跑，所以是空表（= 不知道用量）
        let first = src.used_tokens();
        assert!(first.is_empty(), "第一次调用必须立刻返回、不当场查库：{first:?}");

        // ② 刷新落地
        let landed = wait_for_channel_tokens(&src, "ch-a", 300).await;
        assert_eq!(landed.get(&key).copied(), Some(300));

        // ③ TTL 内不查库：新写一条 700，立刻再取仍旧是 300
        insert_usage(&state, "r2", 1_700_000_000_000, "ch-a", 700).await;
        let cached = src.used_tokens();
        assert_eq!(
            cached.get(&key).copied(),
            Some(300),
            "TTL 内不该重新查库（这正是「压力不大」的根据）：{cached:?}"
        );

        // ④ 推过 TTL → 再刷一次，新用量 1000 才出现
        clock.advance(BALANCE_TTL_MS);
        let _ = src.used_tokens(); // 踢一脚
        let refreshed = wait_for_channel_tokens(&src, "ch-a", 1000).await;
        assert_eq!(refreshed.get(&key).copied(), Some(1000), "过 TTL 后应看到新总量 300+700");
    }

    /// 并发调用**只踢一次**刷新：门闩挡住"每个请求都去查一次"。
    ///
    /// 用真库跑会太快看不出差别，所以这里直接盯门闩的可观察后果：
    /// 同一时刻连调 50 次，只能有一次刷新在飞 —— 表现是**不会 panic、不会阻塞**，
    /// 且返回的都是同一份快照（而不是"最后一个查完的覆盖前面的"）。
    #[tokio::test]
    async fn concurrent_calls_share_one_refresh_and_never_block() {
        let state = temp_state("balance-concurrent").await;
        let clock = Arc::new(ta_core::clock::ManualClock::new(1_700_000_000_000));
        let src = Arc::new(SqliteBalanceSource::new(state.usage.clone(), clock));
        insert_usage(&state, "r1", 1_700_000_000_000, "ch-a", 42).await;

        // 50 次同步连调：每一次都必须立刻返回（空快照），不能有任何一次阻塞在查询上
        for i in 0..50 {
            let snap = src.used_tokens();
            assert!(snap.is_empty(), "第 {i} 次调用阻塞/查库了：{snap:?}");
        }
        // 刷新最终落地成一份完整快照
        wait_for_channel_tokens(&src, "ch-a", 42).await;
    }
}
