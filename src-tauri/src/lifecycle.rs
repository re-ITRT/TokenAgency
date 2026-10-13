//! 生命周期编排：[12 §5 T2] ④⑤ —— 启动编排、关窗退托盘、单实例、通知去重。
//!
//! 自启动的**系统读写**在 [`crate::autostart`]（单独一个文件，便于单独审查）；
//! 这里只负责"启动时怎么用这些参数"。
//!
//! # 启动顺序（[05 §2] 骨架屏 → 内核就绪 → 转绿）
//!
//! ```text
//! on_setup()            解析命令行 → 隐藏窗口（mode=tray）→ 装 QuitGuard/通知中心 → 广播 Skeleton
//!   ↓
//! tray::init() + tray::set_source()   托盘先亮起来（此刻状态来自真实数据源）
//!   ↓
//! BootOrchestrator::run()             延迟 → 拉内核 → 等内核就绪 → 拉网关 → 等网关监听 → Ready
//! ```
//!
//! **本文件只有编排逻辑**（顺序 / 超时 / 降级），所以它能用假依赖离线测。
//! 生产装配（怎么造内核、阶段怎么广播、托盘数据从哪来）在 [`crate::boot`]。
//!
//! **转绿的判据是"真的就绪"，不是"调用过函数"**：`wait_kernel_ready` 只认内核自己给出的
//! `Ready`（那已经是带 provider 判据的状态），`wait_gateway_ready` 只认网关**真的在监听**。
//! 任何一环没就绪都收敛成 `Degraded` 并把可读原因交给 UI —— 绝不先亮绿灯。
//!
//! # 启动延迟为什么由我们自己做
//!
//! autostart 的条目只是"开机时跑这个命令行"，操作系统**没有**"延迟 N 秒再跑"这种能力
//! （[12 §5 T2] ④ 明确要求自己实现）。所以延迟写成命令行参数 `--autostart-delay=<s>`，
//! 进程起来后先睡再拉内核 —— 这样"开机瞬间磁盘/网络都很忙"时不会和系统抢资源。
//! 参数范围 0–300（[02 §2.10]），**命令行里的值也要再夹一次**（注册表可能被手改）。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, Runtime, Window, WindowEvent};

use ta_core::clock::{Clock, SystemClock};
use ta_core::domain::behavior::MAX_AUTOSTART_DELAY_S;
/// 启动时要不要把主窗口藏进托盘（[docs/33]）。
///
/// **`TA_E2E` 在场时永不隐藏**：真机端到端需要一个**可见**窗口 —— 驱动器只能驱动一个真正
/// 渲染出来的 WebView；隐藏窗口时它拿到的是**空页面**（真机上表现为"窗口起来了但页面文本为空"，
/// 排查了很久才定位到这里）。
///
/// 抽成纯函数是为了能断言：`e2e = true` 必须压过"托盘模式"。
fn hide_window_for_start(args: &StartupArgs, e2e: bool) -> bool {
    !e2e && args.hide_window_at_start()
}

use ta_core::domain::AutostartMode;

use crate::autostart::{CLI_DELAY_PREFIX, CLI_FLAG, CLI_MODE_PREFIX, CLI_UNINSTALL_FLAG};
use crate::tray;

/// 启动阶段事件名（前端骨架屏订阅它）。
pub const EVENT_BOOT: &str = "app:boot";
/// 轻提示事件名（[10 §2.3]：同 key 10 分钟内去重）。
pub const EVENT_TOAST: &str = "toast";
/// 同 key 去重窗口（[05 §1.3]、[10 §2.3]：10 分钟）。
pub const TOAST_DEDUPE_WINDOW_MS: i64 = 10 * 60 * 1000;
/// 等内核就绪的默认超时。
pub const KERNEL_READY_TIMEOUT: Duration = Duration::from_secs(20);

/// 验收探针开关：`tokenagency --t2-probe-autostart [--desired=on|off]`
/// —— 只读系统自启动状态并打印 JSON，**不写任何东西**（给主管/CI 验"漂移必须真读系统"）。
pub const PROBE_FLAG: &str = "--t2-probe-autostart";
/// 探针的期望状态前缀。
pub const PROBE_DESIRED_PREFIX: &str = "--desired=";
/// 显式开/关自启动：`--t2-set-autostart=on|off`（开发和运维入口，走同一个控制器）。
pub const SET_FLAG_PREFIX: &str = "--t2-set-autostart=";

// ================================================================ 命令行

/// 启动参数（由开机自启动的注册命令行传进来）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StartupArgs {
    /// 由开机自启动拉起（`--autostart`）
    pub autostart: bool,
    /// 启动模式：`tray`（默认，静默进托盘）/ `window`
    pub mode: AutostartMode,
    /// 启动延迟秒（0–300）
    pub delay_s: u32,
    /// 卸载钩子调用：清掉自启动条目后立刻退出（[09 §1.2] E2）
    pub uninstall_autostart: bool,
    /// 验收探针：只**读**系统自启动状态并打印 JSON，然后退出（开发/验收用）
    pub probe_autostart: bool,
    /// 验收探针的期望状态（`--desired=on|off`）；给了才会算 drifted
    pub probe_desired: Option<bool>,
    /// 显式开/关自启动（与 UI 的 `app_set_autostart` 同一个控制器、同一套语义）
    pub set_autostart: Option<bool>,
}

impl StartupArgs {
    /// 解析命令行。**不 panic、不认识就忽略**：命令行可能来自旧版本的注册表条目。
    pub fn parse<I, S>(argv: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut args = Self { mode: AutostartMode::Tray, ..Default::default() };
        let items: Vec<String> = argv.into_iter().map(|s| s.as_ref().to_string()).collect();
        let mut i = 0;
        while i < items.len() {
            let a = items[i].as_str();
            if a == CLI_FLAG {
                args.autostart = true;
            } else if a == CLI_UNINSTALL_FLAG {
                args.uninstall_autostart = true;
            } else if a == PROBE_FLAG {
                args.probe_autostart = true;
            } else if let Some(v) = a.strip_prefix(PROBE_DESIRED_PREFIX) {
                args.probe_desired = parse_on_off(v);
            } else if let Some(v) = a.strip_prefix(SET_FLAG_PREFIX) {
                args.set_autostart = parse_on_off(v);
            } else if let Some(v) = a.strip_prefix(CLI_MODE_PREFIX) {
                args.mode = parse_mode(v);
            } else if a == "--autostart-mode" {
                // 也接受空格分隔形式（人手敲命令时更自然）
                if let Some(v) = items.get(i + 1) {
                    args.mode = parse_mode(v);
                    i += 1;
                }
            } else if let Some(v) = a.strip_prefix(CLI_DELAY_PREFIX) {
                args.delay_s = parse_delay(v);
            } else if a == "--autostart-delay" {
                if let Some(v) = items.get(i + 1) {
                    args.delay_s = parse_delay(v);
                    i += 1;
                }
            }
            i += 1;
        }
        args
    }

