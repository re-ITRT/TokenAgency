//! 启动编排的**真实装配**：把 [`crate::lifecycle::BootOrchestrator`] 接到内核、事件与托盘上。
//!
//! 分工（这是本文件存在的理由）：
//! * `lifecycle.rs` 只有**编排逻辑**（顺序 / 超时 / 降级），它能离线测；
//! * `boot.rs` 负责**生产接线**（怎么造内核、怎么把阶段广播出去、就绪判据从哪来）。
//!
//! # 就绪判据：不是"调用过启动函数"
//!
//! B2 的教训：provider 加载失败时 mihomo **一边 `level=error` 一边照常启动**，
//! REST 也通。所以"进程活着 + `/version` 有响应"会把**整个内核静默降级**报成"已就绪"。
//! 本文件的 `wait_kernel_ready` 只认 `KernelState::Ready`，而 `ta-kernel` 的
//! `KernelSupervisor` 只在**两个关都过**之后才置 Ready：
//! 1. `wait_rest_ready`（进程活着、REST 通）；
//! 2. `wait_providers` → `verify_providers()`（我们写进配置的**每个** provider 的
//!    `proxies[]` 都非空）。
//!
//! 也就是说"转绿"这件事在这里是**传递**的：我们只信内核自己给出的 Ready，
//! 而那个 Ready 已经是带 provider 判据的。反过来，我们绝不自己拼一个"看起来就绪"。
//!
//! # 网关：故意留成可见的缺口
//!
//! `BootDeps` 的 `start_gateway` / `wait_gateway_ready` 在本文件里**不覆盖**，走默认实现，
//! 明确返回"装配入口尚未接线"。网关的生产装配由 E5 在 `ta-gateway` 里提供 ——
//! 在他落地之前，编排会收敛到 `Degraded` 并把原因显示给用户。
//! **我们这边不另造一条 `IngressState` 构造路径**：两套构造方式必然漂移，
//! 而"假装网关已就绪"比"明确说没接"危险得多。

use std::path::Path;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tauri::{AppHandle, Emitter, Runtime};

use ta_core::domain::ConfigDoc;
use ta_core::error::TaError;
use ta_kernel::config_gen::{GeneratedConfigSource, MhConfigGen};
use ta_kernel::ports::{FilePortStore, PortAllocator, RangePortAllocator};
use ta_kernel::process::{KernelProcessConfig, KernelSupervisor};
use ta_kernel::rest::KernelRest;
use ta_kernel::{KernelController, KernelState, KernelStatus, PortMap};

use crate::lifecycle::{BootDeps, BootPhase, BootPhasePayload, Sleeper, TokioSleeper, EVENT_BOOT};
use crate::tray;
use crate::tray::{
    ActionOutcome, EmergencyMode, TrayAction, TrayDataSource, TraySnapshot, TrayStatus,
};

/// 等**每一个**就绪阶段（内核、网关）的超时预算。
///
/// ⚠️ 这是**每阶段各一份**，不是"整场启动的总预算" —— `BootOrchestrator::run` 把
/// 同一个 `timeout` 分别交给 `wait_kernel_ready` 与 `wait_gateway_ready`，
/// 所以最坏情况是"内核起的耗时 + 60s + 60s"。
///
/// 为什么必须比 `lifecycle::KERNEL_READY_TIMEOUT`（20s）宽：内核自己的两道关就有
/// `ready_timeout`(5s) + `provider_ready_timeout`(30s)，而订阅抓取可能更慢 ——
/// 单阶段预算比内核内层短会把"订阅慢"误判成"内核起不来"。
pub const READY_TIMEOUT: Duration = Duration::from_secs(60);

/// 轮询内核状态的间隔。
pub const POLL_INTERVAL: Duration = Duration::from_millis(200);

// ================================================================ 内核装配

/// 装配好的内核：控制器（给编排与命令层）+ 配置源（改配置后要 `set_doc`/`set_ports`）。
pub struct KernelBuild {
    pub supervisor: Arc<KernelSupervisor>,
    /// 生成配置的真源。命令层 `kernel_apply` / 配置变更时要更新它，
    /// 所以除了交给 supervisor，还要单独留一份句柄。
    pub source: Arc<GeneratedConfigSource>,
    /// lane → 内核 listener 端口。
    ///
    /// 网关的出站拨号器要用它，而 `ta_kernel::PortMap` **已经**实现了
    /// `ta_gateway::dispatch::LanePorts`（blanket impl），所以这里直接把表交出去，
    /// 不需要任何适配器。
    pub port_map: PortMap,
}

impl std::fmt::Debug for KernelBuild {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KernelBuild")
            .field("program", &self.supervisor.config().program)
            .field("controller", &self.supervisor.config().controller)
            .finish()
    }
}

