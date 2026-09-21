# 12 · 工作分解（给子 agent 的派发契约）

> 依据 [10 对外契约](10-interface-spec.md) 与 [11 内部契约](11-internal-contracts.md)，把实现拆成**文件所有权互不重叠**的工作包。
> 规则：**一个工作包 = 一组独占文件 + 一份完成定义 + 一条验证命令**。两个 agent 永不同时写同一个文件。

---

# 0. 三条派发铁律

1. **独占文件所有权**。每个包在下方明确列出"你拥有哪些路径"。不在列表里的文件**只读**，禁止修改。发现需要改别人的文件 → 停下来报告，由我做统一适配。
2. **每包独立可验**。完成定义里必须有一条能在该包内单独跑通的命令（通常是 `cargo check -p <crate>` 或 `cargo test -p <crate>`）。
3. **不许改契约**。[10]/[11] 里的类型、trait、字段名、错误码是冻结的。要改 → 先报告。

**共同基线**：Wave 0 完成后，`cargo check --workspace` 必须是绿的。所有包从这里出发，不允许把基线弄红。

---

# 1. 波次总览

## 1.1 编制约束：**同时最多 5 名员工**

这是硬约束。它带来三条必须遵守的规则：

1. **工作包必须合并到"一包一人、一人一域"** —— 不再出现三个人挤在同一个 crate 里（原 P1/P2/P3 都在 `ta-core`，会抢 `Cargo.toml`、抢 cargo 构建锁）。
2. **契约先冻结，才能并行** —— W0 已把全部类型与 trait 定义完，所以网关可以在 store/kernel 还没实现时就对着 trait 开工。
3. **员工有主岗与副岗** —— 主岗长期不变（积累上下文 = 培养），副岗在主岗交付后顶上，避免闲人。

## 1.2 固定 5 人编制（长期，不频繁换岗）

| 员工 | 主岗 | 主岗文件范围 | 副岗（主岗交付后顶上） |
|---|---|---|---|
| **E1 领域官** | 领域与配置 | `crates/ta-core/src/{domain,config}/**` | 前端外壳 F1（路由/主题/i18n/API 封装） |
| **E2 决策官** | 决策与账本 | `crates/ta-core/src/{routing,sticky,quota,pricing,metering}/**` | **OB1 可观测性（§5.5，用户要求，优先于副岗）** → 前端 看板+日志 F2 |
| **E3 数据官** | 数据与密钥 | `crates/ta-store/**`、`crates/ta-secrets/**` | 前端 配置类页面 F3 |
| **E4 网络官** | 网络接入 | `crates/ta-kernel/**`、`crates/ta-subscription/**` | 前端 网络+成本 F4 |
| **E5 网关官** | 网关（**关键路径 Owner**） | `crates/ta-gateway/**` | Tauri 层 T1+T2（命令/托盘/生命周期） |

**主岗为什么这么分**：一人一层，正好对应架构的依赖方向（[09 §3.3](09-architecture-design.md)），边界天然不重叠，协作成本最低。

### 副岗顶上的临时调整记录

副岗默认按上表归属（E1→F1、E2→F2、E3→F3、E4→F4、E5→T1/T2）。**当副岗归属者正卡在关键路径上时，主管可以把该副岗临时支援给其他员工，但必须在此登记** —— 目的不是审批，是让 W5 集成时能一眼看出哪些 `Work-Package` trailer 是临时调整、不要按固定编制去理解。

| 时间 | 副岗包 | 原归属 | 实际执行 | 理由 |
|---|---|---|---|---|
| 第 9 轮 | **F1** 前端外壳 | E1（主岗已交付） | **E1** | 按编制表正常顶上。选 E1 另有一层理由：F1 的核心产出是 `src/shared/**` 对外契约（bindings/api/通用组件），而 `bindings.ts` 必须与 `ta-core/domain/mod.rs` 逐字段对齐 —— E1 是那份契约的作者 |
| 第 9 轮 | **T2** 托盘+生命周期+开机自启动 | E5 | **E3（支援）** | E5 是网关关键路径 Owner，G2 依赖它正在定义的 `RequestPipeline` trait，不能抽身。T2 不依赖网关，是全包唯一"完全无阻塞"的可并行重活。<br>选 E3 的理由是能力匹配而非凑人手：**T2 要写操作系统状态**（HKCU Run / LaunchAgent），是该功能唯一会碰 OS 的包，而 E3 在 `ta-secrets` 上表现出的"加密不许造假"式判断力正是这类活最需要的。<br>E3 的 T2 提交请打 `Work-Package: E3/T2-支援` |
| 第 9 轮 | **T1** 命令层 + 绑定生成 | E5 | **E3（支援）** | E5 仍在网关关键路径（G2 → G3 → G4），而 **T1 是当前唯一无人认领的关键路径工作包** —— F2/F3/F4 全部等 `bindings.ts`，T2 也要等 `AppState`。<br>同一位员工同时接 T1+T2 是有意的：它们共用 `src-tauri/**`，由一个人做**接缝不会漂**（我另定了 T1↔T2 的接线形状并广播给双方）。<br>提交请打 `Work-Package: E3/T1-支援` |

