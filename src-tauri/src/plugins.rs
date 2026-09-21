//! 插件注册（[12 §5 T2] ③）—— 12 个插件在这里**统一**注册，顺序有意。
//!
//! 为什么单独一个文件而不是散在 `lib.rs` 里：注册顺序、以及"哪些插件**故意不授予权限**"
//! 都是要长期维护的决策，散在 builder 链上没人看得见（见 [`PLUGINS`] 与
//! `capabilities/README.md`）。
//!
//! # 顺序
//!
//! 1. **single-instance 必须最前**：它在插件初始化时就去抢单实例锁，越早越能避免
//!    第二个实例先建窗口/先碰数据库再被要求退出（那是真实的脏数据来源）。
//! 2. **log 第二**：后面的插件初始化失败也要能留日志。
//! 3. `window-state` → `notification` → `deep-link` → …… → `shell`：其余顺序无强依赖，
//!    按"越基础越靠前"排。
//!
//! # ⚠️ autostart 插件：注册但**绝不使用**、**绝不授予权限**
//!
//! `tauri-plugin-autostart` 的能力（`enable/disable/isEnabled`）**不满足** [09 §1.2]
//! 的 E1–E5：它没有漂移检测、没有启动延迟、没有 `mode`、也读不到实际注册的命令行。
//! 如果前端能直接调它的 `plugin:autostart|enable`，就等于给"写 OS 状态"开了**第二条路径**，
//! 绕过 `app_get_autostart` 的真读逻辑（[10 §2.2] 规则 1）和 E3/E4。
//!
//! 因此：
//! * 注册它是为了插件清单完整（[12 §5 T2] ③ 的 12 个），**不调用它的 manager**；
//! * `capabilities/` 里**不授予**任何 `autostart:*` 权限（未授予 = 前端调不到）；
//! * 单元测试 `capabilities_never_grant_the_autostart_plugin` 会守住这条线，
//!   谁以后手滑加了权限，测试立刻红。
//!
//! 自启动的真实现是 [`crate::lifecycle`] 里的 `AutostartController`。

use tauri::Runtime;

use crate::lifecycle;

/// 一个插件的静态描述。**注册表要和 `src-tauri/Cargo.toml` 对得上**，
/// 单元测试会校验数量与 id 唯一性。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PluginSpec {
    /// Tauri 侧的插件名（权限标识的前缀，如 `notification` → `notification:default`）
    pub id: &'static str,
    /// 对应的 crate 名（`Cargo.toml` 里怎么写）
    pub krate: &'static str,
    /// 为什么需要它 / 注意事项
    pub note: &'static str,
}

/// 12 个插件（[12 §5 T2] ③）。顺序 = 注册顺序，见模块文档。
pub const PLUGINS: [PluginSpec; 12] = [
    PluginSpec {
        id: "single-instance",
        krate: "tauri-plugin-single-instance",
        note: "必须最先：抢单实例锁，第二个实例唤起初窗后直接退出（[12 §5 T2] ④）",
    },
    PluginSpec {
        id: "log",
        krate: "tauri-plugin-log",
        note: "第二位：后续插件初始化失败也要留痕",
    },
    PluginSpec {
        id: "window-state",
        krate: "tauri-plugin-window-state",
        note: "记住窗口位置/大小；只写应用数据目录，不碰系统设置",
    },
    PluginSpec {
        id: "notification",
        krate: "tauri-plugin-notification",
        note: "系统通知；条数与去重由 crate::lifecycle::NotificationCenter 控制",
    },
    PluginSpec {
        id: "deep-link",
        krate: "tauri-plugin-deep-link",
        note: "tokenagency:// 唤起；**不调 register_all**，注册表写入交给安装包，避免运行时写 OS 状态",
    },
    PluginSpec {
        id: "opener",
        krate: "tauri-plugin-opener",
        note: "打开日志/配置目录、外部链接",
    },
    PluginSpec {
        id: "dialog",
        krate: "tauri-plugin-dialog",
        note: "导入导出 YAML、选内核路径",
    },
    PluginSpec {
        id: "fs",
        krate: "tauri-plugin-fs",
        note: "仅前端需要的文件选择/预览；权限按需最小化",
    },
    PluginSpec {
        id: "process",
        krate: "tauri-plugin-process",
        note: "退出/重启；退出前必须走 lifecycle 的 QuitGuard，否则关窗会被当成退托盘",
    },
    PluginSpec {
        id: "clipboard-manager",
        krate: "tauri-plugin-clipboard-manager",
        note: "「复制接入信息」（BASE_URL + KEY）—— 只复制，不落盘",
    },
    PluginSpec {
        id: "autostart",
        krate: "tauri-plugin-autostart",
        note: "⚠️ 只注册不使用、不授予权限：不支持 E1–E5（漂移/延迟/mode/命令行），真实现在 lifecycle::AutostartController",
    },
    PluginSpec {
        id: "shell",
        krate: "tauri-plugin-shell",
        note: "最后注册：能起外部进程，权限必须最小化（当前 capability 未授予 shell:default）",
    },
];

