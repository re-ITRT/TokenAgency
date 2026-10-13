//! `settings_ext` —— 系统集成 / 诊断导出 / 首启向导 / 托盘控制这批命令的实现处。
//!
//! 契约：`docs/10-interface-spec.md` §2.2（app / config / log / onboarding / secrets / tray）。
//! 本文件是**新建的**：既有 `app.rs` / `config.rs` / `log.rs` / `catalog.rs` / `usage.rs`
//! 属于别的写者，这里不碰它们的行。
//!
//! # 三条铁律在本文件里怎么落地
//!
//! | 铁律 | 落地方式 |
//! |---|---|
//! | **诊断包里搜不到密钥** | [`log_export_to`] 只走 `ta_core::diag::package::build_package`（组包 + 打包后解压再扫）；本文件**没有任何**自己的脱敏/扫描实现 |
//! | **actual（系统事实）≠ desired（配置期望）** | [`AutostartState`] 同时带出两者与 `drifted`/`command_matches`；[`set_autostart`] 先写系统、**回读一致**才写配置，写配置失败会把系统回滚 |
//! | **I2 零系统网络副作用** | 本文件不碰代理 / DNS / 路由 / 网卡 / TUN。唯一会写 OS 持久状态的是 `app_*autostart`（注册表 / LaunchAgent / XDG，且只碰自启动那一项） |
//!
//! # 写命令的"读回来一致"
//!
//! 每个写命令都走 [`persist_config`]（校验 → 事务写库 → 快照 → 换内存快照），
//! 然后调 [`read_back`] **从库里真读**一遍并断言与请求一致。**内存快照不算证据** ——
//! `set_doc` 成功而库里没写进去，是这套结构最容易出现、又最难发现的一种假成功。
//!
//! # 已知缺口（不藏着，详见各自函数的文档）
//!
//! 1. `app_check_update` —— 更新通道（`tauri-plugin-updater` + 更新清单地址）**没有接入**，
//!    所以它**如实报错**而不是回一个 `null`（`null` 会被读成"已是最新"）。
//! 2. `config_set_file_watch_mode` —— 只翻转配置开关（`restart_required: true`）。
//!    **文件监听本身没有实现**（`notify` 依赖没进本 crate，也没有 watcher 服务）。
//! 3. `onboarding_test_all` —— 线路维度是**真测**（内核 `/proxies/{target}/delay`），
//!    凭据维度只做"密钥引用能否解析"。**不向上游发请求**，所以不编造 HTTP 状态码；
//!    每行 `note` 都写明这个口径。
//! 4. `tray_emergency` 的 `vision_only` —— 路由策略引擎没有"显式拒绝"语义
//!    （`NoCandidateAction` 只有 `error` / `use_any`），如实报不支持。
//! 5. 托盘**菜单**里的动作仍然走 `boot.rs::PhaseTraySource::dispatch`（那个文件不在本包范围内），
//!    本文件提供的是前端 IPC 路径与可复用的服务函数。
//! 6. **发现的一处 ta-core 不对称（不是本文件能修的）**：`diag::sanitize::sanitize` 刻意保留
//!    `Authorization: Bearer` 里的方案词，而 `diag::sanitize::find_secret`（打包后扫描用的那把尺）
//!    会**无条件**把认证头形态算命中。后果：**已经脱敏过的**认证头行仍会让整个诊断包导出被拒。
//!    修法在 `crates/ta-core/src/diag/sanitize.rs` 的 `find_secret_with`（对 `header` 规则
//!    套用 `is_auth_scheme_word` 过滤，与 `sanitize_with` 对称）；本文件按"不许重写脱敏逻辑"
//!    的约束**不自己绕过**，只在测试里把这个事实钉住。

use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, RwLock};

use serde::{Deserialize, Serialize};
use tauri::State;

use ta_core::diag::package::{self as diag_package, PackageError, PackageFile};
use ta_core::diag::query::{self as diag_query, CodeCount};
use ta_core::diag::sanitize;
use ta_core::domain::{AutostartMode, ConfigDoc, Credential, LaneKind, Policy, Provider};
use ta_core::error::{FieldError, TaError};
use ta_core::types::{LaneId, Protocol, ProviderId, SubscriptionId};
use ta_store::{
    ConfigStore, HealthSample, HealthStore, LogQuery, SecretMetaStore, TraceRow, TraceStore,
};

use ta_secrets::{mask_secret, EnvStore, SecretStore};

use crate::autostart::{
    AutostartController, AutostartDesired, AutostartState as CoreAutostartState,
};
use crate::commands::config::ApplyResult;
use crate::commands::{IpcError, IpcResult};
use crate::state::AppState;
use crate::tray::{EmergencyMode, TrayStatus};

// ================================================================ 共用小工具

/// 配置写路径的**唯一**入口：校验（只看本次动到的节）→ 事务写库 → 留快照 → 换内存快照。
///
/// 为什么只过滤本次动到的节：用户库里可能存着一份**别处**有校验错误的配置
/// （例如某个页面的写命令还有 bug）。那种情况下，"改自启动"不该被别的问题连坐 ——
/// 但也**不能假装整份配置合法**，所以过滤掉的错误数会以 warn 记下来。
async fn persist_config(
    state: &AppState,
    doc: &ConfigDoc,
    sections: &[&str],
    label: &str,
) -> IpcResult<()> {
    let report = ta_core::config::validate(doc);
    let mine: Vec<FieldError> = report
        .errors
        .iter()
        .filter(|e| sections.iter().any(|s| e.path.starts_with(s)))
        .cloned()
        .collect();
    if !mine.is_empty() {
        return Err(IpcError::from_field_errors(&mine));
    }
    let others = report.errors.len();
    if others > 0 {
        tracing::warn!(
            count = others,
            sections = ?sections,
            "配置里还有与本次改动无关的校验错误：本次只保证改动的那几节"
        );
    }
    state.config.save(doc).await?;
    state.config.snapshot(doc, label).await?;
    state.set_doc(doc.clone());
    Ok(())
}

/// **从库里真读**配置并换掉内存快照 —— 写命令的"读回来一致"必须对着它断言。
///
/// 用 `config.load()` 而不是 `state.doc()`：后者是我们自己刚塞进内存的那份，
/// 拿它当证据等于自己证明自己（写库失败时会静默通过）。
async fn read_back(state: &AppState) -> IpcResult<ConfigDoc> {
    let doc = state.config.load().await?;
    state.set_doc(doc.clone());
    Ok(doc)
}

/// 生产用的密钥优先级链：钥匙串 → 便携文件 → 环境变量（与 `gateway.rs` 的降级策略一致）。
///
/// `OnceLock` 而不是每次新建：`PortableStore::open` 会碰磁盘，每个命令都建一遍很浪费。
fn secret_store() -> Arc<dyn SecretStore> {
    static STORE: OnceLock<Arc<dyn SecretStore>> = OnceLock::new();
    STORE
        .get_or_init(|| match ta_secrets::CompositeStore::with_defaults() {
            Ok(store) => Arc::new(store) as Arc<dyn SecretStore>,
            Err(e) => {
                // 降级而不是空实现：空实现会让每个 ${secret:…} 都解析失败，
                // 用户看到的是"我的 key 坏了"，与真实原因（钥匙串打不开）完全不同。
                tracing::error!(
                    code = e.code(),
                    error = %e,
                    "密钥优先级链初始化失败，降级为便携文件 + 环境变量"
                );
                let portable = ta_secrets::PortableStore::open(
                    ta_secrets::portable::default_path()
                        .unwrap_or_else(|_| PathBuf::from("secrets.enc")),
                );
                Arc::new(ta_secrets::CompositeStore::portable_mode(portable))
                    as Arc<dyn SecretStore>
            }
        })
        .clone()
}

/// 现在（Unix 毫秒）。命令层不引时钟抽象（`AppState` 里那份是给存储层的）。
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// ================================================================ ① 开机自启动

/// `system_integration` 是自启动命令唯一允许改的配置节。
const AUTOSTART_SECTION: &str = "system_integration";

/// `app_get_autostart` / `app_set_autostart` / `app_repair_autostart` 的返回值。
///
/// 形状与 `docs/10 §2.2` 的 `AutostartState` 逐字段一致；末尾两个字段是**附加**的
/// （契约之外的诚实补充，前端多两个只读信息，不会破坏既有渲染）：
///
/// * `command_matches`：注册的命令行是否**正好**是当前期望（exe + mode + delay）。
///   `drifted` 只看"在不在"；换过安装目录、改过延迟之后"在但指向别处"这种情况
///   只有它能看出来 —— 而这正是"改了配置却什么都没变"的典型症状。
/// * `expected_command`：期望写进系统的命令行（UI 可以把它和 `registered_command` 并排显示）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
pub struct AutostartState {
    /// 配置里的期望状态
    pub desired: bool,
    /// **从操作系统读到的**实际状态（UI 开关显示的是这个）
    pub actual: bool,
    pub drifted: bool,
    /// `windows-run-key` | `macos-launch-agent` | `linux-xdg` | `unsupported`
    pub mechanism: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub registered_command: Option<String>,
    pub mode: AutostartMode,
    pub delay_s: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unsupported_reason: Option<String>,
    /// 附加：注册的命令行是否与当前期望逐字符一致
    pub command_matches: bool,
    /// 附加：当前期望写进系统的命令行
    pub expected_command: String,
}

fn autostart_view(
    sys: CoreAutostartState,
    ctl: &AutostartController,
    desired: &AutostartDesired,
) -> AutostartState {
    AutostartState {
        desired: sys.desired,
        actual: sys.actual,
        drifted: sys.drifted,
        mechanism: sys.mechanism,
        registered_command: sys.registered_command,
        mode: sys.mode,
        delay_s: sys.delay_s,
        unsupported_reason: sys.unsupported_reason,
        command_matches: ctl.probe().matches_expected(desired),
        expected_command: ctl.command_line(desired),
    }
}

