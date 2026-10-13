# ta-store — SQLite 持久化：配置真源、账本、trace、粘性、扫描目录

> 上一层：[../../AGENTS.md](../../AGENTS.md)（全仓地图）。本文件是该模块的**入口索引**。

## 入口文件
| 文件 | 作用 |
| --- | --- |
| `src/lib.rs` | crate 根：`pub mod` 清单 = 公开面；`open` / `open_memory` 是唯一的建库入口，两者都先跑迁移 |
| `src/migrations.rs` | `sqlx::migrate!` 宏（编译期把 `migrations/` 嵌进二进制）的封装：`run` / `expected_version`，并把 `PRAGMA user_version` 对齐到最新迁移版本 |
| `migrations/` | 只增不改的 schema：`0001_init.sql` … `0008_drop_quota_tables.sql`（表与 trait 的对应见下） |
| `src/util.rs` | 内部工具（zstd、JSON、`StoreResultExt`）—— `mod util`，**不是**公开面 |

## 公开 API（对外符号）
| 符号 | 位置（`文件:行`） | 作用 |
| --- | --- | --- |
| `open` / `open_memory` | `src/lib.rs`:56 / `src/lib.rs`:79 | 真库 / 内存库（测试用），跑完迁移才返回 |
| `ConfigStore` / `SqliteConfigStore` | `src/config_store.rs`:54 / `src/config_store.rs`:67 | 配置真源（`config` 表一行一实体 JSON）+ 快照/回滚（`config_snapshot`） |
| `UsageStore` / `SqliteUsageStore` | `src/usage_store.rs`:454 / `src/usage_store.rs`:474 | 账本：`usage_request` 明细 + `usage_daily` 聚合 + `api_key_usage`；看板与 CSV |
| `TraceStore` / `SqliteTraceStore` | `src/trace_store.rs`:297 / `src/trace_store.rs`:318 | 请求日志 `trace`：zstd BLOB、脱敏、双限清理 |
| `StickyStore` / `SqliteStickyStore` | `src/sticky_store.rs`:32 / `src/sticky_store.rs`:59 | 会话粘性 `sticky` |
| `HealthStore` / `SqliteHealthStore` | `src/health_store.rs`:52 / `src/health_store.rs`:84 | 线路健康样本 `health_sample` |
| `SecretMetaStore` / `SqliteSecretMetaStore` | `src/health_store.rs`:71 / `src/health_store.rs`:201 | 密钥**元数据** `secret_meta`（只存条目名，绝不存密钥值） |
| `SqliteApiKeyStore` | `src/api_key.rs`:95 | **我们对外发的**访问密钥 `api_key`（只存 sha256） |
| `ModelCatalogStore` / `SqliteModelCatalogStore` | `src/model_catalog.rs`:67 / `src/model_catalog.rs`:92 | 扫描目录 `provider_model_catalog`（含上游原文 `raw_json`） |
| `SqliteSubscriptionTrafficStore` | `src/subscription_traffic.rs`:56 | `subscription` 表上"上次已知流量与更新时间" |
| `SqliteAutoChoiceStore` | `src/auto_choice.rs`:33 | 自动出口选择（复用 `store_setting` KV，键 `auto_export:<provider_id>`） |
| `TimeBucket` + 分桶函数 | `src/timebucket.rs`:26 | 本地时区的日/周/月分桶（账本聚合的时间口径） |
| `sanitize_headers` / `sanitize_body` | `src/trace_store.rs`:1003 / `src/trace_store.rs`:1025 | trace **写入前**的脱敏；公开出来是为了让测试直接核对 |

## 依赖
- **上游 crate**：`ta-core`（`TaError`、注入式 `Clock`、领域类型与 `UsageRecord`）；打开 `specta` feature 时会同时打开 `ta-core` 的同名 feature。
- **关键第三方**（每条都说清为什么）：
  - `sqlx`：SQLite 驱动 + `sqlx::migrate!` 把 `migrations/*.sql` 编进二进制；
  - `tokio`：异步运行时（连接池与查询都是 async）；
  - `async-trait`：`*Store` trait 的方法都是 async，需要它做 dyn 分发；
  - `zstd`：`trace` 的 `body` / `resp_body` 压缩存储（读时解压）；
  - `sha2` + `base64`：trace 图片块的 sha256 前缀、访问密钥哈希；
  - `rand`：`ta-sk-` 明文随机段（`src/api_key.rs`）；
  - `chrono`：本地时区与日期计算（`src/timebucket.rs`，账本分桶的时间口径）；
  - `serde` / `serde_json`：配置"一行一实体"的 JSON 真源；
  - `specta` + `specta-typescript`（**可选**）：让 DTO 直接生成 IPC 的 TS 类型。

