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

use crate::commands::{IpcError, IpcResult};
use crate::state::AppState;
use ta_core::config::ConfigValidator;
use ta_store::{ConfigStore, HealthStore};

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

// ================================================================ 写路径（task-16 批次 A 第 1 组）

/// 配置写路径的**唯一入口**（[docs/02 §4]）：校验 → 事务写库 → 留快照 → 换内存快照。
///
/// 与 `config_apply` 是同一条路径（同一份 validator、同样的顺序），差别只在"改哪里" ——
/// 实体级命令不该各自发明一套写库顺序。
///
/// **校验不过不写库**：宁可前端拿到字段路径，也不要库里出现一份非法配置。
async fn save_validated(state: &AppState, doc: ConfigDoc, label: &str) -> IpcResult<()> {
    let report = ta_core::config::TaConfigValidator.validate(&doc);
    if !report.ok {
        return Err(IpcError::from_field_errors(&report.errors));
    }
    state.config.save(&doc).await?;
    state.config.snapshot(&doc, label).await?;
    state.set_doc(doc);
    Ok(())
}

/// "你要改的东西不存在" —— 必须报错，**不能静默成功**。
///
/// 用 `MalformedRequest`（TA-1004）而不是 `Referenced`（TA-5002 的语义是"被引用、禁止删除"）。
/// 语义上更精确的是"没有这个实体"，但 `TaError` 现在没有那个变体 ——
/// **不自己发明码**，用最接近的既有码，并在汇报里登记这个缺口。
fn not_found(what: &str, id: &str) -> IpcError {
    IpcError::from(ta_core::error::TaError::MalformedRequest(format!(
        "{what}不存在：{id}（无法修改一个不存在的实体）"
    )))
}

/// `provider_upsert`：同 id 覆盖、不存在则新增（幂等，不会写出重复条目）。
pub async fn upsert_provider(state: &AppState, provider: Provider) -> IpcResult<Provider> {
    let mut doc = (*state.doc()).clone();
    match doc.providers.iter_mut().find(|x| x.id == provider.id) {
        Some(slot) => *slot = provider.clone(),
        None => doc.providers.push(provider.clone()),
    }
    save_validated(state, doc, "provider_upsert").await?;
    Ok(provider)
}

#[tauri::command]
#[specta::specta]
pub async fn provider_upsert(
    state: State<'_, AppState>,
    provider: Provider,
) -> IpcResult<Provider> {
    upsert_provider(&state, provider).await
}

/// `channel_upsert`：同 id 覆盖、不存在则新增。
pub async fn upsert_channel(state: &AppState, channel: ChannelConfig) -> IpcResult<ChannelConfig> {
    let mut doc = (*state.doc()).clone();
    match doc.channels.iter_mut().find(|x| x.id == channel.id) {
        Some(slot) => *slot = channel.clone(),
        None => doc.channels.push(channel.clone()),
    }
    save_validated(state, doc, "channel_upsert").await?;
    Ok(channel)
}

#[tauri::command]
#[specta::specta]
pub async fn channel_upsert(
    state: State<'_, AppState>,
    channel: ChannelConfig,
) -> IpcResult<ChannelConfig> {
    upsert_channel(&state, channel).await
}

/// `channel_set_enabled`：只翻转一个通道的启用状态。
pub async fn set_channel_enabled(state: &AppState, id: &str, enabled: bool) -> IpcResult<()> {
    let mut doc = (*state.doc()).clone();
    match doc.channels.iter_mut().find(|c| c.id.as_str() == id) {
        Some(slot) => slot.enabled = enabled,
        None => return Err(not_found("通道", id)),
    }
    save_validated(state, doc, "channel_set_enabled").await
}

#[tauri::command]
#[specta::specta]
pub async fn channel_set_enabled(
    state: State<'_, AppState>,
    id: String,
    enabled: bool,
) -> IpcResult<()> {
    set_channel_enabled(&state, &id, enabled).await
}

#[cfg(test)]
mod write_tests {
    use super::*;

    use ta_core::domain::{Credential, Model};
    use ta_core::types::Protocol;