/// **内置内核**的位置（随安装包分发；）。找不到就返回 `None` —— **绝不猜**。
///
/// 查找顺序（都相对**当前可执行文件**，安装后资源就在它旁边）：
///   1. `<exe 目录>/mihomo.exe`（NSIS 平铺安装）；
///   2. `<exe 目录>/resources/mihomo/mihomo.exe`（Tauri 按 `bundle.resources` 保留相对路径）；
///   3. `<exe 目录>/resources/resources/mihomo/mihomo.exe`（某些 Tauri 版本会多套一层）。
pub fn bundled_kernel_path() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    for rel in
        ["mihomo.exe", "resources/mihomo/mihomo.exe", "resources/resources/mihomo/mihomo.exe"]
    {
        let candidate = dir.join(rel);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// 随包的 **GeoIP 库**（`Country.mmdb`）在哪。
///
/// 为什么必须有它：mihomo **找不到 `Country.mmdb` 会在启动阶段去下载**它，而在大陆网络下
/// 这常常卡住或失败 ⇒ 控制器根本不监听 ⇒ 我们读 `/version` 超时（`TA-9001 内核就绪超时`）
/// ⇒ 应用探测全失败（如实报 `all_unreachable`）、真机验证整轮挂住。
/// 实测：把 7.35 MB 的 `Country.mmdb` 预置进内核目录后，**节点立刻可测**（42/45/40/60 ms ✓）。
pub fn bundled_country_mmdb() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    for rel in
        ["Country.mmdb", "resources/mihomo/Country.mmdb", "resources/resources/mihomo/Country.mmdb"]
    {
        let candidate = dir.join(rel);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// 按配置装配内核。
///
/// * `Ok(None)` = **配置里没写内核路径**（`net.kernel.path` 为空）。这不是错误：
///   用户可能只想用直连通道。调用方应把它转成一句可读原因，而不是当成"就绪"。
/// * `Err(TaError)` = 内核**想装配但装配不了**（端口分配失败、配置源建不起来），
///   这时要如实降级并说明原因。
///
/// 返回 `TaError` 而不是 `String` 是刻意的：`TaError` 带 `code()`（
/// `TA-xxxx`），调用方才能按 的契约打出**可被码检索**的错误日志。
/// 早期版本在这里 `format!("{e}")` 把码丢了 —— 那正是 `error_log_guard` 要抓的那类问题。
pub fn build_kernel(doc: &ConfigDoc, data_dir: &Path) -> Result<Option<KernelBuild>, TaError> {
    let kernel_cfg = &doc.net.kernel;
    // **内置内核**：配置没写路径时，改用**随安装包分发**的 mihomo（如果这一版带上了）。
    //
    // 优先级铁律：**用户显式写的路径永远赢**（上面那条分支）；内置只是"开箱能用"的兜底。
    // 兜底也找不到就**如实说没有**（不猜、不下载、不静默尝试）—— 这条语义与原来一致。
    let configured = kernel_cfg.path.as_deref().map(str::trim).unwrap_or("");
    let program: String = if configured.is_empty() {
        match bundled_kernel_path() {
            Some(p) => {
                tracing::info!(path = %p.display(), "net.kernel.path 为空：改用内置内核（随包分发）");
                p.to_string_lossy().into_owned()
            }
            None => {
                tracing::info!("net.kernel.path 为空且未随包分发内核：不装配内核（只用直连通道）");
                return Ok(None);
            }
        }
    } else {
        configured.to_string()
    };

    let kernel_dir = data_dir.join("kernel");
    let logs_dir = data_dir.join("logs");
    std::fs::create_dir_all(&kernel_dir)
        .map_err(|e| TaError::Internal(format!("创建内核目录失败：{e}")))?;

    // **把随包的 GeoIP 库铺到内核数据目录**（`-d` 目录）。
    // 不铺的话：mihomo 会在启动阶段去**下载** `Country.mmdb`，大陆网络下常常卡住/失败
    // ⇒ 控制器不监听 ⇒ `/version` 超时（`TA-9001 内核就绪超时`）⇒ 应用探测全失败、
    // 真机验证整轮挂住。实测：预置后节点立刻可测 ✓。
    let mmdb_dst = kernel_dir.join("Country.mmdb");
    if !mmdb_dst.is_file() {
        match bundled_country_mmdb() {
            Some(src) => match std::fs::copy(&src, &mmdb_dst) {
                Ok(_) => {
                    tracing::info!("已把随包 GeoIP 库铺到内核目录（内核无需联网下载）")
                }
                // 错误日志**必须带 TA 码**（`crates/ta-core/tests/error_log_guard.rs`
                // 会拦）：这里原来只记了 `error = %e`（io::Error），被守卫测试抓个正着。
                // 包成 `TaError::Internal` 之后既带上码，又把"源文件是哪一条"写进去。
                Err(e) => {
                    let err =
                        TaError::Internal(format!("铺 GeoIP 库失败（源：{}）：{e}", src.display()));
                    tracing::warn!(
                        code = err.code(),
                        error = %err,
                        "铺 GeoIP 库失败：内核启动时可能会尝试联网下载（可能很慢或失败）"
                    );
                }
            },
            None => tracing::info!("未随包分发 GeoIP 库：内核会自行获取（可能需要联网）"),
        }
    }

    // 1) lane 端口：落库复用，否则每次启动都换端口，
    //    客户端/订阅里的端口引用会跟着漂。
    let store = Arc::new(FilePortStore::new(kernel_dir.join("ports.json")));
    let alloc = RangePortAllocator::new(kernel_cfg.lane_port_range, store)?;
    for lane in doc.lanes.iter().filter(|l| l.enabled) {
        alloc
            .allocate(&lane.id)
            .map_err(|e| TaError::Kernel(format!("lane {} 分配端口失败：{e}", lane.id)))?;
    }
    let ports = alloc.map();
    let lane_port_count = ports.map.len();
    // 交一份给网关做拨号表（`port_map` 语义相同，但源对象会被配置源吃掉）
    let port_map = ports.clone();

    // 2) REST secret：同一份同时写进生成的配置与 REST 客户端，两者必须一致。
    let gen = MhConfigGen::random_secret();
    let secret = gen.secret().to_string();

    // 3) 配置源。`apply()` 会**先原子落盘再拉起** —— 冷启动时配置文件还不存在，
    //    所以必须走 apply，直接 start 会让内核拿着一个不存在的 `-f` 路径启动。
    //
    //    ## `strict_refs(false)` 的**已知缺口**（主管评审 A1，已核实）
    //
    //    为什么关它：内核默认自己拉订阅（`net.kernel.subscription_source = kernel`），
    //    首次启动时我们**还没有**组名/节点名清单，strict 会让配置生成直接失败 ——
    //    而那时内核连启动的机会都没有，等于永远起不来。
    //
    //    ⚠️ **但"有两道防线兜底"是错的，这个缺口是真的**（评审逐条拆过）：
    //    1. `strict_refs` 正是"组名/节点名在订阅里真的存在吗"这**唯一一道生成期校验**
    //       （`config_gen.rs:435` 严格报错 vs `:446` 不严格只 `warn!` 放行）；
    //    2. `empty-fallback: REJECT` **只加在我们生成的组上**（`config_gen.rs:538-548`
    //       只遍历有 `group` 的 lane）；而 `subscription_group` / `subscription_node`
    //       两类 lane 的 `listeners[].proxy` **直接指向订阅自带的名字、不建组**
    //       （`config_gen.rs:311` / `:335`）→ REJECT 对它们不生效；
    //    3. `verify_providers()`（ta-kernel）只断言"provider 节点数非空"
    //       （`process.rs:795` 起），**不校验 listener 目标能否解析**；
    //    4. 内核自己也不校验引用。
    //
    //    ⇒ **可达路径**：lane 指向不存在的组/节点 → 生成成功 → provider 有节点 →
    //    内核 `Ready` → 编排 Ready、托盘绿，而那条 lane 每个请求 502。
    //    **登记为已知缺口**，修法见下方 TODO 之前的说明（需要 lane 目标可解析断言）。
    //
    //    TODO(A1)：内核 Ready 后，取 `/proxies` 顶层 ∪ provider 节点，逐条 lane 校验
    //    其目标存在；缺失则**不 Ready / Degraded 且点名 `lane_id`**。
    //    这一步要落在就绪链上（`wait_kernel_ready` 之后），不能只靠生成期预检。
    let gen = gen.with_strict_refs(false);
    let source =
        Arc::new(GeneratedConfigSource::new(Box::new(gen), doc.clone(), ports, kernel_dir.clone()));

    // **生成的配置文件，就是内核要读的那一份**。
    //
    // `net.kernel.config_path` 是"用户自己指定内核配置"时才填的字段，绝大多数用户从没填过 → 空。
    // 而 `KernelProcessConfig::validate()` 要求它非空 ⇒ 以前只要真的走到"拉起内核"，
    // 就会报 `TA-5001 配置校验失败`（内置内核上线后才暴露出来：旧路径在 path 为空时就 return 了）。
    // 这里把 `GeneratedConfigSource` 实际落盘的那个路径接上（`kernel_dir/kernel.generated.yaml`）。
    let mut process_cfg =
        KernelProcessConfig::from_doc(doc, &kernel_dir, &logs_dir).with_secret(&secret);
    if process_cfg.config_path.as_os_str().is_empty() {
        process_cfg.config_path = kernel_dir.join("kernel.generated.yaml");
        tracing::info!(path = %process_cfg.config_path.display(), "内核配置路径未指定：使用工具生成的那一份");
    }
    // **把内置内核的路径写进进程配置**：`from_doc` 是从 `net.kernel.path` 取 `program` 的，
    // 而这条路正是"用户没配置"的场景 ⇒ `program` 为空 ⇒ `validate()` 报 `kernel_path_missing`（TA-5001），
    // 内置内核**永远起不来**（真机日志一条条把原因打出来了）。
    if process_cfg.program.as_os_str().is_empty() {
        process_cfg.program = std::path::PathBuf::from(&program);
        tracing::info!(program = %program, "内核可执行路径未指定：使用内置内核");
    }
    let rest = KernelRest::new(&process_cfg.controller, secret)?;

    let supervisor = Arc::new(KernelSupervisor::with_system_spawner(
        process_cfg,
        Arc::new(rest),
        source.clone(),
    ));

    tracing::info!(
        program = %program,
        controller = %kernel_cfg.controller,
        lane_count = lane_port_count,
        "内核已装配（尚未拉起）"
    );
    Ok(Some(KernelBuild { supervisor, source, port_map }))
}

// ================================================================ 阶段广播

/// 阶段广播的落点。生产 = 事件 + 托盘；测试 = 记录器（这样编排逻辑能离线断言）。
pub trait PhaseSink: Send + Sync {
    /// 阶段变化（`app:boot`）。
    fn phase(&self, phase: BootPhase, detail: &str);
    /// 观察到内核状态（用于把就绪真相带进托盘/看板）。默认忽略。
    fn observe_kernel(&self, _status: &KernelStatus) {}
}

/// 编排阶段 + 最近一次观察到的内核状态（Tauri 托管状态，命令层与托盘都读它）。
///
/// 这是"就绪"的**唯一对外口径**：谁想知道应用能不能用，都读这里，
/// 而不是各自去猜（否则托盘绿、看板灰这种事一定会出现）。
///
/// ⚠️ **已知待办（评审 A2）**：本 holder 是**一次性收敛**的 —— 编排 `Ready` 之后
/// 没有任何东西再更新它，所以内核在那之后崩溃/被杀时，它会继续报 `Ready`
/// （**陈旧绿灯**，是"内核静默降级"的镜像：这次是我们的 UI 静默说谎）。
/// 补看护循环（`supervisor.supervise(&token)` + 一条"由 `Ready` 回落"的反向写回路径，
/// `token` 与退出路径绑定）是已知待办，登记在主管的后续工作包。
/// 在那之前，"唯一对外口径"这句话**只覆盖收敛那一刻**，不是持续保证。
#[derive(Debug)]
pub struct BootPhaseHolder {
    inner: RwLock<HolderInner>,
}

#[derive(Debug, Clone)]
struct HolderInner {
    phase: BootPhase,
    detail: String,
    kernel: Option<KernelStatus>,
}

impl Default for BootPhaseHolder {
    fn default() -> Self {
        Self {
            inner: RwLock::new(HolderInner {
                // 启动即骨架屏：**初始不是 Ready**，避免"还没编排就先报绿"。
                phase: BootPhase::Skeleton,
                detail: "正在启动".to_string(),
                kernel: None,
            }),
        }
    }
}

/// 对外快照。
#[derive(Debug, Clone)]
pub struct BootSnapshot {
    pub phase: BootPhase,
    pub detail: String,
    pub kernel: Option<KernelStatus>,
}

impl BootSnapshot {
    /// 是否已收敛到全绿。
    pub fn is_ready(&self) -> bool {
        matches!(self.phase, BootPhase::Ready)
    }
}

impl BootPhaseHolder {
    pub fn set_phase(&self, phase: BootPhase, detail: &str) {
        let mut g = self.inner.write().unwrap_or_else(|e| e.into_inner());
        g.phase = phase;
        g.detail = detail.to_string();
    }

    pub fn set_kernel(&self, status: &KernelStatus) {
        let mut g = self.inner.write().unwrap_or_else(|e| e.into_inner());
        g.kernel = Some(status.clone());
    }

    pub fn snapshot(&self) -> BootSnapshot {
        let g = self.inner.read().unwrap_or_else(|e| e.into_inner());
        BootSnapshot { phase: g.phase, detail: g.detail.clone(), kernel: g.kernel.clone() }
    }
}

/// 生产用广播：`app:boot` 事件 + 托盘刷新（托盘状态从这里取）。
pub struct TauriPhaseSink<R: Runtime> {
    app: AppHandle<R>,
    holder: Arc<BootPhaseHolder>,
}

impl<R: Runtime> TauriPhaseSink<R> {
    pub fn new(app: AppHandle<R>, holder: Arc<BootPhaseHolder>) -> Self {
        Self { app, holder }
    }
}

impl<R: Runtime> PhaseSink for TauriPhaseSink<R> {
    fn phase(&self, phase: BootPhase, detail: &str) {
        self.holder.set_phase(phase, detail);

        let payload = BootPhasePayload { phase, detail: detail.to_string() };
        if let Err(err) = self.app.emit(EVENT_BOOT, payload) {
            // `tauri::Error` **没有 TA 码**。包成 `TaError::Internal` 再取 `code()`：
            // TA-9001（tokenagency_internal，"其它"）正是这件事的语义。
            //
            // 为什么不用"把码拼进 message"或"靠 EXEMPT 白名单"：
            // 的判据是**能按 TA-xxxx 检索**，而检索要的是 `code` **字段**
            // （拼进 message 的人在肉眼看得到、筛不出来）；映射成 TA-9001 则真的可筛，
            // 也省掉一条"豁免会不会腐烂"的长期负担。
            let e = TaError::Internal(format!("广播启动阶段事件失败：{err}"));
            tracing::warn!(code = e.code(), error = %e, "广播启动阶段事件失败");
        }

        // 托盘图标 / tooltip 跟着阶段走：`refresh` 读的是注入的数据源，
        // 数据源又读这个 holder —— 所以"转绿"的时机由真实就绪判据决定。
        if let Err(err) = tray::refresh(&self.app) {
            let e = TaError::Internal(format!("启动阶段刷新托盘失败：{err}"));
            tracing::debug!(code = e.code(), error = %e, "启动阶段刷新托盘失败");
        }

        // `detail` 是自由文本，按 的裁决**不开字段**（开了就变垃圾桶，
        // 而且与 message 职责重复）—— 写进 message 里。
        if phase.is_terminal() {
            tracing::info!(phase = phase.as_str(), "启动编排已收敛：{detail}");
        } else {
            tracing::info!(phase = phase.as_str(), "启动阶段：{detail}");
        }
    }

    fn observe_kernel(&self, status: &KernelStatus) {
        self.holder.set_kernel(status);
    }
}

// ================================================================ BootDeps 的生产实现

/// 把编排接到真实内核（以及网关，若已装配）上。
///
/// **网关是可选接缝**：没有网关时走 `BootDeps` 的默认实现（返回
/// [`crate::lifecycle::GATEWAY_NOT_WIRED`]），于是"网关还没接线"是可见的降级原因，
/// 而不是一个假的绿灯。装配好之后由 [`KernelBootDeps::with_gateway`] 接上。
pub struct KernelBootDeps {
    kernel: Option<Arc<dyn KernelController>>,
    /// 内核为 `None` 时的可读原因。**刻意区分**"没配内核路径"与"装配失败" ——
    /// 两者的处置完全不同，混成一句话会让用户查错方向。
    missing_reason: String,
    /// 已装配的网关（`None` = 还没接线）
    gateway: Option<Arc<dyn crate::gateway::GatewayStarter>>,
    sink: Arc<dyn PhaseSink>,
    sleeper: Arc<dyn Sleeper>,
    poll: Duration,
}

/// 默认的"没有内核"原因。
pub const NO_KERNEL_CONFIGURED: &str =
    "没有配置内核可执行文件（net.kernel.path 为空）—— 直连通道仍可用";

impl KernelBootDeps {
    pub fn new(kernel: Option<Arc<dyn KernelController>>, sink: Arc<dyn PhaseSink>) -> Self {
        Self {
            kernel,
            missing_reason: NO_KERNEL_CONFIGURED.to_string(),
            gateway: None,
            sink,
            sleeper: Arc::new(TokioSleeper),
            poll: POLL_INTERVAL,
        }
    }

    /// 换睡眠实现（测试用）。
    pub fn with_sleeper(mut self, sleeper: Arc<dyn Sleeper>) -> Self {
        self.sleeper = sleeper;
        self
    }

    /// 换轮询间隔（测试用）。
    pub fn with_poll(mut self, poll: Duration) -> Self {
        self.poll = poll;
        self
    }

    /// 指定"没有内核"的可读原因（装配失败时用它把真实原因带出去）。
    pub fn with_missing_reason(mut self, reason: impl Into<String>) -> Self {
        self.missing_reason = reason.into();
        self
    }

    /// 接上已装配的网关。**不接**就保持"未接线"的默认降级（可见，不假绿）。
    pub fn with_gateway(mut self, gateway: impl crate::gateway::GatewayStarter + 'static) -> Self {
        self.gateway = Some(Arc::new(gateway));
        self
    }
}

/// 内核状态的可读名（进错误文案）。
fn state_label(state: &KernelState) -> &'static str {
    match state {
        KernelState::Stopped => "stopped",
        KernelState::Starting => "starting",
        KernelState::Ready => "ready",
        KernelState::Restarting => "restarting",
        KernelState::Failed => "failed",
    }
}

#[async_trait]
impl BootDeps for KernelBootDeps {
    async fn wait(&self, d: Duration) {
        self.sleeper.sleep(d).await;
    }

    async fn start_kernel(&self) -> Result<(), String> {
        let Some(kernel) = &self.kernel else {
            return Err(self.missing_reason.clone());
        };

        // 用 `apply()` 而不是 `start()`：apply 会先把生成的配置原子落盘再拉起，
        // 冷启动时配置文件还不存在，直接 start 会让内核拿着不存在的 `-f` 路径启动。
        match kernel.apply().await {
            Ok(status) => {
                // 用 `apply()` 自己返回的状态，而不是再查一次：
                // 少一次状态读取，也就少一次"读到的已经是下一个状态"的机会。
                self.sink.observe_kernel(&status);
                Ok(())
            }
            Err(e) => {
                let status = kernel.status().await;
                self.sink.observe_kernel(&status);
                match status.state {
                    // 彻底起不来：如实报错，编排会落到 Degraded。
                    KernelState::Failed | KernelState::Stopped => Err(e.to_string()),
                    // 进程活着、只是 provider 还没加载到节点（慢订阅很常见）：
                    // 不在这一步判死 —— 交给 wait_kernel_ready 在超时窗口内继续等，
                    // 否则"订阅慢"会被误报成"内核起不来"。
                    _ => Ok(()),
                }
            }
        }
    }

    async fn wait_kernel_ready(&self, timeout: Duration) -> Result<(), String> {
        let Some(kernel) = &self.kernel else {
            return Err(self.missing_reason.clone());
        };

        let deadline = Instant::now() + timeout;
        loop {
            let status = kernel.status().await;
            self.sink.observe_kernel(&status);

            match status.state {
                // ★ 只有内核自己给出的 Ready 才算就绪，而那个 Ready 已经带了
                //   provider 判据（见模块文档）。我们绝不在这里另立判据。
                KernelState::Ready => return Ok(()),
                KernelState::Failed => {
                    return Err(status
                        .last_error
                        .unwrap_or_else(|| "内核进入 failed 状态".to_string()))
                }
                KernelState::Stopped => {
                    return Err("内核没有在运行（stopped）".to_string());
                }
                KernelState::Starting | KernelState::Restarting => {}
            }

            if Instant::now() >= deadline {
                let reason = status
                    .last_error
                    .unwrap_or_else(|| format!("当前状态 {}", state_label(&status.state)));
                return Err(format!("{} 秒内没有就绪：{reason}", timeout.as_secs()));
            }
            self.sleeper.sleep(self.poll).await;
        }
    }

    fn on_phase(&self, phase: BootPhase, detail: &str) {
        self.sink.phase(phase, detail);
    }

    /// 真的把网关拉起来（`spawn` 只是提交任务；**就绪由 `wait_gateway_ready` 判**）。
    async fn start_gateway(&self) -> Result<(), String> {
        match &self.gateway {
            Some(gateway) => {
                gateway.spawn();
                Ok(())
            }
            // 没接线：如实暴露缺口（默认实现的那句话），而不是假装已启动
            None => Err(crate::lifecycle::GATEWAY_NOT_WIRED.to_string()),
        }
    }

    /// 就绪判据 = **真的能连上 `gateway.listen`**，不是"调用过 `spawn()`"。
    async fn wait_gateway_ready(&self, timeout: Duration) -> Result<(), String> {
        match &self.gateway {
            Some(gateway) => gateway.wait_listening(timeout).await,
            None => Err(crate::lifecycle::GATEWAY_NOT_WIRED.to_string()),
        }
    }
}

// ================================================================ 托盘数据源（真实就绪驱动）

/// 内核状态的可读中文名（给 tooltip 用，不写 emoji ——）。
fn kernel_label(state: &KernelState) -> &'static str {
    match state {
        KernelState::Stopped => "未运行",
        KernelState::Starting => "启动中",
        KernelState::Ready => "已就绪",
        KernelState::Restarting => "重启中",
        KernelState::Failed => "启动失败",
    }
}

