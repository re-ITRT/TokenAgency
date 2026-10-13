# src-tauri — Tauri 桌面壳：IPC 命令层 / 托盘 / 插件 / 生命周期装配（薄壳，不含业务逻辑）

> 上一层：[../AGENTS.md](../AGENTS.md)（全仓地图）。本文件是该模块的**入口索引**。
> 行号写法：`` `路径`:行 ``（冒号留在反引号外）—— 这样反引号里永远是一个真实存在的路径。

## 入口文件
| 文件 | 作用 |
| --- | --- |
| `src/lib.rs` | 唯一接线处：`run()` 定装配顺序（插件 → 命令 handler → setup），不放业务逻辑 |
| `src/main.rs` | 二进制入口（调 `run()`） |
| `src/state.rs` | `AppState`（Tauri `State`）：开库结果、配置热读快照、内核控制器、网关 deps 与闸门槽位 |
| `src/boot.rs` | 启动编排的**生产装配**：`build_kernel` / `BootPhaseHolder` / `KernelBootDeps`（编排逻辑在 `src/lifecycle.rs`） |
| `src/lifecycle.rs` | 编排逻辑（可离线测）：启动顺序、超时降级、关窗退托盘、单实例、通知去重、命令行参数 |
| `src/gateway.rs` | 网关装配：`GatewayBoot::assemble` + 各适配器（usage store / secrets / kernel probe / event bus） |
| `src/tray.rs` | 托盘：状态纯逻辑 `judge_status`、真画像素 `render_icon`、菜单 `build_menu_spec`、`init` |
| `src/plugins.rs` | 7 个插件统一注册（`PLUGINS` / `register` / `CAPABILITY_FILES`） |
| `src/logging.rs` | 日志落盘：`file_writer`（写入时脱敏 + 非阻塞）；托盘应用没有控制台，不落盘等于没日志 |
| `src/autostart.rs` | 开机自启动 —— **本项目唯一一处会写操作系统持久状态**的代码 |
| `src/apikey_auth.rs` | 网关**对外**访问密钥的"查库那一半"（`ApiKeyAuthenticator`） |
| `src/import_preview.rs` | 导入预览的进程内存缓存（5 分钟 TTL；故意不落库，见该文件模块文档） |
| `src/commands/` | 命令面 10 个文件 116 个命令；`src/commands/mod.rs` 放 `IpcError` / `specta_builder` / `export_bindings` |

## 公开 API（对外符号）
命令面按文件分组（数量 = 该文件里 `#[tauri::command` 的出现次数，合计 116）：

| 命令文件 | 管什么 | 命令数 |
| --- | --- | --- |
| `src/commands/apikey.rs` | 我们网关**对外**的访问密钥：list / create / set_enabled / delete / usage | 5 |
| `src/commands/app.rs` | 应用状态、应用目录、可粘贴给客户端的接入信息 | 5 |
| `src/commands/catalog.rs` | provider / credential / model / channel / group / policy / lane / subscription 的只读清单与详情 | 12 |
| `src/commands/config.rs` | 配置读 / 校验 / 应用 / 导入导出 / 历史与回滚 | 7 |
| `src/commands/entity.rs` | 实体级**写**命令：provider / credential / model / channel / group / policy / pricing | 25 |
| `src/commands/log.rs` | 请求日志查询 / 详情 / mode 与保留期 / 清理 | 8 |
| `src/commands/net.rs` | 订阅 / 线路 / 内核三张表的读写（含真下载、真 reload） | 27 |
| `src/commands/settings_ext.rs` | 系统集成 / 诊断导出 / 首启向导 / 托盘控制 / secret | 15 |
| `src/commands/upstream.rs` | 上游探测与导入：provider_test / model_import_from_provider / model_apply_import | 6 |
| `src/commands/usage.rs` | 看板 / 成本中心查询 / CSV 导出 | 6 |

命令清单的**唯一真源**是 `src/commands/mod.rs`:150 的 `collect_commands![…]`；TS 侧 `src/shared/bindings.ts` 由 `export_bindings`（`src/commands/mod.rs`:293）生成。

关键非命令符号：

| 符号 | 位置 | 作用 |
| --- | --- | --- |
| `run()` | `src/lib.rs`:50 | 装配顺序（本模块的唯一入口） |
| `AppState` | `src/state.rs`:50 | 命令层的依赖来源（不各自持全局变量） |
| `IpcError` | `src/commands/mod.rs`:30 | 唯一错误信封：`code` / `type` / `message` / `details` |
| `GatewayBoot` | `src/gateway.rs`:370 | 网关装配；出站走 `GatewayDeps`（`crates/ta-gateway/src/lib.rs`:27，`dialer` 在 `crates/ta-gateway/src/lib.rs`:30） |
| `build_kernel` | `src/boot.rs`:138 | 真装配内核（supervisor + 端口分配器 + 配置源），失败带真实原因 |
| `BootOrchestrator` | `src/lifecycle.rs`:674 | 启动编排（顺序 / 超时 / 降级）；`startup_args` 在 `src/lifecycle.rs`:738 |
| `BootPhaseHolder` | `src/boot.rs`:300 | 阶段广播与托盘数据源的**同一份**真值 |
| `judge_status` | `src/tray.rs`:113 | 托盘状态纯逻辑（4 态 + 实验模式角标） |
| `PLUGINS` | `src/plugins.rs`:72 | 7 个插件的注册顺序（含"故意不授权"的决策） |
| `ApiKeyAuthenticator` | `src/apikey_auth.rs`:34 | 对外访问密钥的鉴权查库 |
| `ImportPreviewCache` | `src/import_preview.rs`:67 | 导入预览缓存（内存 + TTL，不落库） |
| `file_writer` | `src/logging.rs`:85 | 日志落盘（脱敏 + 非阻塞写入） |