/// 把 12 个插件注册进 builder。
///
/// 调用方式（`lib.rs`）：
/// ```ignore
/// let builder = plugins::register(tauri::Builder::default());
/// ```
pub fn register<R: Runtime>(builder: tauri::Builder<R>) -> tauri::Builder<R> {
    builder
        // 1) 单实例：第二个实例把参数交给第一个实例，然后自己退出
        .plugin(tauri_plugin_single_instance::init(|app, argv, cwd| {
            lifecycle::on_second_instance(app, &argv, &cwd);
        }))
        // 2) 日志：放在最前，后面出问题也有痕迹
        //
        // ⚠️ 必须 `.skip_logger()`：本应用自己装了 `tracing_subscriber`（`lib.rs::init_tracing`），
        // 而这个插件默认会再 `log::set_logger` 一次 —— 全局 logger 只能有一个，否则
        // **启动即 panic**：`attempted to set a logger after the logging system was already initialized`
        // （这是真机启动才暴露的问题，单元测试发现不了）。
        // 代价：前端 console 经该插件转发过来的 `log` 记录不再落盘。见 T2 汇报的"待裁决 1"。
        .plugin(tauri_plugin_log::Builder::new().skip_logger().build())
        // 3) 窗口位置记忆
        .plugin(tauri_plugin_window_state::Builder::default().build())
        // 4) 系统通知
        .plugin(tauri_plugin_notification::init())
        // 5) 深链（不主动写注册表，见注记）
        .plugin(tauri_plugin_deep_link::init())
        // 6) 打开目录/链接
        .plugin(tauri_plugin_opener::init())
        // 7) 文件对话框
        .plugin(tauri_plugin_dialog::init())
        // 8) 文件访问
        .plugin(tauri_plugin_fs::init())
        // 9) 进程控制
        .plugin(tauri_plugin_process::init())
        // 10) 剪贴板
        .plugin(tauri_plugin_clipboard_manager::init())
        // 11) 自启动插件：注册但永不调用（理由见模块文档）
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--autostart"]),
        ))
        // 12) shell：能力面最大，放最后
        .plugin(tauri_plugin_shell::init())
}

/// `capabilities/` 里的全部 JSON（编译期嵌入，测试用）。
///
/// 用 `include_str!` 而不是运行时读文件：这样"权限被误加"会**在测试里**暴露，
/// 而不是等到用户装上才发现。
pub const CAPABILITY_FILES: [(&str, &str); 1] =
    [("default.json", include_str!("../capabilities/default.json"))];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn twelve_plugins_with_unique_ids() {
        assert_eq!(PLUGINS.len(), 12, "[12 §5 T2] 要求注册 12 个插件");
        let mut ids: Vec<&str> = PLUGINS.iter().map(|p| p.id).collect();
        ids.sort_unstable();
        let before = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), before, "插件 id 不能重复：{ids:?}");

        for p in PLUGINS {
            assert!(!p.krate.is_empty(), "{} 缺 crate 名", p.id);
            assert!(p.note.len() > 10, "{} 的说明太短，等于没写", p.id);
        }
    }

    #[test]
    fn plugin_ids_match_the_cargo_dependencies() {
        // Cargo.toml 里声明的插件依赖必须都在注册表里，反之亦然 —— 防止
        // "加了依赖忘了注册"（那种 bug 只在运行时暴露，而且表现为功能静默失效）
        let cargo = include_str!("../Cargo.toml");
        for p in PLUGINS {
            assert!(cargo.contains(p.krate), "{} 在注册表里但 Cargo.toml 没有 {}", p.id, p.krate);
        }
        for line in cargo.lines() {
            let line = line.trim();
            if let Some(krate) = line.strip_prefix("tauri-plugin-") {
                let krate = format!("tauri-plugin-{}", krate.split('.').next().unwrap_or(""));
                let krate = krate.trim().to_string();
                assert!(
                    PLUGINS.iter().any(|p| p.krate == krate),
                    "Cargo.toml 里的 {krate} 没有出现在 PLUGINS 注册表里"
                );
            }
        }
    }

    #[test]
    fn single_instance_is_registered_first() {
        // 顺序是有理由的（见模块文档）：第一位必须是 single-instance
        assert_eq!(PLUGINS[0].id, "single-instance");
        assert_eq!(PLUGINS[1].id, "log");
    }

    #[test]
    fn capabilities_never_grant_the_autostart_plugin() {
        // 守住"自启动只有一条路径"：前端永远不该能直接调 plugin:autostart|enable
        for (name, body) in CAPABILITY_FILES {
            assert!(
                !body.contains("autostart:"),
                "{name} 里出现了 autostart 权限：那会绕过漂移检测（[09 §1.2] E3/E4）"
            );
        }
    }

    #[test]
    fn capabilities_do_not_grant_shell_by_default() {
        // shell 能起任意进程，默认不给（要开必须显式评审）
        for (name, body) in CAPABILITY_FILES {
            assert!(!body.contains("shell:"), "{name} 里出现了 shell 权限，请先评审");
        }
    }
}