> **临时支援的收尾要求**：支援包交付后，**接缝形状必须写回本文件或 [11]**，否则 E5 接手时只能靠读代码反推。
> 主管负责在 W5 集成前核对每个 `-支援` trailer 是否都已归位。

## 1.3 波次（每波 ≤ 5 并行）

| 波次 | 内容 | 并行 | 依赖 |
|---|---|---|---|
| **W0** | 工作区骨架 + 全部契约类型/trait 定义（**我**） | 1 | — |
| **W1** | E1 领域+配置 · E2 决策+账本 · E3 数据+密钥 · E4 网络+订阅 · **E5 网关入站+全透传（关键路径，提前开工）** | **5** | W0 |
| **W2** | E5 网关剩余（管线/分发/适配/观测） · E1–E4 集成修复 + 单测补强 · **OB1 可观测性（新增，见 §5.5）** | 5 | W1 |
| **W3** | E5 Tauri 命令层+托盘 · E1–E4：内核真机 spike + 三条不变量自检脚本 | 5 | W2 |
| **W4** | 前端 4 包并行（E1–E4 转副岗）× E5 收尾集成 | **5** | W3 |
| **W5** | 集成、修复、编译、出安装包（**我**） | 1 | W4 |

> **W2 插入 OB1 的理由（第 9 轮，用户要求）**：用户提出"日志系统做完善，用于追踪错误和调试，
> 后续的开发会好很多"。**这不是一个功能，是一个乘数** —— 它决定后面每一轮开发里
> "出问题能不能查"的成本。**越早做，后面每一轮都受益；越晚做，回头改的调用点越多。**
> 所以它插在 W2（而不是等 W5 收尾），且**优先级高于所有非关键路径的功能**。

> **W1 的关键优化**：原计划 W1 只做底层，网关要等 W2 才动。因为 W0 已把网关依赖的 trait 全部冻结，**E5 可以在 W1 就做 G1（入站 + 全透传）** —— 这是全项目最长的关键路径，提前一轮并行就是净赚。

## 1.4 首批 7 人 → 5 人编制的收敛

第一批按 7 个包派发（已超出 5 人上限），交付后按下表收敛：

| 原包 | 原员工 ID | 收敛去向 |
|---|---|---|
| P1 领域与配置 | `562f5273` | → **E1**（留任主岗） |
| P2 路由与策略 | `3c6da6cc` | → **E2**（留任，接手 P3 的范围） |
| P3 额度/定价/计量 | `24798a8e` | 交付后并入 E2；该员工转**机动岗** |
| P4 存储 | `ac193295` | → **E3**（留任，接手 P7 的范围） |
| P5 内核 | `5eebd5cb` | → **E4**（留任，接手 P6 的范围） |
| P6 订阅 | `3c6d0981` | 交付后并入 E4；转**机动岗** |
| P7 密钥 | `a2a89c4a` | 交付后并入 E3；转**机动岗** |
| G1–G4 网关 | 未派发 | → **E5**（新员工，W1 就开工做 G1） |

**机动岗的用途**（不浪费人力，也不破坏 5 人上限）：内核真机 spike、golden 快照补强、三条不变量的自动检查脚本。用完即回收。

---

# 2. W0 · 工作区骨架（我做）

**产出**：一个 `cargo check --workspace` + `npm run build` 双绿的空壳，所有契约类型已就位。