/// 启动阶段的托盘状态映射（纯函数，可以离线断言）。
///
/// **这是"转绿"的唯一定义**：只有编排收敛到 `Ready`（= 内核与网关都真的就绪）
/// 才可能出现 `Normal`（绿）。启动中一律 `Paused`（灰），降级 `Degraded`（黄），
/// 失败 `Error`（红）。绝不在"还没编排完"时先报绿。
///
/// ⚠️ 它映射的是**编排当时的结论**。收敛之后没人复检内核，所以内核后来崩溃时
/// 这里仍会返回绿色（评审 A2 的陈旧绿灯）—— 修法见 [`BootPhaseHolder`] 的注记。
pub fn tray_status_for(phase: BootPhase) -> TrayStatus {
    match phase {
        BootPhase::Ready => TrayStatus::Normal,
        BootPhase::Degraded => TrayStatus::Degraded,
        BootPhase::Failed => TrayStatus::Error,
        BootPhase::Skeleton
        | BootPhase::Delaying
        | BootPhase::StartingKernel
        | BootPhase::WaitingKernel
        | BootPhase::StartingGateway
        | BootPhase::WaitingGateway => TrayStatus::Paused,
    }
}

/// 只用启动编排状态驱动的托盘数据源。
///
/// 为什么现在只有这些字段：策略/线路/通道/额度的真实数据要等服务层（命令面那一步）。
/// 这里**宁可留空**，也不填看起来像真的假数字 —— 空子菜单会被
/// `build_menu_spec` 置灰，而"置灰"是诚实的，"显示一个猜来的额度"不是。
///
/// 但这个数据源已经把这一条做实了：**图标颜色只在编排真就绪时才变绿**。
pub struct PhaseTraySource {
    holder: Arc<BootPhaseHolder>,
}

