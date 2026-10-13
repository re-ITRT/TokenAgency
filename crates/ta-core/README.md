# ta-core — TokenAgency 的纯领域层（**无 IO**）

> 上一层：[../../AGENTS.md](../../AGENTS.md)（全仓地图）。本文件是该模块的**入口索引**。

## 入口文件
| 文件 | 作用 |
| --- | --- |
| `crates/ta-core/src/lib.rs` | crate 根；11 个 `pub mod` 就是公开面（clock/config/diag/domain/error/metering/pricing/routing/sticky/trace/types） |
| `crates/ta-core/src/domain/mod.rs` | 全部配置实体（`ConfigDoc` 及其子树；本文件 63 个 `pub struct/enum`） |
| `crates/ta-core/src/config/mod.rs` | 配置读写（YAML / JSON）+ 校验入口 + JSON Schema |
| `crates/ta-core/src/routing/mod.rs` | 路由决策的公开面（含 `simulate` 对拍入口） |
| `crates/ta-core/src/diag/mod.rs` | 可观测性的**规则**（字段名 / 脱敏 / 诊断包 / 轮转）；IO 不在本 crate |

## 公开 API（对外符号）
| 符号 | 位置 | 作用 |
| --- | --- | --- |
| `Clock` / `SystemClock` / `ManualClock` / `FixedClock` | `crates/ta-core/src/clock.rs` | 时钟注入点；"无 IO"的关键，测试用 `ManualClock` 推进时间 |
| `TaError` / `Result` / `FieldError` | `crates/ta-core/src/error.rs` | 全项目错误类型；`code()` 的 `TA-xxxx` 本身就是接口契约 |
| `Protocol` / `TokenKind` / `TokenUsage` / `Money` / `UsageSource` / `UsageRecord` / `LogMode` / `Retention` / `RequestSummary` | `crates/ta-core/src/types.rs` | 跨 crate 的基础类型（词汇表见 `docs/11-internal-contracts.md`） |
| `ConfigDoc` / `Provider` / `Credential` / `Lane` / `Model` / `ChannelConfig` / `Policy` / `Subscription` / `NetConfig` | `crates/ta-core/src/domain/mod.rs` | 用户配置的实体与根对象（真源）；`ConfigDoc` 的 `extra` 保留未知字段 |
| `from_yaml` / `to_yaml` / `from_json` / `to_json` / `validate` / `semantic_eq` / `default_doc` / `config_json_schema` | `crates/ta-core/src/config/mod.rs` | 配置 IO 与校验；`config_json_schema` 是前端类型生成的输入 |
| `ValidationReport` / `ConfigValidator` / `TaConfigValidator` / `validate_doc` | `crates/ta-core/src/config/validator.rs` | 校验结果与字段级错误路径（`channels[2].credential` 这种） |
| `CURRENT_CONFIG_VERSION` / `needs_migration` / `migrate` / `migrate_to` / `migration_steps` | `crates/ta-core/src/config/migrate.rs` | 配置版本迁移（当前 v3，下一版常量已是 v4） |
| `matches_glob` / `Model::new` / `ConfigDoc::lines_of_model` / `ConfigDoc::models_of_channel` / `resolve_lane` / `ChannelConfig::effective_lane` | `crates/ta-core/src/domain/behavior.rs` | 领域行为：模型名 glob、模型↔线路关系、线路继承链 |
| `capabilities_equal` / `capability_field_diffs` / `capability_mismatches_for_channels` | `crates/ta-core/src/domain/capabilities.rs` | 能力一致性与逐字段差异（`TA-2007` 的判定层） |
| `UpstreamModelHints` | `crates/ta-core/src/domain/upstream_hints.rs` | 上游扫描回来的模型信息（能力 / 上下文） |
| `Router` / `PolicyEngineImpl` / `ChannelSelectorImpl` / `RoutingIndex` / `simulate` / `SimulateResult` | `crates/ta-core/src/routing/mod.rs` | 策略求值 → 候选 → 选择；`simulate` 是"同一份配置给出同一条通道"的对拍入口 |
| `PricingEngine` / `PricingEngineImpl` / `ResolvedPrice` / `cost_of_snapshot` | `crates/ta-core/src/pricing/mod.rs` | 定价与成本（本地估算，不是上游账单） |
| `TokenMeter` / `DefaultMeter` / `MeteredUsage` / `normalize_openai_chat` / `estimated_usage` | `crates/ta-core/src/metering/mod.rs` | 从上游响应解析用量；估算与"上游给的"必须可区分 |
| `StickyResolver` / `StickyRecord` / `StickyBackend` / `MemoryStickyStore` / `BackedStickyResolver` | `crates/ta-core/src/sticky/mod.rs` | 粘性的纯逻辑与内存实现（持久化在 `crates/ta-store`） |
| `HeaderDiff` | `crates/ta-core/src/trace.rs` | 透传自检：客户端头 vs 上游头 |
| `diag::fields` / `diag::paths` / `diag::sanitize` / `diag::package` / `diag::query` / `diag::rotation` / `diag::scan` / `diag::zipstore` | `crates/ta-core/src/diag/mod.rs` | 日志字段白名单、落点、脱敏、诊断包、轮转决策；IO 在 `src-tauri/src/logging.rs` |

