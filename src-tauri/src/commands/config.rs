//! `config_*`：配置读 / 校验 / 应用 / 导入导出 / 历史与回滚。
//!
//! 契约：`docs/10-interface-spec.md` §2.2 `config / 配置`、`docs/02` §4（真源是 SQLite）。
//!
//! 写路径的形状（[02 §4]）：校验 → 事务写库 → 留快照 → 换内存快照。
//! **网关/内核的重新加载不在这里**：那是 G2/T2 的编排（命令层只回答
//! `kernel_reload: false`，不假装已经 reload）。

use serde::{Deserialize, Serialize};
use tauri::State;

use ta_core::config::ConfigValidator;
use ta_core::domain::ConfigDoc;
use ta_core::error::TaError;

use crate::commands::{IpcError, IpcResult};
use crate::state::AppState;
use ta_store::ConfigStore;

#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct ApplyResult {
    pub applied: bool,
    pub restart_required: bool,
    pub kernel_reload: bool,
}

/// 校验实现来自 ta-core（P1 的 `TaConfigValidator`：JSON Schema + 引用完整性）。
fn validator() -> ta_core::config::TaConfigValidator {
    ta_core::config::TaConfigValidator
}

/// 校验**但不写库**（前端编辑时实时调用）。
#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn config_validate(
    _state: State<'_, AppState>,
    doc: ConfigDoc,
) -> IpcResult<ta_core::config::ValidationReport> {
    Ok(validator().validate(&doc))
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn config_get(state: State<'_, AppState>) -> IpcResult<ConfigDoc> {
    Ok((*state.doc()).clone())
}

/// 全量应用：校验不过直接拒（不写库），通过则事务写库 + 快照 + 换内存快照。
#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn config_apply(state: State<'_, AppState>, doc: ConfigDoc) -> IpcResult<ApplyResult> {
    let report = validator().validate(&doc);
    if !report.ok {
        return Err(IpcError::from_field_errors(&report.errors));
    }
    state.config.save(&doc).await?;
    state.config.snapshot(&doc, "apply").await?;
    state.set_doc(doc);
    Ok(ApplyResult { applied: true, restart_required: false, kernel_reload: false })
}

/// 导出 YAML。`with_secrets=false` 时把明文 `local_key` 打码
/// （上游 key 本来就只有 `${secret:...}` 引用，**不存在**明文可导）。
#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn config_export_yaml(
    state: State<'_, AppState>,
    with_secrets: bool,
) -> IpcResult<String> {
    let mut doc = (*state.doc()).clone();
    if !with_secrets && doc.gateway.local_key.as_deref().is_some_and(|k| !k.is_empty()) {
        doc.gateway.local_key = Some("***".to_string());
    }
    ta_core::config::to_yaml(&doc).map_err(IpcError::from)
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn config_import_yaml(
    state: State<'_, AppState>,
    yaml: String,
    mode: String,
) -> IpcResult<ApplyResult> {
    let incoming = ta_core::config::from_yaml(&yaml)?;
    let doc = match mode.as_str() {
        "replace" => incoming,
        "merge" => merge_docs(&state.doc(), incoming),
        other => {
            return Err(IpcError::from(TaError::MalformedRequest(format!(
                "mode 只能是 merge|replace，收到 {other}"
            ))))
        }
    };
    config_apply(state, doc).await
}

/// 合并：按 id 覆盖同 id 的实体，其余保留（导入别人分享的片段时用）。
fn merge_docs(base: &ConfigDoc, incoming: ConfigDoc) -> ConfigDoc {
    let mut out = base.clone();
    macro_rules! merge_vec {
        ($field:ident) => {
            for item in incoming.$field {
                match out.$field.iter_mut().find(|x| x.id == item.id) {
                    Some(slot) => *slot = item,
                    None => out.$field.push(item),
                }
            }
        };
    }
    merge_vec!(proxy_subscriptions);
    merge_vec!(lanes);
    merge_vec!(providers);
    merge_vec!(credentials);
    merge_vec!(channels);
    merge_vec!(channel_groups);
    merge_vec!(policies);
    for item in incoming.models {
        match out.models.iter_mut().find(|x| x.id == item.id) {
            Some(slot) => *slot = item,
            None => out.models.push(item),
        }
    }
    out
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn config_history_list(
    state: State<'_, AppState>,
    limit: u32,
) -> IpcResult<Vec<ta_store::SnapshotMeta>> {
    Ok(state.config.list_snapshots(limit).await?)
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn config_rollback(
    state: State<'_, AppState>,
    snapshot_id: String,
) -> IpcResult<ApplyResult> {
    let id = ta_store::SnapshotId(snapshot_id);
    let doc = state.config.rollback(&id).await?;
    state.set_doc(doc);
    Ok(ApplyResult { applied: true, restart_required: false, kernel_reload: false })
}