    /// 从进程环境取（正常启动路径）。
    pub fn from_env() -> Self {
        Self::parse(std::env::args().skip(1))
    }

    /// 开机自启动 + `mode=tray` → 起来就静默在托盘里，不弹窗口。
    pub fn hide_window_at_start(&self) -> bool {
        self.autostart && self.mode == AutostartMode::Tray
    }

    /// 实际等待的秒数：**命令行里的值也要夹一次**（`Run` 键被手改成 99999 也不能睡 27 小时）。
    pub fn effective_delay_s(&self) -> u32 {
        self.delay_s.min(MAX_AUTOSTART_DELAY_S)
    }

    pub fn delay(&self) -> Duration {
        Duration::from_secs(u64::from(self.effective_delay_s()))
    }
}

fn parse_mode(v: &str) -> AutostartMode {
    match v.trim().to_ascii_lowercase().as_str() {
        "window" => AutostartMode::Window,
        _ => AutostartMode::Tray,
    }
}

fn parse_delay(v: &str) -> u32 {
    match v.trim().parse::<u32>() {
        Ok(n) => n.min(MAX_AUTOSTART_DELAY_S),
        Err(_) => {
            tracing::warn!(value = %v, "启动延迟参数不是数字，按 0 处理");
            0
        }
    }
}

fn parse_on_off(v: &str) -> Option<bool> {
    match v.trim().to_ascii_lowercase().as_str() {
        "on" | "true" | "1" | "yes" => Some(true),
        "off" | "false" | "0" | "no" => Some(false),
        _ => None,
    }
}

// ================================================================ 早退命令行（卸载钩子 / 验收探针）

/// 探针输出（`--t2-probe-autostart` 打印的 JSON）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AutostartProbeReport {
    pub mechanism: String,
    /// 从操作系统读到的实际状态
    pub actual: bool,
    pub registered_command: Option<String>,
    /// 期望状态（只有给了 `--desired=on|off` 才有）
    pub desired: Option<bool>,
    pub drifted: Option<bool>,
    pub unsupported_reason: Option<String>,
}

/// 处理"起来就能退出"的命令行，返回 `true` = 已处理完毕，调用方应立即返回
/// （**不起窗口、不连数据库**）。
///
/// 两个用途：
/// 1. **卸载钩子**（[09 §1.2] E2）：卸载脚本调 `tokenagency --uninstall-autostart`
///    删掉系统条目。放在这里而不是 Tauri 的 `setup` 里，是因为卸载时 GUI 根本不必起来 ——
///    少一层依赖就少一种"卸载不干净"的可能。
/// 2. **验收探针**：`--t2-probe-autostart [--desired=on|off]` 只读打印系统状态。
///    这是给"漂移必须真读系统、且不能自己改回"这条契约留的可复现验证入口。
pub fn handle_cli_early_exit() -> bool {
    let args = StartupArgs::from_env();

    if args.uninstall_autostart {
        let controller = crate::autostart::AutostartController::detect();
        let desired = crate::autostart::AutostartDesired::new(false, args.mode, args.delay_s);
        match controller.remove(&desired) {
            Ok(state) => tracing::info!(
                mechanism = %state.mechanism,
                actual = state.actual,
                "卸载：已清理开机自启动条目"
            ),
            Err(e) => tracing::warn!(code = e.code(), error = %e, "卸载：清理开机自启动条目失败"),
        }
        return true;
    }

    if args.probe_autostart {
        let controller = crate::autostart::AutostartController::detect();
        let desired = crate::autostart::AutostartDesired::new(
            args.probe_desired.unwrap_or(false),
            args.mode,
            args.delay_s,
        );
        let state = controller.probe().state(&desired);
        let report = AutostartProbeReport {
            mechanism: state.mechanism.clone(),
            actual: state.actual,
            registered_command: state.registered_command.clone(),
            desired: args.probe_desired,
            drifted: args.probe_desired.map(|_| state.drifted),
            unsupported_reason: state.unsupported_reason.clone(),
        };
        match serde_json::to_string_pretty(&report) {
            Ok(json) => println!("{json}"),
            Err(e) => eprintln!("探针序列化失败：{e}"),
        }
        return true;
    }

    if let Some(enabled) = args.set_autostart {
        let controller = crate::autostart::AutostartController::detect();
        let desired = crate::autostart::AutostartDesired::new(enabled, args.mode, args.delay_s);
        match controller.set(&desired, enabled) {
            Ok(state) => {
                let report = AutostartProbeReport {
                    mechanism: state.mechanism.clone(),
                    actual: state.actual,
                    registered_command: state.registered_command.clone(),
                    desired: Some(enabled),
                    drifted: Some(state.drifted),
                    unsupported_reason: state.unsupported_reason.clone(),
                };
                match serde_json::to_string_pretty(&report) {
                    Ok(json) => println!("{json}"),
                    Err(e) => eprintln!("结果序列化失败：{e}"),
                }
            }
            Err(e) => {
                eprintln!("设置开机自启动失败：{e}");
                return true;
            }
        }
        return true;
    }

    false
}

// ================================================================ 可注入的睡眠

/// 睡眠抽象（延迟逻辑要能在测试里秒过）。
#[async_trait]
pub trait Sleeper: Send + Sync {
    async fn sleep(&self, d: Duration);
}

/// 生产用：真 sleep。
#[derive(Debug, Default, Clone, Copy)]
pub struct TokioSleeper;

#[async_trait]
impl Sleeper for TokioSleeper {
    async fn sleep(&self, d: Duration) {
        tokio::time::sleep(d).await;
    }
}

/// 测试用：只记录，不真等。
#[derive(Debug, Default)]
pub struct RecordingSleeper {
    slept: Mutex<Vec<Duration>>,
}

