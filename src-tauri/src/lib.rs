//! TokenAgency 桌面应用库入口。
//!
//! 职责边界：
//! * **本文件只做接线**，不放业务逻辑。
//! * `plugins`（插件注册）、`tray`（托盘）、`lifecycle`（启动编排 / 关窗退托盘 / 通知）、
//!   `autostart`（唯一写 OS 状态的模块）。
//! * `state` / `commands`：命令层（IPC 契约）。
//! * **`boot`**：启动编排的**生产装配** —— 造内核、把阶段广播出去、提供托盘数据源。
//!
//! # 启动顺序（`.setup` 里做的事）
//!
//! ```text
//! on_setup()            解析命令行 → mode=tray 时隐藏窗口 → 装 QuitGuard/通知中心
//! tray::init()          托盘先亮起来（此刻还没数据源，显示"启动中"）
//! AppState::bootstrap() 开库 + 跑迁移 + 载入配置快照
//! boot::build_kernel()  按配置装配内核控制器（没配路径 → None）
//! tray::set_source()    换成"真实就绪驱动"的托盘数据源
//! spawn(BootOrchestrator::run(...))  延迟 → 拉内核 → 等**真**就绪 → 拉网关 → 转绿
//! ```
//!
//! **就绪判据**：只有编排收敛到 `Ready` 才会显示绿灯，而 `Ready` 要求内核与网关
//! 都真的就绪（内核那一关还内含 provider 判据；网关那一关是**真 TCP 连 `gateway.listen`**，
//! 见 [`boot`] 与 [`gateway`] 的模块文档）。
//!
//! ⚠️ **已知待办（评审 A2）**：编排**收敛之后不再复检** —— 内核在那之后崩溃/被杀，
//! 托盘与 `app_status` 会保持在 `Ready`（**陈旧绿灯**）。补看护循环
//! （`supervisor.supervise(&token)` + 一条"由 Ready 回落"的反向写回）是已知待办，
//! 登记在主管的后续工作包。**本文件上面的"只有收敛到 Ready 才显示绿灯"这句话，
//! 只对"收敛那一刻"成立，不对"之后"成立** —— 别把它读成持续保证。
//!
//! 模块声明用 `pub mod`：命令层需要 `use crate::tray::…` / `crate::autostart::…`。

pub mod apikey_auth;
pub mod autostart;
pub mod boot;
pub mod commands;
pub mod gateway;
// ---- docs/19 §1.2：`model_import_from_provider` 的进程内存预览缓存（不落库）
pub mod import_preview;
pub mod lifecycle;
pub mod logging;
pub mod plugins;
pub mod state;
pub mod tray;

use std::sync::Arc;

use tauri::Manager;

