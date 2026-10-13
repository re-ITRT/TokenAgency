//! 插件注册（[12 §5 T2] ③）—— 7 个插件在这里**统一**注册，顺序有意。
//!
//! 为什么单独一个文件而不是散在 `lib.rs` 里：注册顺序、以及"哪些插件**故意不授予权限**"
//! 都是要长期维护的决策，散在 builder 链上没人看得见（见 [`PLUGINS`] 与
//! `capabilities/README.md`）。
//!
//! # 顺序
//!
//! 1. **single-instance 必须最前**：它在插件初始化时就去抢单实例锁，越早越能避免
//!    第二个实例先建窗口/先碰数据库再被要求退出（那是真实的脏数据来源）。
//! 2. **单一日志路径**：**不注册 `tauri-plugin-log`**。
//!    本应用自己装了 `tracing_subscriber`（`lib.rs::init_tracing`），
//!    再挂一个日志插件就是**两套并行系统**（[14 §1.1]）：global logger 只能有一个，
//!    插件默认会再 `log::set_logger` 一次 —— 那是**启动即 panic**
//!    （`attempted to set a logger after the logging system was already initialized`）。
//!    第 10 轮之前用 `.skip_logger()` 绕过，那只是让 panic 不发生：
//!    **两套系统仍然并存，日志该去哪仍然不确定**。正解是移除该插件（本文件已移除）。
//! 3. `window-state` → `notification` → `deep-link` → …… ：其余顺序无强依赖，
//!    按"越基础越靠前"排。
//!
//! # 只注册、零调用的插件一律不留（[docs/50]）
//!
//! `fs` / `process` / `clipboard-manager` / `shell` 四个插件曾是"注册了但前端一次都没调"
//! 的状态，而前三个还在 `capabilities/` 里白给了 `fs:default` / `process:default` /
//! `clipboard-manager:default` —— **未使用的权限就是白送的攻击面**。
//! 它们已连同 Cargo/npm 依赖一起删除。要加回来必须先说明**谁在用**，
//! 并且守卫 `plugin_ids_match_the_cargo_dependencies`（双向）会强制两边同步。
//!
//! # ⚠️ autostart 插件：注册但**绝不使用**、**绝不授予权限**
//!
//! `tauri-plugin-autostart` 的能力（`enable/disable/isEnabled`）**不满足** [09 §1.2]
//! 的 E1–E5：它没有漂移检测、没有启动延迟、没有 `mode`、也读不到实际注册的命令行。
//! 如果前端能直接调它的 `plugin:autostart|enable`，就等于给"写 OS 状态"开了**第二条路径**，
//! 绕过 `app_get_autostart` 的真读逻辑（[10 §2.2] 规则 1）和 E3/E4。
//!
//! 因此：
//! * 注册它是为了插件清单完整（[12 §5 T2] ③ 去 log 后的 7 个之一），**不调用它的 manager**；
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

/// 7 个插件。
///
/// 数字的来历（**每一减都要有理由，不是随手改**）：
/// * [12 §5 T2] ③ 原列 12 个；
/// * 去掉 `tauri-plugin-log` → 11（单一日志路径，见模块文档第 2 条）；
/// * **[docs/50] 去掉 `fs` / `process` / `clipboard-manager` / `shell` → 7**：
///   这四个**只注册、零调用**，却在 `capabilities/` 里白给了 `fs:default` /
///   `process:default` / `clipboard-manager:default` —— 那是攻击面白送。
///   （剪贴板功能走的是**浏览器** `navigator.clipboard`，不需要插件；
///   退出走 `lifecycle` 的 `QuitGuard`；起外部进程是 `ta-kernel` 里的 `std::process`。）
///   守卫 `plugin_ids_match_the_cargo_dependencies` 是**双向**的，
///   所以"Cargo 里还留着、注册表里删了"也会红。
pub const PLUGINS: [PluginSpec; 7] = [
    PluginSpec {
        id: "single-instance",
        krate: "tauri-plugin-single-instance",
        note: "必须最先：抢单实例锁，第二个实例唤起初窗后直接退出（[12 §5 T2] ④）",
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
        id: "autostart",
        krate: "tauri-plugin-autostart",
        note: "⚠️ 只注册不使用、不授予权限：不支持 E1–E5（漂移/延迟/mode/命令行），真实现在 lifecycle::AutostartController",
    },
];

