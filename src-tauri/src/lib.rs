//! TokenAgency 桌面应用库入口。
//!
//! **W0 阶段**：只保证能起来一个窗口 + 日志。
//! **W3 阶段**（工作包 T1/T2）在此基础上补：命令层、specta 绑定、托盘、插件、生命周期编排。

use tauri::Manager;

pub fn run() {
    init_tracing();

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_fs::init())
        .setup(|app| {
            tracing::info!(version = env!("CARGO_PKG_VERSION"), "TokenAgency 启动");
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.show();
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

fn init_tracing() {
    use tracing_subscriber::{fmt, prelude::*, EnvFilter};
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,ta_gateway=debug,ta_kernel=debug"));
    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().with_target(true))
        .try_init();
}