pub fn run() {
    init_tracing();

    // `TA_EXPORT_BINDINGS=1` 时只导出 TS 绑定然后退出：**不建窗口、不连数据库**。
    // 这是 T1 的绑定生成入口（`scripts/gen-bindings.ps1`）。
    if std::env::var("TA_EXPORT_BINDINGS").as_deref() == Ok("1") {
        let path = std::env::var("TA_EXPORT_BINDINGS_PATH")
            .unwrap_or_else(|_| "../src/shared/bindings.ts".to_string());
        match commands::export_bindings(std::path::Path::new(&path)) {
            Ok(()) => tracing::info!(path, "已导出 TypeScript 绑定"),
            Err(e) => {
                tracing::error!(code = %e.code, error = %e.message, "导出 TypeScript 绑定失败");
                std::process::exit(1);
            }
        }
        return;
    }

    // 卸载钩子（`--uninstall-autostart`）与验收探针（`--t2-probe-autostart`）在这里就结束：
    // 不起窗口、不连数据库 —— 卸载时 GUI 少一层依赖就少一种"卸不干净"的可能。
    if lifecycle::handle_cli_early_exit() {
        return;
    }

    // 7 个插件统一注册（顺序有理由，见 plugins.rs；只注册不使用的插件一律不留，见 [docs/50]）
    let builder = plugins::register(tauri::Builder::default());
    // T1：命令注册。同一个 builder 既生成 Tauri handler，也用于导出 TS 绑定，
    // 所以"命令清单"只有一处（漏登记会在绑定文件里露馅）。
    let specta = commands::specta_builder();

    builder
        .invoke_handler(specta.invoke_handler())
        .setup(|app| {
            let handle = app.handle().clone();
            // 0) 主窗口：**在代码里**建（`tauri.conf.json` 里那份是 `create: false`）。
            //
            //    [真机 E2E 的硬前提] 官方 E2E 工具链（tauri-driver + msedgedriver）靠
            //    **`WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=<port>`**
            //    让 WebView2 开一个 DevTools 端点；可 wry 0.55 只要设置了
            //    `AdditionalBrowserArguments`，就在进程内**覆盖**掉那个环境变量
            //    （`wry-0.55.1/src/webview2/mod.rs`：`options.set_additional_browser_arguments(...)`），
            //    于是端点永远不存在，WebDriver 只会报
            //    `DevToolsActivePort file doesn't exist`（实测：release 与 debug 两个 exe 都一样）。
            //
            //    所以这里把该环境变量**并进**窗口参数：设了才并，没设就与原来一字不差 ——
            //    生产环境不会设它，因此这是一个**只在测试里生效**的透传缝，不是产品开关。
            // [docs/31 §28] **窗口建不出来要吵，不许静默退出**。
    //
    // 真机 E2E 排查花了 15 轮的根因之一就是这里：窗口创建失败会一路 `?` 到 `setup` 以外，
    // 进程**安静地退出**，日志停在"网关已装配"那一行 —— 现场只剩下"应用启动后自己没了"。
    // 这里把它降级成**明确的错误日志 + 带原因的失败**（仍然不启动，但不留谜）。
    if let Err(e) = create_main_window(app) {
        tracing::error!(
            error = %e,
            "主窗口创建失败：界面起不来。常见原因：WebView2 运行时缺失 / 用户数据目录不可写",
        );
        return Err(e.into());
    }
            // 1) 生命周期：解析启动参数 / 托盘模式隐藏窗口 / 注册退出守卫与通知中心
            lifecycle::on_setup(&handle)?;
            // 2) 托盘：先用占位数据源亮起来（换真数据是 T2 的独立接缝）
            // [docs/31 §29] **托盘装不上不许把整个应用拖死**。
    //
    // 真机实测（受限环境）：`tray::init` 返回 `托盘图标 error: 拒绝访问。(os error 5)`，
    // 原来这个 `?` 让 `setup` 失败 → Tauri panic → 进程退出（exit code 0xC0000409），
    // 表现为"应用启动后无声消失、界面永远白屏"。**托盘只是便利功能**，
    // 界面与网关不依赖它：这里降级成 WARN，如实记原因，然后继续启动。
    if let Err(e) = tray::init(&handle) {
        tracing::warn!(error = %e, "托盘不可用（装不上）：应用继续启动，界面与网关不受影响");
    }
            // 3) T1：打开库（含迁移）并把 AppState 交给命令层。
            //    setup 是同步的、开库是异步的 → 用 Tauri 的 runtime 跑完再 manage，
            //    这样命令被调用时 `State<AppState>` 一定已就绪（否则前端只会看到
            //    "state not managed" 这种最难排查的错）。
            let db_path = state::AppState::default_db_path()?;
            let app_state = tauri::async_runtime::block_on(state::AppState::bootstrap(&db_path))?;
            tracing::info!(db = %db_path.display(), "数据库就绪");

            // 3.5) [docs/59] **把配置里的 `request_log.*` 同步进 trace store**。
            //
            // 必须在这里（服务对外之前）：配置说 `mode: meta` 而 store 的
            // `store_setting.trace.mode` 还躺着一条古老的 `"off"` 时，
            // `TraceStore::write` 会**直接 return** —— 表现就是"日志一条都没有"，
            // 而用户在界面上找不到原因（第 330 轮的真机现象）。
            //
            // 同步失败**不让启动失败**：日志记不下来是功能缺失，起不来是产品不可用。
            // 但必须喊出来（静默降级正是这一处原本的问题）。
            if let Err(err) = tauri::async_runtime::block_on(
                commands::log::sync_request_log_settings(&app_state, &app_state.doc()),
            ) {
                tracing::warn!(
                    code = %err.code,
                    // 人话嵌进消息里：`message` **不在** [docs/14 §5] 的白名单里，
                    // 而它也确实不该进契约（给人看的，不是给筛子用的）。
                    // `log_field_guard` 会当场拦下白名单外的字段名 —— 它拦得对。
                    "同步 request_log 设置到 trace store 失败：日志可能不落库（{}）",
                    err.message
                );
            }

            // 4) 内核装配：按配置造控制器。
            //    * 没配 `net.kernel.path` → None（用户可能只想用直连通道）
            //    * 装配失败 → 也是 None，但**带上真实原因**，不冒充"没配内核"
            let doc = app_state.doc();
            let data_dir = app_state.data_dir();
            let (kernel, missing_reason, port_map): (
                Option<Arc<dyn ta_kernel::KernelController>>,
                Option<String>,
                ta_kernel::PortMap,
            ) = match boot::build_kernel(&doc, &data_dir) {
                Ok(Some(built)) => {
                    let controller: Arc<dyn ta_kernel::KernelController> = built.supervisor.clone();
                    app_state.set_kernel(controller.clone());
                    // 配置源必须留一份句柄：命令层改配置后要更新它再 apply
                    app_state.set_kernel_source(built.source.clone());
                    // 网关的 KernelProbe 是同步接口 → 也要存一份具体类型
                    app_state.set_kernel_supervisor(built.supervisor.clone());
                    tracing::info!(
                        program = %built.supervisor.config().program.display(),
                        "内核控制器已注入 AppState（尚未拉起）"
                    );
                    (Some(controller), None, built.port_map)
                }
                // 没配内核：直连通道仍然可用，网关也照常装配（只是没有非 direct lane 的端口）
                Ok(None) => (None, None, ta_kernel::PortMap::default()),
                Err(e) => {
                    // `build_kernel` 返回 `TaError`：码必须成对记，否则日志按 TA-xxxx 搜不到
                    // （[14 §6]；`error_log_guard` 会守住这条）。
                    tracing::error!(
                        code = e.code(),
                        error = %e,
                        "内核装配失败，将以直连通道降级运行"
                    );
                    (None, Some(format!("内核装配失败：{e}")), ta_kernel::PortMap::default())
                }
            };

            // 5) 网关装配：走 E5 的 `build_runtime`（**唯一**装配入口，
            //    我们不自己拼管线）。网关与内核是两件事：没配内核时它照样该起来服务直连通道。
            let gateway = gateway::GatewayBoot::assemble(handle.clone(), &app_state, port_map);
            // 把**唯一出站通道**（deps）与准入闸门交给 `AppState`。
            //
            // 顺序是契约：**先有 deps → 再 set_gateway → 再对外可用**（`app.manage` 在下面）。
            // 为什么非接不可：命令层要发 HTTP 只能走 `GatewayDeps.dialer`（`src-tauri` 不依赖
            // `reqwest`，另开一条会绕过线路策略），`app_status` 的"网关状态"也读这个槽位。
            // 这一句以前**只接了闸门**：`set_gateway` 没有任何调用者 → `gateway_ready()` 恒假、
            // 闸门缺失的路径上 `app_status` 会把"装配过"报成 `stopped`（状态撒谎）。
            //
            // **"有意不装配"这件事本版不存在**：`assemble` 无条件执行、且返回 `Self` 而不是
            // `Result`（它不会失败），所以 `gateway_ready() == false` 只可能表示"还没走到这一步"，
            // **不能**被读成"没配网关 / 被停用"。将来真要有"网关停用"的启动路径，必须在
            // `AppState` 里**显式区分**这两种"没有"，而不是继续让一个布尔值兼表两义。
            activate_gateway(&app_state, gateway.deps(), gateway.gate());
            // **暂停不影响 `/healthz`**，所以状态读取必须分两路：健康看探针、暂停看闸门。
            tracing::info!(port = %gateway.listen(), "网关已装配（尚未开始服务）");

            app.manage(app_state);

            // 6) 启动编排：阶段广播与托盘状态共用同一个 holder，
            //    所以"托盘显示什么"和"编排走到哪"不可能各说各话。
            let holder = Arc::new(boot::BootPhaseHolder::default());
            app.manage(holder.clone());

            // 托盘换成"真实就绪驱动"的数据源：图标只在编排真就绪时变绿
            // 同理：换数据源失败也只是"托盘状态不更新"，不该让启动失败（[docs/31 §29]）
    if let Err(e) = tray::set_source(&handle, Arc::new(boot::PhaseTraySource::new(holder.clone()))) {
        tracing::warn!(error = %e, "托盘数据源切换失败：托盘状态可能不刷新");
    }

            let sink: Arc<dyn boot::PhaseSink> =
                Arc::new(boot::TauriPhaseSink::new(handle.clone(), holder.clone()));
            let mut deps = boot::KernelBootDeps::new(kernel, sink).with_gateway(gateway);
            if let Some(reason) = missing_reason {
                deps = deps.with_missing_reason(reason);
            }
            let args = lifecycle::startup_args(&handle);
            tauri::async_runtime::spawn(async move {
                let outcome =
                    lifecycle::BootOrchestrator::run(&deps, &args, boot::READY_TIMEOUT).await;
                // 契约里已有的名字是 `delay_s`（不是 `delayed_s`）；`detail` 按
                // [14 §5.6] 的裁决写进 message，不开自由文本字段。
                let detail = outcome.detail.as_deref().unwrap_or("无");
                tracing::info!(
                    phase = outcome.phase.as_str(),
                    delay_s = outcome.delayed_s,
                    "启动编排结束：{detail}"
                );
            });

            // [docs/29 §2.3] 订阅**自动更新**：算到期（第一步）→ 真刷新（第二步）。
            //
            // 第二步**不重写刷新链路**，只是调用它（`auto_refresh_subscription`，`force = false`）：
            // 下载 / 校验 / 坏更新拦下并保留上一版 / 版本裁剪 / 记流量，全都还是那一套 ——
            // 复用是这里唯一正确的做法，"顺手改一点"就会和手动刷新分叉。
            //
            // 60 秒是**轮询粒度**，不是刷新周期：周期由每份订阅自己的 `interval_s` 决定
            // （默认 6 小时），这里只是"多久来看一眼有没有到期的"。
            let sub_ticks = handle.clone();
            tauri::async_runtime::spawn(async move {
                // 首轮**无论有没有到期的都写一行**：否则"日志里没有这行"既可能是"没有到期的"、
                // 也可能是"这个任务根本没起来"，两者在真机上完全分不清（诊断缺口）。
                let mut first = true;
                loop {
                    // **先算再睡**：新加/刚改过周期的订阅不用等满一个轮询粒度才被发现；
                    // 同时首轮立刻发生，"这个任务活着"在日志里马上看得见。
                    let state = sub_ticks.state::<state::AppState>();
                    match commands::net::due_subscription_ids(&state).await {
                        Ok(ids) if !ids.is_empty() => {
                            // 第二步：**真的去刷**（与手动刷新同一条链路，只是 `force = false`）。
                            //
                            // 这一步现在**诚实**的前提已经满足：到点判断与内层 `refresh_one`
                            // 用的是**同一套钟**（版本元数据的 `fetched_at_ms`），
                            // 所以 `ok: true` 真的意味着"拉到了并采纳了"，不再出现
                            // "内层跳过却报成功"（那一版已撤回，见 [docs/29 §2.3]）。
                            //
                            // 一条失败不影响其余条：逐条记成败，最后一并说出来 ——
                            // 绝不在这里写"全部成功"，那会在部分失败时撒谎。
                            let mut ok = 0usize;
                            let mut failed = 0usize;
                            let mut autos = 0usize;
                            for id in &ids {
                                match commands::net::auto_refresh_subscription(&state, id).await {
                                    Ok(r) if r.ok => {
                                        ok += 1;
                                        // [docs/29 §2.2] **C 期触发点①：订阅更新后重算「自动」出口**
                                        // （`manual = false` → 粘性优先：上次那条还在就沿用）。
                                        // 订阅换了一批节点、上次那条 IP 没了 → 这里才会重挑。
                                        autos += commands::net::reapply_auto_after_refresh(&state, id).await;
                                    }
                                    Ok(_) => failed += 1,
                                    Err(_) => failed += 1,
                                }
                            }
                            tracing::info!(
                                job = "subscription_sync",
                                "订阅自动刷新：到期 {} 条，成功 {} 条，失败 {} 条，「自动」出口重算 {} 家",
                                ids.len(),
                                ok,
                                failed,
                                autos
                            );
                        }
                        Ok(_) => {
                            if first {
                                tracing::info!(
                                    job = "subscription_sync",
                                    "订阅自动刷新：任务已在跑（本轮没有到期的）"
                                );
                            }
                        }
                        Err(e) => {
                            tracing::warn!(
                                code = %e.code,
                                error = %e.message,
                                "订阅自动刷新：算到期订阅失败（本轮跳过）"
                            );
                        }
                    }
                    first = false;
                    tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                }
            });

            tracing::info!(version = env!("CARGO_PKG_VERSION"), "TokenAgency 启动");
            Ok(())
        })
        // 关窗退托盘（点了"退出"才真退，见 lifecycle::decide_close）
        .on_window_event(lifecycle::on_window_event)
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

/// 网关接线的**唯一一处**：把出站通道（deps）与准入闸门交给 [`state::AppState`]。
///
/// 顺序是契约的一部分：**先有 deps → 再 `set_gateway` → 再对外可用**
/// （调用方在这之后才 `app.manage(state)`）。
///
/// 抽成函数不是为了好看，而是为了让"接线真的发生了"**可断言**：
/// 这段代码原来只接了闸门，`gateway` 槽位永远空着（E3 在 `commands/entity.rs` 登记过），
/// 而当时所有测试都是绿的 —— 因为没有任何一条测试问过"命令层拿不拿得到出站通道"。
fn activate_gateway(
    state: &state::AppState,
    deps: Arc<ta_gateway::GatewayDeps>,
    gate: Arc<ta_gateway::ingress::GatewayGate>,
) {
    state.set_gateway(deps);
    state.set_gateway_gate(gate);
}

fn init_tracing() {
    use tracing_subscriber::{fmt, prelude::*, EnvFilter};
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,ta_gateway=debug,ta_kernel=debug"));

    // 唯一日志路径 = tracing_subscriber（[14 §1.1]：**不引 `tauri-plugin-log`**）。
    // 但"唯一"指的是**不并行两套系统**，不是"只输出到一个地方" —— 这是个**托盘应用，
    // 没有控制台**，只写 stdout 等于用户拿不到任何日志（C1 事实未实现）。
    //
    // 所以：stdout（开发看）+ 文件（用户看、诊断包带走）。文件那层自带两点：
    //   * 非阻塞写（[14 §9.1]：文件 IO 绝不发生在请求路径上）
    //   * 写入时脱敏（[14 §11.2]：这个文件是要被分享出去的）
    let stdout_layer = fmt::layer().with_target(true);

    match logging::file_writer() {
        Some(writer) => {
            let file_layer = fmt::layer().with_target(true).with_ansi(false).with_writer(writer);
            let _ = tracing_subscriber::registry()
                .with(filter)
                .with(stdout_layer)
                .with(file_layer)
                .try_init();
        }
        None => {
            // 落不了盘也要能起来：降级到只有 stdout，**不 panic**。
            // 但这条事实必须是**响亮的**（否则又变成"以为有日志、其实没有"）。
            let _ = tracing_subscriber::registry().with(filter).with(stdout_layer).try_init();
            eprintln!("⚠ 日志目录不可用：日志只输出到 stdout，用户将拿不到日志文件（C1 降级）");
        }
    }
}

/// 建主窗口（`tauri.conf.json` 里那份是 `create: false`，在这里建）。
///
/// **为什么不在配置里让 Tauri 建**：要把 WebView2 的远程调试参数透传给 wry。
///
/// * 官方真机 E2E 工具链（`tauri-driver` + `msedgedriver`）启动 app 时会设
///   `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=<port>`；
/// * 可是 wry 0.55 只要拿到 `AdditionalBrowserArguments` 就在进程内把它设进
///   `CoreWebView2EnvironmentOptions`，**覆盖掉**那个环境变量
///   （`wry-0.55.1/src/webview2/mod.rs` 的 `options.set_additional_browser_arguments(...)`）；
/// * 结果：WebView2 永远不开 DevTools 端点，WebDriver 只会报
///   `DevToolsSession not created: DevToolsActivePort file doesn't exist`
///   —— 实测 release 与 debug 两个 exe 都一样，会话建不起来（真机 E2E 卡在这一步）。
///
/// 所以：**环境变量有值就并进窗口参数**（`<配置里的值> <环境变量的值>`），没值就一字不改。
/// 生产启动不会设这个变量，所以这是**只在测试里生效的透传缝**，不是产品开关 ——
/// 它的存在只是把 WebView2 自己的官方约定还给 WebView2。
fn create_main_window(app: &tauri::App) -> tauri::Result<()> {
    let Some(cfg) =
        app.config().app.windows.iter().find(|w| w.label == crate::tray::MAIN_WINDOW).cloned()
    else {
        // 配置里没有 main 窗口就没得建：如实报错，不静默继续（那会让"界面没了"变成谜）
        return Err(tauri::Error::WindowNotFound);
    };
    let mut cfg = cfg;
    let extra = std::env::var("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS").unwrap_or_default();
    let extra = extra.trim();
    if !extra.is_empty() {
        let base = cfg.additional_browser_args.clone().unwrap_or_default();
        cfg.additional_browser_args =
            Some(if base.is_empty() { extra.to_string() } else { format!("{base} {extra}") });
        tracing::info!("主窗口：已把 WebView2 附加参数并进窗口参数（真机 E2E 的远程调试口）");
    }
    tauri::WebviewWindowBuilder::from_config(app.handle(), &cfg)?.build()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一个**真的库**（临时目录里的 SQLite）—— 接线测试要在真 `AppState` 上做，
    /// 因为被测的就是 `AppState` 的槽位。
    async fn temp_state(tag: &str) -> state::AppState {
        let dir = std::env::temp_dir().join(format!("ta-wire-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("建临时目录");
        state::AppState::bootstrap(dir.join("db.sqlite")).await.expect("开库")
    }

    /// 最小 `GatewayDeps`：夹具只有一份（`gateway::test_gateway_deps`），
    /// 命令层的出站测试用的是**同一份** —— 否则两处会悄悄漂移。
    fn test_deps(state: &state::AppState) -> Arc<ta_gateway::GatewayDeps> {
        gateway::test_gateway_deps(state)
    }

    /// **两个方向都钉住**：没接线时必须是假（反向断言），接线后必须为真，
    /// 而且取出来的就是**同一份** deps（不是复制品）。
    ///
    /// 这条测试值得存在的原因：接线点曾经**完全没人调用**，于是
    /// `gateway_ready()` 恒假、命令层拿不到唯一出站通道 —— 而当时全部测试都是绿的。
    #[tokio::test]
    async fn activate_gateway_wires_deps_and_unset_state_is_false() {
        let state = temp_state("activate").await;
        assert!(!state.gateway_ready(), "未接线时必须为假（反向断言）");
        assert!(state.gateway_gate().is_none(), "未接线时闸门也必须为空");

        let deps = test_deps(&state);
        activate_gateway(&state, deps.clone(), Arc::new(ta_gateway::ingress::GatewayGate::new()));

        assert!(state.gateway_ready(), "接线后必须为真");
        assert!(state.gateway_gate().is_some(), "闸门也必须接上");
        let slot =
            state.gateway.read().expect("读锁不该中毒").clone().expect("命令层必须能取出出站通道");
        assert!(Arc::ptr_eq(&slot, &deps), "取出的必须是同一份 deps，不是复制品");
    }

    /// 状态口径：闸门缺失时，**装配过**就必须报 `running`，只有"什么都没装配"才允许报
    /// `stopped`。这条钉住的是 `set_gateway` 缺失的后果 —— 否则装配过也会被报成"已停止"。
    #[test]
    fn gateway_label_only_says_stopped_when_nothing_was_assembled() {
        use ta_gateway::ingress::GatewayRunState;
        let label = commands::app::gateway_label;
        assert_eq!(label(Some(GatewayRunState::Paused), true), "paused");
        assert_eq!(label(Some(GatewayRunState::Running), true), "running");
        assert_eq!(label(None, true), "running", "装配过（闸门缺失）不能报成停止");
        assert_eq!(label(None, false), "stopped", "什么都没装配才如实报停止");
    }

    /// **防漂移**：生产启动路径必须真的调用接线点。
    ///
    /// 为什么需要这条（而不是只留上面那条行为测试）：行为测试直接调 `activate_gateway`，
    /// 所以**把 setup 里的那一行删掉它照样绿** —— 而"没人调用"正是这个 bug 的原始形态。
    /// 上一版守卫我也踩过同类坑（扫整份文件被测试段喂饱），所以这里**只扫生产段**，
    /// 且要求"调用 + 取 deps"两件事同时出现。
    #[test]
    fn the_startup_path_actually_activates_the_gateway() {
        let full = include_str!("lib.rs");
        let production = full.split("#[cfg(test)]").next().unwrap_or(full);
        assert!(
            production.contains("activate_gateway(&app_state, gateway.deps(), gateway.gate())"),
            "启动路径必须调用 activate_gateway(...)：只接闸门就是原来的 bug"
        );
        assert!(
            !production.contains("app_state.set_gateway_gate(gateway.gate());"),
            "闸门与 deps 必须走同一个接线点，不许只接闸门（那正是漏掉出站通道的写法）"
        );
    }
}