/// 把 7 个插件注册进 builder。
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
        // 2) 窗口位置记忆
        .plugin(tauri_plugin_window_state::Builder::default().build())
        // 3) 系统通知
        .plugin(tauri_plugin_notification::init())
        // 4) 深链（不主动写注册表，见注记）
        .plugin(tauri_plugin_deep_link::init())
        // 5) 打开目录/链接
        .plugin(tauri_plugin_opener::init())
        // 6) 文件对话框（`/cost` 的导出选路径——前端**唯一**用到的插件）
        .plugin(tauri_plugin_dialog::init())
        // 7) 自启动插件：注册但永不调用（理由见模块文档）
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--autostart"]),
        ))
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
    fn seven_plugins_with_unique_ids() {
        // [12 §5 T2] ③ 原列 12 个 →（去 `tauri-plugin-log`）11 个
        // →（[docs/50] 去 `fs` / `process` / `clipboard-manager` / `shell` 四个零调用插件）7 个。
        // 这个数字是**决策的结果**，不是随手改的常量；改它必须同时改上面的理由。
        assert_eq!(PLUGINS.len(), 7, "插件数量与 [docs/50] 的当前基线不一致");
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
    fn no_second_logging_system() {
        // 模块文档第 2 条的强制化：`tracing_subscriber` 是**唯一**日志路径。
        // 谁把 `tauri-plugin-log` 加回来，日志就变成两套并行系统（[14 §1.1]），
        // 而且它的默认行为会再 `log::set_logger` 一次 → 启动即 panic。
        // 这条守卫比"数量 == 7"更能表达真实意图：数量只是它的结果。
        assert!(
            !PLUGINS.iter().any(|p| p.krate == "tauri-plugin-log"),
            "PLUGINS 里出现了 tauri-plugin-log：会与 tracing_subscriber 形成两套日志系统"
        );
        let cargo = include_str!("../Cargo.toml");
        for line in cargo.lines() {
            let line = line.trim();
            if line.starts_with('#') {
                // Cargo.toml 里保留了"为什么不用它"的注记，那是文字不是依赖
                continue;
            }
            assert!(
                !line.contains("tauri-plugin-log"),
                "Cargo.toml 重新引入了 tauri-plugin-log：{line}"
            );
        }
    }

    #[test]
    fn single_instance_is_registered_first() {
        // 顺序是有理由的（见模块文档）：第一位必须是 single-instance
        assert_eq!(PLUGINS[0].id, "single-instance");
        assert_eq!(PLUGINS[1].id, "window-state");
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

    /// **通用版的最小权限守卫**（[docs/50]）：能力集里的每一个 `<plugin>:*` 权限，
    /// 都必须对应一个**已注册**的插件。
    ///
    /// 为什么需要它：`fs:default` / `process:default` / `clipboard-manager:default` 就是这么
    /// 白留了好几个轮次 —— 插件早就不用了，能力集却还在授权。**未使用的权限 = 白送的攻击面**，
    /// 而它不会以任何方式报错（不像缺权限那样运行时炸）。
    ///
    /// `core:` 是 Tauri 内核自己的命名空间（窗口/事件等），不来自插件，所以放行。
    #[test]
    fn capabilities_only_grant_registered_plugins() {
        for (name, body) in CAPABILITY_FILES {
            let parsed: serde_json::Value =
                serde_json::from_str(body).unwrap_or_else(|e| panic!("{name} 不是合法 JSON：{e}"));
            let perms = parsed
                .get("permissions")
                .and_then(|v| v.as_array())
                .unwrap_or_else(|| panic!("{name} 缺 permissions 数组"));
            for perm in perms {
                let Some(text) = perm.as_str() else {
                    // 对象形式（带 scope）也要取到它的 identifier —— 目前仓里没有，
                    // 真出现了就让这条守卫明确报出来，而不是静默跳过。
                    panic!("{name} 里的权限用了对象形式，守卫还没覆盖：{perm}");
                };
                if text.starts_with("core:") {
                    continue;
                }
                let plugin = text.split(':').next().unwrap_or(text);
                assert!(
                    PLUGINS.iter().any(|p| p.id == plugin),
                    "{name} 授予了 `{text}`，但 `{plugin}` 不在 PLUGINS 注册表里 —— \
                     未注册插件的权限是白送的攻击面，删掉它（[docs/50]）"
                );
            }
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
