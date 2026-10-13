//! 可观测性与诊断的**纯逻辑**部分。
//!
//! 为什么这些放在 `ta-core` 而不是应用层：它们是**规则**，不是 IO。
//!
//! | 模块 | 内容 | 谁用 |
//! |---|---|---|
//! | [`fields`] | 的字段名约定（唯一落点） | 所有打日志的地方 + 守卫测试 |
//! | [`paths`] | 的日志落点（**C1 判定**） | `src-tauri/src/logging.rs`（E3） |
//! | [`sanitize`] | 的脱敏规则（6 类形态） | 应用日志的格式化层 + 诊断包导出前的自检 |
//! | [`package`] | 诊断包（**C3 判定**：打包后必须零命中） | 诊断包导出（E3） |
//! | [`query`] | 运行时日志行的解析与筛选（**C2/C4 判定** + 诚实提示） | 日志页（F2/E3） |
//! | [`rotation`] | 的轮转/保留策略（给定状态 → 该做什么） | 文件 appender + 守卫测试 |
//! | [`scan`] | 源码扫描器（找 `tracing` 字段名 / 缺 `code` 的错误日志） | `tests/log_field_guard.rs`、`tests/error_log_guard.rs` |
//! | [`zipstore`] | 极简 ZIP（store 方式） | 诊断包导出（不新增依赖） |
//!
//! **本模块不做 IO**：轮转策略只"决策"不"删文件"，脱敏只做字符串替换，
//! 源码扫描只解析字符串（读文件是测试的事）。IO 全部在 `src-tauri/src/logging.rs`。

pub mod fields;
pub mod package;
pub mod paths;
pub mod query;
pub mod rotation;
pub mod sanitize;
pub mod scan;
pub mod zipstore;
