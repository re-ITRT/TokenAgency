//! IPC 错误信封与命令注册。
//!
//! 契约：`docs/10-interface-spec.md` §2.1 的 `IpcError { code, message, details? }`
//! 与 `docs/11-internal-contracts.md` §5 的错误码总表。
//!
//! **只有一个错误类型**：`TaError` → `IpcError`。命令层不做任何错误分类决策，
//! 否则前端要为每个命令猜一套错误语义。

pub mod app;
pub mod catalog;
pub mod config;
pub mod log;
pub mod usage;

use serde::{Deserialize, Serialize};
use ta_core::error::TaError;

/// 前端可见的错误信封。
#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct IpcError {
    /// [11 §5] 的错误码，如 `TA-2003`
    pub code: String,
    /// 面向机器的类型串，如 `tokenagency_no_viable_channel`
    #[serde(rename = "type")]
    pub kind: String,
    /// 面向用户的中文可读信息
    pub message: String,
    /// 结构化细节（字段路径、冲突项等）
    ///
    /// **装箱是有意的**：`serde_json::Value` 内联在结构体里会让 `IpcError` 超过 128 字节，
    /// 于是每个返回 `IpcResult<T>` 的命令都会触发 clippy 的 `result_large_err`（34 处）。
    /// 装箱后 `Err` 变体降到 ~80 字节，**而线上 JSON 形态一字不变**（`Box<Value>` 序列化与 `Value` 相同）。
    /// 这比加 `#[allow]` 好：一个是真把类型变小了，一个是让检查闭嘴。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Box<serde_json::Value>>,
}

impl IpcError {
    pub fn new(code: &str, kind: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_string(),
            kind: kind.to_string(),
            message: message.into(),
            details: None,
        }
    }

    pub fn with_details(mut self, details: serde_json::Value) -> Self {
        self.details = Some(Box::new(details));
        self
    }

    /// 配置校验失败：把**具体字段路径**带出去（[10 §2.2] `config_validate` 的契约）。
    pub fn from_field_errors(errors: &[ta_core::error::FieldError]) -> Self {
        let details = serde_json::to_value(errors).unwrap_or(serde_json::Value::Null);
        Self::new(
            "TA-5001",
            "tokenagency_config_invalid",
            format!("配置校验失败：{} 处问题", errors.len()),
        )
        .with_details(details)
    }
}

impl From<TaError> for IpcError {
    fn from(e: TaError) -> Self {
        // 错误码/类型串的唯一来源是 TaError 自己（[11 §5]），这里不重新映射。
        // `details` 装箱（见字段文档）：只有 `ConfigInvalid` 一份，且要带出全部字段路径。
        let details = match &e {
            TaError::ConfigInvalid { errors } => serde_json::to_value(errors).ok().map(Box::new),
            _ => None,
        };
        Self {
            code: e.code().to_string(),
            kind: e.type_str().to_string(),
            message: e.to_string(),
            details,
        }
    }
}

impl From<sqlx::Error> for IpcError {
    fn from(e: sqlx::Error) -> Self {
        IpcError::from(TaError::Store(e.to_string()))
    }
}

impl From<std::io::Error> for IpcError {
    fn from(e: std::io::Error) -> Self {
        IpcError::from(TaError::Internal(format!("io: {e}")))
    }
}

pub type IpcResult<T> = std::result::Result<T, IpcError>;