impl RecordingSleeper {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn total_ms(&self) -> u128 {
        self.slept.lock().unwrap_or_else(|e| e.into_inner()).iter().map(Duration::as_millis).sum()
    }
    pub fn calls(&self) -> usize {
        self.slept.lock().unwrap_or_else(|e| e.into_inner()).len()
    }
}

#[async_trait]
impl Sleeper for RecordingSleeper {
    async fn sleep(&self, d: Duration) {
        self.slept.lock().unwrap_or_else(|e| e.into_inner()).push(d);
    }
}

/// 施加启动延迟，返回实际等了多久。
pub async fn apply_startup_delay(args: &StartupArgs, sleeper: &dyn Sleeper) -> Duration {
    let d = args.delay();
    if d.is_zero() {
        return Duration::ZERO;
    }
    tracing::info!(
        delay_s = args.effective_delay_s(),
        "启动延迟：先等一会儿再拉内核（避免和开机抢资源）"
    );
    sleeper.sleep(d).await;
    d
}

// ================================================================ 关窗退托盘

/// 退出守卫：`true` = 用户在菜单里点了"退出"（真的要退），不是关窗。
#[derive(Debug, Default)]
pub struct QuitGuard {
    quitting: AtomicBool,
}

impl QuitGuard {
    /// 标记"这次是真的要退出"（托盘"退出"菜单 / 单实例转发退出请求）。
    pub fn request(&self) -> bool {
        !self.quitting.swap(true, Ordering::SeqCst)
    }

    pub fn is_quitting(&self) -> bool {
        self.quitting.load(Ordering::SeqCst)
    }
}

/// 关窗时该做什么。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseAction {
    /// 藏进托盘，进程继续（[12 §5 T2] ④）
    HideToTray,
    /// 真的关闭（用户点了"退出"）
    AllowClose,
}

/// 关窗决策（纯函数）。只有主窗口退托盘；真退出时放行。
pub fn decide_close(quitting: bool, window_label: &str) -> CloseAction {
    if quitting || window_label != tray::MAIN_WINDOW {
        CloseAction::AllowClose
    } else {
        CloseAction::HideToTray
    }
}

/// 请求退出：置位 QuitGuard 再 exit —— 否则 `exit` 会触发 CloseRequested 又被我们自己拦回来。
pub fn request_quit<R: Runtime>(app: &AppHandle<R>) {
    if let Some(guard) = app.try_state::<QuitGuard>() {
        guard.request();
    } else {
        tracing::warn!("退出时没有 QuitGuard（setup 未执行？），直接退出");
    }
    tracing::info!("用户请求退出 TokenAgency");
    app.exit(0);
}

/// 窗口事件（接缝：`Builder::on_window_event`）。
pub fn on_window_event<R: Runtime>(window: &Window<R>, event: &WindowEvent) {
    let WindowEvent::CloseRequested { api, .. } = event else {
        return;
    };
    let quitting =
        window.app_handle().try_state::<QuitGuard>().map(|g| g.is_quitting()).unwrap_or(false);

    match decide_close(quitting, window.label()) {
        CloseAction::HideToTray => {
            api.prevent_close();
            let _ = window.hide();
            tracing::debug!(window = window.label(), "关窗退托盘：进程继续在后台跑");
        }
        CloseAction::AllowClose => {
            tracing::debug!(window = window.label(), quitting, "窗口关闭");
        }
    }
}

/// 第二个实例启动（single-instance 插件回调）：唤起已有窗口，不新建进程。
pub fn on_second_instance<R: Runtime>(app: &AppHandle<R>, argv: &[String], _cwd: &str) {
    tracing::info!(argc = argv.len(), "检测到第二个实例：唤起已运行的主窗口");
    tray::show_main_window(app);
}

// ================================================================ 通知（克制 + 去重）

/// 轻提示级别。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToastLevel {
    Info,
    Warn,
    Error,
}

/// `toast` 事件载荷（[10 §2.3]）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ToastPayload {
    pub level: ToastLevel,
    pub title: String,
    pub body: String,
    pub dedupe_key: String,
}

/// [05 §1.3]：只有这四件事配得上一条**系统通知**，其余一律进日志。
///
/// 把"克制"写成枚举而不是散落的 `if`：加第五种通知时必须先想清楚"是不是又滥用了"。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotifyKind {
    /// 某个凭据额度耗尽并已自动切换
    QuotaExhaustedSwitched,
    /// 全部通道失败
    AllChannelsFailed,
    /// 机场订阅更新失败
    SubscriptionUpdateFailed,
    /// 所有 lane 的节点都不可达
    AllLanesUnreachable,
}

impl NotifyKind {
    /// [05 §1.3] 的四条 —— 单元测试会守住条数。
    pub const ALL: [NotifyKind; 4] = [
        NotifyKind::QuotaExhaustedSwitched,
        NotifyKind::AllChannelsFailed,
        NotifyKind::SubscriptionUpdateFailed,
        NotifyKind::AllLanesUnreachable,
    ];

    pub fn title(&self) -> &'static str {
        match self {
            NotifyKind::QuotaExhaustedSwitched => "额度用尽，已自动切换",
            NotifyKind::AllChannelsFailed => "所有通道不可用",
            NotifyKind::SubscriptionUpdateFailed => "订阅更新失败",
            NotifyKind::AllLanesUnreachable => "网络不可用",
        }
    }

    /// `subject` = 具体对象（凭据名 / 订阅名），从不包含密钥值。
    pub fn body(&self, subject: &str) -> String {
        match self {
            NotifyKind::QuotaExhaustedSwitched => {
                format!("{subject} 额度用尽，已切到下一个可用通道")
            }
            NotifyKind::AllChannelsFailed => "所有通道不可用，请检查网络/订阅".to_string(),
            NotifyKind::SubscriptionUpdateFailed => {
                format!("订阅「{subject}」更新失败，仍在用上一次的节点")
            }
            NotifyKind::AllLanesUnreachable => "网络不可用，请检查订阅或节点".to_string(),
        }
    }

    /// 去重 key：同一种事件 + 同一个对象 10 分钟内只弹一次。
    pub fn dedupe_key(&self, subject: &str) -> String {
        let kind = match self {
            NotifyKind::QuotaExhaustedSwitched => "quota",
            NotifyKind::AllChannelsFailed => "all-channels",
            NotifyKind::SubscriptionUpdateFailed => "sub",
            NotifyKind::AllLanesUnreachable => "lanes",
        };
        format!("{kind}:{subject}")
    }
}

