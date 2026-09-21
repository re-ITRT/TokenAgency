# `src-tauri/capabilities/` —— 权限清单（谁都不能随手加）

Tauri 2 的能力（capability）是**白名单**：没写进去的权限，前端就调不到。
这个目录由 **T2** 维护。加权限之前请先读这一页 —— 每一条"不加"都是有意为之。

## 现有能力

| 文件 | 作用窗口 | 授予的权限 |
|---|---|---|
| `default.json` | `main` | `core:default`、窗口 show/hide/set-focus、`core:event:default`、`opener/dialog/fs/process/notification/clipboard-manager:default` |

## ⚠️ 故意**不授予**的权限（加回来之前必须过评审）

| 权限 | 为什么默认不给 |
|---|---|
| `autostart:*` | **给前端一条绕过漂移逻辑的路径**。`app_get_autostart` 必须真读系统状态（[10 §2.2] 规则 1），并在 `desired != actual` 时只提示、不自动改回（[09 §1.2] E3）。`tauri-plugin-autostart` 的 `enable/disable` 没有这些语义 —— 前端能直接调它，就等于绕过了 T2 的 `AutostartController`。**自启动只有一条路径**：`src-tauri/src/autostart.rs`。 |
| `shell:*` | 能起任意外部进程。需要它时必须同时说清楚"起什么、参数怎么来的、怎么防止被前端注入"。当前没有任何必须的用法。 |
| `deep-link:*` 的注册类权限 | 深链注册要写注册表（`HKCU\Software\Classes`）。运行时写 OS 状态是 `I2` 的红线，注册交给安装包做。 |
| `fs:*` 的宽作用域（`fs:allow-*` 任意路径） | 现在只给 `fs:default`。要读写具体目录时，加**最小作用域**的权限项，不要放 `fs:allow-all`。 |

## 为什么有测试守着这里

`src-tauri/src/plugins.rs` 里把 `default.json` 用 `include_str!` 编进测试：

* `capabilities_never_grant_the_autostart_plugin` —— 谁把 `autostart:` 加进能力文件，测试立刻红；
* `capabilities_do_not_grant_shell_by_default` —— 同上，针对 `shell:`。

这比"评审时记得看一眼"可靠：门禁能自动发现，评审只花在真正需要判断的地方。

## 改动流程

1. 说清楚**为什么**需要（哪个功能、哪个命令、不加会怎样）；
2. 用**最小作用域**（具体命令 + 具体路径/窗口），不要用 `*`；
3. 更新上面的表格；
4. 如果是放开上表中任何一条"故意不给"，先找主管（`docs/12 §1.2` 的范围规定）。
