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

| 波次 | 内容 | 并行度 | 依赖 |
|---|---|---|---|
| **W0** | 工作区骨架 + 全部共享类型/trait 定义（**我做**） | 1 | — |
| **W1** | 底层 crate 实现：core / store / kernel / subscription / secrets | 6 | W0 |
| **W2** | 网关实现：ingress+透传 / 管线+分发 / 协议适配 / trace+事件 | 4 | W1 |
| **W3** | Tauri 层：命令 + 托盘 + 插件 | 2 | W2 |
| **W4** | 前端：外壳 / 看板+日志 / 配置类页面 / 网络+成本 | 4 | W3 |
| **W5** | 集成、修复、编译、出安装包（**我做**） | 1 | W4 |

后续波次（M2/M3 功能补全）在本轮跑通后再开。

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
| **做** | ① 托盘状态色图标（4 态 + 实验模式角标）② 动态菜单（策略/节点/额度/紧急开关，[05 §1.2]）③ 12 个插件注册 ④ 关窗退托盘、单实例、开机自启、通知去重 ⑤ 启动编排（骨架屏 → 内核就绪 → 转绿） |
| **DoD** | 手动/自动验证：托盘菜单能切策略与节点；关窗不退进程；二次启动只唤起已有窗口 |

---

# 6. W4 · 前端

四个包共用 `src/shared/**`（我提供，只读给它们）：`bindings.ts`、`api/`（invoke 封装）、`components/`（数字卡/Sparkline/额度条/状态灯）。

| 包 | 拥有 | 页面 |
|---|---|---|
| **F1** 外壳 | `src/app/**`、`src/stores/**`、`src/shared/**` | 路由、主题（MUI CSS 变量 + 深色优先）、i18n、布局、侧边栏 |
| **F2** 看板 + 日志 | `src/pages/dashboard/**`、`src/pages/logs/**` | [07 §1] 看板（4 数字卡 + ECharts 趋势 + 实时流 + 额度条）、[07 §2] 日志（DataGrid + 详情抽屉 + 耗时瀑布） |
| **F3** 配置类 | `src/pages/providers/**`、`models/**`、`channels/**`、`policies/**` | Provider（含 echo 测试页）、模型目录（一键导入）、通道矩阵（线路下拉）、策略（条件构造器 + 模拟器） |
| **F4** 网络 + 成本 + 设置 | `src/pages/network/**`、`cost/**`、`settings/**`、`onboarding/**` | 订阅管理、节点列表、分配 Tab、反向视图、成本中心查询构建器、设置、首启向导 |

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
