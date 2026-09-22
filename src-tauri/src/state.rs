//! 应用运行时状态（Tauri 的 `State<AppState>`）。
//!
//! 设计约束（`docs/10-interface-spec.md` §2.1、`docs/12-work-breakdown.md` §5 T1）：
//! - **命令层只做转发**，所有依赖从 `AppState` 取，不各自持有全局变量；
//! - 配置的真源是 SQLite（`docs/02` §4），内存里只放一份**热读快照**（`ArcSwap`），
//!   避免每个命令都去查库；写路径由 `SqliteConfigStore` 负责事务与快照。
//!
//! 谁会往这里注入东西：
//! - 网关依赖（`ta_gateway::GatewayDeps`）：G2/G4 的启动编排；
//! - 内核控制器（`ta_kernel::KernelController`）：T2 的生命周期编排。
//!
//! T1a 阶段这两个槽位允许是 `None`（命令会如实报告"未就绪"，而不是假装正常）。

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use arc_swap::ArcSwap;
use sqlx::SqlitePool;

use ta_core::clock::{Clock, SystemClock};
use ta_core::domain::ConfigDoc;
use ta_core::error::{Result, TaError};
use ta_store::{
    ConfigStore, SqliteConfigStore, SqliteHealthStore, SqliteQuotaBackend, SqliteSecretMetaStore,
    SqliteStickyStore, SqliteTraceStore, SqliteUsageStore,
};

/// 数据根覆盖用的环境变量名（见 [`AppState::app_data_root`]）。
pub const DATA_DIR_ENV: &str = "TA_DATA_DIR";

/// [`AppState::app_data_root`] 的**纯函数内核**：不读环境、不碰进程状态，只做决策。
///
/// 抽出来的理由和 `TimeRange` 那类守卫一样：环境变量会让测试互相干扰
/// （`set_var` 是进程级共享状态），而纯函数可以把"覆盖生效 / 空值不算 / 没有系统目录时报错"
/// 三种情形逐条钉死。
fn resolve_data_root(
    env_override: Option<std::ffi::OsString>,
    system: Option<PathBuf>,
) -> Result<PathBuf> {
    if let Some(v) = env_override {
        if !v.is_empty() {
            return Ok(PathBuf::from(v));
        }
    }
    system.ok_or_else(|| TaError::Store("找不到系统数据目录（dirs::data_dir）".into()))
}

pub struct AppState {
    pub db: SqlitePool,
    /// 库文件路径（`app_open_dir("data")` 与设置页要显示）
    pub db_path: PathBuf,
    pub started_at_ms: i64,
    pub clock: Arc<dyn Clock>,

    /// 配置热读快照（写路径永远走 `config`）
    pub doc: Arc<ArcSwap<ConfigDoc>>,
    pub config: Arc<SqliteConfigStore>,

    pub usage: Arc<SqliteUsageStore>,
    pub trace: Arc<SqliteTraceStore>,
    pub sticky: Arc<SqliteStickyStore>,
    pub health: Arc<SqliteHealthStore>,
    pub secret_meta: Arc<SqliteSecretMetaStore>,
    /// 额度账本的持久化后端（同步 trait，见 `ta-store::quota_backend`）
    pub quota: Arc<SqliteQuotaBackend>,

    /// 网关依赖，由 G2/G4 注入
    pub gateway: RwLock<Option<Arc<ta_gateway::GatewayDeps>>>,
    /// 内核控制器，由 T2 注入
    pub kernel: RwLock<Option<Arc<dyn ta_kernel::KernelController>>>,
    /// 生成内核配置的真源（`kernel_apply` / 配置变更后要 `set_doc`/`set_ports`）。
    ///
    /// 它同时也是 `KernelController::apply()` 的输入 —— 两份必须指向**同一个**
    /// `GeneratedConfigSource`，否则"命令层改了配置、内核却按旧配置 reload"。
    pub kernel_source: RwLock<Option<Arc<ta_kernel::config_gen::GeneratedConfigSource>>>,
    /// 内核 supervisor 的**具体类型**。
    ///
    /// 为什么除了 `kernel`（`Arc<dyn KernelController>`）还要存一份：网关的
    /// `KernelProbe` 是**同步**接口（`/healthz` 每来一次请求就查一次），
    /// 而 `KernelController::status()` 是异步的。存具体类型才能直接调
    /// `KernelSupervisor::state()`，不必引入缓存 —— **缓存会让"/healthz 说 ready、
    /// 内核其实已经死了"成为可能**。
    pub kernel_supervisor: RwLock<Option<Arc<ta_kernel::process::KernelSupervisor>>>,
    /// 网关准入闸门（`app_pause_gateway` / `app_resume_gateway`）。
    ///
    /// 与 `gateway` 槽位分开：那个是"装配过了没有"，这个是"现在接不接单"。
    /// **暂停不影响 `/healthz`**（E5 接缝语义第 4 条），所以两者不能互相替代 ——
    /// 把"暂停"读成"失联"会让托盘显示成故障。
    pub gateway_gate: RwLock<Option<Arc<ta_gateway::ingress::GatewayGate>>>,
}