| 拥有文件 | 说明 |
|---|---|
| `Cargo.toml`（workspace 根） | members、`[workspace.dependencies]` 统一版本、release profile（lto/codegen-units/opt-level/strip） |
| `crates/*/Cargo.toml` | 每个 crate 的依赖声明（严格按 [11] 的依赖方向，禁止环） |
| `crates/ta-core/src/**` | **完整**的领域类型、`TokenKind`、`TokenUsage`、`ResolvedPrice`、`RouteDecision`、`TaError`、所有 trait 定义（空实现或 `todo!()`） |
| `crates/*/src/lib.rs` | 模块声明 + trait 空实现骨架 |
| `src-tauri/{Cargo.toml,tauri.conf.json,capabilities/*,build.rs,icons/*,src/main.rs}` | 最小可运行 Tauri 应用 |
| `src/{main.tsx,App.tsx,package.json,vite.config.ts,tsconfig.json,index.html}` | 最小可运行前端 |
| `scripts/{dev.ps1,build.ps1,gen-bindings.ps1}` | 开发/构建脚本 |
| `rust-toolchain.toml`、`.gitignore` | |

**DoD**：`cargo check --workspace` 通过；`npm install && npm run build` 通过；`cargo tauri dev` 能起一个空窗口。

---

# 3. W1 · 底层 crate

## P1 · `ta-core` 领域与配置

| 项 | 内容 |
|---|---|
| **拥有** | `crates/ta-core/src/domain/**`、`crates/ta-core/src/config/**` |
| **依赖契约** | [11 §1.2] 的领域类型（W0 已定义结构，这里补**行为**：默认值、校验、序列化） |
| **做** | ① 领域结构体 + 默认值 + `schemars` 派生 ② `ConfigValidator` 实现：JSON Schema + 引用完整性（channel→credential/provider/lane/model 必须存在）③ 配置版本与迁移 ④ `serde_path_to_error` 报错定位 |
| **不做** | 路由、额度、定价（P2/P3） |
| **DoD** | `cargo test -p ta-core config::` 全绿；非法配置能报出**具体字段路径** |

## P2 · `ta-core` 路由与策略

| 项 | 内容 |
|---|---|
| **拥有** | `crates/ta-core/src/routing/**`、`crates/ta-core/src/sticky/**` |
| **做** | ① `PolicyEngine::decide`：有序规则表，首个命中即生效 ② 候选集计算 + `TagFilter`（默认不过滤）③ 全部 7 种 selector ④ `StickyResolver`：key 派生顺序 `x-session-id → x-conversation-id → metadata.user_id → body_hash` ⑤ 模拟器（`policy_simulate` 的纯函数实现） |
| **DoD** | 单测覆盖：规则顺序、每种 selector、粘性命中/释放/复活、模拟器输出与 decide 一致（同一输入比对 100 次） |

## P3 · `ta-core` 额度 / 定价 / 计量

| 项 | 内容 |
|---|---|
| **拥有** | `crates/ta-core/src/quota/**`、`pricing/**`、`metering/**` |
| **做** | ① `QuotaLedger`：四观测源取最保守值 + 状态机（ok/soft/exhausted/unknown）+ 窗口滚动 ② `PricingEngine`：精确/通配匹配、倍率、`plan_fee`、价格快照 ③ `TokenMeter::normalize`：**严格按 [11 §6] 表实现，必须先把 cached 从 input 总数里减掉** |
| **DoD** | 单测覆盖：三家上游 usage 样本各 5 个 → 归一化结果断言；额度到 100% 自动 exhausted；改价不改历史（快照） |

## P4 · `ta-store`

| 项 | 内容 |
|---|---|
| **拥有** | `crates/ta-store/**`（含 `migrations/*.sql`） |
| **做** | ① sqlx 连接池（WAL）+ 迁移 ② 实现 `ConfigStore` / `UsageStore` / `TraceStore` / `StickyStore` / `HealthStore` / `SecretMetaStore` ③ **事务边界**按 [11 §4.3] ④ `usage_daily` 按 `token_kind` **行维度**写入 ⑤ 清理任务 `gc()` |
| **硬约束** | 图片只存 `{type,media_type,bytes,sha256_prefix}`；key 绝不入库；`price_snapshot` 必写 |
| **DoD** | 内存 SQLite 集成测试：写 1000 条请求 → `usage_query` 按任意维度分组结果正确；`gc()` 按 retention 与 max_rows 双限清理 |