    /// 一个**真的库**（临时目录里的 SQLite）。
    ///
    /// 写命令的测试必须真的落库再读回来 —— 只断言"函数返回 Ok"证明不了"写进去了"。
    async fn fresh_state(tag: &str) -> AppState {
        let dir = std::env::temp_dir().join(format!("ta-cat-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("建临时目录");
        AppState::bootstrap(dir.join("db.sqlite")).await.expect("开库")
    }

    /// 种一份**引用完整**的最小配置（校验器查引用完整性，缺一个就存不进去）。
    async fn seeded(tag: &str) -> (AppState, ChannelConfig) {
        let state = fresh_state(tag).await;
        let mut doc = ConfigDoc::default();
        doc.providers.push(Provider::new("p-a", "A", Protocol::Openai, "https://a.example/v1"));
        doc.credentials.push(Credential::new("c-a", "p-a", "A", "${secret:a}"));
        doc.models.push(Model::new("m-a"));
        let ch = ChannelConfig::new("ch-a", "m-a", "p-a", "c-a", "upstream-a");
        doc.channels.push(ch.clone());
        state.config.save(&doc).await.expect("种配置");
        state.set_doc(doc);
        (state, ch)
    }

    #[tokio::test]
    async fn provider_upsert_writes_then_reads_back_and_stays_idempotent() {
        let state = fresh_state("prov-upsert").await;
        let p = Provider::new("p-new", "新服务商", Protocol::Openai, "https://api.example/v1");
        upsert_provider(&state, p.clone()).await.expect("首次 upsert");

        // 内存快照
        assert!(state.doc().providers.iter().any(|x| x.id == p.id));
        // **真读库**
        let from_db = state.config.load().await.expect("load");
        assert!(from_db.providers.iter().any(|x| x.id == p.id), "写进去的必须读得回来");

        // 同 id 再 upsert（改名）→ 覆盖而不是新增
        let mut renamed = p.clone();
        renamed.name = "改名后".to_string();
        upsert_provider(&state, renamed).await.expect("再次 upsert");
        let after = state.config.load().await.expect("load");
        assert_eq!(after.providers.iter().filter(|x| x.id == p.id).count(), 1, "不该重复");
        assert_eq!(
            after.providers.iter().find(|x| x.id == p.id).map(|x| x.name.clone()),
            Some("改名后".to_string())
        );
    }

    #[tokio::test]
    async fn channel_set_enabled_persists_is_reversible_and_rejects_unknown_id() {
        let (state, ch) = seeded("ch-enable").await;
        assert!(state.doc().channels.iter().all(|c| c.enabled), "省略 enabled 即启用");

        set_channel_enabled(&state, ch.id.as_str(), false).await.expect("停用");
        assert!(!state.doc().channels[0].enabled, "内存快照要立刻反映");
        assert!(
            !state.config.load().await.expect("load").channels[0].enabled,
            "停用必须落库（不是只改内存）"
        );

        set_channel_enabled(&state, ch.id.as_str(), true).await.expect("再启用");
        assert!(state.config.load().await.expect("load").channels[0].enabled, "可逆：启用也要落库");

        // 未知 id 必须报错，不能静默成功
        assert!(set_channel_enabled(&state, "ch-not-exist", false).await.is_err());
    }

    #[tokio::test]
    async fn channel_upsert_adds_and_overwrites_by_id() {
        let (state, ch) = seeded("ch-upsert").await;
        let mut ch2 = ChannelConfig::new("ch-b", "m-a", "p-a", "c-a", "upstream-b");
        ch2.enabled = false;
        upsert_channel(&state, ch2.clone()).await.expect("新增");
        let after = state.config.load().await.expect("load");
        assert_eq!(after.channels.len(), 2, "新增应落库");
        assert!(!after.channels.iter().find(|c| c.id == ch2.id).expect("刚加进去的").enabled);

        // 覆盖同一个 id：改上游名，不新增
        let mut ch2b = ch2.clone();
        ch2b.upstream_model = "upstream-b2".to_string();
        upsert_channel(&state, ch2b).await.expect("覆盖");
        let after = state.config.load().await.expect("load");
        assert_eq!(after.channels.len(), 2, "同 id 不该重复");
        assert_eq!(
            after.channels.iter().find(|c| c.id == ch2.id).map(|c| c.upstream_model.clone()),
            Some("upstream-b2".to_string())
        );
        assert!(after.channels.iter().any(|c| c.id == ch.id), "另一个通道不该被动过");
    }
}