impl AppState {
    /// 打开库、跑迁移、载入配置快照，并把各个 Store 组装好。
    pub async fn bootstrap(db_path: impl AsRef<Path>) -> Result<Self> {
        let db_path = db_path.as_ref().to_path_buf();
        let db = ta_store::open(&db_path).await?;
        let clock: Arc<dyn Clock> = Arc::new(SystemClock);
        let started_at_ms = clock.now_ms();

        let config = Arc::new(SqliteConfigStore::with_clock(db.clone(), Arc::clone(&clock)));
        let doc = Arc::new(ArcSwap::from_pointee(config.load().await?));

        // 额度账本后端要在运行时里构造（它 spawn 一个同 runtime 的后台落库任务）
        let quota = Arc::new(SqliteQuotaBackend::open(db.clone()).await?);

        Ok(Self {
            doc,
            config,
            usage: Arc::new(SqliteUsageStore::with_clock(db.clone(), Arc::clone(&clock))),
            trace: Arc::new(SqliteTraceStore::with_clock(db.clone(), Arc::clone(&clock))),
            sticky: Arc::new(SqliteStickyStore::new(db.clone())),
            health: Arc::new(SqliteHealthStore::new(db.clone())),
            secret_meta: Arc::new(SqliteSecretMetaStore::new(db.clone())),
            quota,
            db,
            db_path,
            started_at_ms,
            clock,
            gateway: RwLock::new(None),
            kernel: RwLock::new(None),
            kernel_source: RwLock::new(None),
            kernel_supervisor: RwLock::new(None),
            gateway_gate: RwLock::new(None),
        })
    }

    /// 默认库位置：`<数据根>/db.sqlite`（`docs/02` §4）。
    pub fn default_db_path() -> Result<PathBuf> {
        Ok(Self::app_data_root()?.join("db.sqlite"))
    }

    /// 数据根的**唯一解析口**：`TA_DATA_DIR` 覆盖 > `<AppData>/TokenAgency`（`docs/02` §4）。
    ///
    /// **为什么需要覆盖口**：端到端验证要用 `tauri-driver` 驱动**真的界面**（真 IPC、真库），
    /// 必须能在完全不碰用户真实数据的前提下跑完整链路。而 Windows 上 `dirs::data_dir()`
    /// 读的是系统已知文件夹，改 `%APPDATA%` 环境变量对它无效 —— 覆盖口只能由我们自己的代码读。
    ///
    /// 规则：环境变量**存在且非空**才生效（`TA_DATA_DIR=` 空值不算，否则会静默把数据落到当前目录）。
    /// 日志目录（`logging::log_dir`）与内核/订阅目录都从这一个根派生，见 `logging.rs` 的说明。
    pub fn app_data_root() -> Result<PathBuf> {
        resolve_data_root(
            std::env::var_os(DATA_DIR_ENV),
            dirs::data_dir().map(|d| d.join("TokenAgency")),
        )
    }