/// 只读：真去读系统 + 与配置比对。**绝不写系统**（E3）。
pub fn read_autostart(state: &AppState, ctl: &AutostartController) -> AutostartState {
    let desired = AutostartDesired::from_config(&state.doc());
    autostart_view(ctl.probe().state(&desired), ctl, &desired)
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn app_get_autostart(state: State<'_, AppState>) -> IpcResult<AutostartState> {
    Ok(read_autostart(&state, &AutostartController::detect()))
}

/// 用户显式开 / 关自启动。
///
/// 顺序是这套语义的关键，**不能反**：
/// 1. 写系统（`AutostartController::set` 内部写完会重新读系统）；
/// 2. 回读结果与请求不一致 → 直接失败，**此时配置一个字节都没动**；
/// 3. 只有系统真的变了才写配置；写配置失败 → 把系统退回"配置里的期望"（不留半套）；
/// 4. 从库里读回配置，断言与请求一致。
pub async fn set_autostart(
    state: &AppState,
    ctl: &AutostartController,
    enabled: bool,
    mode: Option<AutostartMode>,
    delay_s: Option<u32>,
) -> IpcResult<AutostartState> {
    let before = state.doc();
    let desired = AutostartDesired::new(
        enabled,
        mode.unwrap_or(before.system_integration.autostart_mode),
        delay_s.unwrap_or(before.system_integration.autostart_delay_s),
    );

    let sys = ctl.set(&desired, enabled)?;
    if sys.actual != enabled {
        // 规则 6 + 规则 2：不许"改了配置就报告成功"，也不许留"配置说开、系统没开"
        return Err(IpcError::from(TaError::Internal(format!(
            "开机自启动没有真正改变：请求 enabled={enabled}，回读 actual={}（机制 {}）—— 配置未改动，界面开关显示的仍是系统事实",
            sys.actual, sys.mechanism
        ))));
    }

    let mut doc = (*before).clone();
    doc.system_integration.autostart = sys.actual;
    doc.system_integration.autostart_mode = desired.mode;
    doc.system_integration.autostart_delay_s = desired.delay_s;
    if let Err(e) = persist_config(state, &doc, &[AUTOSTART_SECTION], "autostart").await {
        let rollback = AutostartDesired::from_config(&before);
        match ctl.set(&rollback, rollback.enabled) {
            Ok(_) => tracing::warn!(
                mechanism = ctl.mechanism(),
                "自启动配置落库失败，已把系统状态退回配置里的期望值"
            ),
            Err(rb) => tracing::error!(
                code = rb.code(),
                error = %rb,
                "自启动配置落库失败，且回滚系统状态也失败：现在系统与配置可能不一致，界面会如实报漂移"
            ),
        }
        return Err(e);
    }

    let after = read_back(state).await?;
    let got = AutostartDesired::from_config(&after);
    if got != desired {
        return Err(IpcError::from(TaError::Internal(format!(
            "自启动配置写入后回读不一致：期望 {desired:?}，库里是 {got:?}"
        ))));
    }
    Ok(autostart_view(sys, ctl, &desired))
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn app_set_autostart(
    state: State<'_, AppState>,
    enabled: bool,
    mode: Option<AutostartMode>,
    delay_s: Option<u32>,
) -> IpcResult<AutostartState> {
    // 延迟上限（0–300）由 `AutostartDesired::validate` 在 `set` 的最前面拦下，
    // 报的还是 `system_integration.autostart_delay_s` 那条字段错误 —— 这里不重复实现一遍。
    let out = set_autostart(&state, &AutostartController::detect(), enabled, mode, delay_s).await?;
    tracing::info!(
        enabled,
        actual = out.actual,
        mode = ?out.mode,
        delay_s = out.delay_s,
        mechanism = %out.mechanism,
        "自启动：已按用户操作更新（配置与系统都已回读确认）"
    );
    Ok(out)
}

/// 用户显式"重新对齐"（`app_repair_autostart`）：把**系统**改成与**配置**一致。
///
/// 只动系统、不动配置 —— E3 说得很清楚：配置是期望，系统是事实，
/// "重新对齐"是用户点的按钮，不是我们偷偷做的。
pub async fn repair_autostart(
    state: &AppState,
    ctl: &AutostartController,
) -> IpcResult<AutostartState> {
    let doc = state.doc();
    let desired = AutostartDesired::from_config(&doc);
    let before = ctl.probe().state(&desired);
    let sys = ctl.repair(&desired)?;
    if sys.actual != desired.enabled {
        return Err(IpcError::from(TaError::Internal(format!(
            "重新对齐失败：期望 actual={}，回读 actual={}（机制 {}）",
            desired.enabled, sys.actual, sys.mechanism
        ))));
    }
    tracing::info!(
        before_actual = before.actual,
        before_drifted = before.drifted,
        after_actual = sys.actual,
        mechanism = %sys.mechanism,
        "自启动：已按用户操作把系统状态重新对齐到配置（配置未改）"
    );
    Ok(autostart_view(sys, ctl, &desired))
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn app_repair_autostart(state: State<'_, AppState>) -> IpcResult<AutostartState> {
    repair_autostart(&state, &AutostartController::detect()).await
}

// ================================================================ ② 检查更新

/// `app_check_update` 的返回（`docs/10 §2.2`：`UpdateInfo?`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
pub struct UpdateInfo {
    pub version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// 更新通道没接上的**准确原因**（也会原样进错误信封的 message）。
pub const UPDATE_UNWIRED_REASON: &str = "检查更新不可用：本版本没有接入更新通道\
（`tauri-plugin-updater` 既没有在 `Cargo.toml` 里声明、也没有注册，更没有更新清单地址与签名公钥）。\
这不是「已是最新」，而是「没能检查」—— 所以这里报错，而不是回一个会被读成「已是最新」的空结果。";

/// 检查更新。**永远返回 Err** —— 在通道接通之前，"没发现新版本"这个结论我们无权给出。
pub fn check_update() -> IpcResult<Option<UpdateInfo>> {
    tracing::warn!(
        code = "TA-9001",
        "用户点了检查更新，但更新通道未接入：如实报错，不假装已是最新"
    );
    Err(IpcError::from(TaError::Internal(UPDATE_UNWIRED_REASON.to_string())))
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn app_check_update() -> IpcResult<Option<UpdateInfo>> {
    check_update()
}

// ================================================================ ③ 文件监听模式

/// 文件监听模式要动的配置节（`net` 不在其中：这一笔不碰网络相关配置）。
const FILE_WATCH_SECTIONS: &[&str] = &["system_integration"];

/// 翻转"以 YAML 为真源"的开关，并把 `path` 一起落库。
///
/// * `enabled = true` 且没给 path → 沿用配置里已有的 path；
/// * `enabled = false` 且没给 path → **清掉 path**（留着它会让校验器报一条警告，
///   而且下次开启时会莫名其妙地指向旧文件）；
/// * 关掉时给了 path → 记下来（用户可能想"先填好、下次再开"）。
///
/// ⚠️ **`restart_required: true` 是诚实的**：本 crate 里**没有**文件监听器
/// （`notify` 依赖没进来，也没有 watcher 服务），所以这个开关只是把"下次启动按文件真源走"
/// 记进配置。**我们不能说它"已经生效"**。
pub async fn set_file_watch_mode(
    state: &AppState,
    enabled: bool,
    path: Option<String>,
) -> IpcResult<ApplyResult> {
    let mut doc = (*state.doc()).clone();
    let si = &mut doc.system_integration;
    si.file_watch_mode = enabled;
    si.file_watch_path = match path {
        Some(p) if !p.trim().is_empty() => Some(p.trim().to_string()),
        Some(_) => None,
        None if enabled => si.file_watch_path.clone(),
        None => None,
    };
    let expected = (si.file_watch_mode, si.file_watch_path.clone());

    persist_config(state, &doc, FILE_WATCH_SECTIONS, "file-watch-mode").await?;

    let after = read_back(state).await?;
    let got = (after.system_integration.file_watch_mode, after.system_integration.file_watch_path);
    if got != expected {
        return Err(IpcError::from(TaError::Internal(format!(
            "文件监听开关写入后回读不一致：期望 {expected:?}，库里是 {got:?}"
        ))));
    }
    tracing::warn!(
        enabled,
        "文件监听模式已写进配置，但**监听器本身尚未实现**：该开关只在下次启动时被读取"
    );
    Ok(ApplyResult {
        applied: true,
        restart_required: true,
        kernel_reload: false,
        channels_created: 0,
    })
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn config_set_file_watch_mode(
    state: State<'_, AppState>,
    enabled: bool,
    path: Option<String>,
) -> IpcResult<ApplyResult> {
    set_file_watch_mode(&state, enabled, path).await
}

// ================================================================ ④ 日志导出（诊断包）

/// 一个诊断包最多带几个日志文件 / 每个文件最多读多少字节。
///
/// 超大的日志按**尾部**截断（诊断要看的是"最近发生了什么"），截断事实会写进 `export.json`。
pub const MAX_LOG_FILES: usize = 24;
pub const MAX_LOG_FILE_BYTES: u64 = 8 * 1024 * 1024;
/// 一次导出最多带多少条请求日志（`LogQuery.limit` 仍可调小）。
pub const MAX_TRACE_ROWS: u32 = 2000;

/// 组包所需的全部素材。
pub struct ExportSources {
    /// `("logs/<文件名>", 内容)` —— 日志文件**原样**进包（它们本该在写入时就已脱敏）
    pub log_files: Vec<(String, Vec<u8>)>,
    pub traces: Vec<TraceRow>,
    pub config_yaml: String,
    pub env_json: String,
    pub meta_json: String,
    pub error_codes: Vec<CodeCount>,
}

/// **组包的唯一实现**（`log_export` 与测试都走这里）。
///
/// 脱敏与"打包后解压再扫一遍"全部复用 `ta_core::diag`：
/// * 配置 YAML → [`sanitize::sanitize_config_yaml`]（保留结构，只把密钥值换成 `***`）
/// * 请求日志 → [`sanitize::sanitize`]（上游错误文本里可能混进 `Bearer …`）
/// * 组包 + 打包后重扫 → [`diag_package::build_package`]（**命中即中止导出**）
///
/// **日志文件不做二次脱敏**：它们本来就该在写入路径上脱敏（`src-tauri/src/logging.rs`）。
/// 如果那里漏了，我们的行为是**响亮地中止导出**，而不是"悄悄洗干净再发出去" ——
/// 后者会让"日志写入路径漏脱敏"这个真 bug 永远没人发现。
pub fn build_diagnostic_package(
    src: &ExportSources,
    generated_ms: i64,
    app_version: &str,
) -> Result<Vec<u8>, PackageError> {
    let mut files: Vec<PackageFile> = Vec::with_capacity(src.log_files.len() + 4);
    for (name, bytes) in &src.log_files {
        files.push(PackageFile::new(name.clone(), bytes.clone()));
    }
    files.push(PackageFile::text("traces/requests.jsonl", &traces_jsonl(&src.traces)));
    files.push(PackageFile::text(
        "config/config.yaml",
        &sanitize::sanitize_config_yaml(&src.config_yaml),
    ));
    files.push(PackageFile::text("env.json", &src.env_json));
    files.push(PackageFile::text("export.json", &src.meta_json));

    // 上面两个 ::sanitize::* 是"第一道"；build_package 是最后那道闸（[14 §11.3]）
    diag_package::build_package(
        &files,
        generated_ms,
        app_version,
        ta_core::CORE_VERSION,
        &src.error_codes,
    )
}

/// 逐行 JSON（JSONL）。**先脱敏再序列化进包**（见 [`build_diagnostic_package`] 的注记）。
fn traces_jsonl(rows: &[TraceRow]) -> String {
    let raw: String = rows
        .iter()
        .map(|r| serde_json::to_string(r).unwrap_or_else(|_| "{\"error\":\"serialize\"}".into()))
        .map(|line| format!("{line}\n"))
        .collect();
    sanitize::sanitize(&raw)
}

/// 收集日志目录里的文件（`("logs/<名>", 字节)`）。
///
/// 目录不存在 → 空列表（**不是错误**：全新安装还没有日志文件，导出"只有配置与环境"的包
/// 依然有意义）。`.zip` 一概跳过 —— 否则会把上一次的导出包塞进这一次的包里。
pub fn collect_log_files(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .filter(|p| !p.extension().map(|e| e.eq_ignore_ascii_case("zip")).unwrap_or(false))
        .collect();
    paths.sort();

    let mut out = Vec::new();
    for p in paths.into_iter().take(MAX_LOG_FILES) {
        let Some(name) = p.file_name().and_then(|n| n.to_str()) else { continue };
        let Ok(meta) = std::fs::metadata(&p) else { continue };
        let bytes = if meta.len() > MAX_LOG_FILE_BYTES {
            read_tail(&p, MAX_LOG_FILE_BYTES).unwrap_or_default()
        } else {
            std::fs::read(&p).unwrap_or_default()
        };
        out.push((format!("logs/{name}"), bytes));
    }
    out
}

/// 只读文件尾部 `max` 字节（超大日志的截断读；失败返回 None，调用方会记成 0 字节）。
fn read_tail(path: &Path, max: u64) -> Option<Vec<u8>> {
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let start = len.saturating_sub(max);
    f.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = Vec::new();
    f.take(max).read_to_end(&mut buf).ok()?;
    Some(buf)
}

/// `export.json`：这次导出到底带了什么（**含截断事实**，便于用户判断"为什么没看到某一行"）。
fn export_meta_json(
    query: &LogQuery,
    log_files: &[(String, Vec<u8>)],
    trace_rows: usize,
    log_dir: Option<&Path>,
) -> String {
    let files: Vec<serde_json::Value> =
        log_files.iter().map(|(n, b)| serde_json::json!({ "name": n, "bytes": b.len() })).collect();
    let value = serde_json::json!({
        "note": "日志文件按写入时的原样进包（写入路径已脱敏）；配置与请求日志在组包前过 ta_core::diag::sanitize；\
                 组包后由 ta_core::diag::package 解压重扫，命中敏感形态则**整个导出被拒绝**",
        "log_dir": log_dir.map(|p| p.display().to_string()),
        "max_log_files": MAX_LOG_FILES,
        "max_log_file_bytes": MAX_LOG_FILE_BYTES,
        "log_files": files,
        "trace_rows": trace_rows,
        "query": serde_json::to_value(query).unwrap_or(serde_json::Value::Null),
    });
    format!("{}\n", serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".into()))
}

/// `env.json`：环境信息（**不含密钥、不含数据库路径**）。
fn env_info_json(state: &AppState, log_dir: Option<&Path>) -> String {
    let value = serde_json::json!({
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "app_version": env!("CARGO_PKG_VERSION"),
        "core_version": ta_core::CORE_VERSION,
        "log_dir": log_dir.map(|p| p.display().to_string()),
        "log_dir_exists": log_dir.map(|p| p.is_dir()).unwrap_or(false),
        "uptime_s": state.uptime_s(),
    });
    format!("{}\n", serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".into()))
}

/// 把诊断包写到 `path`，返回真正写出的文件路径。
///
/// * `path` 是**已存在的目录** → 在它下面生成 `tokenagency-diagnostic-<毫秒>.zip`；
/// * 否则按文件路径处理（父目录会被创建）；
/// * **导出被拒时一个字节都不写**（不允许"拒绝导出但留下半个包"）。
pub async fn log_export_to(
    state: &AppState,
    query: LogQuery,
    path: &Path,
    log_dir: Option<&Path>,
) -> IpcResult<PathBuf> {
    let target = resolve_export_target(path, now_ms());

    let log_files = match log_dir {
        Some(dir) => collect_log_files(dir),
        None => Vec::new(),
    };
    let mut q = query.clone();
    q.limit = Some(q.limit.unwrap_or(500).min(MAX_TRACE_ROWS));
    let page = state.trace.query(&q).await?;

    let yaml = ta_core::config::to_yaml(&state.doc())?;
    let all_lines: Vec<String> = log_files
        .iter()
        .flat_map(|(_, b)| {
            String::from_utf8_lossy(b).lines().map(str::to_string).collect::<Vec<_>>()
        })
        .collect();
    let error_codes = diag_query::error_code_summary(all_lines.iter().map(String::as_str), 20);

    let src = ExportSources {
        traces: page.rows,
        config_yaml: yaml,
        env_json: env_info_json(state, log_dir),
        meta_json: export_meta_json(
            &q,
            &log_files,
            page.total.min(u64::from(MAX_TRACE_ROWS)) as usize,
            log_dir,
        ),
        error_codes,
        log_files,
    };

    let archive = build_diagnostic_package(&src, now_ms(), env!("CARGO_PKG_VERSION"))
        .map_err(|e| package_error_to_ipc(&e, &target))?;

    if let Some(parent) = target.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    std::fs::write(&target, &archive)?;
    tracing::info!(
        path = %target.display(),
        bytes = archive.len(),
        "诊断包已导出（组包后已解压重扫，零敏感命中）"
    );
    Ok(target)
}

/// `path` → 真实落点（已存在目录则生成文件名）。
pub fn resolve_export_target(path: &Path, stamp_ms: i64) -> PathBuf {
    if path.is_dir() {
        path.join(format!("tokenagency-diagnostic-{stamp_ms}.zip"))
    } else {
        path.to_path_buf()
    }
}

/// 组包错误 → IPC 错误。**绝不回显命中的原文**（`PackageError` 的 Display 只给位置与类型）。
fn package_error_to_ipc(e: &PackageError, target: &Path) -> IpcError {
    IpcError::from(TaError::Internal(format!(
        "诊断包导出被拒绝（未写出 {}）：{e}",
        target.display()
    )))
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn log_export(
    state: State<'_, AppState>,
    query: LogQuery,
    path: String,
) -> IpcResult<LogExportReceipt> {
    let log_dir = crate::logging::log_dir();
    log_export_with_receipt(&state, query, Path::new(&path), log_dir.as_deref()).await
}

/// `log_export` 的回执（[docs/10 §2.2]）。
///
/// **为什么必须回执**：后端会把"目录"解析成
/// `tokenagency-diagnostic-<时间戳>.zip`（见 [`resolve_export_target`]），
/// 所以**入参根本不是落盘路径**。沿用 `void` 时界面只能显示自己传进去的路径，
/// 用户会以为后端确认了 —— 而那个路径上并没有文件。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
pub struct LogExportReceipt {
    /// **实际落盘路径**（后端解析后的最终值，不是入参）
    pub path: String,
    /// 磁盘上的真实字节数
    pub bytes: u64,
}

/// 真正导出并回执（`log_export` 的实现体；测试直接调它，不经 Tauri `State`）。
pub async fn log_export_with_receipt(
    state: &AppState,
    query: LogQuery,
    path: &Path,
    log_dir: Option<&Path>,
) -> IpcResult<LogExportReceipt> {
    let target = log_export_to(state, query, path, log_dir).await?;
    // 字节数**从磁盘上取**：回执说的是文件的事实，不是我们的估算、也不是入参。
    // 读不到元数据就报错 —— **不回半成品回执**（那会让界面显示一个假字节数）。
    let bytes = std::fs::metadata(&target)
        .map_err(|e| {
            IpcError::from(TaError::Internal(format!(
                "导出后读取文件信息失败（{}）：{e}",
                target.display()
            )))
        })?
        .len();
    Ok(LogExportReceipt { path: target.to_string_lossy().into_owned(), bytes })
}

// ================================================================ ⑤ 首启向导

/// `onboarding_detect` 的返回（`docs/10 §2.2`：`{ kernel_found, existing_config, suggested_providers }`
/// + 两个附加字段：找内核时实际看过的位置，用户能据此看出"为什么没找到"）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
pub struct OnboardingDetect {
    pub kernel_found: bool,
    pub existing_config: bool,
    pub suggested_providers: Vec<String>,
    /// 附加：实际检查过的位置（路径或 `PATH:<名>`）
    pub kernel_checked: Vec<String>,
    /// 附加：命中的内核路径
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kernel_path: Option<String>,
}

/// 内核可执行文件名（Windows 带 `.exe` 两个都试）。
pub const KERNEL_BINARIES: &[&str] = &["mihomo.exe", "mihomo", "clash-meta.exe", "clash-meta"];

/// 环境变量 → 建议的服务商。**只看这个变量存不存在，绝不读它的值。**
pub const ENV_PROVIDER_HINTS: &[(&str, &str)] = &[
    ("OPENAI_API_KEY", "OpenAI"),
    ("ANTHROPIC_API_KEY", "Anthropic"),
    ("DEEPSEEK_API_KEY", "DeepSeek"),
    ("GEMINI_API_KEY", "Google Gemini"),
    ("GOOGLE_API_KEY", "Google Gemini"),
    ("MOONSHOT_API_KEY", "Moonshot (Kimi)"),
    ("DASHSCOPE_API_KEY", "阿里云百炼（通义）"),
    ("ARK_API_KEY", "火山方舟（字节）"),
    ("ZHIPUAI_API_KEY", "智谱 GLM"),
    ("SILICONFLOW_API_KEY", "SiliconFlow"),
    ("OPENROUTER_API_KEY", "OpenRouter"),
    ("GROQ_API_KEY", "Groq"),
];

/// 生产用的 PATH 查找（不引 `which` 依赖：`split_paths` + `is_file` 就够）。
fn find_in_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(name)).find(|p| p.is_file())
}

/// 配置里"已经有东西了"的判据（**只看会写进库的实体**，不看内存/环境）。
pub fn has_existing_config(doc: &ConfigDoc) -> bool {
    !doc.providers.is_empty()
        || !doc.credentials.is_empty()
        || !doc.channels.is_empty()
        || !doc.lanes.is_empty()
}

/// 建议的服务商：**环境变量存在** + **配置里已有**（去重，顺序确定）。
pub fn suggest_providers(doc: &ConfigDoc, env_has: &dyn Fn(&str) -> bool) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (var, label) in ENV_PROVIDER_HINTS {
        if env_has(var) && !out.iter().any(|x| x == label) {
            out.push((*label).to_string());
        }
    }
    for p in &doc.providers {
        if !out.iter().any(|x| x == &p.name) {
            out.push(p.name.clone());
        }
    }
    out
}

/// 环境检测（**纯逻辑**：`exists` / `env_has` / `which` 全部注入，测试可以穷举）。
pub fn detect_onboarding(
    doc: &ConfigDoc,
    exists: &dyn Fn(&Path) -> bool,
    env_has: &dyn Fn(&str) -> bool,
    which: &dyn Fn(&str) -> Option<PathBuf>,
    exe_dir: Option<&Path>,
) -> OnboardingDetect {
    let mut checked: Vec<String> = Vec::new();
    let mut found: Option<String> = None;

    // 1) 配置里显式给的路径最优先（用户说了算）
    if let Some(p) = doc.net.kernel.path.as_deref().map(str::trim).filter(|p| !p.is_empty()) {
        checked.push(p.to_string());
        if exists(Path::new(p)) {
            found = Some(p.to_string());
        }
    }
    // 2) 应用同目录（sidecar：更新包会连它一起替换）
    if found.is_none() {
        if let Some(dir) = exe_dir {
            for name in KERNEL_BINARIES {
                let cand = dir.join(name);
                checked.push(cand.display().to_string());
                if exists(&cand) {
                    found = Some(cand.display().to_string());
                    break;
                }
            }
        }
    }
    // 3) PATH
    if found.is_none() {
        for name in KERNEL_BINARIES {
            match which(name) {
                Some(p) => {
                    checked.push(format!("PATH:{name}"));
                    found = Some(p.display().to_string());
                    break;
                }
                None => checked.push(format!("PATH:{name}")),
            }
        }
    }

    OnboardingDetect {
        kernel_found: found.is_some(),
        existing_config: has_existing_config(doc),
        suggested_providers: suggest_providers(doc, env_has),
        kernel_checked: checked,
        kernel_path: found,
    }
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn onboarding_detect(state: State<'_, AppState>) -> IpcResult<OnboardingDetect> {
    let doc = state.doc();
    let exe_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf));
    let exists = |p: &Path| p.is_file();
    let env_has = |k: &str| std::env::var(k).map(|v| !v.trim().is_empty()).unwrap_or(false);
    let which = find_in_path;
    let out = detect_onboarding(&doc, &exists, &env_has, &which, exe_dir.as_deref());
    tracing::info!(
        kernel_found = out.kernel_found,
        existing_config = out.existing_config,
        suggested = out.suggested_providers.len(),
        "首启向导：环境检测完成"
    );
    Ok(out)
}

/// 向导里要写进配置的一个服务商（`secret` 是**明文**，只会进密钥仓库，绝不进配置）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
pub struct OnboardingProvider {
    pub id: String,
    pub name: String,
    pub protocol: Protocol,
    pub base_url: String,
    pub secret: String,
}

/// 向导里要写进配置的一个订阅。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
pub struct OnboardingSubscription {
    pub id: String,
    pub name: String,
    pub url: String,
}

/// `onboarding_apply` 的入参（`docs/10 §2.2` 的 `OnboardingInput`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
pub struct OnboardingInput {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kernel_path: Option<String>,
    pub providers: Vec<OnboardingProvider>,
    pub subscriptions: Vec<OnboardingSubscription>,
}

/// 明文密钥在仓库里的条目名（与 `ta-secrets::migrate` 的建议名同风格）。
pub fn credential_entry_name(provider_id: &str) -> String {
    let mut slug = String::with_capacity(provider_id.len());
    let mut last_dash = false;
    for ch in provider_id.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash && !slug.is_empty() {
            slug.push('-');
            last_dash = true;
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    if slug.is_empty() {
        slug = "provider".to_string();
    }
    format!("cred-{slug}")
}

/// 用 `serde_json` 构造领域实体：**其余字段全部走 `ta-core` 自己的 `#[serde(default)]`**。
///
/// 为什么不用 `Provider { … }` 手写字面量：`TimeoutConfig` 这类没有 `Default`，
/// 手写就得把每个默认值**在命令层再抄一遍**，schema 一改就静默漂移。
/// 反序列化走的是同一套默认值。
fn from_defaults<T: serde::de::DeserializeOwned>(value: serde_json::Value) -> IpcResult<T> {
    serde_json::from_value(value).map_err(|e| {
        IpcError::from(TaError::Internal(format!(
            "构造配置实体失败（内置默认值与 schema 不一致）：{e}"
        )))
    })
}

fn build_provider(p: &OnboardingProvider) -> IpcResult<Provider> {
    let id = ProviderId::new(p.id.trim());
    if id.as_str().is_empty() {
        return Err(IpcError::from(TaError::MalformedRequest("服务商 id 不能为空".to_string())));
    }
    if p.base_url.trim().is_empty() {
        return Err(IpcError::from(TaError::MalformedRequest(format!(
            "服务商 {} 的 base_url 不能为空",
            id.as_str()
        ))));
    }
    from_defaults(serde_json::json!({
        "id": id,
        "name": p.name,
        "protocol": p.protocol,
        "base_url": p.base_url.trim(),
    }))
}

fn build_credential(provider: &ProviderId, secret_spec: &str) -> IpcResult<Credential> {
    from_defaults(serde_json::json!({
        "id": format!("{}-default", provider.as_str()),
        "provider": provider.as_str(),
        "label": "向导创建",
        "secret": secret_spec,
    }))
}

/// upsert 一条凭据（同一个服务商在向导里只会有一条默认凭据）。
fn upsert_credential(
    doc: &mut ConfigDoc,
    provider: &ProviderId,
    secret_spec: &str,
) -> IpcResult<()> {
    let cred = build_credential(provider, secret_spec)?;
    match doc.credentials.iter_mut().find(|c| c.id == cred.id) {
        Some(slot) => slot.secret = cred.secret,
        None => doc.credentials.push(cred),
    }
    Ok(())
}

fn build_subscription(s: &OnboardingSubscription) -> IpcResult<ta_core::domain::Subscription> {
    let id = SubscriptionId::new(s.id.trim());
    if id.as_str().is_empty() {
        return Err(IpcError::from(TaError::MalformedRequest("订阅 id 不能为空".to_string())));
    }
    let url = s.url.trim();
    if url.is_empty() {
        return Err(IpcError::from(TaError::MalformedRequest(format!(
            "订阅 {} 的 URL 不能为空",
            id.as_str()
        ))));
    }
    from_defaults(serde_json::json!({ "id": id, "name": s.name, "url": url }))
}

fn upsert_subscription(doc: &mut ConfigDoc, s: &OnboardingSubscription) -> IpcResult<()> {
    let sub = build_subscription(s)?;
    match doc.proxy_subscriptions.iter_mut().find(|x| x.id == sub.id) {
        Some(slot) => {
            slot.name = sub.name;
            slot.url = sub.url;
        }
        None => doc.proxy_subscriptions.push(sub),
    }
    Ok(())
}

/// 应用向导结果：服务商 / 订阅落配置，明文密钥落**密钥仓库**（配置里只留 `${secret:…}` 引用）。
///
/// 顺序：先把密钥全部写进仓库（任何一个失败就整体失败，配置不动）→ 再落配置 → 读回来一致。
pub async fn apply_onboarding(
    state: &AppState,
    store: &dyn SecretStore,
    input: OnboardingInput,
) -> IpcResult<ApplyResult> {
    let mut doc = (*state.doc()).clone();

    if let Some(p) = input.kernel_path.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        doc.net.kernel.path = Some(p.to_string());
    }

    // 待写进仓库的明文：**只在这个函数的局部变量里出现**，不进日志、不进错误消息
    let mut to_store: Vec<(String, String)> = Vec::new();
    for p in &input.providers {
        let provider = build_provider(p)?;
        let pid = provider.id.clone();
        match doc.providers.iter_mut().find(|x| x.id == pid) {
            Some(slot) => {
                slot.name = provider.name;
                slot.protocol = provider.protocol;
                slot.base_url = provider.base_url;
                slot.enabled = true;
            }
            None => doc.providers.push(provider),
        }
        let raw = p.secret.trim();
        if raw.is_empty() {
            continue; // 无密钥的服务商（本地推理等）是合法的
        }
        if raw.starts_with("${") {
            // 用户粘的是引用而不是明文：直接用，不进仓库
            upsert_credential(&mut doc, &pid, raw)?;
            continue;
        }
        let entry = credential_entry_name(pid.as_str());
        ta_secrets::resolver::validate_entry_name(&entry).map_err(|why| {
            IpcError::from(TaError::Secret(format!(
                "服务商 {} 的密钥条目名不合法：{why}",
                pid.as_str()
            )))
        })?;
        to_store.push((entry.clone(), raw.to_string()));
        upsert_credential(&mut doc, &pid, &format!("${{secret:{entry}}}"))?;
    }
    for s in &input.subscriptions {
        upsert_subscription(&mut doc, s)?;
    }

    // ① 先落密钥（失败 → 配置一个字节都不动）
    for (name, value) in &to_store {
        store.set(name, value).await.map_err(|e| {
            IpcError::from(TaError::Secret(format!("写入密钥条目失败（{name}）：{e}")))
        })?;
        // ② 立刻读回来，证明真的写进去了（不是"调用了 set 就算成功"）
        match store.get(name).await {
            Ok(Some(v)) if v.as_str() == value.as_str() => {}
            Ok(_) => {
                return Err(IpcError::from(TaError::Secret(format!(
                    "密钥条目写入后读回不一致：{name}"
                ))))
            }
            Err(e) => {
                return Err(IpcError::from(TaError::Secret(format!(
                    "密钥条目写入后读取失败（{name}）：{e}"
                ))))
            }
        }
        let hint = mask_secret(value);
        if let Err(e) = state.secret_meta.upsert(name, &hint, now_ms()).await {
            tracing::warn!(code = e.code(), error = %e, "密钥元数据登记失败（密钥本身已写入）");
        }
    }

    // ③ 再落配置
    persist_config(
        state,
        &doc,
        &["providers", "credentials", "proxy_subscriptions", "net"],
        "onboarding",
    )
    .await?;

    // ④ 读回来一致
    let after = read_back(state).await?;
    for p in &input.providers {
        let id = ProviderId::new(p.id.trim());
        match after.providers.iter().find(|x| x.id == id) {
            Some(got) if got.name == p.name && got.base_url == p.base_url.trim() => {}
            _ => {
                return Err(IpcError::from(TaError::Internal(format!(
                    "向导写入后回读不一致：服务商 {}",
                    id.as_str()
                ))))
            }
        }
    }
    for s in &input.subscriptions {
        let id = SubscriptionId::new(s.id.trim());
        if !after.proxy_subscriptions.iter().any(|x| x.id == id) {
            return Err(IpcError::from(TaError::Internal(format!(
                "向导写入后回读不一致：订阅 {}",
                id.as_str()
            ))));
        }
    }
    if let Some(p) = input.kernel_path.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        if after.net.kernel.path.as_deref() != Some(p) {
            return Err(IpcError::from(TaError::Internal(
                "向导写入后回读不一致：内核路径".to_string(),
            )));
        }
    }
    tracing::info!(
        providers = input.providers.len(),
        subscriptions = input.subscriptions.len(),
        secrets = to_store.len(),
        "首启向导：配置与密钥已写入并回读确认（明文只进密钥仓库）"
    );
    Ok(ApplyResult {
        applied: true,
        restart_required: false,
        kernel_reload: false,
        channels_created: 0,
    })
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn onboarding_apply(
    state: State<'_, AppState>,
    input: OnboardingInput,
) -> IpcResult<ApplyResult> {
    let store = secret_store();
    apply_onboarding(&state, store.as_ref(), input).await
}

/// `onboarding_test_all` 的一行（`docs/10 §2.2` 的 `ConnectivityRow`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
pub struct ConnectivityRow {
    pub credential_id: String,
    pub lane_id: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// **本命令 `ok` 的口径**（原样写进每一行的 `note`，不让人猜）。
pub const CONNECTIVITY_NOTE: &str = "口径：线路可达（内核真测延迟）+ 密钥引用可解析；\
本命令不向上游发请求，所以不报告 HTTP 状态码，也不断言凭据一定被上游接受";

/// 一条待测线路的探测结果（由命令层真测后填入）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaneProbe {
    pub lane_id: String,
    pub ok: bool,
    pub latency_ms: Option<u32>,
    pub note: Option<String>,
}

/// 一条凭据的密钥可解析性。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialProbe {
    pub credential_id: String,
    pub secret_ok: bool,
}

/// 组装 key × lane 矩阵（**纯函数**：真探测在外面做，这里只判定与拼文案）。
pub fn connectivity_rows(creds: &[CredentialProbe], lanes: &[LaneProbe]) -> Vec<ConnectivityRow> {
    let mut out = Vec::with_capacity(creds.len() * lanes.len());
    for c in creds {
        for l in lanes {
            let ok = c.secret_ok && l.ok;
            let mut reasons: Vec<String> = Vec::new();
            if !c.secret_ok {
                reasons.push("密钥引用无法解析（条目不存在/为空）".to_string());
            }
            if let Some(n) = &l.note {
                reasons.push(n.clone());
            }
            reasons.push(CONNECTIVITY_NOTE.to_string());
            out.push(ConnectivityRow {
                credential_id: c.credential_id.clone(),
                lane_id: l.lane_id.clone(),
                ok,
                latency_ms: if l.ok { l.latency_ms } else { None },
                status: None,
                note: Some(reasons.join("；")),
            });
        }
    }
    out
}

/// 单条线路探测超时（毫秒）。测速走内核自己，本命令不另发请求。
pub const LANE_PROBE_TIMEOUT_MS: u64 = 5_000;

/// 真测每一条已启用线路 + 检查每个已启用凭据的密钥引用能否解析。
///
/// 内核没装配 → **如实报错**（没有内核就没有"线路可达性"这个事实可言），
/// 而不是回一个空矩阵（空矩阵会被读成"一切正常，没有可测的组合"）。
pub async fn test_all_connectivity(
    state: &AppState,
    store: &dyn SecretStore,
) -> IpcResult<Vec<ConnectivityRow>> {
    let doc = state.doc();

    let env = EnvStore::new();
    let mut creds: Vec<CredentialProbe> = Vec::new();
    for c in doc.credentials.iter().filter(|c| c.enabled) {
        let secret_ok = !c.secret.trim().is_empty()
            && ta_secrets::resolve_spec(&c.secret, store, &env).await.is_ok();
        creds.push(CredentialProbe { credential_id: c.id.as_str().to_string(), secret_ok });
    }

    let supervisor =
        state.kernel_supervisor.read().unwrap_or_else(|e| e.into_inner()).clone().ok_or_else(
            || {
                IpcError::from(TaError::Internal(
                "没有装配内核：无法测线路连通性（线路可达性只能由内核去测，本命令不自己发请求）"
                    .to_string(),
            ))
            },
        )?;
    let rest = supervisor.rest().clone();

    let mut lanes: Vec<LaneProbe> = Vec::new();
    for lane in doc.lanes.iter().filter(|l| l.enabled) {
        let target = match ta_kernel::config_gen::lane_target(&doc, lane) {
            Ok(t) => t,
            Err(e) => {
                lanes.push(LaneProbe {
                    lane_id: lane.id.as_str().to_string(),
                    ok: false,
                    latency_ms: None,
                    note: Some(format!("线路无法解析出内核目标：{e}")),
                });
                continue;
            }
        };
        let (ok, latency, note) = match rest.delay_of(&target, LANE_PROBE_TIMEOUT_MS).await {
            Ok(ms) => (true, Some(ms), None),
            Err(e) => (false, None, Some(format!("内核测速失败：{e}"))),
        };
        lanes.push(LaneProbe {
            lane_id: lane.id.as_str().to_string(),
            ok,
            latency_ms: latency,
            note,
        });
    }

    let rows = connectivity_rows(&creds, &lanes);
    tracing::info!(
        credentials = creds.len(),
        lanes = lanes.len(),
        rows = rows.len(),
        "首启向导：连通性矩阵已生成（线路为内核真测，凭据只验证密钥引用）"
    );
    Ok(rows)
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn onboarding_test_all(state: State<'_, AppState>) -> IpcResult<Vec<ConnectivityRow>> {
    let store = secret_store();
    test_all_connectivity(&state, store.as_ref()).await
}

// ================================================================ ⑥ 明文密钥

/// `secrets_scan_plaintext` 的一行。
///
/// 形状照 `ta_secrets::migrate::PlaintextFinding` 原样带出（`masked` 是脱敏预览，
/// **绝不含完整值**）。`ta-secrets` 没开 specta feature，所以这里做一个等价 DTO。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
pub struct PlaintextFinding {
    /// 字段路径，如 `credentials[0].secret`
    pub location: String,
    /// 所属实体，如 `credential:openai-personal`
    pub entity: String,
    /// 形态：`sk_dash` / `jwt` / `long_literal` / `malformed_ref` / `empty` …
    pub kind: String,
    /// 脱敏预览（可以直接展示）
    pub masked: String,
    /// 建议的钥匙串条目名
    pub suggested_name: String,
    pub migratable: bool,
}

fn finding_view(f: ta_secrets::PlaintextFinding) -> PlaintextFinding {
    PlaintextFinding {
        location: f.location,
        entity: f.entity,
        kind: f.kind.as_str().to_string(),
        masked: f.masked,
        suggested_name: f.suggested_name,
        migratable: f.migratable,
    }
}

/// 扫配置里的明文密钥（判定完全来自 `ta_secrets::scan_plaintext`，这里只做形状转换）。
#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn secrets_scan_plaintext(
    state: State<'_, AppState>,
) -> IpcResult<Vec<PlaintextFinding>> {
    Ok(ta_secrets::scan_plaintext(&state.doc()).into_iter().map(finding_view).collect())
}

/// 把明文迁进密钥仓库，并把配置里的值改写成 `${secret:…}`。
///
/// 关键断言（都在库里真读之后）：
/// 1. 迁移后**不再有可迁移的明文**（`ta-secrets` 自己也会自检一次）；
/// 2. 每个新引用**都能解析回明文**（否则就是把配置指向了一个不存在的条目）。
pub async fn migrate_secrets(
    state: &AppState,
    store: &dyn SecretStore,
    names: Vec<String>,
) -> IpcResult<()> {
    let before = state.doc();
    let mask_by_location: BTreeMap<String, String> = ta_secrets::scan_plaintext(&before)
        .into_iter()
        .map(|f| (f.location.clone(), f.masked.clone()))
        .collect();

    let report = ta_secrets::migrate::migrate_with_report(&before, &names, store).await?;

    persist_config(state, &report.doc, &["credentials", "providers"], "secrets-migrate").await?;

    for m in &report.migrated {
        let hint =
            mask_by_location.get(&m.location).cloned().unwrap_or_else(|| "已迁移".to_string());
        if let Err(e) = state.secret_meta.upsert(&m.name, &hint, now_ms()).await {
            tracing::warn!(code = e.code(), error = %e, "密钥元数据登记失败（密钥本身已写入）");
        }
    }

    let after = read_back(state).await?;
    let left: Vec<PlaintextFinding> = ta_secrets::scan_plaintext(&after)
        .into_iter()
        .filter(|f| f.migratable)
        .map(finding_view)
        .collect();
    if !left.is_empty() {
        return Err(IpcError::from(TaError::Internal(format!(
            "迁移后库里仍有 {} 处明文（首个：{}）",
            left.len(),
            left[0].location
        ))));
    }
    let env = EnvStore::new();
    for m in &report.migrated {
        let spec = format!("${{secret:{}}}", m.name);
        ta_secrets::resolve_spec(&spec, store, &env).await.map_err(|e| {
            IpcError::from(TaError::Secret(format!(
                "迁移后的引用解析不回来（条目 {}）：{e}",
                m.name
            )))
        })?;
    }
    tracing::info!(
        migrated = report.migrated.len(),
        skipped = report.skipped.len(),
        "明文密钥已迁移进密钥仓库（配置里只留引用）"
    );
    Ok(())
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn secrets_migrate(state: State<'_, AppState>, names: Vec<String>) -> IpcResult<()> {
    let store = secret_store();
    migrate_secrets(&state, store.as_ref(), names).await
}

// ================================================================ ⑦ 托盘

/// `tray_state` 的返回。
///
/// 前五个字段**逐字对齐冻结契约**（`src/shared/bindings.ts` 的 `TrayState`：
/// `status` 只取 `ok|warn|error|off`）；后面几个是附加的诚实补充：
///
/// * `status_detail`：托盘的真实**四态**（`normal|degraded|error|paused`）。
///   契约的四个取值装不下"已暂停"与"降级"的区别，所以真值另给一个字段，
///   **不把"用户暂停"说成 `off`**。
/// * `experimental` / `gateway_running` / `emergency`：界面要显示的几个开关态。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
pub struct TrayState {
    /// 冻结契约取值：`ok` | `warn` | `error` | `off`
    pub status: String,
    pub tooltip: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub policy_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u32>,
    /// 附加：托盘四态原文（`normal` | `degraded` | `error` | `paused`）
    pub status_detail: String,
    pub experimental: bool,
    pub gateway_running: bool,
    pub emergency: EmergencyMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_policy_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_lane_id: Option<String>,
}

/// 托盘四态 → 冻结契约的四个取值（纯函数，穷举可测）。
pub fn contract_status(s: TrayStatus) -> &'static str {
    match s {
        TrayStatus::Normal => "ok",
        TrayStatus::Degraded => "warn",
        TrayStatus::Error => "error",
        TrayStatus::Paused => "off",
    }
}

/// 当前生效的策略：**已启用里 priority 最小**的那条（与 `app_status` 同一条规则）。
pub fn active_policy(doc: &ConfigDoc) -> Option<&Policy> {
    doc.policies.iter().filter(|p| p.enabled).min_by_key(|p| p.priority)
}

/// 组装 `tray_state` 的返回（纯函数：状态色与 tooltip 由 `tray.rs` 给，其余由配置/健康算）。
pub fn tray_view(
    status: TrayStatus,
    tooltip: String,
    gateway_running: bool,
    doc: &ConfigDoc,
    health: Option<&HealthSample>,
    emergency: EmergencyMode,
) -> TrayState {
    let policy = active_policy(doc);
    let default_lane_id = doc.net.default_lane.clone();
    TrayState {
        status: contract_status(status).to_string(),
        tooltip,
        policy_name: policy.map(|p| p.name.clone()),
        node: health.and_then(|h| h.node.clone()),
        latency_ms: health.and_then(|h| h.latency_ms),
        status_detail: status.as_str().to_string(),
        experimental: doc.system_integration.enable_system_proxy
            || doc.system_integration.enable_tun,
        gateway_running,
        emergency,
        active_policy_id: policy.map(|p| p.id.as_str().to_string()),
        default_lane_id: default_lane_id.map(|l| l.as_str().to_string()),
    }
}

/// 紧急开关的**运行时**状态（不落配置：见 [`set_emergency`] 的注记）。
#[derive(Debug, Clone, PartialEq, Eq)]
struct EmergencyRuntime {
    mode: EmergencyMode,
    /// `DirectAll` 顶掉的默认出口（`Restore` 时还回去）
    saved_default_lane: Option<Option<LaneId>>,
}

static EMERGENCY: RwLock<EmergencyRuntime> =
    RwLock::new(EmergencyRuntime { mode: EmergencyMode::Restore, saved_default_lane: None });

/// 当前紧急模式（`tray_state` 会带出去）。
pub fn emergency_mode() -> EmergencyMode {
    EMERGENCY.read().unwrap_or_else(|e| e.into_inner()).mode
}

fn set_emergency_runtime(mode: EmergencyMode, saved: Option<Option<LaneId>>) {
    let mut g = EMERGENCY.write().unwrap_or_else(|e| e.into_inner());
    g.mode = mode;
    g.saved_default_lane = saved;
}

/// 「只允许视觉请求」为什么做不了（原文也进错误消息）。
pub const VISION_ONLY_UNSUPPORTED: &str = "「只允许视觉请求」在当前路由策略引擎上做不了：\
`PolicyAction` 只有 `use_channels` / `use_lane` / `exclude_*` / `on_no_candidate`（`error` 或 `use_any`），\
没有「显式拒绝」这个语义。硬塞一条策略会在真实请求路径上产生我们无法预言的影响，\
所以这里如实报不支持，而不是插一条看起来对、实际不知道会发生什么的策略。";

/// 紧急开关。**每个模式都落到一个真实存在的机制上**：
///
/// | 模式 | 真实效果 |
/// |---|---|
/// | `pause_all` | 关网关准入闸门（新请求被 TA-1005 拒绝；**在途不取消**） |
/// | `direct_all` | 把默认出口切到一条启用的 `direct` 线路（并记住原值）+ 开闸门 |
/// | `restore` | 开闸门 + 把默认出口还回 `direct_all` 之前的值 |
/// | `vision_only` | **如实报不支持**（见 [`VISION_ONLY_UNSUPPORTED`]） |
///
/// 三个可用模式是**互斥的模式选择**（不是几个独立开关的堆叠）：切换模式时会把上一个模式的
/// 效果收掉（例如从 `pause_all` 切到 `direct_all` 会开闸门，否则"全部直连"实际是"什么都不收"）。
///
/// ⚠️ 模式本身**只存在运行时**（`AppState` 是别的写者的文件，本包不能给它加字段）。
/// 它是"用户刚按下的一个开关"，重启后回到 `restore` —— 这是本包**已知的缺口**，
/// 不假装它持久化了。
pub async fn set_emergency(state: &AppState, mode: EmergencyMode) -> IpcResult<()> {
    match mode {
        EmergencyMode::PauseAll => {
            let gate = state.gateway_gate().ok_or_else(|| {
                IpcError::from(TaError::Internal(
                    "网关尚未装配，无法暂停（启动编排可能失败了）".to_string(),
                ))
            })?;
            gate.pause();
            set_emergency_runtime(EmergencyMode::PauseAll, None);
            tracing::info!(
                "紧急开关：全部暂停（网关不再接受新请求，在途不取消，/healthz 不受影响）"
            );
        }
        EmergencyMode::Restore => {
            if let Some(gate) = state.gateway_gate() {
                gate.resume();
            }
            let saved =
                EMERGENCY.read().unwrap_or_else(|e| e.into_inner()).saved_default_lane.clone();
            if let Some(prev) = saved {
                let doc0 = state.doc();
                if doc0.net.default_lane != prev {
                    let mut doc = (*doc0).clone();
                    doc.net.default_lane = prev.clone();
                    persist_config(state, &doc, &["net"], "tray-emergency-restore").await?;
                    let after = read_back(state).await?;
                    if after.net.default_lane != prev {
                        return Err(IpcError::from(TaError::Internal(
                            "紧急开关恢复默认出口后回读不一致".to_string(),
                        )));
                    }
                }
            }
            set_emergency_runtime(EmergencyMode::Restore, None);
            tracing::info!("紧急开关：已恢复正常（闸门已开，默认出口已还回）");
        }
        EmergencyMode::DirectAll => {
            let doc0 = state.doc();
            let direct = doc0
                .lanes
                .iter()
                .find(|l| l.enabled && matches!(l.kind, LaneKind::Direct))
                .map(|l| l.id.clone())
                .ok_or_else(|| {
                    IpcError::from(TaError::Internal(
                        "「全部直连」需要一个**启用的 direct 线路**，但配置里没有：\
（先把一条直连线路加进来，或改用「全部暂停」）"
                            .to_string(),
                    ))
                })?;
            if doc0.net.default_lane.as_ref() == Some(&direct) {
                set_emergency_runtime(
                    EmergencyMode::DirectAll,
                    Some(doc0.net.default_lane.clone()),
                );
            } else {
                let prev = doc0.net.default_lane.clone();
                let mut doc = (*doc0).clone();
                doc.net.default_lane = Some(direct.clone());
                persist_config(state, &doc, &["net"], "tray-emergency-direct").await?;
                let after = read_back(state).await?;
                if after.net.default_lane.as_ref() != Some(&direct) {
                    return Err(IpcError::from(TaError::Internal(
                        "紧急开关切换默认出口后回读不一致".to_string(),
                    )));
                }
                set_emergency_runtime(EmergencyMode::DirectAll, Some(prev));
            }
            // 紧急开关是**互斥的模式选择**，不是几个独立开关的堆叠：
            // 从 pause_all 切到 direct_all 时，暂停必须被解除（否则"全部直连"实际是"什么都不收"）。
            if let Some(gate) = state.gateway_gate() {
                gate.resume();
            }
            tracing::info!(
                lane_id = %direct.as_str(),
                "紧急开关：全部直连（默认出口已切到直连线路；显式指定 use_lane 的策略仍会覆盖它）"
            );
        }
        EmergencyMode::VisionOnly => {
            return Err(IpcError::from(TaError::Internal(VISION_ONLY_UNSUPPORTED.to_string())));
        }
    }
    Ok(())
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn tray_emergency(state: State<'_, AppState>, mode: EmergencyMode) -> IpcResult<()> {
    set_emergency(&state, mode).await
}

/// 当前托盘状态（含菜单状态的来源：`tray.rs` 的 `build_menu_spec` 用的是同一份快照）。
///
/// 状态色与 tooltip **直接取托盘自己的**（`crate::tray::tray_state`）：
/// 命令与托盘图标必须说同一句话，否则用户会看到"图标是红的、面板说正常"。
#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn tray_state(app: tauri::AppHandle, state: State<'_, AppState>) -> IpcResult<TrayState> {
    let snapshot = crate::tray::tray_state(&app);
    let doc = state.doc();
    let health = match doc.net.default_lane.clone() {
        Some(id) => state.health.latest(&id).await.ok().flatten(),
        None => None,
    };
    Ok(tray_view(
        snapshot.status,
        snapshot.tooltip,
        snapshot.gateway_running,
        &doc,
        health.as_ref(),
        emergency_mode(),
    ))
}

/// 切换当前生效的策略：把目标策略的 `priority` 调到**比其它已启用策略都小**。
///
/// 为什么不改 `enabled`：那会把别的策略真的停用，用户切回来时还得一条条恢复。
/// 只调 priority（并保留原值语义"数字小的先求值"）是最小、可逆、可读回来验证的改法。
pub async fn set_active_policy(state: &AppState, policy_id: &str) -> IpcResult<()> {
    let doc0 = state.doc();
    let idx = doc0.policies.iter().position(|p| p.id.as_str() == policy_id).ok_or_else(|| {
        IpcError::from(TaError::MalformedRequest(format!("策略不存在：{policy_id}")))
    })?;
    if !doc0.policies[idx].enabled {
        return Err(IpcError::from(TaError::MalformedRequest(format!(
            "策略 {policy_id} 已停用，先启用它再切换（停用的策略永远不会被求值）"
        ))));
    }

    let min_other = doc0
        .policies
        .iter()
        .enumerate()
        .filter(|(i, p)| *i != idx && p.enabled)
        .map(|(_, p)| p.priority)
        .min();

    let mut doc = (*doc0).clone();
    if let Some(m) = min_other {
        if doc.policies[idx].priority >= m {
            doc.policies[idx].priority = m.saturating_sub(1);
        }
    }
    persist_config(state, &doc, &["policies"], "tray-active-policy").await?;

    let after = read_back(state).await?;
    let got = active_policy(&after).map(|p| p.id.as_str().to_string());
    if got.as_deref() != Some(policy_id) {
        return Err(IpcError::from(TaError::Internal(format!(
            "切换策略后回读不一致：生效的是 {got:?}，期望 {policy_id}"
        ))));
    }
    tracing::info!(policy_id = policy_id, "托盘：当前生效策略已切换（配置已回读确认）");
    Ok(())
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn tray_set_active_policy(
    state: State<'_, AppState>,
    policy_id: String,
) -> IpcResult<()> {
    set_active_policy(&state, &policy_id).await
}

/// 切换全局默认出口（`net.default_lane`）。
pub async fn set_global_lane(state: &AppState, lane_id: &str) -> IpcResult<()> {
    let doc0 = state.doc();
    let lane = doc0.lanes.iter().find(|l| l.id.as_str() == lane_id).ok_or_else(|| {
        IpcError::from(TaError::MalformedRequest(format!("线路不存在：{lane_id}")))
    })?;
    if !lane.enabled {
        return Err(IpcError::from(TaError::MalformedRequest(format!(
            "线路 {lane_id} 已停用，不能作为全局默认出口"
        ))));
    }
    let target = lane.id.clone();
    let mut doc = (*doc0).clone();
    doc.net.default_lane = Some(target.clone());
    persist_config(state, &doc, &["net"], "tray-global-lane").await?;

    let after = read_back(state).await?;
    if after.net.default_lane.as_ref().map(|l| l.as_str()) != Some(lane_id) {
        return Err(IpcError::from(TaError::Internal(format!(
            "切换全局出口后回读不一致：期望 {lane_id}，库里是 {:?}",
            after.net.default_lane.as_ref().map(|l| l.as_str())
        ))));
    }
    tracing::info!(lane_id = lane_id, "托盘：全局默认出口已切换（配置已回读确认）");
    Ok(())
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn tray_set_global_lane(state: State<'_, AppState>, lane_id: String) -> IpcResult<()> {
    set_global_lane(&state, &lane_id).await
}

// ================================================================ 命令注册
//
// 注册在 `src-tauri/src/commands/mod.rs` 的 `collect_commands![…]` 里（唯一点）。

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    use ta_core::diag::sanitize::find_secret as find_secret_in;
    use ta_secrets::mock::MockStore;

    /// 假密钥统一带 `ta-allow-fake-secret` 标记（避免撞上 pre-commit 的密钥扫描）。
    const FAKE_KEY: &str = "sk-ta-allow-fake-secret-9f3a2b1c4d5e";
    const HEADER_KEY: &str = "sk-ta-allow-fake-header-112233445566"; // ta-allow-fake-secret

    // ---------------------------------------------------------- 临时目录 / 测试状态

    /// 测试用临时目录（带 Drop 清理）。刻意不用 `tempfile`：它不是本 crate 的依赖，
    /// 而 `src-tauri/Cargo.toml` 不在本包的可改范围里。
    struct Scratch {
        path: PathBuf,
    }

    impl Scratch {
        fn new(tag: &str) -> Self {
            static SEQ: AtomicUsize = AtomicUsize::new(0);
            let n = SEQ.fetch_add(1, Ordering::SeqCst);
            let path =
                std::env::temp_dir().join(format!("ta-t2b-{tag}-{}-{n}", std::process::id()));
            std::fs::create_dir_all(&path).expect("创建临时目录");
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    /// 真开一个库（跑真迁移）—— 写命令的"读回来一致"必须对着真库断言。
    async fn test_state(tag: &str) -> (Scratch, AppState) {
        let scratch = Scratch::new(tag);
        let state =
            AppState::bootstrap(scratch.path().join("db.sqlite")).await.expect("bootstrap 真库");
        (scratch, state)
    }

    /// ★ `log_export` 的回执必须描述**磁盘上真实发生的事**（task-25）。
    ///
    /// 传**目录**进去：后端会自己补 `tokenagency-diagnostic-<时间戳>.zip`。
    /// 所以"把入参原样回传"一定是错的 —— 这条测试就是钉这一点，
    /// 外加"字节数必须等于磁盘真实字节数"（不是 0、不是估算）。
    #[tokio::test]
    async fn log_export_receipt_describes_the_file_actually_written() {
        let (scratch, state) = test_state("export-receipt").await;
        let log_dir = scratch.path().join("logs");
        std::fs::create_dir_all(&log_dir).expect("建日志目录");
        std::fs::write(log_dir.join("tokenagency.log"), "INFO 一行日志\n").expect("写日志");

        let requested = scratch.path().join("exports");
        std::fs::create_dir_all(&requested).expect("建导出目录");

        let receipt =
            log_export_with_receipt(&state, LogQuery::default(), &requested, Some(&log_dir))
                .await
                .expect("导出应成功");

        let actual = PathBuf::from(&receipt.path);
        assert!(actual.is_file(), "回执说的文件必须真的存在：{}", actual.display());
        assert_ne!(
            actual, requested,
            "回执必须是后端解析后的**最终路径**，不是入参（入参是目录，不是文件）"
        );
        assert!(actual.starts_with(&requested), "文件应落在请求的目录里：{}", actual.display());

        let on_disk = std::fs::metadata(&actual).expect("读元数据").len();
        assert_eq!(receipt.bytes, on_disk, "回执字节数必须等于磁盘上的真实字节数");
        assert!(receipt.bytes > 0, "一个诊断包不可能 0 字节");
    }

    async fn put_doc(state: &AppState, doc: &ConfigDoc) {
        state.config.save(doc).await.expect("保存测试配置");
        state.set_doc(doc.clone());
    }

    /// **从库里读**（不是内存快照）：断言"写进去"唯一可信的证据。
    async fn db_doc(state: &AppState) -> ConfigDoc {
        state.config.load().await.expect("从库里读配置")
    }

    fn doc_from_yaml(yaml: &str) -> ConfigDoc {
        ta_core::config::from_yaml(yaml).expect("测试 YAML 必须能解析")
    }

    // ---------------------------------------------------------- 假自启动后端

    #[derive(Clone, Default)]
    struct FakeAutostart {
        registered: Arc<Mutex<Option<String>>>,
        writes: Arc<AtomicUsize>,
        removes: Arc<AtomicUsize>,
        supported: bool,
        fail_write: bool,
    }

    impl FakeAutostart {
        fn new() -> Self {
            Self { supported: true, ..Default::default() }
        }
        fn unsupported() -> Self {
            Self { supported: false, ..Default::default() }
        }
        fn failing_write() -> Self {
            Self { supported: true, fail_write: true, ..Default::default() }
        }
        /// 模拟"用户在别处（任务管理器/注册表/清理软件）动过手脚"。
        fn pre_registered(&self, cmd: &str) {
            *self.registered.lock().unwrap() = Some(cmd.to_string());
        }
        fn write_count(&self) -> usize {
            self.writes.load(Ordering::SeqCst)
        }
        fn remove_count(&self) -> usize {
            self.removes.load(Ordering::SeqCst)
        }
        fn registered(&self) -> Option<String> {
            self.registered.lock().unwrap().clone()
        }
    }

    impl crate::autostart::AutostartRead for FakeAutostart {
        fn mechanism(&self) -> &'static str {
            "windows-run-key"
        }
        fn support(&self) -> std::result::Result<(), String> {
            if self.supported {
                Ok(())
            } else {
                Err("本平台不支持（测试）".to_string())
            }
        }
        fn read(&self) -> std::result::Result<Option<String>, String> {
            Ok(self.registered())
        }
    }

    impl crate::autostart::AutostartWrite for FakeAutostart {
        fn write(&self, command: &str) -> std::result::Result<(), String> {
            self.writes.fetch_add(1, Ordering::SeqCst);
            if self.fail_write {
                return Err("模拟写入失败".to_string());
            }
            *self.registered.lock().unwrap() = Some(command.to_string());
            Ok(())
        }
        fn remove(&self) -> std::result::Result<(), String> {
            self.removes.fetch_add(1, Ordering::SeqCst);
            *self.registered.lock().unwrap() = None;
            Ok(())
        }
    }

    const EXE: &str = r"C:\Program Files\TokenAgency\tokenagency.exe";

    fn ctl(backend: FakeAutostart) -> AutostartController {
        AutostartController::new(Box::new(backend), EXE)
    }

    // ================================================================ ① 自启动

    #[tokio::test]
    async fn autostart_read_reports_the_system_fact_and_never_writes() {
        let (_s, state) = test_state("auto-read").await;
        // 配置想要开，但系统里根本没有这个条目（用户手动清过）
        let mut doc = ConfigDoc::default();
        doc.system_integration.autostart = true;
        put_doc(&state, &doc).await;

        let backend = FakeAutostart::new();
        let out = read_autostart(&state, &ctl(backend.clone()));

        assert!(out.desired, "desired 必须来自配置");
        assert!(!out.actual, "actual 必须来自系统（不许回显配置值）");
        assert!(out.drifted, "配置开 / 系统没开 → 必须报漂移");
        assert_eq!(out.mechanism, "windows-run-key");
        assert!(!out.command_matches);
        assert!(out.expected_command.starts_with('"'), "{}", out.expected_command);
        assert_eq!(backend.write_count(), 0, "读状态绝不许写系统（E3）");
        assert_eq!(backend.remove_count(), 0, "读状态更不许删条目（E3）");
    }

    #[tokio::test]
    async fn autostart_read_reports_drift_when_the_user_added_the_entry_manually() {
        let (_s, state) = test_state("auto-read2").await;
        // 配置说关，系统里却有（用户自己在别处加的）
        let backend = FakeAutostart::new();
        backend.pre_registered("someone-else.exe --autostart");
        let out = read_autostart(&state, &ctl(backend.clone()));

        assert!(!out.desired && out.actual && out.drifted);
        assert!(!out.command_matches, "指向别的 exe → 命令行不匹配");
        assert_eq!(backend.remove_count(), 0, "就算多出条目也不许自己删（E3）");
    }

    #[tokio::test]
    async fn set_autostart_writes_system_then_config_and_reads_back_consistent() {
        let (_s, state) = test_state("auto-set-on").await;
        let backend = FakeAutostart::new();
        let controller = ctl(backend.clone());

        let out = set_autostart(&state, &controller, true, Some(AutostartMode::Window), Some(15))
            .await
            .expect("注册应成功");

        assert!(out.actual, "written → 必须重新读系统");
        assert!(!out.drifted);
        assert!(out.command_matches);
        assert_eq!(
            out.registered_command.as_deref(),
            Some(controller.command_line(&AutostartDesired::new(true, AutostartMode::Window, 15)))
                .as_deref()
        );
        assert_eq!(backend.write_count(), 1);

        // 读回来一致：**从库里读**
        let doc = db_doc(&state).await;
        assert!(doc.system_integration.autostart);
        assert_eq!(doc.system_integration.autostart_mode, AutostartMode::Window);
        assert_eq!(doc.system_integration.autostart_delay_s, 15);

        // 关掉
        let out = set_autostart(&state, &controller, false, None, None).await.expect("关闭");
        assert!(!out.actual);
        assert!(backend.registered().is_none(), "系统里的条目必须真的没了");
        let doc = db_doc(&state).await;
        assert!(!doc.system_integration.autostart);
        assert_eq!(doc.system_integration.autostart_delay_s, 15, "关掉不该抹掉用户设的延迟");
    }

    #[tokio::test]
    async fn set_autostart_never_reports_success_when_the_system_write_failed() {
        let (_s, state) = test_state("auto-set-fail").await;
        let err = set_autostart(&state, &ctl(FakeAutostart::failing_write()), true, None, None)
            .await
            .expect_err("写入失败必须上抛");
        assert!(err.message.contains("写入开机自启动条目失败"), "{}", err.message);

        // 关键：配置一个字节都没动 —— 不出现"配置说开、系统没开"
        let doc = db_doc(&state).await;
        assert!(!doc.system_integration.autostart);
    }

    #[tokio::test]
    async fn set_autostart_rejects_an_out_of_range_delay_without_touching_anything() {
        let (_s, state) = test_state("auto-set-delay").await;
        let backend = FakeAutostart::new();
        let err = set_autostart(&state, &ctl(backend.clone()), true, None, Some(301))
            .await
            .expect_err(">300 必须被拒");
        assert_eq!(err.code, "TA-5001");
        assert_eq!(
            err.details
                .as_ref()
                .and_then(|d| d.get(0))
                .and_then(|e| e.get("path"))
                .and_then(|p| p.as_str()),
            Some("system_integration.autostart_delay_s")
        );
        assert_eq!(backend.write_count(), 0, "校验不过就不该碰系统");
        assert!(!db_doc(&state).await.system_integration.autostart);
    }

    #[tokio::test]
    async fn repair_autostart_aligns_the_system_without_touching_the_config() {
        let (_s, state) = test_state("auto-repair").await;
        let backend = FakeAutostart::new();
        let controller = ctl(backend.clone());

        // ① 配置关、系统里有别人加的条目 → 对齐 = 删掉
        backend.pre_registered("someone-else.exe");
        let out = repair_autostart(&state, &controller).await.expect("对齐（删）");
        assert!(!out.actual && !out.drifted);
        assert!(backend.registered().is_none());
        assert!(!db_doc(&state).await.system_integration.autostart, "repair 不许动配置");

        // ② 配置开、系统里没有 → 对齐 = 按期望注册
        let mut doc = ConfigDoc::default();
        doc.system_integration.autostart = true;
        doc.system_integration.autostart_delay_s = 20;
        put_doc(&state, &doc).await;
        let out = repair_autostart(&state, &controller).await.expect("对齐（注册）");
        assert!(out.actual && !out.drifted && out.command_matches);
        assert_eq!(
            backend.registered().as_deref(),
            Some(
                controller
                    .command_line(&AutostartDesired::new(true, AutostartMode::Tray, 20))
                    .as_str()
            )
        );
        assert!(db_doc(&state).await.system_integration.autostart);
    }

    #[tokio::test]
    async fn repair_autostart_detects_a_stale_command_line_even_when_actual_is_true() {
        // "在，但指向别处/参数过时" —— drifted 看不出来，command_matches 能。
        let (_s, state) = test_state("auto-repair-stale").await;
        let backend = FakeAutostart::new();
        let controller = ctl(backend.clone());
        let stale = AutostartDesired::new(true, AutostartMode::Tray, 0);
        backend.pre_registered(&controller.command_line(&stale));
        let mut doc = ConfigDoc::default();
        doc.system_integration.autostart = true;
        doc.system_integration.autostart_delay_s = 60; // 用户改了延迟 → 命令行过期
        put_doc(&state, &doc).await;

        let before = read_autostart(&state, &controller);
        assert!(before.actual && !before.drifted, "只看`在不在`是看不出这种漂移的");
        assert!(!before.command_matches, "但命令行的不一致必须看得出来");

        let out = repair_autostart(&state, &controller).await.expect("重注册");
        assert!(out.command_matches, "repair 必须把命令行也刷新");
        assert!(out.expected_command.contains("--autostart-delay=60"), "{}", out.expected_command);
    }

    #[tokio::test]
    async fn unsupported_platform_reports_the_reason_and_refuses_to_enable() {
        let (_s, state) = test_state("auto-unsupported").await;
        let controller = ctl(FakeAutostart::unsupported());

        let out = read_autostart(&state, &controller);
        assert!(!out.actual && !out.drifted, "不支持不算漂移");
        assert!(out.unsupported_reason.is_some());

        let err = set_autostart(&state, &controller, true, None, None)
            .await
            .expect_err("不支持时必须报错");
        assert!(err.message.contains("不支持"), "{}", err.message);
        assert!(!db_doc(&state).await.system_integration.autostart);
        // 关闭是安全的（本来就没有）
        set_autostart(&state, &controller, false, None, None).await.expect("关闭不该报错");
    }

    // ================================================================ ② 检查更新

    #[test]
    fn check_update_refuses_to_claim_everything_is_up_to_date() {
        let err = check_update().expect_err("通道没接上就不许回 Ok");
        assert!(err.message.contains("没能检查"), "{}", err.message);
        assert!(err.message.contains("tauri-plugin-updater"), "{}", err.message);
        assert_eq!(err.code, "TA-9001");
    }

    // ================================================================ ④ 日志导出（诊断包）

    fn export_sources(log: &str) -> ExportSources {
        ExportSources {
            log_files: vec![("logs/tokenagency.log".to_string(), log.as_bytes().to_vec())],
            traces: vec![],
            config_yaml: "gateway:\n  local_key: \"${secret:gateway-local-key}\"\n".to_string(),
            env_json: "{\"os\":\"test\"}\n".to_string(),
            meta_json: "{}\n".to_string(),
            error_codes: vec![],
        }
    }

    /// ★ 铁律 1 的反例：**带假密钥的日志导不出来**。
    ///
    /// "先证明会红"：这条测试的前两行先证明"输入里真的有密钥、而且脱敏规则确实认得出它"，
    /// 否则下面的 `Err` 可能来自别的原因（例如路径不合法），测试就没有证明力。
    #[test]
    fn a_log_file_with_a_fake_secret_cannot_be_exported() {
        let raw = format!("2026-02-11T12:00:00Z ERROR ta_gateway: upstream 401 key={FAKE_KEY}\n");
        assert!(
            find_secret_in(&raw).is_some(),
            "假密钥必须能被脱敏规则认出来，否则这条测试没有证明力"
        );

        let err = build_diagnostic_package(&export_sources(&raw), 1_770_000_000_000, "0.1.0")
            .expect_err("含明文密钥的日志必须中止导出，而不是悄悄发出去");
        match err {
            PackageError::Secret { ref name, .. } => assert_eq!(name, "logs/tokenagency.log"),
            other => panic!("应当是 Secret 错误，实际：{other:?}"),
        }
        assert!(!err.to_string().contains(FAKE_KEY), "错误消息里不许回显密钥：{err}");
    }

    /// 同一个 builder 的**正例**：脱敏后的日志必须能出包，而且包里搜不到密钥。
    #[test]
    fn a_redacted_log_exports_clean_and_the_package_rechecks_itself() {
        let raw = format!("2026-02-11T12:00:00Z INFO ta_gateway: 上游 401 key={FAKE_KEY}\n");
        let cleaned = sanitize::sanitize(&raw);
        assert!(!cleaned.contains(FAKE_KEY), "脱敏后不该还有密钥：{cleaned}");
        assert!(cleaned.contains("ta_gateway"), "行不能被整条吃掉：{cleaned}");

        let archive =
            build_diagnostic_package(&export_sources(&cleaned), 1_770_000_000_000, "0.1.0")
                .expect("脱敏后的日志必须能出包");

        let text = String::from_utf8_lossy(&archive);
        assert!(!text.contains(FAKE_KEY), "诊断包字节里出现了密钥");
        assert_eq!(find_secret_in(&text), None, "整包全文扫描必须零命中");

        let entries =
            diag_package::verify_and_scan_archive(&archive).expect("自己写的包必须能解开");
        let names: Vec<&str> = entries.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "manifest.json",
                "error-summary.json",
                "logs/tokenagency.log",
                "traces/requests.jsonl",
                "config/config.yaml",
                "env.json",
                "export.json",
            ]
        );
    }

    #[test]
    fn a_plaintext_key_in_the_config_is_sanitized_inside_the_package() {
        let mut src = export_sources("nothing sensitive here\n");
        src.config_yaml =
            format!("providers:\n  - id: p-openai\n    auth:\n      secret: {FAKE_KEY}\n");
        let archive = build_diagnostic_package(&src, 1, "0.1.0")
            .expect("配置里的明文应当在组包前被脱敏（这一条不该中止导出）");
        let text = String::from_utf8_lossy(&archive);
        assert!(!text.contains(FAKE_KEY), "配置里的明文进了包");
        assert!(text.contains("***"), "密钥值应被替换成 ***\n{text}");
        assert!(text.contains("p-openai"), "结构必须保留（排查要看的就是结构）");
    }

    /// 请求日志在进包**之前**就已经被脱敏（值没了）。这一点单独证明一次：
    /// 只断言"包里没有密钥"是证明不了"哪一步去掉的"。
    #[test]
    fn trace_rows_are_redacted_before_they_are_packed() {
        let error = format!("upstream said Authorization: Bearer {FAKE_KEY}");
        assert!(find_secret_in(&error).is_some(), "输入里必须真的有敏感形态");
        let rows = vec![trace_row_with_error(error)];
        let jsonl = traces_jsonl(&rows);
        assert!(!jsonl.contains(FAKE_KEY), "脱敏后不该还有密钥：{jsonl}");
        assert!(jsonl.contains("req_1") && jsonl.contains("gpt-4o"), "行本身要留下：{jsonl}");
    }

    /// ★ **已知的 ta-core 不对称**（不在本包可改范围，已写进模块文档与交付报告）：
    /// `sanitize` 刻意保留 `Authorization: Bearer` 这种"方案词"（它说方案词本身不是秘密），
    /// 而打包后扫描用的 `sanitize::find_secret` 会**无条件**把认证头形态算命中 ——
    /// 于是"已脱敏"的认证头行仍然会让整个导出被拒。
    ///
    /// 这条测试对**两种未来**都成立（ta-core 修好前/后），断言的都是真正要守住的不变量：
    /// 包里不许有密钥、错误里不许回显密钥、拒绝时要指名道姓（哪个条目、哪一类形态）。
    #[test]
    fn a_redacted_auth_header_in_a_trace_row_never_puts_the_key_in_the_package() {
        let rows =
            vec![trace_row_with_error(format!("upstream said Authorization: Bearer {FAKE_KEY}"))];
        let mut src = export_sources("clean line\n");
        src.traces = rows;

        match build_diagnostic_package(&src, 1, "0.1.0") {
            Ok(archive) => {
                let text = String::from_utf8_lossy(&archive);
                assert!(!text.contains(FAKE_KEY), "包里出现了密钥");
            }
            Err(e) => {
                match &e {
                    PackageError::Secret { name, kind, .. } => {
                        assert_eq!(name, "traces/requests.jsonl");
                        assert_eq!(*kind, ta_core::diag::sanitize::SecretKind::AuthHeader);
                    }
                    other => panic!("应当是 Secret 错误，实际：{other:?}"),
                }
                assert!(
                    !e.to_string().contains(FAKE_KEY),
                    "命中的是脱敏后的形态，不是密钥本身：{e}"
                );
            }
        }
    }

    /// 不带认证头形态的请求日志：脱敏之后必须能出包（这条是把"能导出"钉住的那一半）。
    #[test]
    fn a_plain_token_in_a_trace_row_exports_clean() {
        let rows = vec![trace_row_with_error(format!("upstream 401 for key {FAKE_KEY}"))];
        assert!(find_secret_in(&rows[0].error.clone().unwrap()).is_some());
        let mut src = export_sources("clean line\n");
        src.traces = rows;

        let archive = build_diagnostic_package(&src, 1, "0.1.0").expect("脱敏后的行必须能出包");
        let text = String::from_utf8_lossy(&archive);
        assert!(!text.contains(FAKE_KEY), "密钥进了包");
        assert!(text.contains("req_1"), "行本身要留下");
    }

    fn trace_row_with_error(error: String) -> TraceRow {
        TraceRow {
            request_id: "req_1".into(),
            ts_ms: 1,
            client: None,
            model: "gpt-4o".into(),
            protocol_in: "openai".into(),
            protocol_out: "openai".into(),
            stream: false,
            policy_id: None,
            group_id: None,
            chosen_channel: None,
            attempts: 1,
            status: 401,
            outcome: "failed".into(),
            total_ms: 3,
            ttfb_ms: None,
            tokens: 0,
            error: Some(error),
            mode: "meta".into(),
            has_body: false,
            body_bytes: 0,
        }
    }

    #[tokio::test]
    async fn log_export_writes_a_clean_package_and_leaves_nothing_when_rejected() {
        let (scratch, state) = test_state("export-e2e").await;
        let log_dir = scratch.path().join("logs");
        std::fs::create_dir_all(&log_dir).expect("建日志目录");

        // ① 干净的日志 → 出包
        std::fs::write(
            log_dir.join("tokenagency.log"),
            format!(
                "2026-02-11T12:00:00Z INFO ta_gateway: ok key={}\n",
                sanitize::sanitize(FAKE_KEY)
            ),
        )
        .expect("写日志");
        let target = scratch.path().join("out.zip");
        let written = log_export_to(&state, LogQuery::default(), &target, Some(&log_dir))
            .await
            .expect("干净日志必须能导出");
        assert_eq!(written, target);
        let bytes = std::fs::read(&written).expect("读包");
        assert!(!String::from_utf8_lossy(&bytes).contains(FAKE_KEY));
        diag_package::verify_and_scan_archive(&bytes).expect("包必须可解且零命中");

        // ② 同一目录里出现一把明文密钥 → 导出被拒，且**不留下半个包**
        std::fs::write(log_dir.join("tokenagency.log"), format!("key={FAKE_KEY}\n"))
            .expect("写脏日志");
        std::fs::remove_file(&target).expect("清掉上一次的包");
        let err = log_export_to(&state, LogQuery::default(), &target, Some(&log_dir))
            .await
            .expect_err("含明文密钥的日志必须被拒");
        assert!(err.message.contains("诊断包导出被拒绝"), "{}", err.message);
        assert!(!err.message.contains(FAKE_KEY), "错误消息里不许回显密钥");
        assert!(!target.exists(), "被拒时不许留下半个包");
    }

    #[test]
    fn collect_log_files_skips_previous_packages_and_uses_archive_names() {
        let dir = Scratch::new("collect");
        std::fs::write(dir.path().join("tokenagency.log"), "a\n").unwrap();
        std::fs::write(dir.path().join("tokenagency.20260101-000000.log"), "b\n").unwrap();
        std::fs::write(dir.path().join("previous-export.zip"), "PK\u{3}\u{4}\n").unwrap();

        let files = collect_log_files(dir.path());
        let names: Vec<&str> = files.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["logs/tokenagency.20260101-000000.log", "logs/tokenagency.log"]);
        // 不存在的目录 → 空列表（不是错误）
        assert!(collect_log_files(&dir.path().join("nope")).is_empty());
    }

    #[test]
    fn export_target_uses_a_generated_name_inside_an_existing_directory() {
        let dir = Scratch::new("target");
        let inside = resolve_export_target(dir.path(), 1_770_000_000_000);
        assert_eq!(
            inside.file_name().and_then(|n| n.to_str()),
            Some("tokenagency-diagnostic-1770000000000.zip")
        );
        let explicit = dir.path().join("my.zip");
        assert_eq!(resolve_export_target(&explicit, 1), explicit);
    }

    // ================================================================ ⑤ 首启向导

    #[test]
    fn detect_prefers_the_configured_kernel_path_and_reports_what_it_checked() {
        let mut doc = ConfigDoc::default();
        doc.net.kernel.path = Some(r"C:\tools\mihomo.exe".into());
        let exists = |p: &Path| p == Path::new(r"C:\tools\mihomo.exe");
        let out = detect_onboarding(&doc, &exists, &|_| false, &|_| None, None);

        assert!(out.kernel_found);
        assert_eq!(out.kernel_path.as_deref(), Some(r"C:\tools\mihomo.exe"));
        assert_eq!(out.kernel_checked, vec![r"C:\tools\mihomo.exe".to_string()]);
        assert!(!out.existing_config);
        assert!(out.suggested_providers.is_empty());
    }

    #[test]
    fn detect_falls_back_to_the_app_dir_then_path_and_says_where_it_looked() {
        let doc = ConfigDoc::default();
        let exists = |p: &Path| p == Path::new(r"C:\app\mihomo.exe");
        let out =
            detect_onboarding(&doc, &exists, &|_| false, &|_| None, Some(Path::new(r"C:\app")));
        assert!(out.kernel_found);
        assert_eq!(out.kernel_path.as_deref(), Some(r"C:\app\mihomo.exe"));
        assert!(
            out.kernel_checked.iter().any(|c| c.ends_with("mihomo.exe")),
            "{:?}",
            out.kernel_checked
        );

        // 什么都不在 → 必须如实说没找到，并列出查过的位置
        let none = detect_onboarding(&doc, &|_| false, &|_| false, &|_| None, None);
        assert!(!none.kernel_found);
        assert!(none.kernel_path.is_none());
        assert!(
            none.kernel_checked.iter().all(|c| c.starts_with("PATH:")),
            "{:?}",
            none.kernel_checked
        );
        assert_eq!(none.kernel_checked.len(), KERNEL_BINARIES.len());

        // PATH 命中
        let which =
            |name: &str| (name == "clash-meta").then(|| PathBuf::from(r"C:\bin\clash-meta"));
        let hit = detect_onboarding(&doc, &|_| false, &|_| false, &which, None);
        assert_eq!(hit.kernel_path.as_deref(), Some(r"C:\bin\clash-meta"));
    }

    #[test]
    fn detect_suggests_providers_from_env_presence_and_never_reads_values() {
        let mut doc = ConfigDoc::default();
        doc.providers.push(
            from_defaults(serde_json::json!({
                "id": "mine", "name": "我的中转", "protocol": "openai", "base_url": "https://x/v1"
            }))
            .unwrap(),
        );
        let env_has = |k: &str| k == "OPENAI_API_KEY" || k == "DEEPSEEK_API_KEY";
        let out = detect_onboarding(&doc, &|_| false, &env_has, &|_| None, None);

        assert_eq!(out.suggested_providers, vec!["OpenAI", "DeepSeek", "我的中转"]);
        assert!(out.existing_config, "配置里有服务商 → existing_config");
        assert!(!format!("{out:?}").contains(FAKE_KEY), "检测结果里不许出现任何值");
    }

    #[tokio::test]
    async fn apply_onboarding_puts_the_plaintext_in_the_store_and_a_reference_in_config() {
        let (_s, state) = test_state("onboarding-apply").await;
        let store = MockStore::new();
        let input = OnboardingInput {
            kernel_path: Some(r"C:\tools\mihomo.exe".into()),
            providers: vec![OnboardingProvider {
                id: "openai".into(),
                name: "OpenAI".into(),
                protocol: Protocol::Openai,
                base_url: "https://api.openai.com/v1".into(),
                secret: FAKE_KEY.into(),
            }],
            subscriptions: vec![OnboardingSubscription {
                id: "airport-a".into(),
                name: "机场A".into(),
                url: "https://example.com/sub?token=public".into(),
            }],
        };

        let r = apply_onboarding(&state, &store, input.clone()).await.expect("向导应用");
        assert!(r.applied && !r.restart_required);

        let doc = db_doc(&state).await;
        assert_eq!(doc.providers.len(), 1);
        assert_eq!(doc.credentials.len(), 1);
        assert_eq!(doc.credentials[0].secret, "${secret:cred-openai}");
        assert_eq!(doc.proxy_subscriptions.len(), 1);
        assert_eq!(doc.net.kernel.path.as_deref(), Some(r"C:\tools\mihomo.exe"));

        // 明文**只在密钥仓库**里
        assert_eq!(store.snapshot().get("cred-openai").map(String::as_str), Some(FAKE_KEY));
        let yaml = ta_core::config::to_yaml(&doc).expect("序列化");
        assert!(!yaml.contains(FAKE_KEY), "配置里不许有明文密钥：\n{yaml}");
        assert!(state.secret_meta.get("cred-openai").await.unwrap().is_some(), "元数据要登记");

        // 幂等：再来一次不该长出第二份
        apply_onboarding(&state, &store, input).await.expect("二次应用");
        let doc = db_doc(&state).await;
        assert_eq!(
            (doc.providers.len(), doc.credentials.len(), doc.proxy_subscriptions.len()),
            (1, 1, 1)
        );
    }

    #[tokio::test]
    async fn apply_onboarding_keeps_an_existing_reference_instead_of_storing_it() {
        let (_s, state) = test_state("onboarding-ref").await;
        let store = MockStore::new();
        let input = OnboardingInput {
            kernel_path: None,
            providers: vec![OnboardingProvider {
                id: "p1".into(),
                name: "P1".into(),
                protocol: Protocol::Custom,
                base_url: "https://x/v1".into(),
                secret: "${env:MY_KEY}".into(),
            }],
            subscriptions: vec![],
        };
        apply_onboarding(&state, &store, input).await.expect("应用");
        let doc = db_doc(&state).await;
        assert_eq!(doc.credentials[0].secret, "${env:MY_KEY}");
        assert!(store.is_empty(), "引用不是明文，不该往仓库里塞东西");
    }

    #[tokio::test]
    async fn test_all_refuses_honestly_when_no_kernel_is_configured() {
        let (_s, state) = test_state("test-all").await;
        let store = MockStore::new();
        let err =
            test_all_connectivity(&state, &store).await.expect_err("没有内核就不该给一个空矩阵");
        assert!(err.message.contains("没有装配内核"), "{}", err.message);
    }

    #[test]
    fn connectivity_rows_separate_lane_failures_from_secret_failures() {
        let creds = vec![
            CredentialProbe { credential_id: "c-ok".into(), secret_ok: true },
            CredentialProbe { credential_id: "c-bad".into(), secret_ok: false },
        ];
        let lanes = vec![
            LaneProbe { lane_id: "direct".into(), ok: true, latency_ms: Some(3), note: None },
            LaneProbe {
                lane_id: "jp".into(),
                ok: false,
                latency_ms: None,
                note: Some("内核测速失败：超时".into()),
            },
        ];
        let rows = connectivity_rows(&creds, &lanes);
        assert_eq!(rows.len(), 4);
        let get = |c: &str, l: &str| {
            rows.iter().find(|r| r.credential_id == c && r.lane_id == l).expect("行必须存在")
        };
        assert!(get("c-ok", "direct").ok);
        assert_eq!(get("c-ok", "direct").latency_ms, Some(3));
        assert!(!get("c-ok", "jp").ok);
        assert_eq!(get("c-ok", "jp").latency_ms, None, "没测出来就别报数字");
        assert!(!get("c-bad", "direct").ok);
        assert!(get("c-bad", "direct").note.as_deref().unwrap().contains("密钥引用无法解析"));
        assert!(get("c-bad", "jp").note.as_deref().unwrap().contains("内核测速失败"));
        assert!(rows.iter().all(|r| r.status.is_none()), "没发上游请求就不许编 HTTP 状态码");
        assert!(rows.iter().all(|r| r.note.as_deref().unwrap().contains("不向上游发请求")));
    }

    // ================================================================ ⑥ 明文密钥

    #[tokio::test]
    async fn scan_plaintext_reports_masked_previews_only() {
        let (_s, state) = test_state("scan").await;
        let doc = doc_from_yaml(&format!(
            "credentials:\n  - id: c1\n    provider: p\n    label: l\n    secret: \"{FAKE_KEY}\"\n"
        ));
        put_doc(&state, &doc).await;

        let findings: Vec<PlaintextFinding> =
            ta_secrets::scan_plaintext(&state.doc()).into_iter().map(finding_view).collect();
        assert_eq!(findings.len(), 1);
        let f = &findings[0];
        assert_eq!(f.location, "credentials[0].secret");
        assert_eq!(f.kind, "sk_dash");
        assert!(f.migratable);
        assert!(!f.masked.contains(FAKE_KEY), "预览里不许有完整值：{}", f.masked);
        assert_eq!(f.suggested_name, "cred-c1");
    }

    #[tokio::test]
    async fn migrate_secrets_writes_references_and_asserts_readback() {
        let (_s, state) = test_state("migrate").await;
        let doc = doc_from_yaml(&format!(
            "providers:\n  - id: p\n    name: P\n    protocol: openai\n    base_url: https://x/v1\n\
             \x20   headers:\n      x-api-key: \"{HEADER_KEY}\"\n\
             credentials:\n  - id: c1\n    provider: p\n    label: l\n    secret: \"{FAKE_KEY}\"\n"
        ));
        put_doc(&state, &doc).await;

        let store = MockStore::new();
        migrate_secrets(&state, &store, vec![]).await.expect("迁移");

        let after = db_doc(&state).await;
        let yaml = ta_core::config::to_yaml(&after).expect("序列化");
        assert!(!yaml.contains(FAKE_KEY) && !yaml.contains(HEADER_KEY), "库里还有明文：\n{yaml}");
        assert_eq!(after.credentials[0].secret, "${secret:cred-c1}");
        assert_eq!(
            after.providers[0].headers.get("x-api-key").map(String::as_str),
            Some("${secret:p-x-api-key}")
        );
        assert!(store.snapshot().contains_key("cred-c1"));
        assert!(ta_secrets::scan_plaintext(&after).iter().all(|f| !f.migratable));

        let meta = state.secret_meta.get("cred-c1").await.unwrap().expect("元数据要登记");
        assert!(!meta.hint.contains(FAKE_KEY), "元数据提示不许含明文：{}", meta.hint);
    }

    #[tokio::test]
    async fn migrate_secrets_rejects_a_conflicting_name_without_touching_the_config() {
        let (_s, state) = test_state("migrate-conflict").await;
        let doc = doc_from_yaml(&format!(
            "credentials:\n  - id: c1\n    provider: p\n    label: a\n    secret: \"{FAKE_KEY}\"\n\
             \x20 - id: c2\n    provider: p\n    label: b\n    secret: \"{HEADER_KEY}\"\n"
        ));
        put_doc(&state, &doc).await;
        let store = MockStore::new();

        let err = migrate_secrets(&state, &store, vec!["same".into(), "same".into()])
            .await
            .expect_err("同名不同值必须被拒");
        assert!(err.message.contains("冲突"), "{}", err.message);
        assert!(store.is_empty(), "报错时不该写入任何条目");
        // 配置保持原样（还是明文）—— 用户可以改名重试
        let after = db_doc(&state).await;
        assert_eq!(after.credentials[0].secret, FAKE_KEY);
    }

    // ================================================================ ③ 文件监听模式

    #[tokio::test]
    async fn file_watch_mode_needs_a_path_and_persists_what_it_reads_back() {
        let (_s, state) = test_state("file-watch").await;

        // 开着但没路径（库里也没有）→ 字段错误，且库里没动
        let err = set_file_watch_mode(&state, true, None).await.expect_err("必须要求路径");
        assert_eq!(err.code, "TA-5001");
        assert_eq!(
            err.details
                .as_ref()
                .and_then(|d| d.get(0))
                .and_then(|e| e.get("path"))
                .and_then(|p| p.as_str()),
            Some("system_integration.file_watch_path")
        );
        assert!(!db_doc(&state).await.system_integration.file_watch_mode);

        // 给路径 → 落库 + 回读一致
        let r =
            set_file_watch_mode(&state, true, Some("  D:/ta.yaml  ".into())).await.expect("开启");
        assert!(r.applied && r.restart_required && !r.kernel_reload);
        let doc = db_doc(&state).await;
        assert!(doc.system_integration.file_watch_mode);
        assert_eq!(doc.system_integration.file_watch_path.as_deref(), Some("D:/ta.yaml"));

        // 不带路径再开一次 → 沿用已有路径
        set_file_watch_mode(&state, true, None).await.expect("再开一次");
        assert_eq!(
            db_doc(&state).await.system_integration.file_watch_path.as_deref(),
            Some("D:/ta.yaml")
        );

        // 关掉 → 路径清掉
        set_file_watch_mode(&state, false, None).await.expect("关闭");
        let doc = db_doc(&state).await;
        assert!(!doc.system_integration.file_watch_mode);
        assert_eq!(doc.system_integration.file_watch_path, None);
    }

    // ================================================================ ⑦ 托盘

    #[test]
    fn contract_status_covers_four_states_and_does_not_disguise_paused() {
        assert_eq!(contract_status(TrayStatus::Normal), "ok");
        assert_eq!(contract_status(TrayStatus::Degraded), "warn");
        assert_eq!(contract_status(TrayStatus::Error), "error");
        assert_eq!(contract_status(TrayStatus::Paused), "off");

        let mut doc = ConfigDoc::default();
        doc.system_integration.enable_tun = true;
        let v = tray_view(
            TrayStatus::Paused,
            "TokenAgency\n启动中…".into(),
            false,
            &doc,
            None,
            EmergencyMode::Restore,
        );
        assert_eq!(v.status, "off", "冻结契约只有四个取值");
        assert_eq!(v.status_detail, "paused", "但真值不能被抹掉（暂停≠关闭）");
        assert!(v.experimental, "TUN 开着 → 实验模式角标");
        assert_eq!(v.emergency, EmergencyMode::Restore);
        assert!(v.policy_name.is_none() && v.node.is_none() && v.latency_ms.is_none());
    }

    #[test]
    fn tray_view_takes_the_active_policy_and_the_default_lane_health() {
        let doc = doc_from_yaml(
            "net:\n  default_lane: jp\n\
             lanes:\n  - id: jp\n    name: 日本-自动择优\n    kind: subscription_auto\n\
             policies:\n  - id: pol-default\n    name: 默认\n    priority: 100\n\
             \x20 - id: cheap\n    name: 便宜优先\n    priority: 10\n\
             \x20 - id: off\n    name: 停用\n    enabled: false\n    priority: 1\n",
        );
        let health = HealthSample {
            ts_ms: 1,
            lane_id: "jp".into(),
            node: Some("东京-03".into()),
            latency_ms: Some(82),
            ok: true,
            status: Some(200),
        };
        let v = tray_view(
            TrayStatus::Degraded,
            "TokenAgency\n降级".into(),
            true,
            &doc,
            Some(&health),
            EmergencyMode::DirectAll,
        );
        assert_eq!(v.status, "warn");
        assert_eq!(v.active_policy_id.as_deref(), Some("cheap"));
        assert_eq!(v.policy_name.as_deref(), Some("便宜优先"));
        assert_eq!(v.default_lane_id.as_deref(), Some("jp"));
        assert_eq!(v.node.as_deref(), Some("东京-03"));
        assert_eq!(v.latency_ms, Some(82));
        assert!(v.gateway_running);
        assert_eq!(v.emergency, EmergencyMode::DirectAll);
    }

    #[tokio::test]
    async fn set_active_policy_reorders_priority_and_reads_back() {
        let (_s, state) = test_state("tray-policy").await;
        let doc = doc_from_yaml(
            "policies:\n\
             \x20 - id: a\n    name: A\n    priority: 10\n\
             \x20 - id: b\n    name: B\n    priority: 20\n\
             \x20 - id: off\n    name: 停用\n    enabled: false\n    priority: 1\n",
        );
        put_doc(&state, &doc).await;
        assert_eq!(active_policy(&state.doc()).map(|p| p.id.as_str()), Some("a"));

        set_active_policy(&state, "b").await.expect("切到 b");
        let after = db_doc(&state).await;
        assert_eq!(active_policy(&after).map(|p| p.id.as_str()), Some("b"));
        let b = after.policies.iter().find(|p| p.id.as_str() == "b").unwrap();
        let a = after.policies.iter().find(|p| p.id.as_str() == "a").unwrap();
        assert!(b.priority < a.priority, "数字小的先求值：{} vs {}", b.priority, a.priority);

        // 不存在的 / 已停用的都必须被拒，且不改变库里的状态
        assert!(set_active_policy(&state, "nope").await.is_err());
        let err = set_active_policy(&state, "off").await.expect_err("停用策略不能被选中");
        assert!(err.message.contains("已停用"), "{}", err.message);
        assert_eq!(active_policy(&db_doc(&state).await).map(|p| p.id.as_str()), Some("b"));
    }

    #[tokio::test]
    async fn set_global_lane_persists_and_rejects_unknown_or_disabled_lanes() {
        let (_s, state) = test_state("tray-lane").await;
        let doc = doc_from_yaml(
            "lanes:\n\
             \x20 - id: direct\n    name: 直连\n    kind: direct\n\
             \x20 - id: jp\n    name: 日本\n    kind: subscription_auto\n\
             \x20 - id: off\n    name: 停用\n    kind: direct\n    enabled: false\n",
        );
        put_doc(&state, &doc).await;

        set_global_lane(&state, "jp").await.expect("切到 jp");
        assert_eq!(db_doc(&state).await.net.default_lane.as_ref().map(|l| l.as_str()), Some("jp"));
        assert!(set_global_lane(&state, "nope").await.is_err(), "不存在的线路");
        let err = set_global_lane(&state, "off").await.expect_err("停用线路");
        assert!(err.message.contains("已停用"), "{}", err.message);
        assert_eq!(
            db_doc(&state).await.net.default_lane.as_ref().map(|l| l.as_str()),
            Some("jp"),
            "失败不该改配置"
        );
    }

    /// 紧急开关的四个模式放在**同一个测试**里：模式存在模块级运行时状态里，
    /// 拆成多个测试会被并行执行交叉污染（这也是它"不落配置"这一缺口的直接后果）。
    #[tokio::test]
    async fn emergency_switch_lands_on_real_mechanisms_and_restores_them() {
        let (_s, state) = test_state("emergency").await;
        state.set_gateway_gate(Arc::new(ta_gateway::ingress::GatewayGate::new()));
        let doc = doc_from_yaml(
            "net:\n  default_lane: jp\n\
             lanes:\n\
             \x20 - id: direct\n    name: 直连\n    kind: direct\n\
             \x20 - id: jp\n    name: 日本\n    kind: subscription_auto\n",
        );
        put_doc(&state, &doc).await;

        // ① pause_all → 真关闸门（新请求被拒，不是"看起来暂停了"）
        set_emergency(&state, EmergencyMode::PauseAll).await.expect("全部暂停");
        assert!(state.gateway_gate().unwrap().is_paused());
        assert_eq!(emergency_mode(), EmergencyMode::PauseAll);

        // ② direct_all → 默认出口真的切到 direct 线路（落库 + 回读）
        set_emergency(&state, EmergencyMode::DirectAll).await.expect("全部直连");
        assert_eq!(
            db_doc(&state).await.net.default_lane.as_ref().map(|l| l.as_str()),
            Some("direct")
        );
        assert!(
            !state.gateway_gate().unwrap().is_paused(),
            "切到 direct_all 必须把上一模式的暂停收掉"
        );
        assert_eq!(emergency_mode(), EmergencyMode::DirectAll);

        // ③ restore → 闸门开 + 默认出口还回原值
        set_emergency(&state, EmergencyMode::Restore).await.expect("恢复");
        assert!(!state.gateway_gate().unwrap().is_paused());
        assert_eq!(
            db_doc(&state).await.net.default_lane.as_ref().map(|l| l.as_str()),
            Some("jp"),
            "恢复必须把 direct_all 顶掉的默认出口还回去"
        );
        assert_eq!(emergency_mode(), EmergencyMode::Restore);

        // ④ vision_only → 如实报不支持，且**不动配置**
        let err = set_emergency(&state, EmergencyMode::VisionOnly).await.expect_err("必须报不支持");
        assert!(err.message.contains("只允许视觉请求"), "{}", err.message);
        assert_eq!(emergency_mode(), EmergencyMode::Restore);
    }

    #[tokio::test]
    async fn emergency_direct_all_says_so_when_there_is_no_direct_lane() {
        let (_s, state) = test_state("emergency-no-direct").await;
        let doc =
            doc_from_yaml("lanes:\n  - id: jp\n    name: 日本\n    kind: subscription_auto\n");
        put_doc(&state, &doc).await;
        let err = set_emergency(&state, EmergencyMode::DirectAll)
            .await
            .expect_err("没有直连线路时必须报错");
        assert!(err.message.contains("direct"), "{}", err.message);
        assert_eq!(db_doc(&state).await.net.default_lane, None, "失败不该改配置");
    }

    #[tokio::test]
    async fn emergency_pause_all_needs_an_assembled_gateway() {
        let (_s, state) = test_state("emergency-no-gate").await;
        let err =
            set_emergency(&state, EmergencyMode::PauseAll).await.expect_err("网关没装配就必须报错");
        assert!(err.message.contains("网关尚未装配"), "{}", err.message);
    }
}