/// 去重器：[05 §1.3] "同事件 10 分钟内只弹一次"。
///
/// 时钟可注入（[11 §1.1] `Clock`），所以"10 分钟"是能被确定性测试的，而不是靠 sleep。
pub struct NotificationCenter {
    clock: Arc<dyn Clock>,
    last: Mutex<HashMap<String, i64>>,
}

impl NotificationCenter {
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        Self { clock, last: Mutex::new(HashMap::new()) }
    }

    pub fn with_system_clock() -> Self {
        Self::new(Arc::new(SystemClock))
    }

    /// 是否放行。`None` = 10 分钟内已经弹过同 key（被去重）。
    pub fn admit(&self, key: &str) -> Option<()> {
        let now = self.clock.now_ms();
        let mut last = self.last.lock().unwrap_or_else(|e| e.into_inner());
        match last.get(key) {
            Some(prev) if now.saturating_sub(*prev) < TOAST_DEDUPE_WINDOW_MS => None,
            _ => {
                last.insert(key.to_string(), now);
                Some(())
            }
        }
    }

    /// 清掉过期记录，返回清理条数（定期调用，别让 map 无限长大）。
    pub fn gc(&self) -> usize {
        let now = self.clock.now_ms();
        let mut last = self.last.lock().unwrap_or_else(|e| e.into_inner());
        let before = last.len();
        last.retain(|_, ts| now.saturating_sub(*ts) < TOAST_DEDUPE_WINDOW_MS);
        before - last.len()
    }

    pub fn tracked_keys(&self) -> usize {
        self.last.lock().unwrap_or_else(|e| e.into_inner()).len()
    }
}

/// 应用内轻提示（去重后广播 `toast` 事件）。**不弹系统通知**。
pub fn toast<R: Runtime>(
    app: &AppHandle<R>,
    level: ToastLevel,
    title: &str,
    body: &str,
    dedupe_key: &str,
) {
    if let Some(nc) = app.try_state::<NotificationCenter>() {
        if nc.admit(dedupe_key).is_none() {
            tracing::debug!(key = %dedupe_key, "同 key 10 分钟内已提示过，跳过");
            return;
        }
    }
    let payload = ToastPayload {
        level,
        title: title.to_string(),
        body: body.to_string(),
        dedupe_key: dedupe_key.to_string(),
    };
    if let Err(e) = app.emit(EVENT_TOAST, payload) {
        tracing::warn!(error = %e, "广播 toast 事件失败");
    }
}

/// [05 §1.3] 的四类事件：去重 + 系统通知 + 应用内 toast。
pub fn notify<R: Runtime>(app: &AppHandle<R>, kind: NotifyKind, subject: &str) {
    let key = kind.dedupe_key(subject);
    if let Some(nc) = app.try_state::<NotificationCenter>() {
        if nc.admit(&key).is_none() {
            tracing::debug!(key = %key, "同 key 10 分钟内已通知过，跳过");
            return;
        }
    }
    let title = kind.title();
    let body = kind.body(subject);

    // 通知插件只在桌面平台可用（移动端走另一套）
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        use tauri_plugin_notification::NotificationExt;
        if let Err(e) = app.notification().builder().title(title).body(body.clone()).show() {
            // 通知发不出去不是致命错误（用户可能关了系统通知），但要留痕
            tracing::warn!(error = %e, "系统通知发送失败");
        }
    }

    let payload =
        ToastPayload { level: ToastLevel::Warn, title: title.to_string(), body, dedupe_key: key };
    let _ = app.emit(EVENT_TOAST, payload);
}

// ================================================================ 启动编排

/// 启动阶段（[05 §2] 骨架屏 → 内核就绪 → 转绿）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BootPhase {
    /// 骨架屏
    Skeleton,
    /// 自启动延迟中
    Delaying,
    /// 正在拉起内核
    StartingKernel,
    /// 等内核就绪（就绪探测）
    WaitingKernel,
    /// 正在启动本地网关（只绑回环，[09 §1.2] I1）
    StartingGateway,
    /// 等网关就绪 —— 判据是"真的在监听 8787"，不是"调用过启动函数"
    WaitingGateway,
    /// 全绿
    Ready,
    /// 降级可用（内核没起来，但直连还能用）
    Degraded,
    /// 不可用
    Failed,
}

impl BootPhase {
    pub fn as_str(&self) -> &'static str {
        match self {
            BootPhase::Skeleton => "skeleton",
            BootPhase::Delaying => "delaying",
            BootPhase::StartingKernel => "starting_kernel",
            BootPhase::WaitingKernel => "waiting_kernel",
            BootPhase::StartingGateway => "starting_gateway",
            BootPhase::WaitingGateway => "waiting_gateway",
            BootPhase::Ready => "ready",
            BootPhase::Degraded => "degraded",
            BootPhase::Failed => "failed",
        }
    }

    /// 是否已经收敛（骨架屏可以撤了）。
    pub fn is_terminal(&self) -> bool {
        matches!(self, BootPhase::Ready | BootPhase::Degraded | BootPhase::Failed)
    }
}

/// 启动编排需要的外部能力（T1 实现：内核来自 `ta-kernel`，事件与托盘来自本 crate）。
///
/// 用 trait 而不是直接调 `ta-kernel`：编排逻辑（顺序、超时、降级）才是 T2 的交付物，
/// 它能被离线测；内核真机行为由 P5 负责。
#[async_trait]
pub trait BootDeps: Send + Sync {
    /// 等待（真正实现 sleep）
    async fn wait(&self, d: Duration);
    /// 拉起内核
    async fn start_kernel(&self) -> Result<(), String>;
    /// 等内核就绪（内部做就绪探测；超时返回 Err）
    async fn wait_kernel_ready(&self, timeout: Duration) -> Result<(), String>;
    /// 启动本地网关（只绑回环）。
    ///
    /// 默认实现 = **明确暴露缺口**：网关的生产装配入口由 E5 在 `ta-gateway` 里提供
    /// （`build_runtime` 一类）。在他落地之前，这里如实返回"未接线"，
    /// 编排因而落到 `Degraded` 并把这句原因显示给用户 —— **绝不假装网关已就绪**，
    /// 也**不在我们这边另造一条装配路径**（两套构造方式必然漂移，见 docs/13 L21）。
    async fn start_gateway(&self) -> Result<(), String> {
        Err(GATEWAY_NOT_WIRED.to_string())
    }
    /// 等网关**真的在监听**（不是"调用过启动函数"）
    async fn wait_gateway_ready(&self, _timeout: Duration) -> Result<(), String> {
        Err(GATEWAY_NOT_WIRED.to_string())
    }
    /// 阶段变化（广播事件 + 更新托盘色）
    fn on_phase(&self, phase: BootPhase, detail: &str);
}