    /// 应用数据目录（`app_open_dir` 用）。
    pub fn data_dir(&self) -> PathBuf {
        self.db_path.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| PathBuf::from("."))
    }

    pub fn doc(&self) -> Arc<ConfigDoc> {
        self.doc.load_full()
    }

    /// 从库里重读配置并换掉热读快照（`config_apply` / `config_rollback` 之后调用）。
    pub async fn reload_doc(&self) -> Result<ConfigDoc> {
        let doc = self.config.load().await?;
        self.doc.store(Arc::new(doc.clone()));
        Ok(doc)
    }

    pub fn set_doc(&self, doc: ConfigDoc) {
        self.doc.store(Arc::new(doc));
    }

    pub fn uptime_s(&self) -> u64 {
        let now = self.clock.now_ms();
        ((now - self.started_at_ms).max(0) / 1000) as u64
    }

    pub fn set_gateway(&self, deps: Arc<ta_gateway::GatewayDeps>) {
        *self.gateway.write().unwrap_or_else(|e| e.into_inner()) = Some(deps);
    }

    pub fn set_kernel(&self, kernel: Arc<dyn ta_kernel::KernelController>) {
        *self.kernel.write().unwrap_or_else(|e| e.into_inner()) = Some(kernel);
    }

    pub fn kernel_slot(&self) -> Option<Arc<dyn ta_kernel::KernelController>> {
        self.kernel.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn set_kernel_source(&self, source: Arc<ta_kernel::config_gen::GeneratedConfigSource>) {
        *self.kernel_source.write().unwrap_or_else(|e| e.into_inner()) = Some(source);
    }

    pub fn kernel_source_slot(&self) -> Option<Arc<ta_kernel::config_gen::GeneratedConfigSource>> {
        self.kernel_source.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn gateway_ready(&self) -> bool {
        self.gateway.read().unwrap_or_else(|e| e.into_inner()).is_some()
    }

    pub fn set_gateway_gate(&self, gate: Arc<ta_gateway::ingress::GatewayGate>) {
        *self.gateway_gate.write().unwrap_or_else(|e| e.into_inner()) = Some(gate);
    }

    pub fn gateway_gate(&self) -> Option<Arc<ta_gateway::ingress::GatewayGate>> {
        self.gateway_gate.read().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 覆盖口不生效就等于端到端验证会往用户真实数据目录里写 —— 这条必须有牙。
    #[test]
    fn data_root_override_wins() {
        let got = resolve_data_root(
            Some(std::ffi::OsString::from(r"C:\tmp\ta-e2e")),
            Some(PathBuf::from(r"C:\Users\x\AppData\Roaming\TokenAgency")),
        )
        .expect("有覆盖值时不该报错");
        assert_eq!(got, PathBuf::from(r"C:\tmp\ta-e2e"));
    }

    /// `TA_DATA_DIR=`（空值）**不算设置**：否则数据会静默落到当前目录。
    #[test]
    fn empty_override_is_ignored() {
        let got = resolve_data_root(
            Some(std::ffi::OsString::from("")),
            Some(PathBuf::from(r"C:\Users\x\AppData\Roaming\TokenAgency")),
        )
        .expect("空值应回退到系统目录");
        assert_eq!(got, PathBuf::from(r"C:\Users\x\AppData\Roaming\TokenAgency"));
    }

    /// 没设环境变量时走系统目录（并断言拼上了 `TokenAgency`）。
    #[test]
    fn no_override_uses_system_dir() {
        let got =
            resolve_data_root(None, Some(PathBuf::from(r"C:\Users\x\AppData\Roaming\TokenAgency")))
                .expect("有系统目录时不该报错");
        assert_eq!(got, PathBuf::from(r"C:\Users\x\AppData\Roaming\TokenAgency"));
    }

    /// 两个来源都没有 → 报错，而不是悄悄用相对路径 `"TokenAgency"`。
    #[test]
    fn no_source_at_all_is_an_error() {
        let err = resolve_data_root(None, None).expect_err("两个来源都没有时必须报错");
        assert!(err.to_string().contains("数据目录"), "错误信息要说清是数据目录：{err}");
    }

    /// 库与日志目录必须同根：改了根的解析口，日志不能还走老路。
    #[test]
    fn db_path_and_log_dir_share_the_same_root() {
        let root = resolve_data_root(Some(std::ffi::OsString::from(r"C:\tmp\ta-e2e")), None)
            .expect("有覆盖值时不该报错");
        assert_eq!(root.join("db.sqlite"), PathBuf::from(r"C:\tmp\ta-e2e").join("db.sqlite"));
        assert_eq!(root.join("logs"), PathBuf::from(r"C:\tmp\ta-e2e").join("logs"));
    }
}