impl PhaseTraySource {
    pub fn new(holder: Arc<BootPhaseHolder>) -> Self {
        Self { holder }
    }
}

impl TrayDataSource for PhaseTraySource {
    fn snapshot(&self) -> TraySnapshot {
        let s = self.holder.snapshot();
        let mut tooltip_lines = vec![s.detail.clone()];
        if let Some(k) = &s.kernel {
            tooltip_lines.push(format!("内核：{}", kernel_label(&k.state)));
            if let Some(err) = &k.last_error {
                tooltip_lines.push(format!("内核原因：{err}"));
            }
        }

        TraySnapshot {
            status: tray_status_for(s.phase),
            experimental: false,
            gateway_running: matches!(s.phase, BootPhase::Ready),
            tooltip_lines,
            active_policy_id: None,
            policies: Vec::new(),
            lane_group_label: "未配置".to_string(),
            default_lane_id: None,
            lanes: Vec::new(),
            channels: Vec::new(),
            emergency: EmergencyMode::Restore,
        }
    }

    fn dispatch(&self, action: TrayAction) -> ActionOutcome {
        // 动作执行属于服务层（命令面那一步统一提供）。现在**如实**报缺口：
        // 既不假装成功，也不静默吞掉 —— 静默吞掉会让用户以为菜单坏了。
        //
        // 用 `action.label()` 而**不是** `{action:?}`：后者会把 `UsePolicy` 这种
        // Rust 枚举名经 `tray.rs` 的 toast 直接显示给用户（主管评审 B4）。
        ActionOutcome::Failed(format!("「{}」尚未接线", action.label()))
    }
}