## 依赖
- **上游 crate**：**没有** —— `Cargo.toml` 里没有任何本 workspace 依赖（叶子 crate）。
- **关键第三方**：`serde` / `serde_json` / `serde_yaml_ng`（配置模型与读写）、`schemars`（JSON Schema）、`thiserror`（`TaError`）、`regex`（策略里的正则条件）、`chrono`（时间窗口）、`tracing`（唯一被允许的"I/O 味"）。
- **可选**：`specta` feature 才带 `specta` / `specta-typescript`，用于把领域类型导出成前端 TS 类型；默认构建不带。
- **不许有**：`tokio` / `axum` / `reqwest` / `tauri` —— 出现任何一个就是边界破了（[docs/09](../../docs/09-architecture-design.md)）。

## OWNER
- 按 `docs/12-work-breakdown.md` §1.2 的固定编制：`domain` / `config` → **E1 领域官**；`routing` / `sticky` / `pricing` / `metering` → **E2 决策官**。
- `clock` / `error` / `types` / `trace` / `diag` → **未标注**（编制表没点名，文件头也没有归属标记）。
- 注意：`docs/12-work-breakdown.md` 编制表里的 `quota` 模块在本 crate 已不存在（已删除）；那份表在这一点上过期。

## 测试位置
| 类型 | 位置 |
| --- | --- |
| 单元测试 | `crates/ta-core/src/**` 内联 `#[cfg(test)]`：29 个文件，共 401 条 `#[test]`（本 crate 无 `tokio`，全部是同步测试） |
| 集成测试 | `crates/ta-core/tests/` 下 7 个守卫/契约文件 + 1 个共享工具模块（见下） |
| 守卫 | `crates/ta-core/tests/log_field_guard.rs`（`tracing` 字段名必须来自 `docs/14-observability.md` §5 白名单）；`crates/ta-core/tests/error_log_guard.rs`（记 `TaError` 必须同时记 `code`）；`crates/ta-core/tests/metered_usage_guard.rs`（`MeteredUsage` 字面构造只许出现在 `crates/ta-core/src/metering/`）；`crates/ta-core/tests/example_config_guard.rs`（`config.example.yaml` 必须与类型语义一致，落 `extra` 即红）；`crates/ta-core/tests/command_surface_guard.rs`（命令面与归档锚点一致）；`crates/ta-core/tests/command_arg_case_guard.rs`（`#[tauri::command]` 必须显式 `rename_all = "snake_case"`） |

集成测试文件逐个说明：
- `crates/ta-core/tests/example_config_guard.rs` —— 示例配置能读通、无静默落 `extra` 的字段、序列化幂等。
- `crates/ta-core/tests/command_surface_guard.rs` —— 活契约与归档件逐条对拍（命令增删要显式登记）。
- `crates/ta-core/tests/command_arg_case_guard.rs` —— 命令入参命名口径（`camelCase` 会让真机找不到参数）。
- `crates/ta-core/tests/error_log_guard.rs` —— 错误日志必须可按错误码检索。
- `crates/ta-core/tests/log_field_guard.rs` —— 日志字段名不许漂移。
- `crates/ta-core/tests/metered_usage_guard.rs` —— 估算不许冒充上游数据。
- `crates/ta-core/tests/auto_lane_is_select.rs` —— 「自动」出口的组类型必须是 `select`。
- `crates/ta-core/tests/common/mod.rs` —— 上面两条源码扫描守卫共用的工具（不是独立测试目标）。

## 关键数据流 / 边界
- 本 crate **只被调用，不调用别人**：`crates/ta-gateway`（路由/透传判定/计量）、`crates/ta-store`（配置与账本读写）、`crates/ta-kernel`（YAML 生成）、`src-tauri`（命令层）都依赖它。
- **无 IO 是硬边界**：文件、网络、时钟、系统钥匙串全部靠注入的 trait（`Clock` 是样板）。要判断"这段逻辑该放哪"——纯规则放这里，落地动作放调用方。
- 配置真源是 `ConfigDoc`（`crates/ta-core/src/domain/mod.rs`）：改它必须同步 `config.example.yaml` 与 `src/shared/bindings.ts`（生成命令见 [../../AGENTS.md](../../AGENTS.md) 第 4 节）。

## 已知陷阱
- 所有实体都有 `extra`（`#[serde(flatten)]`）：**删字段不会让老配置报错**，值会静默落进 `extra`。所以"删了字段"必须靠 `example_config_guard` 这类守卫兜，不能靠"能解析"证明。
- 改结构体默认值必须同时改 `crates/ta-core/src/config/defaults.rs` 与迁移，否则 `example_config_guard` 直接红。
- `specta` 是可选 feature：默认构建下领域类型不带 `specta::Type` 派生，"导出前端类型"只在开启时成立。
- 时钟不要用 `SystemClock` 参与判定逻辑：需要时间的函数一律吃注入的 `Clock`，否则测试与真机会给出不同答案。
