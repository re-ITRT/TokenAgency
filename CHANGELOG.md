# 更新日志

本项目的里程碑记录。**每个条目都必须写清"到什么程度"**，而不是只写"完成了什么" ——
因为"写完代码"和"验证过"是两件事（[docs/13](docs/13-team-and-delivery.md) 的评审协议就是这么定的）。

---

## v0.2.0-w2 · W2 收口基线（2026-09-22）

> **这是"代码库整理完毕、门禁全绿"的基线，不是 M1 完成。**
> M1 还缺 T1 的命令面补齐与启动编排接线（见下方"尚未完成"）。

### 本轮做了什么

**内核验证（M0 结案）** —— 在真 mihomo v1.19.31 上跑完 K1–K8，只剩 K9：

- **K1 成立**：`PUT /configs?force=true` 是进程内热重载（PID 不变、旧端口关、新端口开、可逆）
  → 现有的"能 reload 就 reload"策略正确，不需要"重启内核 + 请求排队"。
- **K2 成立**（但需细化）：冷启动 `use: [provider]` 的组不失败；**然而 provider 加载失败时
  内核照常启动、REST 照常通，所有 lane 静默降级成 `COMPATIBLE`** → "就绪"的判据必须包含
  "每个 provider 的 `proxies[]` 非空"。
- **K3/K4 成立**（各带一条危险附属发现）：`filter` 匹配不到节点会静默变 `[COMPATIBLE]`；
  `listeners[].proxy` 指向不存在的名字**照样绑定成功、零报错**。
- **K5 成立**：多余字段、`header` 列表形式、空集合全部被接受 —— 反过来说明"内核不报错 ≠ 配置正确"。
- **K6 成立**：`mixed-port: 0` 确实关闭默认入站 → I1 由结构保证。
- **K8 成立**：强杀后选中项仍保留（`store-selected` 在变更时写盘）→ 不需要为它做优雅退出。
- **K9 未验**：需要受限沙箱环境，开发环境是 full-access，测了不代表受限场景。

**统一裁决（M0 最值钱的产出）**：**内核在所有可疑情况下都倾向于"沉默地降级"而不是报错。**
所以凡是我们能在生成/启动阶段判定的，就必须自己判定，并选"响亮地失败"那一侧。

**可观测性基础（OB1 的纯逻辑层）**：

- 新增 `crates/ta-core/src/diag/`：脱敏规则、轮转策略、字段命名契约。
- 新增两条**守卫测试**：`log_field_guard`（字段名必须在契约内）、`error_log_guard`（报 `TaError` 必须同时记 `code`）。
- 把"字段名漂移"从愿望变成**会让 CI 红的规则**；并顺手修掉了它抓出来的**真实违规**：
  `lane`→`lane_id`、`subscription`/`sub`→`sub_id`、`provider`→`provider_id`、`cred`→`credential_id`，
  以及 15 处缺 `code =` 的错误日志。
- **契约修正**：§5.5 原来写"不在白名单里就失败"太绝对（实测有 42 个不同的本地上下文字段）。
  改为两层规则：§5.1–§5.4 强制、本地上下文字段登记后允许。

**日志契约的落地修正**：

- `TaError` 的 `Display` 保持只有中文（给用户看的文案不该出现内部编号），
  改为**强制成对记录** `code = e.code()`。
- 实测生效：日志里现在能看到 `code=TA-9001 error=内部错误：…`。

**桌面层（T2 已入，T1 部分）**：

- T2（托盘、插件注册、生命周期、开机自启动）已提交：自启动漂移逻辑复用 `ta-core` 的纯函数、
  `state()` 只读不写、测试用临时注册表键不污染用户真实启动项。
- **移除 `tauri-plugin-log`**：它与 `tracing_subscriber` 是两套并行日志系统（[docs/14 §1.1]）。
  之前用 `.skip_logger()` 只是压住 panic，问题仍在。

**视觉契约（用户直接要求）**：

