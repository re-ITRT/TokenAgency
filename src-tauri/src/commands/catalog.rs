//! `catalog_*`：provider / credential / model / channel / group / policy / lane / subscription
//! 的**只读**列表与详情（T1a 范围；写操作按 [10 §2.2] 在 T1b 补齐）。
//!
//! 这类命令都是"从热读快照里取"，所以体积极小、不查库。

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tauri::State;

use ta_core::domain::{
    ChannelConfig, ChannelGroup, ConfigDoc, Credential, Lane, Model, Policy, Provider, Subscription,
};
use ta_core::types::{LaneId, ProviderId};

use crate::commands::IpcResult;
use crate::state::AppState;
use ta_store::HealthStore;

/// 凭据 + 额度运行态（[10 §2.2] `credential_list` 的 `CredentialView`）。
#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct CredentialView {
    pub credential: Credential,
    pub quota_state: Option<ta_store::QuotaStateRow>,
    /// 已用比例（无限额或未知 → None，**不猜**）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub used_pct: Option<f64>,
}

/// 模型 + 通道数（[10 §2.2] `model_list`）。
#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct ModelView {
    pub model: Model,
    pub channel_count: u32,
    pub upstream_models: Vec<String>,
}

/// 线路 + 最近健康样本（[10 §2.2] `lane_list`）。
#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct LaneView {
    pub lane: Lane,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest: Option<ta_store::HealthSample>,
    pub channel_count: u32,
}

fn doc(state: &AppState) -> Arc<ConfigDoc> {
    state.doc()
}

#[tauri::command]
#[specta::specta]
pub async fn provider_list(state: State<'_, AppState>) -> IpcResult<Vec<Provider>> {
    Ok(doc(&state).providers.clone())
}

#[tauri::command]
#[specta::specta]
pub async fn provider_get(state: State<'_, AppState>, id: String) -> IpcResult<Option<Provider>> {
    let key = ProviderId::new(id);
    Ok(doc(&state).providers.iter().find(|p| p.id == key).cloned())
}

/// 已用比例只在"用户填了手工限额"时才有意义；`metric: unknown` 或无限额 → `None`
/// （**不猜**：宁可不显示，也不给一个看起来精确的假数字）。
fn used_pct_of(c: &Credential, state: Option<&ta_store::QuotaStateRow>) -> Option<f64> {
    let limit = c.quota.limit?;
    if limit <= 0.0 {
        return None;
    }
    let used = match c.quota.metric {
        ta_core::domain::QuotaMetric::Tokens => state.map(|s| s.used_tokens as f64).unwrap_or(0.0),
        ta_core::domain::QuotaMetric::Requests => {
            state.map(|s| s.used_requests as f64).unwrap_or(0.0)
        }
        ta_core::domain::QuotaMetric::Cost => state.map(|s| s.used_cost).unwrap_or(0.0),
        ta_core::domain::QuotaMetric::Unknown => return None,
    };
    Some((used / limit).clamp(0.0, 10.0))
}

#[tauri::command]
#[specta::specta]
pub async fn credential_list(state: State<'_, AppState>) -> IpcResult<Vec<CredentialView>> {
    let d = doc(&state);
    let states = state.usage.list_quota_states().await.unwrap_or_default();
    Ok(d.credentials
        .iter()
        .map(|c| {
            let st = states.iter().find(|s| s.credential_id == c.id.as_str()).cloned();
            let used_pct = used_pct_of(c, st.as_ref());
            CredentialView { credential: c.clone(), quota_state: st, used_pct }
        })
        .collect())
}

#[tauri::command]
#[specta::specta]
pub async fn model_list(state: State<'_, AppState>) -> IpcResult<Vec<ModelView>> {
    let d = doc(&state);
    Ok(d.models
        .iter()
        .map(|m| {
            let chans: Vec<&ta_core::domain::ChannelConfig> =
                d.channels.iter().filter(|c| c.model == m.id).collect();
            ModelView {
                model: m.clone(),
                channel_count: chans.len() as u32,
                upstream_models: chans.iter().map(|c| c.upstream_model.clone()).collect(),
            }
        })
        .collect())
}

#[tauri::command]
#[specta::specta]
pub async fn channel_list(state: State<'_, AppState>) -> IpcResult<Vec<ChannelConfig>> {
    Ok(doc(&state).channels.clone())
}

#[tauri::command]
#[specta::specta]
pub async fn group_list(state: State<'_, AppState>) -> IpcResult<Vec<ChannelGroup>> {
    Ok(doc(&state).channel_groups.clone())
}

/// 按 priority 升序（UI 的拖拽顺序就是求值顺序，见 [02 §2.8]）。
#[tauri::command]
#[specta::specta]
pub async fn policy_list(state: State<'_, AppState>) -> IpcResult<Vec<Policy>> {
    let mut list = doc(&state).policies.clone();
    list.sort_by_key(|p| p.priority);
    Ok(list)
}

#[tauri::command]
#[specta::specta]
pub async fn lane_list(state: State<'_, AppState>) -> IpcResult<Vec<LaneView>> {
    let d = doc(&state);
    let mut out = Vec::with_capacity(d.lanes.len());
    for lane in &d.lanes {
        let latest = state.health.latest(&LaneId::new(lane.id.as_str())).await.unwrap_or(None);
        out.push(LaneView {
            lane: lane.clone(),
            latest,
            channel_count: d
                .channels
                .iter()
                .filter(|c| c.lane.as_ref().is_some_and(|l| l == &lane.id))
                .count() as u32,
        });
    }
    Ok(out)
}

#[tauri::command]
#[specta::specta]
pub async fn subscription_list(state: State<'_, AppState>) -> IpcResult<Vec<Subscription>> {
    Ok(doc(&state).proxy_subscriptions.clone())
}