/// 所有命令的唯一注册点。
///
/// 用 `tauri_specta::collect_commands!` —— 新增命令忘记登记会在下面的测试里被抓住
/// （绑定文件缺命令 = 前端调用直接失败，是必须拦住的一类错）。
pub fn specta_builder() -> tauri_specta::Builder<tauri::Wry> {
    tauri_specta::Builder::<tauri::Wry>::new()
        .commands(tauri_specta::collect_commands![
            app::app_status,
            app::app_open_dir,
            app::app_clipboard_access_info,
            config::config_get,
            config::config_validate,
            config::config_apply,
            config::config_export_yaml,
            config::config_import_yaml,
            config::config_history_list,
            config::config_rollback,
            catalog::provider_list,
            catalog::provider_get,
            catalog::credential_list,
            catalog::model_list,
            catalog::channel_list,
            catalog::group_list,
            catalog::policy_list,
            catalog::lane_list,
            catalog::subscription_list,
            usage::usage_dashboard,
            usage::usage_query,
            usage::usage_token_kinds,
            usage::usage_request_stream_snapshot,
            usage::pricing_unpriced_models,
            usage::usage_export_csv,
            log::log_query,
            log::log_get_trace,
            log::log_set_mode,
            log::log_set_retention,
            log::log_storage_stats,
            log::log_clear,
        ])
        .error_handling(tauri_specta::ErrorHandlingMode::Result)
}

/// 导出 TypeScript 绑定到 `path`。
///
/// 由 `scripts/gen-bindings.ps1` 触发（它设置 `TA_EXPORT_BINDINGS=1` 与
/// `TA_EXPORT_BINDINGS_PATH`）。见 `bindings_test` 里的说明：脚本目前用 `cargo build`，
/// 而 `build` 不会执行 Rust 代码，所以真正的触发方式是 `cargo test`（或 P7 接手 `lib.rs`
/// 之后由 `cargo run` 触发）。
pub fn export_bindings(path: &std::path::Path) -> IpcResult<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let typescript = specta_typescript::Typescript::default().header(TS_HEADER);
    specta_builder()
        .export(typescript, path)
        .map_err(|e| IpcError::from(TaError::Internal(format!("export bindings: {e}"))))
}

/// 生成文件顶部的**手写**补充说明。
///
/// 为什么需要它：`TokenKind` / `Metric` 这些开放集合在线上是**普通字符串**，
/// specta 只能表达成 `string`（它们的类型由 `impl specta::Type` 手写给出，
/// 见 `ta-core::types` 与 `ta-store::usage_store`）。这里把已知取值列出来，
/// 前端可以据此做穷尽性检查；**不改变线上形态**。
const TS_HEADER: &str = "\
/* eslint-disable */
// 由 `scripts/gen-bindings.ps1` 生成 —— 不要手改。
//
// 开放集合的已知取值（线上形态是字符串，末尾的交叉类型允许上游新增类型）：
//   TokenKind: 'input' | 'input_cached' | 'input_cache_write' | 'input_cache_write_1h'
//            | 'input_audio' | 'input_image' | 'output' | 'output_reasoning'
//            | 'output_audio' | 'output_prediction_accepted' | 'output_prediction_rejected'
//            | (string & {})
//   Metric:    'requests' | 'cost' | 'cache_hit_rate' | 'avg_latency' | TokenKind
//
// 时间一律是 **Unix 毫秒**；\"今天/本周\" 这类聚合按**本地时区**（见 docs/07 §1.3）。
";

#[cfg(test)]
mod tests {
    use super::*;

    /// 构建绑定（保证所有命令的类型都能导出），并在设置了环境变量时写到磁盘。
    ///
    /// `scripts/gen-bindings.ps1` 设的是 `TA_EXPORT_BINDINGS=1` +
    /// `TA_EXPORT_BINDINGS_PATH=<abs path>`。
    #[test]
    fn bindings_build_and_optional_export() {
        // 无论是否导出，都要证明 builder 能收集全部命令（类型都实现 specta::Type）
        let _ = specta_builder();

        if std::env::var("TA_EXPORT_BINDINGS").as_deref() != Ok("1") {
            return;
        }
        let path = std::env::var("TA_EXPORT_BINDINGS_PATH")
            .unwrap_or_else(|_| "../src/shared/bindings.ts".to_string());
        let path = std::path::PathBuf::from(path);
        export_bindings(&path).expect("导出 TS 绑定失败");
        let len = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        assert!(len > 0, "导出的绑定文件为空：{}", path.display());
        eprintln!("已导出 {} ({} bytes)", path.display(), len);
    }
}
