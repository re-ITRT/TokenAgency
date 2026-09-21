//! `log_*`：请求日志查询、详情、模式/保留期、存储统计、清理。
//!
//! 契约：`docs/10-interface-spec.md` §2.2 `log / 请求日志`、`docs/07` §2。
//!
//! 注意（主管裁决 D2）：`filters.status = "retried"` 依赖尝试链，而 `LogMode::Off`
//! 时不落 trace。**实现保留**，UI 需在 off 模式下禁用该筛选项并提示。

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
