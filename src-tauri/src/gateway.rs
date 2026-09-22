//! 网关装配：把 `src-tauri` 的运行时状态接到 `ta-gateway` 的**唯一装配入口**上。
//!
//! 依据 [docs/12 §5 T2] ⑤ 与 [docs/10 §1.1]：网关是启动编排里的一个阶段，
//! **装配只走 E5 的 `build_runtime`**，本文件不自己拼管线（[docs/13 L21]：
//! 两条装配路径必然漂移，而且漂移时不报错）。
//!
//! # 本文件提供什么
//!
//! `ta-gateway` 的 `GatewayDeps` / `RuntimeParts` 里有 4 个接缝需要宿主实现，
//! 它们在这里落地（每一个都必须是**真的**，空实现会让功能静默失效 —— [docs/13 L10]）：
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
use ta_core::quota::QuotaLedgerImpl;
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
/// 事件名只有一个来源：[`TaEvent::name()`]（[docs/10 §2.3] 的 10 个名字由 `EVENT_NAMES`
/// 抄成常量并由测试钉住）。[docs/13 L2] 的教训：`request_new` vs `request:new`
/// 这种错**编译全过、运行全静默**，所以名字必须有测试守着。
///
/// **载荷形状**：`request:new` 按 [docs/10 §2.3] 发**批量数组**；其余变体发
/// `TaEvent` 自身序列化后的对象。**已知缺口**：`usage:tick` 的
/// `{ today, quota }` 形状与 `TaEvent::UsageTick` 的字段不完全一致（`ta-gateway`
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

/// 网关装配入参的**唯一一处**：全透传 + 凭据解析器 + 内核探针 + **API key 验票器**。
///
/// 为什么单独成函数：`RuntimeParts::with_api_keys` 少写一句，网关会静默退化成
/// `NoApiKeys`（既不强制鉴权、也不认任何 api key）—— 那种失效在真机上只表现为
/// "访问密钥建了也挡不住人"（[docs/19 §3.3]），没有任何东西会红。
/// 抽出来之后 [`the_api_key_verifier_is_wired_to_the_real_store`] 能直接对装配入参断言。
fn runtime_parts(state: &AppState) -> RuntimeParts {
    RuntimeParts::new(
        Arc::new(SecretsAdapter::with_defaults()),
        Arc::new(KernelProbeAdapter::new(state.kernel_supervisor())),
    )
    .with_api_keys(api_key_verifier(state))
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
    /// **不在 ingress 外面另包一层准入逻辑**（那会是第二条请求路径，[docs/13 L21]）。
    pub fn gate(&self) -> Arc<ta_gateway::ingress::GatewayGate> {
        self.runtime.gate.clone()
    }

    /// **唯一出站通道**（`GatewayDeps`：dialer / store / pricing / quota / events）。
    ///
    /// 为什么必须能取出来（而不是"装配完就丢掉"）：命令层要发 HTTP 只能走
    /// `GatewayDeps.dialer` —— `src-tauri` 不依赖 `reqwest`，另开一条出站路径会**绕过线路策略**
    /// （[docs/12 §5 T2]）。`app_status` 的"网关状态"也要靠 `AppState.gateway` 是否存在
    /// （`gateway_ready()`），否则状态会撒谎。
    ///
    /// 取回的是**同一个对象**：`assemble` 把 deps 交给 `build_runtime` 之后，运行时仍持有
    /// 那份 `Arc`（可用 `Arc::ptr_eq` 证明），所以这里不是"第二份依赖"。
    pub fn deps(&self) -> Arc<ta_gateway::GatewayDeps> {
        self.runtime.deps.clone()
    }

    /// 按 [docs/12 §5 T2] 的"唯一装配入口"装配（不自己拼管线）。
    pub fn assemble(app: AppHandle<tauri::Wry>, state: &AppState, ports: PortMap) -> Self {
        let doc = state.doc();

        // 出站拨号器：**必须**用 E5 的助手（每 lane 一个 client、关自动解压、
        // 不跟随重定向 —— 这些是透传保真的一部分）
        let dialer = kernel_dialer(Arc::new(ports), DialerOptions::default());

        let events: Arc<dyn EventBus> = Arc::new(TauriEventBus::new(app));

        let deps = Arc::new(GatewayDeps {
            config: state.doc.clone(),
            store: Arc::new(UsageStoreGateway::new(state.usage.clone())),
            dialer,
            events,
            clock: state.clock.clone(),
            pricing: Arc::new(PricingEngineImpl::new(&doc.pricing)),
            quota: Arc::new(QuotaLedgerImpl::with_clock(state.quota.clone(), state.clock.clone())),
        });

        // `meter` 保持 None：G4 在途，**不塞空实现**（空实现 = "接了但没数据"，L10）
        //
        // 装配入参只有一处（[`runtime_parts`]）：它与"验票器接没接上"必须可断言 ——
        // 少写一句 `.with_api_keys(...)` 会**静默**退化成 `NoApiKeys`，
        // 在真机上的表现是"访问密钥建了也挡不住人"（[docs/19 §3.3]）。
        let parts = runtime_parts(state);

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
        quota: Arc::new(QuotaLedgerImpl::with_clock(state.quota.clone(), state.clock.clone())),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一个**真的库**：验票器要证明的就是"它查的是这张表"。
    async fn temp_state(tag: &str) -> AppState {
        let dir = std::env::temp_dir().join(format!("ta-gw-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("建临时目录");
        AppState::bootstrap(dir.join("db.sqlite")).await.expect("开库")
    }

    /// [docs/19 §3.3] 的接线：**装上的是真 store，不是 `NoApiKeys`**。
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

    /// **防漂移**：生产装配路径必须真的用 [`runtime_parts`]（也就是真的接上验票器）。
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
    }

    /// [docs/10 §2.3] 的 10 个事件名，**逐字**与 `TaEvent::name()` 对齐。
    ///
    /// 这条测试存在的唯一理由：[docs/13 L2] ——
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
}