// ================================================================ 测试

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Mutex;

    use std::path::PathBuf;

    use ta_core::error::{Result as TaResult, TaError};
    use ta_core::types::LaneId;

    use crate::lifecycle::{BootOrchestrator, RecordingSleeper, StartupArgs};

    // ---------------------------------------------------------- 假内核

    struct FakeInner {
        state: KernelState,
        last_error: Option<String>,
        /// `apply()` 成功后进入的状态
        post_start: KernelState,
        /// `apply()` 失败时把状态放成这个（用 `Starting` 模拟"报错了但进程还活着"）
        state_on_start_error: KernelState,
        /// `status()` 每次被调用时依序推进到的状态（空 = 不再变）
        next_states: Vec<KernelState>,
        start_ok: bool,
        starts: usize,
    }

    impl Default for FakeInner {
        fn default() -> Self {
            Self {
                state: KernelState::Stopped,
                last_error: None,
                post_start: KernelState::Starting,
                state_on_start_error: KernelState::Stopped,
                next_states: Vec::new(),
                start_ok: true,
                starts: 0,
            }
        }
    }

    struct FakeKernel {
        inner: Mutex<FakeInner>,
    }

    impl Default for FakeKernel {
        fn default() -> Self {
            Self { inner: Mutex::new(FakeInner::default()) }
        }
    }

    impl FakeKernel {
        fn new() -> Arc<Self> {
            Arc::new(Self::default())
        }

        fn configure(&self, f: impl FnOnce(&mut FakeInner)) {
            f(&mut self.inner.lock().unwrap());
        }

        fn starts(&self) -> usize {
            self.inner.lock().unwrap().starts
        }

        fn current_state(&self) -> KernelState {
            self.inner.lock().unwrap().state.clone()
        }
    }

    #[async_trait]
    impl KernelController for FakeKernel {
        async fn status(&self) -> KernelStatus {
            let mut g = self.inner.lock().unwrap();
            if !g.next_states.is_empty() {
                g.state = g.next_states.remove(0);
            }
            KernelStatus {
                state: g.state.clone(),
                version: None,
                pid: None,
                restarts: 0,
                last_error: g.last_error.clone(),
            }
        }

        async fn start(&self) -> TaResult<()> {
            self.apply().await.map(|_| ())
        }

        async fn stop(&self) -> TaResult<()> {
            self.inner.lock().unwrap().state = KernelState::Stopped;
            Ok(())
        }

        async fn restart(&self) -> TaResult<()> {
            self.start().await
        }

        async fn apply(&self) -> TaResult<KernelStatus> {
            let mut g = self.inner.lock().unwrap();
            g.starts += 1;
            if g.start_ok {
                g.state = g.post_start.clone();
                let st = KernelStatus {
                    state: g.state.clone(),
                    version: Some("fake".into()),
                    pid: Some(4242),
                    restarts: 0,
                    last_error: None,
                };
                Ok(st)
            } else {
                g.state = g.state_on_start_error.clone();
                Err(TaError::Kernel("模拟：内核起不来".to_string()))
            }
        }

        async fn set_node(&self, _group: &str, _node: &str) -> TaResult<()> {
            Ok(())
        }

        async fn set_select(&self, _lane: &LaneId, _node: &str) -> TaResult<()> {
            Ok(())
        }

        async fn delay_of(&self, _name: &str, _timeout_ms: u64) -> TaResult<u32> {
            Ok(0)
        }

        async fn provider_delays(
            &self,
            _key: &str,
            _probe_url: &str,
            _wait_ms: u64,
        ) -> TaResult<std::collections::BTreeMap<String, Option<u32>>> {
            Ok(std::collections::BTreeMap::new())
        }

        async fn refresh_provider(&self, _key: &str) -> TaResult<()> {
            Ok(())
        }

        async fn snapshot(&self) -> TaResult<ta_kernel::ProvidersSnapshot> {
            Ok(ta_kernel::ProvidersSnapshot {
                proxies: Vec::new(),
                providers: Vec::new(),
                nodes: std::collections::BTreeMap::new(),
                dangling: Vec::new(),
            })
        }

        async fn log_tail(&self, _lines: u32) -> TaResult<String> {
            Ok(String::new())
        }
    }

    // ---------------------------------------------------------- 假 sink

    #[derive(Default)]
    struct RecordingSink {
        phases: Mutex<Vec<(BootPhase, String)>>,
        kernel_states: Mutex<Vec<KernelState>>,
    }

    impl RecordingSink {
        fn new() -> Arc<Self> {
            Arc::new(Self::default())
        }
        fn phases(&self) -> Vec<BootPhase> {
            self.phases.lock().unwrap().iter().map(|(p, _)| *p).collect()
        }
        fn kernel_states(&self) -> Vec<KernelState> {
            self.kernel_states.lock().unwrap().clone()
        }
    }

    impl PhaseSink for RecordingSink {
        fn phase(&self, phase: BootPhase, detail: &str) {
            self.phases.lock().unwrap().push((phase, detail.to_string()));
        }
        fn observe_kernel(&self, status: &KernelStatus) {
            self.kernel_states.lock().unwrap().push(status.state.clone());
        }
    }

    /// 造一个"内核已就绪 + 网关也已就绪"的 deps，用来证明**绿是可达的**，
    /// 而不是因为我没接线才永远不绿。
    struct GatewayOkDeps {
        inner: KernelBootDeps,
        sink: Arc<RecordingSink>,
    }

    #[async_trait]
    impl BootDeps for GatewayOkDeps {
        async fn wait(&self, d: Duration) {
            self.inner.wait(d).await;
        }
        async fn start_kernel(&self) -> Result<(), String> {
            self.inner.start_kernel().await
        }
        async fn wait_kernel_ready(&self, timeout: Duration) -> Result<(), String> {
            self.inner.wait_kernel_ready(timeout).await
        }
        async fn start_gateway(&self) -> Result<(), String> {
            Ok(())
        }
        async fn wait_gateway_ready(&self, _timeout: Duration) -> Result<(), String> {
            Ok(())
        }
        fn on_phase(&self, phase: BootPhase, detail: &str) {
            self.inner.on_phase(phase, detail);
            let _ = &self.sink;
        }
    }

    fn fast_sleeper() -> Arc<dyn Sleeper> {
        Arc::new(RecordingSleeper::new())
    }

    // ---------------------------------------------------------- 装配

    #[test]
    fn build_kernel_returns_none_when_no_path_is_configured() {
        let doc = ConfigDoc::default();
        let dir = std::env::temp_dir().join("ta-boot-no-kernel");
        // 默认配置：net.kernel.path = None → 不装配，且**不是错误**
        assert!(build_kernel(&doc, &dir).unwrap().is_none());
    }

    #[test]
    fn build_kernel_honours_the_configured_controller_and_program() {
        let mut doc = ConfigDoc::default();
        doc.net.kernel.path = Some(r"C:\fake\mihomo.exe".to_string());
        doc.net.kernel.controller = "127.0.0.1:39077".to_string();
        // 不启内核、不 bind 端口，只验证装配结果指向配置里写的东西
        let dir = std::env::temp_dir().join(format!("ta-boot-build-{}", std::process::id()));
        let built = build_kernel(&doc, &dir).expect("装配应成功").expect("有路径就该装配");
        assert_eq!(built.supervisor.config().program, PathBuf::from(r"C:\fake\mihomo.exe"));
        assert_eq!(built.supervisor.config().controller, "127.0.0.1:39077");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---------------------------------------------------------- 就绪判据

    #[tokio::test]
    async fn green_only_after_the_kernel_and_gateway_are_really_ready() {
        let kernel = FakeKernel::new();
        kernel.configure(|f| {
            f.post_start = KernelState::Starting;
            // 头两次轮询仍是 starting，第三次才 Ready
            f.next_states = vec![KernelState::Starting, KernelState::Starting, KernelState::Ready];
        });
        let sink = RecordingSink::new();
        let deps = GatewayOkDeps {
            inner: KernelBootDeps::new(Some(kernel.clone()), sink.clone())
                .with_sleeper(fast_sleeper()),
            sink: sink.clone(),
        };

        let out = BootOrchestrator::run(&deps, &StartupArgs::default(), READY_TIMEOUT).await;

        assert_eq!(out.phase, BootPhase::Ready, "两个依赖都就绪时必须转绿");
        assert_eq!(kernel.starts(), 1, "内核只该被拉起一次");
        // 关键：Ready 只能出现在内核自己报 Ready 之后 —— 也就是 phases 里
        // WaitingKernel 之后才可能有 Ready，且在 Gateway 两阶段之后。
        let phases = sink.phases();
        let ready_at = phases.iter().position(|p| *p == BootPhase::Ready).expect("要有 Ready");
        let waiting_kernel_at =
            phases.iter().position(|p| *p == BootPhase::WaitingKernel).expect("要有 WaitingKernel");
        let waiting_gateway_at = phases
            .iter()
            .position(|p| *p == BootPhase::WaitingGateway)
            .expect("要有 WaitingGateway");
        assert!(ready_at > waiting_kernel_at, "还没等内核就 Ready 了：{phases:?}");
        assert!(ready_at > waiting_gateway_at, "还没等网关就 Ready 了：{phases:?}");
        // sink 确实观察到了内核状态（说明我们把真相带出去了，而不是自说自话）
        assert!(sink.kernel_states().contains(&KernelState::Ready));
    }

    #[tokio::test]
    async fn kernel_that_never_loads_providers_degrades_and_never_turns_green() {
        // 这就是 B2 的场景：进程活着、REST 通，但 provider 没有节点。
        // 期望：永远不到 Ready，错误文案里带出 provider 的原因。
        let kernel = FakeKernel::new();
        kernel.configure(|f| {
            f.post_start = KernelState::Starting;
            f.next_states = Vec::new(); // 一直停在 starting
            f.last_error = Some("provider 没有节点：ta-lane-provider（不报就绪）".to_string());
        });
        let sink = RecordingSink::new();
        let deps = GatewayOkDeps {
            inner: KernelBootDeps::new(Some(kernel.clone()), sink.clone())
                .with_sleeper(fast_sleeper())
                .with_poll(Duration::from_millis(1)),
            sink: sink.clone(),
        };

        let out =
            BootOrchestrator::run(&deps, &StartupArgs::default(), Duration::from_millis(80)).await;

        assert_eq!(out.phase, BootPhase::Degraded);
        assert!(!sink.phases().contains(&BootPhase::Ready), "provider 没节点就不许绿");
        assert!(!sink.phases().contains(&BootPhase::StartingGateway), "内核没就绪就不该往下走");
        let detail = out.detail.expect("要有可读原因");
        assert!(detail.contains("provider 没有节点"), "{detail}");
    }

    #[tokio::test]
    async fn hard_start_failure_degrades_without_waiting() {
        let kernel = FakeKernel::new();
        kernel.configure(|f| {
            f.start_ok = false;
            f.state_on_start_error = KernelState::Stopped; // 彻底没起来
        });
        let sink = RecordingSink::new();
        let deps =
            KernelBootDeps::new(Some(kernel.clone()), sink.clone()).with_sleeper(fast_sleeper());

        let out = BootOrchestrator::run(&deps, &StartupArgs::default(), READY_TIMEOUT).await;

        assert_eq!(out.phase, BootPhase::Degraded);
        assert!(!sink.phases().contains(&BootPhase::WaitingKernel), "起不来就别再等");
        assert!(out.detail.expect("要有原因").contains("模拟：内核起不来"));
    }

    #[tokio::test]
    async fn start_error_with_a_live_process_keeps_waiting_instead_of_giving_up() {
        // 慢订阅：`apply()` 报了错，但进程活着（state=Starting）。
        // 这时**不能**判死 —— 否则"订阅慢"永远起不来。继续等，后面确实就绪了。
        let kernel = FakeKernel::new();
        kernel.configure(|f| {
            f.start_ok = false;
            f.state_on_start_error = KernelState::Starting;
            f.next_states = vec![KernelState::Ready];
        });
        let sink = RecordingSink::new();
        let deps = GatewayOkDeps {
            inner: KernelBootDeps::new(Some(kernel.clone()), sink.clone())
                .with_sleeper(fast_sleeper()),
            sink: sink.clone(),
        };

        let out = BootOrchestrator::run(&deps, &StartupArgs::default(), READY_TIMEOUT).await;

        assert_eq!(out.phase, BootPhase::Ready, "进程活着就该继续等，而不是直接降级");
        assert_eq!(kernel.starts(), 1);
    }

    #[tokio::test]
    async fn failed_state_short_circuits_with_the_kernel_reason() {
        let kernel = FakeKernel::new();
        kernel.configure(|f| {
            f.post_start = KernelState::Failed;
            f.last_error = Some("连续 5 次启动失败".to_string());
        });
        let sink = RecordingSink::new();
        let deps =
            KernelBootDeps::new(Some(kernel.clone()), sink.clone()).with_sleeper(fast_sleeper());

        let out = BootOrchestrator::run(&deps, &StartupArgs::default(), READY_TIMEOUT).await;

        assert_eq!(out.phase, BootPhase::Degraded);
        assert!(out.detail.expect("要有原因").contains("连续 5 次启动失败"));
    }

    #[tokio::test]
    async fn no_kernel_configured_degrades_with_a_readable_reason() {
        let sink = RecordingSink::new();
        let deps = KernelBootDeps::new(None, sink.clone()).with_sleeper(fast_sleeper());

        let out = BootOrchestrator::run(&deps, &StartupArgs::default(), READY_TIMEOUT).await;

        assert_eq!(out.phase, BootPhase::Degraded);
        assert!(!sink.phases().contains(&BootPhase::Ready));
        let detail = out.detail.expect("要有原因");
        assert!(detail.contains("net.kernel.path"), "要点名是哪个配置项：{detail}");
    }

    /// 假网关：`listening=false` 时永远不监听（模拟端口被占 / 绑定失败）。
    #[derive(Clone)]
    struct FakeGateway {
        listening: bool,
        spawned: Arc<std::sync::atomic::AtomicUsize>,
    }

    #[async_trait]
    impl crate::gateway::GatewayStarter for FakeGateway {
        fn spawn(&self) {
            self.spawned.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        async fn wait_listening(&self, timeout: Duration) -> Result<(), String> {
            if self.listening {
                Ok(())
            } else {
                Err(format!("{} 秒内没有在 127.0.0.1:8787 上监听：拒绝连接", timeout.as_secs()))
            }
        }
    }

    /// **正向**：网关真的在监听 → 编排才转绿，且网关确实被启动过一次。
    #[tokio::test]
    async fn green_requires_the_gateway_to_really_listen() {
        let kernel = FakeKernel::new();
        kernel.configure(|f| f.post_start = KernelState::Ready);
        let sink = RecordingSink::new();
        let spawned = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let deps = KernelBootDeps::new(Some(kernel), sink.clone())
            .with_sleeper(fast_sleeper())
            .with_gateway(FakeGateway { listening: true, spawned: spawned.clone() });

        let out = BootOrchestrator::run(&deps, &StartupArgs::default(), READY_TIMEOUT).await;

        assert_eq!(out.phase, BootPhase::Ready, "内核与网关都就绪时必须转绿");
        assert_eq!(spawned.load(std::sync::atomic::Ordering::SeqCst), 1, "网关必须真的被启动过");
        let phases = sink.phases();
        let ready_at = phases.iter().position(|p| *p == BootPhase::Ready).expect("要有 Ready");
        let gw_at = phases
            .iter()
            .position(|p| *p == BootPhase::WaitingGateway)
            .expect("要有 WaitingGateway");
        assert!(ready_at > gw_at, "还没等网关监听就绿了：{phases:?}");
    }

    /// **反向**：网关没在监听（端口被占 / 绑定失败）→ 绝不转绿。
    #[tokio::test]
    async fn gateway_not_listening_never_turns_green() {
        let kernel = FakeKernel::new();
        kernel.configure(|f| f.post_start = KernelState::Ready);
        let sink = RecordingSink::new();
        let deps = KernelBootDeps::new(Some(kernel), sink.clone())
            .with_sleeper(fast_sleeper())
            .with_gateway(FakeGateway {
                listening: false,
                spawned: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            });

        let out = BootOrchestrator::run(&deps, &StartupArgs::default(), READY_TIMEOUT).await;

        assert_eq!(out.phase, BootPhase::Degraded);
        assert!(!sink.phases().contains(&BootPhase::Ready), "网关没在监听就不许绿");
        assert!(out.detail.expect("要有原因").contains("8787"));
    }

    #[tokio::test]
    async fn gateway_gap_is_reported_instead_of_faking_green() {
        // 生产装配现在的真实状态：内核能就绪，但网关装配入口还没接线（等 E5）。
        // 期望：Degraded + 明说没在监听，而不是绿。
        let kernel = FakeKernel::new();
        kernel.configure(|f| f.post_start = KernelState::Ready);
        let sink = RecordingSink::new();
        let deps = KernelBootDeps::new(Some(kernel), sink.clone()).with_sleeper(fast_sleeper());

        let out = BootOrchestrator::run(&deps, &StartupArgs::default(), READY_TIMEOUT).await;

        assert_eq!(out.phase, BootPhase::Degraded);
        assert!(!sink.phases().contains(&BootPhase::Ready));
        let detail = out.detail.expect("要有原因");
        assert!(detail.contains("8787"), "要说清网关没在监听：{detail}");
    }

    #[tokio::test]
    async fn phase_sink_observes_every_kernel_state_it_sees() {
        let kernel = FakeKernel::new();
        kernel.configure(|f| {
            f.post_start = KernelState::Starting;
            f.next_states = vec![KernelState::Ready];
        });
        let sink = RecordingSink::new();
        let deps = GatewayOkDeps {
            inner: KernelBootDeps::new(Some(kernel.clone()), sink.clone())
                .with_sleeper(fast_sleeper()),
            sink: sink.clone(),
        };
        BootOrchestrator::run(&deps, &StartupArgs::default(), READY_TIMEOUT).await;

        let observed = sink.kernel_states();
        assert!(observed.contains(&KernelState::Starting), "{observed:?}");
        assert!(observed.contains(&KernelState::Ready), "{observed:?}");
        assert_eq!(kernel.current_state(), KernelState::Ready);
    }

    // ---------------------------------------------------------- 托盘（转绿）

    #[test]
    fn tray_is_grey_while_booting_and_only_green_on_ready() {
        for phase in [
            BootPhase::Skeleton,
            BootPhase::Delaying,
            BootPhase::StartingKernel,
            BootPhase::WaitingKernel,
            BootPhase::StartingGateway,
            BootPhase::WaitingGateway,
        ] {
            assert_eq!(tray_status_for(phase), TrayStatus::Paused, "{phase:?} 不该是绿");
        }
        assert_eq!(tray_status_for(BootPhase::Ready), TrayStatus::Normal);
        assert_eq!(tray_status_for(BootPhase::Degraded), TrayStatus::Degraded);
        assert_eq!(tray_status_for(BootPhase::Failed), TrayStatus::Error);
    }

    #[test]
    fn tray_source_needs_a_ready_outcome_to_show_green() {
        let holder = Arc::new(BootPhaseHolder::default());
        let src = PhaseTraySource::new(holder.clone());

        // 编排还没结束：灰，而且不是"网关运行中"
        assert_eq!(src.snapshot().status, TrayStatus::Paused);
        assert!(!src.snapshot().gateway_running);

        holder.set_phase(BootPhase::WaitingKernel, "等待内核就绪");
        assert_eq!(src.snapshot().status, TrayStatus::Paused);
        assert!(!src.snapshot().gateway_running, "还没收敛就不许报网关在跑");

        // 内核 provider 没加载到节点 → 编排降级 → 黄，且原因在 tooltip 里看得到
        holder.set_kernel(&KernelStatus {
            state: KernelState::Starting,
            last_error: Some("provider 没有节点".to_string()),
            ..Default::default()
        });
        holder.set_phase(BootPhase::Degraded, "内核在 60 秒内没有就绪：provider 没有节点");
        let s = src.snapshot();
        assert_eq!(s.status, TrayStatus::Degraded);
        assert!(
            s.tooltip_lines.iter().any(|l| l.contains("provider 没有节点")),
            "原因要能在 tooltip 里看到：{:?}",
            s.tooltip_lines
        );

        // 真就绪才绿
        holder.set_phase(BootPhase::Ready, "内核与网关均已就绪");
        assert_eq!(src.snapshot().status, TrayStatus::Normal);
        assert!(src.snapshot().gateway_running);
    }

    #[test]
    fn tray_dispatch_is_honest_about_the_gap() {
        let holder = Arc::new(BootPhaseHolder::default());
        let src = PhaseTraySource::new(holder);
        match src.dispatch(TrayAction::ToggleGateway) {
            ActionOutcome::Failed(msg) => assert!(msg.contains("尚未接线"), "{msg}"),
            other => panic!("不许假装成功，也不许静默吞掉：{other:?}"),
        }
    }

    // ---------------------------------------------------------- holder

    #[test]
    fn holder_starts_at_skeleton_and_is_not_ready_until_set() {
        let holder = BootPhaseHolder::default();
        let s = holder.snapshot();
        assert_eq!(s.phase, BootPhase::Skeleton);
        assert!(!s.is_ready(), "初始绝不能是 Ready");

        holder.set_kernel(&KernelStatus::default());
        holder.set_phase(BootPhase::Ready, "内核与网关均已就绪");
        let s = holder.snapshot();
        assert!(s.is_ready());
        assert_eq!(s.detail, "内核与网关均已就绪");
        assert!(s.kernel.is_some());
    }
}