/// 网关装配入口缺位时的统一文案（`BootDeps` 默认实现与装配层共用，避免两处措辞漂移）。
pub const GATEWAY_NOT_WIRED: &str =
    "网关装配入口尚未接线（等 ta-gateway 的 build_runtime 落地）—— 网关还没有在监听 8787";

/// 编排结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootOutcome {
    pub phase: BootPhase,
    /// 实际等待的启动延迟
    pub delayed_s: u32,
    /// 降级/失败原因（可读，可直接展示）
    pub detail: Option<String>,
}

/// 启动编排器。
pub struct BootOrchestrator;

impl BootOrchestrator {
    /// 跑一次启动编排。
    ///
    /// 内核起不来**不是致命错误**：用户可能只想用直连通道（[11 §5] TA-3001 的语义是
    /// "内核未就绪，且没有可用的直连通道" 才算失败）。所以这里落到 `Degraded`
    /// 并带上可读原因，让用户自己决定。
    pub async fn run(deps: &dyn BootDeps, args: &StartupArgs, timeout: Duration) -> BootOutcome {
        let delayed_s = args.effective_delay_s();
        let degraded = |detail: String| -> BootOutcome {
            deps.on_phase(BootPhase::Degraded, &detail);
            BootOutcome { phase: BootPhase::Degraded, delayed_s, detail: Some(detail) }
        };

        deps.on_phase(BootPhase::Skeleton, "界面已就绪，正在准备网关");

        let delay = args.delay();
        if !delay.is_zero() {
            deps.on_phase(
                BootPhase::Delaying,
                &format!("自启动延迟 {} 秒后开始（避免和开机抢资源）", args.effective_delay_s()),
            );
            deps.wait(delay).await;
        }

        // ---- 内核：先拉起，再等它**真的**就绪
        deps.on_phase(BootPhase::StartingKernel, "正在启动网络内核");
        if let Err(e) = deps.start_kernel().await {
            return degraded(format!("网络内核未能启动：{e}（直连通道仍可用）"));
        }
        deps.on_phase(BootPhase::WaitingKernel, "等待内核就绪");
        if let Err(e) = deps.wait_kernel_ready(timeout).await {
            return degraded(format!(
                "内核在 {} 秒内没有就绪：{e}（直连通道仍可用）",
                timeout.as_secs()
            ));
        }

        // ---- 网关：内核之后再拉起本地网关（只绑回环，[09 §1.2] I1）
        deps.on_phase(BootPhase::StartingGateway, "正在启动本地网关");
        if let Err(e) = deps.start_gateway().await {
            return degraded(format!("本地网关未能启动：{e}"));
        }
        deps.on_phase(BootPhase::WaitingGateway, "等待网关开始监听");
        if let Err(e) = deps.wait_gateway_ready(timeout).await {
            return degraded(format!("网关在 {} 秒内没有开始监听：{e}", timeout.as_secs()));
        }

        // ---- 两个依赖都**真的**就绪，才转绿
        deps.on_phase(BootPhase::Ready, "内核与网关均已就绪");
        BootOutcome { phase: BootPhase::Ready, delayed_s, detail: None }
    }
}

// ================================================================ 托管状态 + 接缝

/// `on_setup` 期间解析出来的启动上下文（`run_boot` 需要它）。
#[derive(Debug, Clone, Copy)]
pub struct StartupContext {
    pub args: StartupArgs,
}

/// 读启动参数（T1 在 setup 之后拉内核时用）。
pub fn startup_args<R: Runtime>(app: &AppHandle<R>) -> StartupArgs {
    app.try_state::<StartupContext>().map(|c| c.args).unwrap_or_default()
}

/// 应用启动时的编排入口（接缝：`lib.rs` 的 `.setup(...)` 第一件事）。
///
/// 只做三件**不依赖业务状态**的事：解析参数、按 `mode` 决定是否隐藏窗口、
/// 注册托盘之外的托管状态（退出守卫 / 通知中心 / 启动上下文）。
/// 托盘安装与内核拉起由 T1 在 `AppState` 就绪后调用
/// （`tray::init` / `tray::set_source` / `BootOrchestrator::run`）。
pub fn on_setup<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    let args = StartupArgs::from_env();

    // 卸载钩子 / 验收探针在 `run()` 最前面就被 `handle_cli_early_exit` 处理掉了，
    // 走不到这里；能到这里说明是正常的 GUI 启动。

    if app.try_state::<QuitGuard>().is_none() {
        app.manage(QuitGuard::default());
    }
    if app.try_state::<NotificationCenter>().is_none() {
        app.manage(NotificationCenter::with_system_clock());
    }
    if app.try_state::<StartupContext>().is_none() {
        app.manage(StartupContext { args });
    }

    if let Some(window) = app.get_webview_window(tray::MAIN_WINDOW) {
        if hide_window_for_start(&args, std::env::var_os("TA_E2E").is_some()) {
            let _ = window.hide();
            tracing::info!("开机自启动（mode=tray）：静默进托盘，不弹窗口");
        } else {
            // 显式 show：window-state 插件会把上次会话的可见性也恢复回来
            // （用户上次"关窗退托盘"过 → 这次普通启动会带着 hidden 状态），
            // 所以非托盘模式必须把它显示出来，否则用户双击图标看不到窗口。
            tracing::info!(
                e2e = std::env::var_os("TA_E2E").is_some(),
                "主窗口：显式显示（非托盘模式；E2E 下还会再延迟显示一次）"
            );
            let _ = window.show();
            let _ = window.set_focus();
            // [docs/31 §19] **真机 E2E：延迟再显示一次**。
            //
            // 为什么需要它：驱动器建完会话后，窗口会被 `window-state` 插件（恢复上次会话的
            // 可见性）或托盘逻辑**再次藏起来** —— 真机表现是"会话即刻失效"：
            // `wd.exec` / `pageText` / `window/handles` **全部返回 null**（协议层证据，第 160 轮）。
            // 上面那次 `show()` 发生在插件恢复**之前**，所以这里再补一次、确保晚于它。
            if std::env::var_os("TA_E2E").is_some() {
                let w = window.clone();
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(2500)).await;
                    let _ = w.show();
                    let _ = w.set_focus();
                });
            }
        }
    }

    tracing::info!(
        autostart = args.autostart,
        mode = ?args.mode,
        delay_s = args.effective_delay_s(),
        "TokenAgency 启动（生命周期就绪）"
    );

    // 骨架屏阶段（前端订阅 app:boot）
    let payload = BootPhasePayload {
        phase: BootPhase::Skeleton,
        detail: "界面已就绪，正在准备网关".to_string(),
    };
    let _ = app.emit(EVENT_BOOT, payload);
    Ok(())
}

