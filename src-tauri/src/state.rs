//! 应用运行时状态（Tauri 的 `State<AppState>`）。
//!
//! 设计约束（T1）：
//! - **命令层只做转发**，所有依赖从 `AppState` 取，不各自持有全局变量；
//! - 配置的真源是 SQLite，内存里只放一份**热读快照**（`ArcSwap`），
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

use crate::import_preview::ImportPreviewCache;
use ta_store::{
    ConfigStore, SqliteApiKeyStore, SqliteConfigStore, SqliteHealthStore, SqliteModelCatalogStore,
    SqliteSecretMetaStore, SqliteStickyStore, SqliteTraceStore, SqliteUsageStore,
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
    /// 我们网关对外的访问密钥。
    ///
    /// 与 `secret_meta` 是两件事：那个记的是**上游凭据**的元数据，
    /// 这个是我们**发出去**的 key（只存 sha256）。网关的多 key 鉴权也用它
    /// （`crate::apikey_auth`），所以必须挂在 `AppState` 上而不是每个命令各造一份。
    pub api_keys: Arc<SqliteApiKeyStore>,
    /// 服务商**模型目录**：扫描记录下来的上游模型（观察数据，落 SQLite）。
    ///
    /// 与 `import_previews` 的区别正是这一轮要修的事：预览是"这次扫描的结果"（内存、5 分钟），
    /// 目录是"这家扫描到过什么"（**活得比进程久**）。用户在服务商里勾一个没勾过的模型时，
    /// 靠的就是它 —— 而不是让他再扫一遍。
    ///
    /// 不进配置真源：目录会随上游漂移，写进配置就是第二份真相源
    /// （与 `api_key` / 健康样本同类，见）。
    pub model_catalog: Arc<SqliteModelCatalogStore>,

    /// `model_import_from_provider` 扫描结果的**进程内存**预览缓存。
    ///
    /// 为什么挂在 `AppState` 上而不是每个命令各造一份：`preview_token` 是在
    /// `model_import_from_provider` 里发的、在 `model_apply_import` 里取的 ——
    /// 两份各自持有就永远取不到（`model_apply_import` 会一直报"预览已过期"）。
    /// **不落库**：预览讲的是"上游现在有什么"，落库就是第二个真相源（见 `import_preview` 的模块文档）。
    pub import_previews: Arc<ImportPreviewCache>,

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

    /// **可热更新的 lane → 内核 listener 端口表**（与拨号器读的**同一个**对象）。
    ///
    /// 为什么必须有这个槽位：端口是运行期才分配的，而拨号器原来拿的是 boot 那一刻的冻结副本
    /// —— 于是 boot 之后新建的 lane（「自动」/ 订阅节点）在拨号器里查不到端口 →
    /// `TA-3002`，真机表现就是"只有直连能出网"。每个写配置的命令都要在这里
    /// `replace` 一次，端口才跟得上配置。
    pub lane_ports: RwLock<Option<Arc<ta_gateway::dispatch::SharedPorts>>>,
}