- 新增 [docs/15-visual-design.md](docs/15-visual-design.md)：高级极简、**emoji 零容忍**。
- 唯一例外：**用户数据里的 emoji 原样呈现**（真实机场组名就是 `🚀 节点选择`）。
  规则是"我们永不自己产生 emoji"，而不是"禁止 emoji"。
- 已清理 `tray.rs` 里的全部 emoji（菜单项、tooltip、额度状态）。

### 归档：那份手写契约

`docs/contracts/bindings-handwritten-reference.ts.txt` —— F1 当年手写的 **95 条命令契约**。
它一直躺在 `src/shared/bindings.ts` 里，而**那个路径被 `.gitignore` 忽略、从未进版本控制**；
生成绑定一旦跑通就会覆盖它，**而当前 T1 只注册了 31 条命令** ——
直接覆盖会把契约从 95 缩到 31，砸掉 F2/F3/F4 的 import。

存档的用途是做"95 vs 31"的差集对照。**后缀是 `.ts.txt` 而不是 `.ts`**：
否则 `tsc` 会把它当源码一起编译，把前端类型检查搞红（这一点本轮真踩到了）。

### 修掉的真实缺陷

- **`Channel` 类型名与 Tauri 的运行时类型冲突**，导致绑定导出完全失败 →
  改名为 `ChannelConfig`（结构体名不进 JSON，**线上形态不变**）。
- **根 `Cargo.toml` 把 `typescript` feature 写在 `specta` 上**（该 crate 没有这个 feature）→
  任何 `specta = { workspace = true }` 的下游 crate 都会解析失败。
- **`gen-bindings.ps1` 跑的是 `cargo build`**（不执行代码，永远写不出文件）→ 改用 `cargo run` 的导出分支；
  并且**先删除输出文件**，否则导出失败时会把旧文件当成"已生成"。
- **pre-commit 的 clippy 会连带 lint 整个依赖图** → 同事在途的告警会卡住别人的提交。
  改为 `--no-deps`：**各人为自己的 crate 负责**。
- **`commit.ps1` 的 `git add` 退出码被静默忽略** → 钩子会检查到空的暂存集（**假绿**）。
- **`IpcError` 144 字节触发 34 处 `result_large_err`** → 装箱 `details`（线上 JSON 形态不变），
  而不是加 `allow` 让检查闭嘴。

### 尚未完成（**这是 v0.2.0 的真实边界**）

| # | 项 | 状态 |
|---|---|---|
| 1 | **T1 命令面**：已注册 31 条，契约要 95 条 | 缺 64 条 |
| 2 | **事件桥接**：`TaEvent` 10 个变体，前端一个都收不到 | 未接线 |
| 3 | **启动编排**：`BootOrchestrator::run` 从未被调用 → 内核不会被拉起 | 未接线 |
| 4 | **绑定导出的第二个冲突**：`TraceDetail.header_diff: serde_json::Value` 里的 `i64` 触发 specta 的 BigInt 禁令 | 见 [docs/06 §11](docs/06-roadmap-and-risks.md) |
| 5 | **`tokenagency` 的 lib 测试目标无法运行**（`STATUS_ENTRYPOINT_NOT_FOUND`） | 见 [docs/06 §11](docs/06-roadmap-and-risks.md) |
| 6 | 前端 F1 视觉重做（按 [docs/15](docs/15-visual-design.md)） | 未开始 |
| 7 | W5 打包与三条不变量验收 | 未开始 |

### 门禁状态（本基线实测）

```
cargo check   --workspace --all-targets        → exit 0
cargo clippy  --workspace --all-targets -D warnings → exit 0
cargo test    --workspace --exclude tokenagency     → 712 passed / 0 failed
```

> ⚠️ **`--exclude tokenagency` 不是偷懒，是第 5 条那个已知问题**：
> 该 crate 的 lib 测试目标在本机加载即崩（`0xc0000139`），与代码无关
> （正式应用 exe 能正常启动），已排除的项见下一条。
