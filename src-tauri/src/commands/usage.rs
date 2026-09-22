//! `usage_*` / `pricing_*`：看板、成本中心查询、CSV 导出。
//!
//! 契约：`docs/10-interface-spec.md` §2.2 `usage / 用量与成本中心`、§2.4 `DashboardData`。
//! 计算全在 `ta-store`（`docs/07` §6），命令层只做转发与参数透传。

use serde::{Deserialize, Serialize};
use tauri::State;

use crate::commands::IpcResult;
use crate::state::AppState;
use ta_store::UsageStore;

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn usage_dashboard(
    state: State<'_, AppState>,
    range: ta_core::clock::TimeRange,
) -> IpcResult<ta_store::DashboardData> {
    Ok(state.usage.dashboard(range).await?)
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn usage_query(
    state: State<'_, AppState>,
    q: ta_store::UsageQuery,
) -> IpcResult<ta_store::UsageTable> {
    Ok(state.usage.query(&q).await?)
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn usage_token_kinds(state: State<'_, AppState>) -> IpcResult<Vec<String>> {
    Ok(state.usage.token_kinds_seen().await?.into_iter().map(|k| k.as_key().to_string()).collect())
}

/// 实时流首次填充（之后靠 `request:new` 事件，[10 §2.3]）。
#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn usage_request_stream_snapshot(
    state: State<'_, AppState>,
    limit: u32,
) -> IpcResult<Vec<ta_core::types::RequestSummary>> {
    Ok(state.usage.recent_summaries(limit).await?)
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn pricing_unpriced_models(state: State<'_, AppState>) -> IpcResult<Vec<String>> {
    Ok(state.usage.unpriced_models().await?)
}

/// 导出 CSV 到指定路径（[10 §2.2] `usage_export_csv`；任何统计都能导出是硬要求）。
#[tauri::command(rename_all = "snake_case")]
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

// ================================================================ 额度缺口（[06 §10.1 B2]）

/// 一个"额度上限有缺口"的套餐 —— UI 必须在套餐列表 / 通道矩阵 / 首启向导
/// **三处显示同一句原文**，否则用户会以为"用完自动切"坏了。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
pub struct QuotaLimitGap {
    pub credential_id: String,
    /// `ta_core::quota::LimitGap::message()` 的**原文**。**本层一个字都不改写** ——
    /// 改写的代价是三处文案分叉（[docs/13 L21] 那类"两人两套都不报错"）。
    pub message: String,
}

/// 纯函数：从配置里挑出额度有缺口的凭据。
///
/// 判定与文案**全部**来自 `ta_core::quota`：用 E2 的
/// `credentials_without_usable_limit` 做枚举、`limit_gap` 取判定与原文，
/// 本层不重复实现"什么算有缺口"（`0` / 负数 / `NaN` / `inf` 这些边界
/// 自己写一定会漏，[06 §10.1 B2] 点过这个坑）。
pub fn limit_gaps_for(doc: &ta_core::domain::ConfigDoc) -> Vec<QuotaLimitGap> {
    let ids = ta_core::quota::credentials_without_usable_limit(
        doc.credentials.iter().map(|c| (&c.id, &c.quota)),
    );
    ids.into_iter()
        .filter_map(|id| {
            let cfg = &doc.credentials.iter().find(|c| &c.id == id)?.quota;
            let gap = ta_core::quota::limit_gap(cfg)?;
            Some(QuotaLimitGap {
                credential_id: id.as_str().to_string(),
                message: gap.message().to_string(),
            })
        })
        .collect()
}

/// [10 §2.2] `quota_limit_gaps`。
#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn quota_limit_gaps(state: State<'_, AppState>) -> IpcResult<Vec<QuotaLimitGap>> {
    Ok(limit_gaps_for(&state.doc()))
}

#[cfg(test)]
mod tests {
    use super::*;

    use ta_core::domain::{ConfigDoc, Credential, QuotaConfig};

    fn doc_with(limits: &[(&str, Option<f64>)]) -> ConfigDoc {
        let mut doc = ConfigDoc::default();
        for (id, limit) in limits {
            let mut c = Credential::new(*id, "p-a", *id, "${secret:x}");
            c.quota = QuotaConfig { limit: *limit, ..Default::default() };
            doc.credentials.push(c);
        }
        doc
    }

    #[test]
    fn flags_missing_and_unusable_limits_and_leaves_good_ones_alone() {
        // 边界一次全上：正常 / 未填 / 0 / 负数 / NaN / inf —— 后四种都是"不可用"
        let doc = doc_with(&[
            ("c-ok", Some(1000.0)),
            ("c-none", None),
            ("c-zero", Some(0.0)),
            ("c-neg", Some(-5.0)),
            ("c-nan", Some(f64::NAN)),
            ("c-inf", Some(f64::INFINITY)),
        ]);
        let gaps = limit_gaps_for(&doc);
        let ids: Vec<&str> = gaps.iter().map(|g| g.credential_id.as_str()).collect();
        assert_eq!(ids, vec!["c-none", "c-zero", "c-neg", "c-nan", "c-inf"]);
        assert!(!ids.contains(&"c-ok"), "配了可用 limit 的套餐不该被点名");
    }

    #[test]
    fn message_is_the_verbatim_ta_core_text() {
        // 三处 UI 显示同一句的前提是"我们不改写"。这条把它钉住。
        let doc = doc_with(&[("c-none", None), ("c-zero", Some(0.0))]);
        let gaps = limit_gaps_for(&doc);
        assert_eq!(gaps[0].message, ta_core::quota::LimitGap::Missing.message());
        assert_eq!(
            gaps[1].message,
            ta_core::quota::LimitGap::Unusable(0.0).message(),
            "0 与负数/NaN 是同一类（不可用的数），文案必须一致"
        );
        assert!(gaps.iter().all(|g| !g.message.is_empty()));
    }

    #[test]
    fn empty_config_yields_no_gaps() {
        assert!(limit_gaps_for(&ConfigDoc::default()).is_empty());
    }
}