## P5 · `ta-kernel`

| 项 | 内容 |
|---|---|
| **拥有** | `crates/ta-kernel/**` |
| **做** | ① `KernelConfigGen::generate`：**严格按 [11 §2.2] 的模板**，强制写死隔离项 ② `PortAllocator`：bind 试探 + 落库复用 ③ 进程生命周期（就绪探测 / 退避重启 / Job Object / 无孤儿）④ REST client（§2.3 全部端点）⑤ `snapshot()` 解析节点与分组 |
| **关键** | 生成前必须校验 `listeners[].proxy` 引用的组名存在，否则内核会启动失败 |
| **DoD** | ① 配置生成快照测试（`insta`）② 用一个假 REST server（wiremock）验证 `set_node` / `delay` / `snapshot` ③ 进程看护单测（杀掉后能重启） |

## P6 · `ta-subscription`

| 项 | 内容 |
|---|---|
| **拥有** | `crates/ta-subscription/**` |
| **做** | ① 独立 Client 下载（不走 lane）+ `backon` 重试 ② 格式嗅探（Html / ClashYaml / Base64Links / PlainLinks）③ `userinfo` 与 `Profile-Update-Interval` 解析 ④ `SubscriptionValidator::accept`：**坏更新拒绝采纳**（节点数骤降、HTML、空列表）⑤ 版本保留 N 份 + 回滚 |
| **DoD** | fixture 测试：真实订阅样本（脱敏）→ 节点数/组数正确；HTML 响应 → 保留上版 |

## P7 · `ta-secrets`

| 项 | 内容 |
|---|---|
| **拥有** | `crates/ta-secrets/**` |
| **做** | ① `keyring` 封装（Windows Credential Manager / macOS Keychain / Linux Secret Service）② `${secret:name}` / `${env:VAR}` 解析 ③ 明文检测（`sk-...`）与迁移 ④ 便携模式加密文件回退 |
| **DoD** | 单测：引用解析全分支；明文检测；无钥匙串环境下的回退路径 |

---

# 4. W2 · 网关

## G1 · ingress + 全透传（**关键路径**）

| 项 | 内容 |
|---|---|
| **拥有** | `crates/ta-gateway/src/ingress/**`、`src/passthrough/**`、`src/admin/**` |
| **做** | ① axum 路由表（[10 §1.2] 全部端点）② 本地密钥 + RPM 限流 ③ `PassthroughPolicy::build_headers`（**整体转发 + 4 处定点修改**）④ `patch_body`（Value 定点 patch）⑤ `diff_headers` 自检 ⑥ 管理 API（[10 §3]） |
| **禁止** | 任何 `#[derive(Deserialize)]` 用于请求体结构 |
| **DoD** | **golden 快照测试**（`insta`）：含 `image_url(detail)`、未知 part 类型、未知顶层字段、`anthropic-beta`、自定义 UA 的请求 → 上游收到的字节与客户端发出的逐字段一致（除 4 处例外） |

## G2 · 管线 + 分发 + 流式

| 项 | 内容 |
|---|---|
| **拥有** | `crates/ta-gateway/src/pipeline/**`、`src/dispatch/**`、`src/failover/**` |
| **做** | ① 12 步管线编排 ② `KernelDialer::execute`（每 lane 一个 reqwest Client，绑定 3910x）③ SSE 零缓冲转发（`bytes_stream`）④ 下游断连 → `CancellationToken` 取消上游 ⑤ 失败链 + **重试边界（已输出字节绝不重试）** + 冷却退避 |
| **DoD** | wiremock 测试：429→换通道、5xx→换通道、connect 失败→换 lane；**"输出 3 个 chunk 后断连 → 只有 1 次上游请求"** |

## G3 · 协议适配（IR 路径）

| 项 | 内容 |
|---|---|
| **拥有** | `crates/ta-gateway/src/adapt/**` |
| **做** | ① 四协议 `ProtocolAdapter`（openai-chat / openai-responses / anthropic / gemini）② canonical IR ③ `verify_no_loss` ④ 跨协议图片块映射；**无法映射 → 拒绝（415），绝不文本化** |
| **DoD** | 双向往返测试：openai→anthropic→openai 的图片块/工具调用保持语义；无法映射时返回 TA-2006 |

