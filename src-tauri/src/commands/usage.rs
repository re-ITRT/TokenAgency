//! `usage_*` / `pricing_*`：看板、成本中心查询、CSV 导出。
//!
//! 契约：`docs/10-interface-spec.md` §2.2 `usage / 用量与成本中心`、§2.4 `DashboardData`。
//! 计算全在 `ta-store`（`docs/07` §6），命令层只做转发与参数透传。

use tauri::State;

use crate::commands::IpcResult;
use crate::state::AppState;
use ta_store::UsageStore;

#[tauri::command]
#[specta::specta]
pub async fn usage_dashboard(
    state: State<'_, AppState>,
    range: ta_core::clock::TimeRange,
) -> IpcResult<ta_store::DashboardData> {
    Ok(state.usage.dashboard(range).await?)
}

#[tauri::command]
#[specta::specta]
pub async fn usage_query(
    state: State<'_, AppState>,
    q: ta_store::UsageQuery,
) -> IpcResult<ta_store::UsageTable> {
    Ok(state.usage.query(&q).await?)
}

#[tauri::command]
#[specta::specta]
pub async fn usage_token_kinds(state: State<'_, AppState>) -> IpcResult<Vec<String>> {
    Ok(state.usage.token_kinds_seen().await?.into_iter().map(|k| k.as_key().to_string()).collect())
}

/// 实时流首次填充（之后靠 `request:new` 事件，[10 §2.3]）。
#[tauri::command]
#[specta::specta]
pub async fn usage_request_stream_snapshot(
    state: State<'_, AppState>,
    limit: u32,
) -> IpcResult<Vec<ta_core::types::RequestSummary>> {
    Ok(state.usage.recent_summaries(limit).await?)
}

#[tauri::command]
#[specta::specta]
pub async fn pricing_unpriced_models(state: State<'_, AppState>) -> IpcResult<Vec<String>> {
    Ok(state.usage.unpriced_models().await?)
}

/// 导出 CSV 到指定路径（[10 §2.2] `usage_export_csv`；任何统计都能导出是硬要求）。
#[tauri::command]
#[specta::specta]
pub async fn usage_export_csv(
    state: State<'_, AppState>,
    q: ta_store::UsageQuery,
    path: String,
) -> IpcResult<()> {
    if let Some(parent) = std::path::Path::new(&path).parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = std::fs::File::create(&path)?;
    let mut writer = std::io::BufWriter::new(file);
    state.usage.export_csv(&q, &mut writer).await?;
    Ok(())
}
