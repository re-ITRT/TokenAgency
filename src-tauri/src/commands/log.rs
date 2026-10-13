//! `log_*`：请求日志查询、详情、模式/保留期、存储统计、清理。
//!
//! 契约： `log / 请求日志`、。
//!
//! 注意（主管裁决 D2）：`filters.status = "retried"` 依赖尝试链，而 `LogMode::Off`
//! 时不落 trace。**实现保留**，UI 需在 off 模式下禁用该筛选项并提示。

use serde::{Deserialize, Serialize};
use tauri::State;

use crate::commands::IpcResult;
use crate::state::AppState;
use ta_core::domain::ConfigDoc;
use ta_core::types::LogMode;
use ta_store::{ConfigStore, TraceLimits, TraceStore, UsageStore};

/// 把配置里的 `request_log.*` **同步进 trace store**（第 330 轮修）。
///
/// # 为什么必须有这一步（它修的是一个真机 bug）
///
/// `request_log.{mode, retention, max_rows, max_size_mb}` 住在**配置**里，
/// 而真正决定"写不写、写多少、留多久"的是 trace store 自己的 `store_setting`
/// （`trace.mode` / `trace.retention` / `trace.max_rows` / `trace.max_size_bytes`）。
///
/// 在这之前**没有任何一处把配置喂给 store**（全仓搜 `request_log` 在 `src-tauri` 里零命中），
/// 而 `TraceStore::write` 在 `mode == Off` 时**直接 return**。于是出现这种真机现象：
///
/// > 配置里明明是 `request_log.mode: meta`，但 `store_setting` 里躺着一条古老的
/// > `trace.mode = "off"` ⇒ **日志一条都不落库**，而用户从界面上看不出任何原因
/// > （日志页只会说"可能是 off 模式"）。
///
/// 修法：**配置是唯一真相源**，启动时与每次配置应用后同步一次。
/// 日志页上的即时开关（`log_set_mode` / `log_set_retention`）也**回写配置**，
/// 所以两边不会再分叉。
///
/// `sample_rate` / `image_handling` **没有对应机制**，本轮不假装同步它们
/// （它们在 store 里没有槽位；编一个字段名去写只会留下"看起来同步了"的假象）。
pub(crate) async fn sync_request_log_settings(state: &AppState, doc: &ConfigDoc) -> IpcResult<()> {
    let cfg = &doc.request_log;
    state.trace.set_mode(cfg.mode).await?;
    state
        .trace
        .set_limits(&TraceLimits {
            retention: cfg.effective_retention(),
            max_rows: cfg.max_rows,
            max_size_bytes: cfg.max_size_mb.saturating_mul(1024 * 1024),
        })
        .await?;
    Ok(())
}

/// 把日志页上的即时开关**回写配置**，避免"store 说是 off、配置说 meta"这种分叉。
///
/// 直接 `config.save` + `set_doc`，**不走** `save_validated`：那一条会顺手让内核
/// reload 一次配置，而改一个日志开关不该扰动内核。校验也不必再跑一遍 ——
/// 这里改的是 `LogMode` / `Retention` 两个**枚举**，构造不出非法值。
async fn persist_request_log(
    state: &AppState,
    mutate: impl FnOnce(&mut ConfigDoc),
) -> IpcResult<()> {
    // `state.doc()` 给的是 `Arc<ConfigDoc>`：要改必须**先克隆内容**。
    // 不能改共享那份 —— `set_doc` 才是唯一生效路径（见它的注释）。
    let mut doc = (*state.doc()).clone();
    mutate(&mut doc);
    state.config.save(&doc).await?;
    state.set_doc(doc);
    Ok(())
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn log_query(
    state: State<'_, AppState>,
    q: ta_store::LogQuery,
) -> IpcResult<ta_store::LogPage> {
    Ok(state.trace.query(&q).await?)
}

/// **L1 聚合统计**：总数 / 成功 / 失败 / 平均总耗时。
///
/// 与 `log_query` **同一套过滤**（都走 `TraceStore`），所以筛选片与列表不会说两套话。
/// 搜索模式下存储层会**如实拒绝**（正文是压缩体，SQL 数不准）——把错误原样传上去，
/// 界面据此隐藏统计卡，而不是显示一个偏小的数字。
#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn log_stats(
    state: State<'_, AppState>,
    q: ta_store::LogQuery,
) -> IpcResult<ta_store::LogStats> {
    Ok(state.trace.stats(&q).await?)
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn log_get_trace(
    state: State<'_, AppState>,
    request_id: String,
) -> IpcResult<Option<ta_store::TraceDetail>> {
    let Some(mut detail) = state.trace.get(&request_id).await? else {
        return Ok(None);
    };
    // L6：把**按 kind 的 token 明细**一并带回去（它在用量表里，不在这张 trace 表里）。
    //
    // 为什么在这里补而不是在 store 层：trace 与 usage 是两张表、两条保留期，
    // `trace_store` 不该知道用量表的存在。查不到就是 `None` → 界面显示「—」，
    // **不拿 `detail.row.tokens` 凑分项**（那是编数）。
    //
    // 查询失败**不吞**：如实返回错误（宁可让这次详情打不开，也不给一份半真半假的分项）。
    detail.token_kinds = state.usage.token_kinds_of(&request_id).await?;
    Ok(Some(detail))
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn log_set_mode(state: State<'_, AppState>, mode: LogMode) -> IpcResult<()> {
    // 先写 store（立即生效），再回写配置（下次启动仍一致）——
    // 顺序反过来的话，写配置成功而 store 失败会留下"界面显示已改、实际没改"的窗口。
    state.trace.set_mode(mode).await?;
    persist_request_log(&state, |doc| doc.request_log.mode = mode).await
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn log_set_retention(
    state: State<'_, AppState>,
    retention: ta_core::types::Retention,
) -> IpcResult<()> {
    state.trace.set_retention(retention).await?;
    // 配置侧的 `RetentionDefault` 与运行态的 `Retention` 是**同一个取值集合**
    // （`7d/30d/90d/forever`），这里做一次显式映射而不是 `transmute` 式转换：
    // 多了或少了一个档位时，编译器会在这里拦住。
    persist_request_log(&state, |doc| {
        doc.request_log.retention = match retention {
            ta_core::types::Retention::D7 => ta_core::domain::RetentionDefault::D7,
            ta_core::types::Retention::D30 => ta_core::domain::RetentionDefault::D30,
            ta_core::types::Retention::D90 => ta_core::domain::RetentionDefault::D90,
            ta_core::types::Retention::Forever => ta_core::domain::RetentionDefault::Forever,
        }
    })
    .await
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn log_storage_stats(state: State<'_, AppState>) -> IpcResult<ta_store::StorageStats> {
    Ok(state.trace.storage_stats().await?)
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn log_clear(state: State<'_, AppState>, mode: ta_store::ClearMode) -> IpcResult<u64> {
    Ok(state.trace.clear(mode).await?)
}

// ================================================================ 查询能力探测

/// 一项查询能力是否可用。形状按 E1/E2 已确认的约定：
/// `enabled = true` 时 `reason` 为 `None`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
pub struct CapabilitySupport {
    pub enabled: bool,
    /// 不可判定时的**诚实原因**（给 UI 直接显示，文案原文来自 `ta_core::diag::query`）
    pub reason: Option<String>,
}

/// 日志后端在**当前模式**下支持哪些查询能力。
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

/// `log_query_capabilities` —— 前端进日志页时取一次（E2 建议的 ② 方案，
/// 不动 `LogPage` 契约）。
#[tauri::command(rename_all = "snake_case")]
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