## G4 · trace + 事件 + 计量接线

| 项 | 内容 |
|---|---|
| **拥有** | `crates/ta-gateway/src/trace/**`、`src/events/**`、`src/meter/**` |
| **做** | ① trace 落库（尝试链、耗时瀑布、`header_diff`）② `EventBus` 实现 + **200ms 批量合并 + 环形缓冲** ③ usage 解析接线（调 `TokenMeter`）+ 记账 + 价格快照 ④ 响应头注入 `x-tokenagency-*` |
| **DoD** | 单测：事件高频写入被合并（1000 条 → ≤ 若干批次）；trace 与实际尝试链一致 |

---

# 5. W3 · Tauri 层

## T1 · 命令层 + 绑定生成

| 项 | 内容 |
|---|---|
| **拥有** | `src-tauri/src/commands/**`、`src-tauri/src/state.rs`、`src-tauri/src/lib.rs` |
| **做** | ① [10 §2.2] 全部命令（**函数体 ≤ 20 行**，只做转发）② `tauri-specta` 导出 `src/shared/bindings.ts` ③ `IpcError` 映射（用 [11 §5] 错误码）④ 事件桥接（EventBus → `emit`） |
| **DoD** | `npm run gen-bindings` 产出 TS；`cargo check -p tokenagency` 通过 |

## T2 · 托盘 + 插件 + 生命周期

| 项 | 内容 |
|---|---|
| **拥有** | `src-tauri/src/tray.rs`、`src-tauri/src/plugins.rs`、`src-tauri/src/lifecycle.rs`、`capabilities/` |
| **做** | ① 托盘状态色图标（4 态 + 实验模式角标）② 动态菜单（策略/节点/额度/紧急开关，[05 §1.2]）③ 12 个插件注册 ④ 关窗退托盘、单实例、通知去重 ⑤ 启动编排（骨架屏 → 内核就绪 → 转绿）⑥ **开机自启动**（见下） |
| **开机自启动（本包必做，不是可选）** | ① `tauri-plugin-autostart` 接线，`MacosLauncher::LaunchAgent`；Windows 走 HKCU Run 键（免提权）② **`app_get_autostart` 必须真的去读系统状态**，不许回显配置值；填 `AutostartState` 全部字段（[10 §2.2](10-interface-spec.md)）③ 启动模式 `tray`（默认，静默进托盘）/ `window` ④ 启动延迟由我们自己实现（命令行传参 → 启动后先睡再拉内核）⑤ **卸载钩子删除系统条目**（NSIS `customUnInstall`）⑥ 漂移检测：`desired != actual` 时不自动改回，交给 UI 提示 ⑦ 边界规则见 [09 §1.2](09-architecture-design.md) E1–E5 |
| **DoD** | 手动/自动验证：托盘菜单能切策略与节点；关窗不退进程；二次启动只唤起已有窗口 |

---

## 5.5 OB1 · 可观测性与诊断（**用户直接要求，W2 插入**）

> 完整契约见 **[14 · 日志、错误追踪与诊断](14-observability.md)** —— 本表只写范围与所有权。

| 项 | 内容 |
|---|---|
| **背景** | 现有 101 处 `tracing` 调用基础不错，但有四个洞：① 日志只进 stdout，**托盘应用没有控制台** → 无处可看；② `TaError` 的 `Display` 不含错误码 → **无法按 TA 码检索**；③ 字段名不统一 → 拼不出一次请求的轨迹；④ 应用日志这一路**没有脱敏层** |
| **拥有** | `crates/ta-core/src/diag/**`、`src-tauri/src/logging.rs`、`scripts/logs.ps1`、三条守卫测试（`log_field_guard` / `error_log_guard` / `sanitize`） |
| **做** | ① 落点：文件 + 8MiB/跨天轮转 + 保留 7 份、**非阻塞写**（含丢弃计数）② **脱敏格式化层**（规则见 [14 §7.2]，正例+反例+端到端三条测试）③ panic 钩子 + 崩溃文件 ④ **级别热更新**（不重启改级别）⑤ 错误日志**强制带 `code`** ⑥ 诊断包导出（**打完后二次全文扫描**，命中即中止）⑦ 三条守卫测试 |
| **不做** | 不改 `TaError::Display`（[14 §6.2] 说明理由）；不把应用日志写进 SQLite（[14 §9.3]）；不引入 OpenTelemetry；不做任何日志上报 |
| **契约边界（重要）** | `src-tauri/src/lib.rs` 与 `plugins.rs` 属 **E3（T1/T2）**，OB1 **不直接改**：由 OB1 给出确切改动内容（`logging::init` 的调用位置、`tauri-plugin-log` 的去留），E3 落笔。**避免两个人在 `src-tauri/**` 里抢文件** —— 这是 [12 §1.1] 的纪律 |
| **DoD** | [14 §13] 的 10 条验收全部可复现；三条守卫测试**先证其会红、再修到绿** |

