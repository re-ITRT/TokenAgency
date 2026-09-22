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

    // 12 个插件统一注册（顺序有理由，见 plugins.rs）
    let builder = plugins::register(tauri::Builder::default());

    // T1：命令注册。同一个 builder 既生成 Tauri handler，也用于导出 TS 绑定，
    // 所以"命令清单"只有一处（漏登记会在绑定文件里露馅）。
    let specta = commands::specta_builder();

    builder
        .invoke_handler(specta.invoke_handler())
        .setup(|app| {
            let handle = app.handle().clone();
            // 1) 生命周期：解析启动参数 / 托盘模式隐藏窗口 / 注册退出守卫与通知中心
            lifecycle::on_setup(&handle)?;
            // 2) 托盘：先用占位数据源亮起来（换真数据是 T2 的独立接缝）
            tray::init(&handle)?;
            // 3) T1：打开库（含迁移）并把 AppState 交给命令层。
            //    setup 是同步的、开库是异步的 → 用 Tauri 的 runtime 跑完再 manage，
            //    这样命令被调用时 `State<AppState>` 一定已就绪（否则前端只会看到
            //    "state not managed" 这种最难排查的错）。
            let db_path = state::AppState::default_db_path()?;
            let app_state = tauri::async_runtime::block_on(state::AppState::bootstrap(&db_path))?;
            tracing::info!(db = %db_path.display(), "数据库就绪");

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
            tray::set_source(&handle, Arc::new(boot::PhaseTraySource::new(holder.clone())))?;

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

#[cfg(test)]
mod tests {
    use super::*;

    use ta_core::domain::ConfigDoc;
    use ta_core::pricing::PricingEngineImpl;
    use ta_core::quota::QuotaLedgerImpl;
    use ta_gateway::events::{EventBus, TaEvent};
    use ta_gateway::GatewayDeps;

    /// 空的 `EventBus`：本测试只证明"接线发生了"，不证明事件真的流起来。
    struct NoEvents;

    impl EventBus for NoEvents {
        fn publish(&self, _e: TaEvent) {}

        fn subscribe(&self) -> tokio::sync::broadcast::Receiver<TaEvent> {
            let (tx, rx) = tokio::sync::broadcast::channel(1);
            drop(tx);
            rx
        }
    }

    /// 一个**真的库**（临时目录里的 SQLite）—— 接线测试要在真 `AppState` 上做，
    /// 因为被测的就是 `AppState` 的槽位。
    async fn temp_state(tag: &str) -> state::AppState {
        let dir = std::env::temp_dir().join(format!("ta-wire-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("建临时目录");
        state::AppState::bootstrap(dir.join("db.sqlite")).await.expect("开库")
    }

    /// 最小 `GatewayDeps`：每个部件都只做"能被构造"这一件事 ——
    /// 这条测试断言的是**接线**，不是网关行为（那是 `crates/ta-gateway` 的测试）。
    fn test_deps(state: &state::AppState) -> Arc<GatewayDeps> {
        let doc = ConfigDoc::default();
        Arc::new(GatewayDeps {
            config: state.doc.clone(),
            store: Arc::new(gateway::UsageStoreGateway::new(state.usage.clone())),
            dialer: ta_gateway::kernel_dialer(
                Arc::new(ta_kernel::PortMap::default()),
                ta_gateway::dispatch::DialerOptions::default(),
            ),
            events: Arc::new(NoEvents),
            clock: state.clock.clone(),
            pricing: Arc::new(PricingEngineImpl::new(&doc.pricing)),
            quota: Arc::new(QuotaLedgerImpl::with_clock(state.quota.clone(), state.clock.clone())),
        })
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