impl AppState {
    /// 打开库、跑迁移、载入配置快照，并把各个 Store 组装好。
    pub async fn bootstrap(db_path: impl AsRef<Path>) -> Result<Self> {
        let db_path = db_path.as_ref().to_path_buf();
        let db = ta_store::open(&db_path).await?;
        let clock: Arc<dyn Clock> = Arc::new(SystemClock);
        let started_at_ms = clock.now_ms();

        let config = Arc::new(SqliteConfigStore::with_clock(db.clone(), Arc::clone(&clock)));
        // 关系翻转的**配置迁移**：老库的 `channels[].model` 要在载入时搬进
        // `models[].lines`，否则翻转后的读路径（按 `lines` 找线路）在老库上一条线路都找不到。
        //
        // 迁移发生在**启动时**、而不是每个读路径里：`migrate()` 之后立刻落库（版本号也被写下去），
        // 下一次启动就是恒等迁移。`config_rollback` 可能把老文档放回来 —— 迁移是幂等的，
        // 那样也只是再搬一次。
        let loaded = config.load().await?;
        let doc = if ta_core::config::migrate::needs_migration(&loaded) {
            let migrated = ta_core::config::migrate::migrate(loaded);
            tracing::info!(
                "配置已从旧版本迁移到 v{}（[docs/28 §2]：线路归属搬到 Model.lines）",
                migrated.version
            );
            config.save(&migrated).await?;
            migrated
        } else {
            loaded
        };
        let doc = Arc::new(ArcSwap::from_pointee(doc));

        Ok(Self {
            doc,
            config,
            usage: Arc::new(SqliteUsageStore::with_clock(db.clone(), Arc::clone(&clock))),
            trace: Arc::new(SqliteTraceStore::with_clock(db.clone(), Arc::clone(&clock))),
            sticky: Arc::new(SqliteStickyStore::new(db.clone())),
            health: Arc::new(SqliteHealthStore::new(db.clone())),
            secret_meta: Arc::new(SqliteSecretMetaStore::new(db.clone())),
            api_keys: Arc::new(SqliteApiKeyStore::new(db.clone())),
            model_catalog: Arc::new(SqliteModelCatalogStore::new(db.clone())),
            import_previews: Arc::new(ImportPreviewCache::new()),
            db,
            db_path,
            started_at_ms,
            clock,
            gateway: RwLock::new(None),
            kernel: RwLock::new(None),
            kernel_source: RwLock::new(None),
            kernel_supervisor: RwLock::new(None),
            gateway_gate: RwLock::new(None),
            lane_ports: RwLock::new(None),
        })
    }

    /// 默认库位置：`<数据根>/db.sqlite`。
    pub fn default_db_path() -> Result<PathBuf> {
        Ok(Self::app_data_root()?.join("db.sqlite"))
    }

    /// 数据根的**唯一解析口**：`TA_DATA_DIR` 覆盖 > `<AppData>/TokenAgency`。
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
        // **内核的"配置真源"必须跟着变**。它在 `boot.rs` 里只 `clone` 了一份开机的 doc
        // （那时还没有订阅、没有自动线路）；不更新的话，内核永远按那份旧配置生成 ——
        // 实测表现就是 `proxy-groups: []`、「自动」出口没有可 `set_select` 的目标。
        // 它的声明注释本来就写着"配置变更后要 `set_doc`/`set_ports`"，这里是把那句话接上线。
        if let Some(src) = self.kernel_source.read().unwrap_or_else(|e| e.into_inner()).clone() {
            src.set_doc(doc.clone());
        }
        self.doc.store(Arc::new(doc));
    }

    pub fn uptime_s(&self) -> u64 {
        let now = self.clock.now_ms();
        ((now - self.started_at_ms).max(0) / 1000) as u64
    }

    pub fn set_gateway(&self, deps: Arc<ta_gateway::GatewayDeps>) {
        *self.gateway.write().unwrap_or_else(|e| e.into_inner()) = Some(deps);
    }

    /// 取网关依赖 —— **唯一出站通道**（`GatewayDeps.dialer`）就在这里。
    ///
    /// `None` = 启动编排还没装配网关。调用方（`commands::upstream`）必须据此**如实报错**
    /// （`TA-3002`），绝不退回"自己发一个请求"或者"假装探测成功"。
    pub fn gateway_deps(&self) -> Option<Arc<ta_gateway::GatewayDeps>> {
        self.gateway.read().unwrap_or_else(|e| e.into_inner()).clone()
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

    /// 装配时把**可热更新**的端口表句柄存进来（与拨号器读的是同一个对象）。
    pub fn set_lane_ports(&self, ports: Arc<ta_gateway::dispatch::SharedPorts>) {
        *self.lane_ports.write().unwrap_or_else(|e| e.into_inner()) = Some(ports);
    }

    /// 取端口表句柄。`None` = 网关还没装配（那时也没有拨号器要更新）。
    pub fn lane_ports(&self) -> Option<Arc<ta_gateway::dispatch::SharedPorts>> {
        self.lane_ports.read().unwrap_or_else(|e| e.into_inner()).clone()
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