**为什么给 E2（决策官）而不是写日志最多的 E5**：OB1 的核心不是"打日志"，是
**"日志说的话必须是真的"** —— 脱敏不能漏、错误码不能缺、丢弃要计数、诊断包不能"以为安全"。
E2 在额度账本上做的是同一类工作（[11 §6] 铁律：**不猜不估算**，总额守恒）。
**"一条会骗人的日志" 比 "没有日志" 危险得多** —— 这需要的是对"诚实"的敏感，而不是打字速度。

---

# 6. W4 · 前端

四个包共用 `src/shared/**`（我提供，只读给它们）：`bindings.ts`、`api/`（invoke 封装）、`components/`（数字卡/Sparkline/额度条/状态灯）。

| 包 | 拥有 | 页面 |
|---|---|---|
| **F1** 外壳 | `src/app/**`、`src/stores/**`、`src/shared/**` | 路由、主题（MUI CSS 变量 + 深色优先）、i18n、布局、侧边栏 |
| **F2** 看板 + 日志 | `src/pages/dashboard/**`、`src/pages/logs/**` | [07 §1] 看板（4 数字卡 + ECharts 趋势 + 实时流 + 额度条）、[07 §2] 日志（DataGrid + 详情抽屉 + 耗时瀑布） |
| **F3** 配置类 | `src/pages/providers/**`、`models/**`、`channels/**`、`policies/**` | Provider（含 echo 测试页）、模型目录（一键导入）、通道矩阵（线路下拉）、策略（条件构造器 + 模拟器） |
| **F4** 网络 + 成本 + 设置 | `src/pages/network/**`、`cost/**`、`settings/**`、`onboarding/**` | 订阅管理、节点列表、分配 Tab、反向视图、成本中心查询构建器、首启向导、**设置页（含开机自启动开关 + 漂移提示，UI 规格见 [07 §7.2](07-ui-pages-and-cost.md)）** |

**共同 DoD**：`npm run build` 通过；页面用 mock 数据可渲染（Rust 未就绪时不阻塞开发）。

---

# 7. W5 · 集成与打包（我做）

1. 全量 `cargo check --workspace` / `cargo test --workspace` → 修接口缝隙
2. `cargo tauri build` → 产出 MSI/NSIS 安装包
3. 冒烟：起 App → 托盘出现 → 配一个 provider → 用 curl 打 `127.0.0.1:8787/v1/models` → 装一个 Clash 订阅 → 通道矩阵指定线路 → 出口 IP 验证
4. 三条不变量的自动检查（I1 netstat / I2 快照 diff / I3 golden）
5. 写 `CHANGELOG.md` + 更新 README 的"如何运行"

---

# 8. 本轮目标（先说清楚，避免期望错位）

**本轮跑通的是 M1 范围**：

- ✅ 单协议（OpenAI Chat）全透传网关 + 多 provider/凭据/通道 + 模型映射
- ✅ 内核 sidecar 接入 + `direct` lane + 1 条订阅线 + 通道级线路分配
- ✅ 策略引擎 + 粘性 + 额度账本 + 故障转移
- ✅ 请求日志 + 成本中心（token 细分 + 查询构建器）
- ✅ 托盘 + 上述页面
- ✅ Windows 安装包

**本轮不含**（下一轮）：跨协议 IR 的完整覆盖（G3 先做能跑的最小集）、内建拨号 `BuiltinDialer`、TUN/系统代理高级模式、macOS/Linux 打包、自动更新。

理由：先把"客户端 → 网关 → 内核 → 美国节点 → 上游"这条主链路和你的三个核心痛点（换套餐 / 换线路 / 不丢字段）打通并装出来，比铺满功能更有价值。铺满功能是后续波次的事。
