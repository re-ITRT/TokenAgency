//! `log_*`：请求日志查询、详情、模式/保留期、存储统计、清理。
//!
//! 契约：`docs/10-interface-spec.md` §2.2 `log / 请求日志`、`docs/07` §2。
//!
//! 注意（主管裁决 D2）：`filters.status = "retried"` 依赖尝试链，而 `LogMode::Off`
//! 时不落 trace。**实现保留**，UI 需在 off 模式下禁用该筛选项并提示。

use serde::{Deserialize, Serialize};
use tauri::State;

use crate::commands::IpcResult;
use crate::state::AppState;
use ta_store::TraceStore;

#[tauri::command]
#[specta::specta]
pub async fn log_query(
    state: State<'_, AppState>,
    q: ta_store::LogQuery,
) -> IpcResult<ta_store::LogPage> {
    Ok(state.trace.query(&q).await?)
}

#[tauri::command]
#[specta::specta]
pub async fn log_get_trace(
    state: State<'_, AppState>,
    request_id: String,
) -> IpcResult<Option<ta_store::TraceDetail>> {
    Ok(state.trace.get(&request_id).await?)
}

#[tauri::command]
#[specta::specta]
pub async fn log_set_mode(
    state: State<'_, AppState>,
    mode: ta_core::types::LogMode,
) -> IpcResult<()> {
    Ok(state.trace.set_mode(mode).await?)
}

#[tauri::command]
#[specta::specta]
pub async fn log_set_retention(
    state: State<'_, AppState>,
    retention: ta_core::types::Retention,
) -> IpcResult<()> {
    Ok(state.trace.set_retention(retention).await?)
}

#[tauri::command]
#[specta::specta]
pub async fn log_storage_stats(state: State<'_, AppState>) -> IpcResult<ta_store::StorageStats> {
    Ok(state.trace.storage_stats().await?)
}

#[tauri::command]
#[specta::specta]
pub async fn log_clear(state: State<'_, AppState>, mode: ta_store::ClearMode) -> IpcResult<u64> {
    Ok(state.trace.clear(mode).await?)
}

// ================================================================ 查询能力探测（[06 §10.2 D2]）

/// 一项查询能力是否可用。形状按 E1/E2 已确认的约定：
/// `enabled = true` 时 `reason` 为 `None`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
pub struct CapabilitySupport {
    pub enabled: bool,
    /// 不可判定时的**诚实原因**（给 UI 直接显示，文案原文来自 `ta_core::diag::query`）
    pub reason: Option<String>,
}

/// [10 §2.2] 日志后端在**当前模式**下支持哪些查询能力。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
pub struct LogQueryCapabilities {
    /// "重试"筛选项是否可用（`mode = off` 时没有尝试链可看）
    pub retried: CapabilitySupport,
    /// 请求列表的 `outcome` 是否可显示（否则必须显示"未知"，**不许显示成"正常"**）
    pub outcome: CapabilitySupport,
}

fn support_of(s: ta_core::diag::query::Support) -> CapabilitySupport {
    CapabilitySupport { enabled: s.enabled, reason: s.reason.map(str::to_string) }
}

/// 纯函数：判定全部来自 E2 的 `ta_core::diag::query`（本层不另写一套）。
pub fn capabilities_for(mode: ta_core::types::LogMode) -> LogQueryCapabilities {
    use ta_core::diag::query::{outcome_display_support, retried_filter_support};
    LogQueryCapabilities {
        retried: support_of(retried_filter_support(mode)),
        outcome: support_of(outcome_display_support(mode)),
    }
}

/// [10 §2.2] `log_query_capabilities` —— 前端进日志页时取一次（E2 建议的 ② 方案，
/// 不动 `LogPage` 契约）。
#[tauri::command]
#[specta::specta]
pub async fn log_query_capabilities(state: State<'_, AppState>) -> IpcResult<LogQueryCapabilities> {
    let mode = state.trace.mode().await?;
    Ok(capabilities_for(mode))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ta_core::types::LogMode;

    #[test]
    fn off_mode_disables_and_explains_instead_of_pretending() {
        let caps = capabilities_for(LogMode::Off);
        assert!(!caps.retried.enabled, "off 模式没有尝试链，重试筛选必须禁用");
        assert!(!caps.outcome.enabled, "off 模式下 outcome 不可判定，不能显示成正常");
        // 原因必须是 E2 的原文，UI 直接展示（三处同一句的前提）
        assert_eq!(
            caps.retried.reason.as_deref(),
            Some(ta_core::diag::query::RETRIED_UNKNOWN_REASON)
        );
        assert!(caps.outcome.reason.is_some(), "禁用必须给原因，否则用户以为功能坏了");
    }

    #[test]
    fn every_enabled_mode_has_full_capability_and_no_reason() {
        for mode in [LogMode::Meta, LogMode::Shape, LogMode::Full] {
            let caps = capabilities_for(mode);
            assert!(caps.retried.enabled, "{mode:?} 下重试筛选应可用");
            assert!(caps.outcome.enabled, "{mode:?} 下 outcome 应可判定");
            assert!(caps.retried.reason.is_none() && caps.outcome.reason.is_none());
        }
    }
}
