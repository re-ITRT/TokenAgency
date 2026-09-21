//! TokenAgency 桌面应用库入口。
//!
//! 职责边界：
//! * **[T2]（本文件之外）**：`plugins`（插件注册）、`tray`（托盘）、`lifecycle`（启动编排 /
//!   关窗退托盘 / 通知）、`autostart`（唯一写 OS 状态的模块）。
//! * **[T1]（另一个工作包）**：`state.rs`、`commands/**`、specta 绑定生成、`AppState` 注入。
//!
//! 本文件只做接线，**不放业务逻辑**。T1 需要的三个接缝：
//!
//! ```ignore
//! // 1) 命令注册（T1 自己加）
//! .invoke_handler(tauri::generate_handler![ /* commands */ ])
//! // 2) AppState 就绪后注入托盘数据源 + 跑启动编排（T1 在 setup 里加）
//! tray::set_source(app.handle(), state.clone())?;
//! tauri::async_runtime::spawn(lifecycle::BootOrchestrator::run(&deps, &args, timeout));
//! // 3) 退出走 lifecycle::request_quit（已由托盘"退出"菜单调用）
//! ```
//!
//! 模块声明用 `pub mod`：T1 的 `commands/**` 需要 `use crate::tray::…` / `crate::autostart::…`。

pub mod autostart;
pub mod commands;
pub mod lifecycle;
pub mod plugins;
pub mod state;
pub mod tray;

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
            app.manage(app_state);
            tracing::info!(version = env!("CARGO_PKG_VERSION"), "TokenAgency 启动");
            Ok(())
        })
        // 关窗退托盘（点了"退出"才真退，见 lifecycle::decide_close）
        .on_window_event(lifecycle::on_window_event)
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

fn init_tracing() {
    use tracing_subscriber::{fmt, prelude::*, EnvFilter};
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,ta_gateway=debug,ta_kernel=debug"));
    let _ =
        tracing_subscriber::registry().with(filter).with(fmt::layer().with_target(true)).try_init();
}