## 依赖
- **上游 crate**：`ta-core` / `ta-store`（两者都开 `specta` feature，IPC 类型直接由领域类型与 DTO 生成）、`ta-kernel`（内核控制器与端口表）、`ta-subscription`、`ta-secrets`、`ta-gateway`（网关本体）。
- **关键第三方**：`tauri`（壳）、`specta` + `tauri-specta`（一份命令清单同时产 handler 与 TS 绑定）、`sqlx`（开库与迁移）、`tokio`（异步运行时）、`tracing` + `tracing-appender`（落盘日志）、`arc-swap`（配置热读快照）、`reqwest`（**额度查询的出网口**，`src-tauri/Cargo.toml`:23 注明与 provider_test 同一套纪律 —— 见"已知陷阱" 1）。
- 判断标准：任何会让壳层**复制网关管线**或**在转发链路上另开 client** 的依赖都不该加。

## OWNER
`T1`（命令层 / 状态 / 绑定导出）与 `T2`（托盘 / 生命周期 / 网关装配）—— 来自文件头注释：
`src/state.rs`:3 引 `docs/12-work-breakdown.md` §5 T1、`src/lifecycle.rs`:1 引 `[12 §5 T2]`。
其余文件未标注。

## 测试位置
| 类型 | 位置 |
| --- | --- |
| 单元测试 | 20 个源文件内联 `#[cfg(test)]`（**28 处**）：命令入参形状、托盘纯逻辑、autostart 后端、插件与权限清单、boot 编排（假内核） |
| 集成测试 | `src-tauri/tests/boot_real_kernel.rs`（**真 mihomo** 跑完整启动路径：配置落盘 → spawn → REST 就绪）、`src-tauri/tests/runtime_manifest.rs`（Windows 加载期清单守卫） |
| 源码级守卫 | `src/commands/upstream.rs`:1893 的 `the_production_slice_has_no_second_egress_path` 与 `the_source_never_logs_a_secret`（都在该文件的 `#[cfg(test)]` 里） |
| 权限清单 | `src-tauri/capabilities/default.json` + `src-tauri/capabilities/README.md`（编译期嵌入见 `src/plugins.rs`:143） |

## 关键数据流 / 边界（**可选但强烈建议**）
- 装配顺序（都在 `src/lib.rs`:50 的 `run()` 里）：`plugins::register` → `commands::specta_builder().invoke_handler` → `setup`：`create_main_window` → `lifecycle::on_setup` → `tray::init` → `AppState::bootstrap`（含迁移）→ 同步 `request_log` 设置进 trace store → `boot::build_kernel` → `GatewayBoot::assemble` → `activate_gateway` → `app.manage(app_state)` → `BootOrchestrator::run`（异步）→ 订阅到期轮询（`src/lib.rs`:256 起，60 秒粒度）。
- **边界 1：用户流量的转发链路上只有一条出站**。转发必须走 `GatewayDeps.dialer`（`src/gateway.rs`:418 用 `kernel_dialer` 造）；`src/commands/upstream.rs` 的生产段被源码级测试禁止出现 `reqwest::` / `TcpStream` / `hyper::Client` / `ureq::`，并要求探测走 `UpstreamProbe::new`。注意：`src-tauri` **是**依赖 `reqwest` 的（额度查询的出网口），所以这条铁律不是"不许用 reqwest"。
- **边界 2：唯一的 OS 持久状态**。`src/autostart.rs`:1 自述"本项目唯一一处会写操作系统持久状态的代码"，所以它单独成文件、单独可审、`AutostartBackend` 可换实现。
- 命令层只从 `AppState` 取依赖（`src/state.rs`:3）；网关与内核是两件事：没配内核时网关照样服务直连 lane（`src/lib.rs`:174）。

## 已知陷阱（**可选但强烈建议**）
1. **别把"壳层不依赖 reqwest"当铁律**：`src-tauri/Cargo.toml`:23 明确写了额度查询的出网口就是 reqwest；那条禁令只在 `src/commands/upstream.rs` 的生产段被测试强制（测试只 `include_str!("upstream.rs")`，见 `src/commands/upstream.rs`:1893）。`src/gateway.rs`:394 的旧注释按老口径写，别照它推断整个 crate 的依赖。
2. **不许自己拼网关管线**：装配只走 `GatewayBoot::assemble` → `ta_gateway::build_runtime`（`crates/ta-gateway/src/runtime.rs`:370）。两条装配路径必然漂移，而且漂移时不报错（`src/gateway.rs`:5）。
3. 启动期有三处"降级但不许拖死应用"的 WARN（托盘装不上 `src/lib.rs`:117、托盘数据源换不上 `src/lib.rs`:215、`request_log` 设置同步失败 `src/lib.rs`:138）；反过来**主窗口建不出来必须报错并退出**（`src/lib.rs`:101）。改这几处等于改"什么算启动失败"。
4. 订阅轮询的 60 秒是**粒度**不是周期：真周期由每份订阅自己的 `interval_s` 决定（`src/lib.rs`:245）。
5. 新增插件却忘了在 `src-tauri/capabilities/default.json` 开权限 = 前端调用了没反应；权限文件是编译期嵌入的，误加/漏加会在测试里暴露（`src/plugins.rs`:143）。