/// `app:boot` 事件载荷。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BootPhasePayload {
    pub phase: BootPhase,
    pub detail: String,
}

// ================================================================ 测试

#[cfg(test)]
mod tests {
    use super::*;
    use ta_core::clock::ManualClock;

    // ---------------------------------------------------------- 命令行

    #[test]
    fn parses_autostart_command_line() {
        let args =
            StartupArgs::parse(["--autostart", "--autostart-mode=tray", "--autostart-delay=30"]);
        assert!(args.autostart);
        assert_eq!(args.mode, AutostartMode::Tray);
        assert_eq!(args.delay_s, 30);
        assert!(args.hide_window_at_start());
        assert_eq!(args.delay(), Duration::from_secs(30));

        // 普通启动：什么都不带
        let plain = StartupArgs::parse(["app.exe"]);
        assert!(!plain.autostart);
        assert_eq!(plain.mode, AutostartMode::Tray, "默认 tray（[02 §2.10]）");
        assert_eq!(plain.delay_s, 0);
        assert!(!plain.hide_window_at_start());
    }

    #[test]
    fn window_mode_does_not_hide_at_start() {
        let args = StartupArgs::parse(["--autostart", "--autostart-mode=window"]);
        assert_eq!(args.mode, AutostartMode::Window);
        assert!(!args.hide_window_at_start(), "mode=window 时用户就是想看见窗口");
    }

    #[test]
    fn accepts_space_separated_form_and_uninstall_flag() {
        let args = StartupArgs::parse(["--autostart-mode", "window", "--autostart-delay", "5"]);
        assert_eq!(args.mode, AutostartMode::Window);
        assert_eq!(args.delay_s, 5);

        let un = StartupArgs::parse([CLI_UNINSTALL_FLAG]);
        assert!(un.uninstall_autostart);
        assert!(!un.autostart);
    }

    #[test]
    fn parses_probe_and_set_flags() {
        let probe = StartupArgs::parse([PROBE_FLAG, "--desired=on"]);
        assert!(probe.probe_autostart);
        assert_eq!(probe.probe_desired, Some(true));
        assert!(!probe.autostart, "探针不是自启动拉起");

        let off = StartupArgs::parse([PROBE_FLAG, "--desired=off"]);
        assert_eq!(off.probe_desired, Some(false));
        // 不给 --desired 就只报实际状态（不算漂移）
        assert_eq!(StartupArgs::parse([PROBE_FLAG]).probe_desired, None);
        // 乱七八糟的值不能让探针误判成 on
        assert_eq!(StartupArgs::parse([PROBE_FLAG, "--desired=maybe"]).probe_desired, None);

        assert_eq!(StartupArgs::parse(["--t2-set-autostart=on"]).set_autostart, Some(true));
        assert_eq!(StartupArgs::parse(["--t2-set-autostart=0"]).set_autostart, Some(false));
        assert_eq!(StartupArgs::parse(["--t2-set-autostart=zzz"]).set_autostart, None);
    }

    #[test]
    fn probe_report_serializes_the_contract_fields() {
        let report = AutostartProbeReport {
            mechanism: "windows-run-key".to_string(),
            actual: false,
            registered_command: None,
            desired: Some(true),
            drifted: Some(true),
            unsupported_reason: None,
        };
        let json = serde_json::to_value(&report).expect("序列化");
        assert_eq!(json["mechanism"], serde_json::json!("windows-run-key"));
        assert_eq!(json["actual"], serde_json::json!(false));
        assert_eq!(json["drifted"], serde_json::json!(true));
        assert!(json["registered_command"].is_null());
        assert!(json["unsupported_reason"].is_null());
    }

    #[test]
    fn garbage_arguments_are_ignored_and_delay_is_clamped() {
        // 注册表被手改成离谱的值：不能让它睡 27 小时，也不能 panic
        let args = StartupArgs::parse([
            "--autostart",
            "--autostart-delay=999999",
            "--whatever",
            "--autostart-mode=bogus",
            "-x",
        ]);
        assert_eq!(args.delay_s, MAX_AUTOSTART_DELAY_S, "解析时就夹到上限");
        assert_eq!(args.effective_delay_s(), MAX_AUTOSTART_DELAY_S);
        assert_eq!(args.mode, AutostartMode::Tray, "不认识的值退回默认");
        assert!(args.autostart);

        let bad = StartupArgs::parse(["--autostart-delay=abc"]);
        assert_eq!(bad.delay_s, 0, "非数字按 0 处理，不 panic");
        // 空命令行
        assert_eq!(StartupArgs::parse(Vec::<String>::new()), StartupArgs::default());
    }

    #[test]
    fn effective_delay_double_clamps_tampered_values() {
        let mut args = StartupArgs { delay_s: u32::MAX, ..Default::default() };
        assert_eq!(args.effective_delay_s(), MAX_AUTOSTART_DELAY_S);
        args.delay_s = MAX_AUTOSTART_DELAY_S;
        assert_eq!(args.delay().as_secs(), 300);
    }

    #[tokio::test]
    async fn startup_delay_is_skipped_when_zero_and_applied_otherwise() {
        let sleeper = RecordingSleeper::new();
        let zero = StartupArgs::default();
        assert_eq!(apply_startup_delay(&zero, &sleeper).await, Duration::ZERO);
        assert_eq!(sleeper.calls(), 0, "delay=0 时一次都不该睡");

        let args = StartupArgs { autostart: true, delay_s: 12, ..Default::default() };
        assert_eq!(apply_startup_delay(&args, &sleeper).await, Duration::from_secs(12));
        assert_eq!(sleeper.total_ms(), 12_000);
    }