## OWNER
`P4（持久化）` —— `src/lib.rs` 文件头写着"实现（P4）"，`crates/ta-store/tests/p4_integration.rs` 同名。

## 测试位置
| 类型 | 位置 |
| --- | --- |
| 单元测试 | `src/**.rs` 内联 `#[cfg(test)]`（**约 51 条**），主要分布在 `src/model_catalog.rs` / `src/trace_store.rs`（各 7）、`src/auto_choice.rs`（6）、`src/usage_store.rs`（6） |
| 集成测试 | `tests/p4_integration.rs`（千条账本三维分组、本地时区分桶、gc 三限、图片只存哈希、事务回滚、空库看板）、`tests/api_key.rs`（只存 hash、明文只回一次、`last_used_at` 为 NULL 而非 0、`key_hash` 唯一、表上没有第二条查询路径） |
| 守卫 | 无独立守卫文件；schema 断言在 `src/migrations.rs` 的内联测试里：列存在性 / 可空性 / 主键列序 / 默认值，并**显式断言不存在 `origin` 列**（防止有人把"手动添加"加回来） |

## 关键数据流 / 边界
**表 <-> trait 的对应（改表先看这里）**：

| 表 | 建在哪 | 谁在用 |
| --- | --- | --- |
| `config` / `config_snapshot` | `migrations/0001_init.sql` | `SqliteConfigStore` |
| `store_setting` | `migrations/0001_init.sql` | `SqliteAutoChoiceStore`、`SqliteTraceStore`（`trace.retention` / `trace.max_rows` / `trace.max_size_bytes` 三个清理阈值） |
| `usage_request` / `usage_daily` / `api_key_usage` | `migrations/0001_init.sql` | `SqliteUsageStore` |
| `sticky` / `trace` / `health_sample` / `secret_meta` / `subscription` | `migrations/0001_init.sql` | `SqliteStickyStore` / `SqliteTraceStore` / `SqliteHealthStore` / `SqliteSecretMetaStore` / `SqliteSubscriptionTrafficStore` |
| `subscription_version` | `migrations/0001_init.sql` | **没有任何模块读写它**（见"已知陷阱"） |
| `api_key` | `migrations/0004_api_key.sql` | `SqliteApiKeyStore` |
| `provider_model_catalog` | `migrations/0006_provider_model_catalog.sql` + `0007_provider_model_catalog_raw.sql` | `SqliteModelCatalogStore` |
| `quota_state` / `quota_window_state` / `quota_window_bucket` | `0002` / `0005` 建、`0008_drop_quota_tables.sql` 删 | 已下线，无使用者 |

- 被谁调用：`src-tauri` 的命令层（经 `GatewayDeps` 注入各 store）与 `ta-gateway`（trace / 账本写入）；本 crate **不反向依赖**它们。
- 它调用谁：只调 `ta-core`（错误、时钟、领域类型）与 SQLite —— **没有网络 IO**。
- **脱敏在写入前做**（`src/trace_store.rs` 顶部的硬约束）：store 侧再兜一层，所以即使调用方漏了脱敏，库里也不会有 key。

## 已知陷阱
1. **`subscription_version` 是死表**：全工作区 `rg -n subscription_version -g "*.rs"` 只在 `src/migrations.rs` 的表名断言里出现；订阅版本历史的真源是 `ta-subscription` 的 `FileVersionStore`（在 `src-tauri/src/commands/net.rs` 里装配）。别照表名去接。
2. **迁移只增不改**：改已发布的 `0001`–`0007` 会让 `_sqlx_migrations` 的校验和对不上，老库直接启动失败；要删东西就再写一条（`0008` 就是这么删额度三表的）。
3. **`raw_json` 只接受 JSON 对象**：非对象 / 坏文本一律读成 `{}`，**不补字段**（`src/model_catalog.rs`）。
4. **`gc()` 不做全量 VACUUM**：只走 `PRAGMA incremental_vacuum`，且 `auto_vacuum` 必须建表前设成 `INCREMENTAL`（`src/migrations.rs` 的 `prepare_auto_vacuum`）。
5. **DTO 里放 `serde_json::Value` 必须配 `specta(type = specta_typescript::Any)`**，否则绑定导出会因"无限递归类型"整体失败。
