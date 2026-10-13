# `src-tauri/capabilities/` —— 权限清单（谁都不能随手加）

Tauri 2 的能力（capability）是**白名单**：没写进去的权限，前端就调不到。
这个目录由 **T2** 维护。加权限之前请先读这一页 —— 每一条"不加"都是有意为之。

## 现有能力

| 文件 | 作用窗口 | 授予的权限 |
|---|---|---|
| `default.json` | `main` | `core:default`、窗口 show/hide/set-focus、`core:event:default`、`opener:default`、`dialog:default`、`notification:default` |

**只有 8 条**。曾经多出
`fs:default` / `process:default` / `clipboard-manager:default` 三条 —— 那三个插件**只注册、零调用**
（前端从未 import 过它们），**未使用的权限就是白送的攻击面**。它们已连同插件、Cargo 依赖、
npm 依赖一起删除。同一轮还删了 `tauri-plugin-shell`（连注册都没有，能力集里本来也没给它）。

> 前端**唯一**真正用到的插件是 `dialog`（`src/pages/cost.tsx` 导出 CSV 时选路径）。
> 剪贴板功能走**浏览器**的 `navigator.clipboard`，不需要 `clipboard-manager` 插件；
> 退出走 `lifecycle` 的 `QuitGuard`，不需要 `process` 插件。

## ⚠️ 故意**不授予**的权限（加回来之前必须过评审）

| 权限 | 为什么默认不给 |
|---|---|
| `autostart:*` | **给前端一条绕过漂移逻辑的路径**。`app_get_autostart` 必须真读系统状态（[10 §2.2] 规则 1），并在 `desired != actual` 时只提示、不自动改回（[09 §1.2] E3）。`tauri-plugin-autostart` 的 `enable/disable` 没有这些语义 —— 前端能直接调它，就等于绕过了 T2 的 `AutostartController`。**自启动只有一条路径**：`src-tauri/src/autostart.rs`。 |
| `shell:*` | 能起任意外部进程。需要它时必须同时说清楚"起什么、参数怎么来的、怎么防止被前端注入"。当前没有任何必须的用法（**插件本身也已移除**）。 |
| `deep-link:*` 的注册类权限 | 深链注册要写注册表（`HKCU\Software\Classes`）。运行时写 OS 状态是 `I2` 的红线，注册交给安装包做。 |
| `fs:*` | **插件已移除**，所以这些权限根本不存在了。将来真需要读写文件时，加**最小作用域**的权限项，不要放 `fs:allow-all`。 |
| `process:*` / `clipboard-manager:*` | 同上：插件已移除。要加回来先说明**谁在用**（现在没有调用方）。 |

## 为什么有测试守着这里

`src-tauri/src/plugins.rs` 里把 `default.json` 用 `include_str!` 编进测试：

* `capabilities_never_grant_the_autostart_plugin` —— 谁把 `autostart:` 加进能力文件，测试立刻红；
* `capabilities_do_not_grant_shell_by_default` —— 同上，针对 `shell:`；
* `capabilities_only_grant_registered_plugins`（新增）—— **通用版**：能力集里每一个
  `<plugin>:*` 权限都必须对应 `PLUGINS` 里**已注册**的插件（`core:` 放行）。
  这条正是"插件删了、权限忘了删"那个盲区的守卫。

### 诚实说明：这条守卫是**纵深防御**，不是唯一防线

Tauri 自己的 build script **也会**在编译期拒绝未注册插件的权限，报
`Permission fs:default not found, expected one of …`（实测：把 `fs:default` 加回去就编不过）。
所以"删插件忘了删权限"其实**过不了构建**。

那为什么还要留这条单元测试？三点，都不夸大：

1. **报错更早、更清楚**：build script 那行消息只说"权限不存在"，不告诉你
   "它属于一个没注册的插件、应当删掉"；
2. **多守一层**：如果哪天插件的注册路径变了（例如改成按 feature 条件注册），
   build script 的检查未必还兜得住，而这条断言读的是 `PLUGINS` 这张唯一的真源表；
3. **它还把规则写下来了**：下一个人看到"为什么能力集只能引用已注册插件"时有据可依。

`README` 里写清楚"我这条守卫其实不是唯一防线"，比让读者以为离了它就出事要好。

## 改动流程

1. 说清楚**为什么**需要（哪个功能、哪个命令、不加会怎样）；
2. 用**最小作用域**（具体命令 + 具体路径/窗口），不要用 `*`；
3. 更新上面的表格；
4. 如果是放开上表中任何一条"故意不给"，先找主管（范围规定）。