    // ---------------------------------------------------------- 关窗退托盘

    #[test]
    fn close_decision_hides_main_window_but_allows_real_quit() {
        assert_eq!(decide_close(false, "main"), CloseAction::HideToTray);
        assert_eq!(decide_close(true, "main"), CloseAction::AllowClose, "点了退出就必须能退");
        assert_eq!(decide_close(false, "other"), CloseAction::AllowClose, "只有主窗口退托盘");
    }

    #[test]
    fn quit_guard_is_monotonic() {
        let guard = QuitGuard::default();
        assert!(!guard.is_quitting());
        assert!(guard.request(), "第一次置位应返回 true");
        assert!(guard.is_quitting());
        assert!(!guard.request(), "重复置位返回 false（幂等）");
    }

    // ---------------------------------------------------------- 通知去重

    #[test]
    fn notification_dedupe_window_is_ten_minutes() {
        let clock = Arc::new(ManualClock::new(1_000_000));
        let nc = NotificationCenter::new(clock.clone());

        assert!(nc.admit("quota:中转站A").is_some(), "首次必须放行");
        assert!(nc.admit("quota:中转站A").is_none(), "10 分钟内必须去重");
        assert!(nc.admit("quota:中转站B").is_some(), "同事件不同对象是另一条");

        clock.advance(TOAST_DEDUPE_WINDOW_MS - 1);
        assert!(nc.admit("quota:中转站A").is_none(), "差 1ms 还没到窗口");
        clock.advance(1);
        assert!(nc.admit("quota:中转站A").is_some(), "满 10 分钟应重新放行");
    }

    #[test]
    fn notification_gc_drops_expired_keys_only() {
        let clock = Arc::new(ManualClock::new(0));
        let nc = NotificationCenter::new(clock.clone());
        nc.admit("a");
        clock.advance(TOAST_DEDUPE_WINDOW_MS);
        nc.admit("b");
        assert_eq!(nc.tracked_keys(), 2);
        assert_eq!(nc.gc(), 1, "只清掉过期的 a");
        assert_eq!(nc.tracked_keys(), 1);
    }

    #[test]
    fn only_the_four_documented_events_deserve_a_system_notification() {
        // [05 §1.3]：只有这四件事弹系统通知
        assert_eq!(NotifyKind::ALL.len(), 4);
        let mut keys: Vec<String> = NotifyKind::ALL.iter().map(|k| k.dedupe_key("x")).collect();
        keys.sort();
        let n = keys.len();
        keys.dedup();
        assert_eq!(keys.len(), n, "四类通知的去重 key 必须互不相同");
        for k in NotifyKind::ALL {
            assert!(!k.title().is_empty());
            // 文案里不许出现占位符或密钥样式的串
            let body = k.body("中转站A");
            assert!(!body.contains("sk-"), "{body}");
            assert!(!body.contains("{"), "文案里还有未替换的占位符：{body}");
        }
        // 额度耗尽必须点名具体对象（"已切到 X" 才有用）
        assert!(NotifyKind::QuotaExhaustedSwitched.body("中转站A").contains("中转站A"));
        assert!(NotifyKind::SubscriptionUpdateFailed.body("机场A").contains("机场A"));
    }

    // ---------------------------------------------------------- 启动编排

    /// 假的内核依赖。`Result` 没有 `Default`，所以手写一份（默认"一切顺利"）。
    struct FakeDeps {
        phases: Mutex<Vec<(BootPhase, String)>>,
        waits: Mutex<Vec<Duration>>,
        start_result: Result<(), String>,
        ready_result: Result<(), String>,
        start_gateway_result: Result<(), String>,
        wait_gateway_result: Result<(), String>,
    }

    impl Default for FakeDeps {
        fn default() -> Self {
            Self {
                phases: Mutex::new(Vec::new()),
                waits: Mutex::new(Vec::new()),
                start_result: Ok(()),
                ready_result: Ok(()),
                // 默认"网关也能起来"，这样内核相关的既有断言不被网关噪音干扰；
                // 网关自己的分支由下面两个专门的测试覆盖。
                start_gateway_result: Ok(()),
                wait_gateway_result: Ok(()),
            }
        }
    }

    impl FakeDeps {
        fn ok() -> Self {
            Self::default()
        }
        fn phases(&self) -> Vec<BootPhase> {
            self.phases.lock().unwrap().iter().map(|(p, _)| *p).collect()
        }
        fn details(&self) -> Vec<String> {
            self.phases.lock().unwrap().iter().map(|(_, d)| d.clone()).collect()
        }
        fn total_wait_ms(&self) -> u128 {
            self.waits.lock().unwrap().iter().map(Duration::as_millis).sum()
        }
    }

    #[async_trait]
    impl BootDeps for FakeDeps {
        async fn wait(&self, d: Duration) {
            self.waits.lock().unwrap().push(d);
        }
        async fn start_kernel(&self) -> Result<(), String> {
            self.start_result.clone()
        }
        async fn wait_kernel_ready(&self, _timeout: Duration) -> Result<(), String> {
            self.ready_result.clone()
        }
        async fn start_gateway(&self) -> Result<(), String> {
            self.start_gateway_result.clone()
        }
        async fn wait_gateway_ready(&self, _timeout: Duration) -> Result<(), String> {
            self.wait_gateway_result.clone()
        }
        fn on_phase(&self, phase: BootPhase, detail: &str) {
            self.phases.lock().unwrap().push((phase, detail.to_string()));
        }
    }

    #[tokio::test]
    async fn boot_sequence_is_skeleton_then_kernel_then_ready() {
        let deps = FakeDeps::ok();
        let args = StartupArgs::default();
        let out = BootOrchestrator::run(&deps, &args, KERNEL_READY_TIMEOUT).await;

        assert_eq!(out.phase, BootPhase::Ready);
        assert!(out.detail.is_none());
        assert_eq!(out.delayed_s, 0);
        assert_eq!(
            deps.phases(),
            vec![
                BootPhase::Skeleton,
                BootPhase::StartingKernel,
                BootPhase::WaitingKernel,
                BootPhase::StartingGateway,
                BootPhase::WaitingGateway,
                BootPhase::Ready
            ]
        );
        assert_eq!(deps.total_wait_ms(), 0, "没有延迟就不该等");
    }

    #[tokio::test]
    async fn boot_applies_autostart_delay_between_skeleton_and_kernel() {
        let deps = FakeDeps::ok();
        let args = StartupArgs { autostart: true, delay_s: 45, ..Default::default() };
        let out = BootOrchestrator::run(&deps, &args, KERNEL_READY_TIMEOUT).await;

        assert_eq!(out.phase, BootPhase::Ready);
        assert_eq!(out.delayed_s, 45);
        assert_eq!(deps.total_wait_ms(), 45_000);
        assert_eq!(
            deps.phases(),
            vec![
                BootPhase::Skeleton,
                BootPhase::Delaying,
                BootPhase::StartingKernel,
                BootPhase::WaitingKernel,
                BootPhase::StartingGateway,
                BootPhase::WaitingGateway,
                BootPhase::Ready
            ],
            "延迟必须夹在骨架屏与拉内核之间"
        );
        assert!(deps.details()[1].contains("45"));
    }

    #[tokio::test]
    async fn kernel_failure_degrades_instead_of_failing_hard() {
        let deps = FakeDeps {
            start_result: Err("找不到内核二进制".to_string()),
            ready_result: Ok(()),
            ..Default::default()
        };
        let out = BootOrchestrator::run(&deps, &StartupArgs::default(), KERNEL_READY_TIMEOUT).await;

        assert_eq!(out.phase, BootPhase::Degraded);
        let detail = out.detail.expect("要有可读原因");
        assert!(detail.contains("找不到内核二进制"), "{detail}");
        assert!(detail.contains("直连"), "要告诉用户还能用直连：{detail}");
        assert!(!deps.phases().contains(&BootPhase::WaitingKernel), "起不来就不该再等");
        assert!(!deps.phases().contains(&BootPhase::Ready));
    }

    #[tokio::test]
    async fn kernel_timeout_degrades_with_the_timeout_in_the_message() {
        let deps = FakeDeps {
            start_result: Ok(()),
            ready_result: Err("就绪探测超时".to_string()),
            ..Default::default()
        };
        let timeout = Duration::from_secs(7);
        let out = BootOrchestrator::run(&deps, &StartupArgs::default(), timeout).await;

        assert_eq!(out.phase, BootPhase::Degraded);
        let detail = out.detail.expect("要有可读原因");
        assert!(detail.contains('7'), "要把超时秒数说清楚：{detail}");
        assert!(detail.contains("就绪探测超时"), "{detail}");
        assert_eq!(deps.phases().last(), Some(&BootPhase::Degraded), "最后一个阶段必须是收敛态");
    }

    #[tokio::test]
    async fn missing_gateway_wiring_degrades_and_never_turns_green() {
        // 这就是"不能只说调用了函数"的那条：网关没接线 → 绝不转绿，
        // 而且原因要能在 UI 上读到（用户才知道为什么没有绿灯）。
        let deps = FakeDeps {
            start_gateway_result: Err(GATEWAY_NOT_WIRED.to_string()),
            ..Default::default()
        };
        let out = BootOrchestrator::run(&deps, &StartupArgs::default(), KERNEL_READY_TIMEOUT).await;

        assert_eq!(out.phase, BootPhase::Degraded);
        assert!(!deps.phases().contains(&BootPhase::Ready), "网关没起来就不许转绿");
        assert!(!deps.phases().contains(&BootPhase::WaitingGateway), "起不来就别再等");
        let detail = out.detail.expect("要有可读原因");
        assert!(detail.contains("8787"), "要说清网关没在监听：{detail}");
        assert_eq!(deps.phases().last(), Some(&BootPhase::Degraded));
    }

    #[tokio::test]
    async fn gateway_started_but_not_listening_times_out_without_green() {
        // 进程起来了但端口没在听（绑定失败 / 端口被占）→ 同样是 Degraded，不是绿。
        let deps = FakeDeps {
            wait_gateway_result: Err("8787 上没有监听".to_string()),
            ..Default::default()
        };
        let timeout = Duration::from_secs(3);
        let out = BootOrchestrator::run(&deps, &StartupArgs::default(), timeout).await;

        assert_eq!(out.phase, BootPhase::Degraded);
        assert!(!deps.phases().contains(&BootPhase::Ready));
        let detail = out.detail.expect("要有可读原因");
        assert!(detail.contains('3'), "要把超时秒数说清楚：{detail}");
        assert!(detail.contains("8787"), "{detail}");
    }

    #[test]
    fn boot_phase_labels_are_stable_for_the_frontend() {
        // 前端按这些字符串分支，改名就是 breaking change
        assert_eq!(BootPhase::Skeleton.as_str(), "skeleton");
        assert_eq!(BootPhase::Ready.as_str(), "ready");
        assert_eq!(BootPhase::Degraded.as_str(), "degraded");
        assert_eq!(BootPhase::StartingGateway.as_str(), "starting_gateway");
        assert_eq!(BootPhase::WaitingGateway.as_str(), "waiting_gateway");
        assert!(!BootPhase::Skeleton.is_terminal());
        assert!(!BootPhase::Delaying.is_terminal());
        assert!(!BootPhase::StartingGateway.is_terminal(), "网关还没就绪，骨架屏不能撤");
        assert!(!BootPhase::WaitingGateway.is_terminal());
        assert!(BootPhase::Failed.is_terminal());
        assert!(BootPhase::Degraded.is_terminal());
        let payload = BootPhasePayload { phase: BootPhase::Ready, detail: "ok".to_string() };
        let json = serde_json::to_value(&payload).expect("序列化");
        assert_eq!(json["phase"], serde_json::json!("ready"));
        assert_eq!(json["detail"], serde_json::json!("ok"));
    }

    #[test]
    fn toast_payload_matches_the_documented_shape() {
        let p = ToastPayload {
            level: ToastLevel::Warn,
            title: "稍后重试".to_string(),
            body: "内核正在热重载".to_string(),
            dedupe_key: "tray:retry".to_string(),
        };
        let json = serde_json::to_value(&p).expect("序列化");
        assert_eq!(json["level"], serde_json::json!("warn"));
        assert_eq!(json["dedupe_key"], serde_json::json!("tray:retry"));
    }
}
