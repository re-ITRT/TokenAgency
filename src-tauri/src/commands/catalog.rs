//! `catalog_*`：provider / credential / model / channel / group / policy / lane / subscription
//! 的**只读**列表与详情（T1a 范围；写操作按 [10 §2.2] 在 T1b 补齐）。
//!
//! 这类命令都是"从热读快照里取"，所以体积极小、不查库。

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tauri::State;

use ta_core::domain::capabilities::channel_capability_mismatches;
use ta_core::domain::{
    ChannelConfig, ChannelGroup, ConfigDoc, Credential, Lane, Model, Policy, Provider,
    PublicModelInfo,
};
use ta_core::types::{LaneId, ProviderId, SubscriptionId};

use crate::commands::net::SubscriptionView;
use crate::commands::{IpcError, IpcResult};
use crate::state::AppState;
use ta_core::config::ConfigValidator;
use ta_store::{ConfigStore, HealthStore, ModelCatalogStore};

/// 凭据（[10 §2.2] `credential_list` 的 `CredentialView`）。
#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct CredentialView {
    pub credential: Credential,
}

/// 模型 + 通道数（[10 §2.2] `model_list`）。
#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct ModelView {
    pub model: Model,
    pub channel_count: u32,
    pub upstream_models: Vec<String>,
    /// [docs/46 §304] **对外暴露的信息**（上下文窗口 / 最大输出 / 能力 / `owned_by`）——
    /// 与网关 `/v1/models` 用的是**同一个** `ConfigDoc::public_model_info`，
    /// 所以「查看模型信息」面板里显示的就是客户端拿到的那一份。
    pub public_info: PublicModelInfo,
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

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn provider_list(state: State<'_, AppState>) -> IpcResult<Vec<Provider>> {
    Ok(doc(&state).providers.clone())
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn provider_get(state: State<'_, AppState>, id: String) -> IpcResult<Option<Provider>> {
    let key = ProviderId::new(id);
    Ok(doc(&state).providers.iter().find(|p| p.id == key).cloned())
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn credential_list(state: State<'_, AppState>) -> IpcResult<Vec<CredentialView>> {
    let d = doc(&state);
    Ok(d.credentials.iter().map(|c| CredentialView { credential: c.clone() }).collect())
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn model_list(state: State<'_, AppState>) -> IpcResult<Vec<ModelView>> {
    let d = doc(&state);
    Ok(d.models
        .iter()
        .map(|m| {
            let chans = d.lines_of_model(m);
            ModelView {
                model: m.clone(),
                channel_count: chans.len() as u32,
                upstream_models: chans.iter().map(|c| c.upstream_model.clone()).collect(),
                public_info: d.public_model_info(m),
            }
        })
        .collect())
}

/// `channel_list` 的**线上形状**（[docs/28 §2]）：通道自己的字段 + **现算的** `models`。
///
/// 为什么需要这个包装：`models`（哪些模型在用这条线路）是从 `Model.lines` 翻过来的**派生值**，
/// 不是配置里存的字段 —— 它是只读的，写命令永远不接受它。`#[serde(flatten)]` 让 JSON 形状
/// 与 `ChannelConfig` 逐字段一致（多一个 `models`），前端契约不用改结构。
#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct ChannelView {
    #[serde(flatten)]
    pub channel: ChannelConfig,
    /// 哪些模型在用这条线路（按 `Model.lines` 现算；遗留 `model` 兜底）。
    pub models: Vec<String>,
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn channel_list(state: State<'_, AppState>) -> IpcResult<Vec<ChannelView>> {
    let d = doc(&state);
    // [docs/28 §2] `models` 是**按当前配置现算**的反向派生值（哪些模型在用这条线路）。
    // 关系的真源是 `Model.lines`；这里只是把它翻过来给界面看 —— 不写回、不持久化。
    Ok(d.channels
        .iter()
        .map(|c| ChannelView {
            channel: c.clone(),
            models: d.models_of_channel(c).iter().map(|m| m.id.clone()).collect(),
        })
        .collect())
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn group_list(state: State<'_, AppState>) -> IpcResult<Vec<ChannelGroup>> {
    Ok(doc(&state).channel_groups.clone())
}

/// 按 priority 升序（UI 的拖拽顺序就是求值顺序，见 [02 §2.8]）。
#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn policy_list(state: State<'_, AppState>) -> IpcResult<Vec<Policy>> {
    let mut list = doc(&state).policies.clone();
    list.sort_by_key(|p| p.priority);
    Ok(list)
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn lane_list(state: State<'_, AppState>) -> IpcResult<Vec<LaneView>> {
    let d = doc(&state);
    let mut out = Vec::with_capacity(d.lanes.len());
    for lane in &d.lanes {
        let latest = state.health.latest(&LaneId::new(lane.id.as_str())).await.unwrap_or(None);
        out.push(LaneView {
            lane: lane.clone(),
            latest,
            // [docs/55]（第 322 轮）线路只在**服务商**上，所以"有几条通道走这条线"
            // 要按**通道所属服务商**的 lane 来数（原来数的是 `channel.lane`，那个字段已删除）。
            //
            // 语义变化如实说明：以前这是"显式写了这条线的通道数"，
            // 现在是"生效线路会是这条线的通道数"——后者才是这个数字本来要回答的问题。
            channel_count: d
                .channels
                .iter()
                .filter(|c| {
                    d.provider(c.provider.as_str()).and_then(|p| p.lane.as_ref()) == Some(&lane.id)
                })
                .count() as u32,
        });
    }
    Ok(out)
}

/// 订阅清单：**配置实体 + 当前生效版本的解析状态**（冻结契约的 `SubscriptionView`）。
///
/// 为什么不在这里返回裸 `Subscription`：解析状态（`node_count` / `last_update_ms` / `parser`）
/// 的真相在**版本存储**里，只有 `net.rs` 那侧的 `VersionKeeper` 够得着；而节点页的硬前置
/// （[docs/19 §2.3]）就判 `last_update_ms`/`error` —— 少给这两个字段，真机上页面的表现是
/// "刷新成功了却说没有解析结果、一个节点都不显示"。实现放在 `net` 侧（订阅域的读路径），
/// 这里只做命令登记。
#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn subscription_list(state: State<'_, AppState>) -> IpcResult<Vec<SubscriptionView>> {
    crate::commands::net::views_of_subscriptions(&state)
}

// ================================================================ 写路径（task-16 批次 A 第 1 组）

/// 配置写路径的**唯一入口**（[docs/02 §4]）：校验 → 事务写库 → 留快照 → 换内存快照。
///
/// 与 `config_apply` 是同一条路径（同一份 validator、同样的顺序），差别只在"改哪里" ——
/// 实体级命令不该各自发明一套写库顺序。
///
/// **校验不过不写库**：宁可前端拿到字段路径，也不要库里出现一份非法配置。
///
/// **为什么是 `pub(crate)`**（第 12 轮由主管放开）：其它命令模块（如 `entity.rs`）的实体写命令
/// 必须走这**同一条**写路径；私有会让它们各自复制一份"validate → save → snapshot → set_doc"，
/// 那就是两份写库顺序，将来必然分叉（[docs/13 L21] 同族错误）。
pub(crate) async fn save_validated(state: &AppState, doc: ConfigDoc, label: &str) -> IpcResult<()> {
    let report = ta_core::config::TaConfigValidator.validate(&doc);
    if !report.ok {
        return Err(IpcError::from_field_errors(&report.errors));
    }
    state.config.save(&doc).await?;
    state.config.snapshot(&doc, label).await?;
    // [docs/46] **内核在跑时，lane 端口表必须跟着配置走**。这条路径原来只 `set_doc`，
    // 不分配端口、也不同步内核配置源 —— 于是 `catalog.rs:267` 建的「自动」线路没有端口，
    // 生成期校验以 `port_missing` 拦下整份配置（文件不更新）⇒ 「自动」出口在真机上从未生效。
    // 分配失败就 `?` 上去：宁可报错，也不要库里出现一条"没有端口的 lane"。
    if state.kernel_source_slot().is_some() {
        let ports = crate::commands::net::allocate_ports(state, &doc)?;
        if let Some(src) = state.kernel_source_slot() {
            src.set_doc(doc.clone());
            src.set_ports(ports.clone());
        }
        // [docs/59] **同一份端口表也要推给网关拨号器**。只喂给内核配置源是不够的：
        // 拨号器按 lane 查端口，它的表不更新 → 新建的 lane 查不到端口 → TA-3002。
        if let Some(hot) = state.lane_ports() {
            hot.replace(ports);
        }
    }
    state.set_doc(doc);
    // [docs/59] **最后一步：让运行中的内核真的吃下这份配置。**
    //
    // 这一条是用户第 326 轮的真机现象（"网络代理只有直连可用"）的次因之一：
    // 上面那几行只把新配置写进了 `GeneratedConfigSource`（**下次**生成 YAML 时才生效），
    // 但正在跑的内核**不会**重新生成、也不会 reload —— 于是 `provider_upsert` 走
    // `ensure_auto_lane` 新建的那条「自动」lane 在内核里**连 listener 都没有**，
    // 请求打到它的端口上必然连不上（`TA-3002 lane_unavailable`）。
    //
    // 与 `net.rs` 的 `commit_config` 那一条写路径保持一致：内核没在跑时它是 no-op，
    // 所以这里是"该做才做"，不是无条件开销。
    crate::commands::net::maybe_apply_kernel(state, label).await?;
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

/// 「自动」出口的**标记**：`lane: "auto:<订阅 id>"`（[docs/29 §2.2]，与 `direct:` 同一套约定）。
///
/// 为什么用标记 + 在这里解析，而不是让前端先建线路再发 id：
/// 前端不该成为"这条线路该长什么样（种类 / 组类型 / id 撞了怎么办）"的第二个真相来源 ——
/// 那套判断在 `ta_core::domain::ConfigDoc::plan_auto_lane` 里，只有一份。
const AUTO_LANE_PREFIX: &str = "auto:";

/// `lane-<服务商 id>-auto` —— 「自动」是**每家服务商自己的**出口（用户第 202 轮原话：
/// 「每一个服务商都有自己的『自动』节点，相当于这个选哪个节点是记录在每一个服务商自身的」）。
///
/// ★ 为什么 id 里带**服务商**而不是订阅：id 里带订阅时，两家服务商挂同一份订阅会**共用一条** lane
/// ⇒ 各自按自己存的地址（`auto_choice` 是按 provider 存的）重算、写**同一个组** ⇒
/// 组上的 `now` 变成"最后写的那家"的选择，日志里还会互相打脸（真机实测，[docs/46 §301]）。
/// 非法字符归一成 `-`。
fn auto_lane_base_id(provider_id: &str) -> String {
    let slug: String = provider_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect();
    format!("lane-{slug}-auto")
}

/// 确保**这家服务商**的「自动」线路存在，返回它的 id（幂等：已有就用已有的，绝不建第二条）。
///
/// 三步，顺序有语义：
/// 1. 这家服务商**当前挂的那条**（库里那条，不是入参里的标记）如果够格 → **就地复用**。
///    这一步同时是**老配置的迁移路径**：早先建的 `lane-<订阅>-auto` 就靠它被继续用下去，
///    不会因为 id 规则改成"带服务商"而多出一条线、把用户已经选好的出口丢掉；
/// 2. 否则按**服务商自己的** id 规划一条（`lane-<服务商>-auto`，撞了就 `-2`）；
/// 3. 名字带上服务商名，界面上两条「自动」不会看起来一模一样。
async fn ensure_auto_lane(
    state: &AppState,
    provider_id: &str,
    provider_name: &str,
    subscription: &str,
    current: Option<&LaneId>,
) -> IpcResult<LaneId> {
    use ta_core::domain::{AutoLanePlan, Lane};

    let doc = state.doc();
    let sub: SubscriptionId = subscription.into();
    if let Some(cur) = current {
        if doc.auto_lane_by_id(cur, &sub).is_some() {
            return Ok(cur.clone());
        }
    }
    let base_id = auto_lane_base_id(provider_id);
    let name = format!("自动（{provider_name}）");
    match doc.plan_auto_lane(&sub, &base_id, &name) {
        AutoLanePlan::Reuse(id) => Ok(id),
        AutoLanePlan::Create { id, name } => {
            let mut next = (*doc).clone();
            next.lanes.push(Lane::automatic(id.clone(), name, sub));
            drop(doc);
            save_validated(state, next, "lane_ensure_auto").await?;
            Ok(id)
        }
    }
}

/// 「直连」出口的**标记**：`lane: "direct:"`（与 `auto:` 同一套约定）。
///
/// 为什么也要标记：用户在向导里选「直连」时，那条 `kind: direct` 的线路可能**还不存在**
/// （新装的配置里一条线路都没有）。让前端先建线路再发 id，等于把"直连线路该长什么样"
/// 变成前端第二份真相；统一由后端解析，前端只发一个标记。
const DIRECT_LANE_MARKER: &str = "direct:";

/// 确保一条 `kind: direct` 的线路存在，返回它的 id（幂等：已有就用已有的）。
async fn ensure_direct_lane(state: &AppState) -> IpcResult<LaneId> {
    use ta_core::domain::{Lane, LaneKind};

    let doc = state.doc();
    if let Some(existing) = doc.lanes.iter().find(|l| l.kind == LaneKind::Direct) {
        return Ok(existing.id.clone());
    }
    let id = LaneId::new("direct");
    let mut next = (*doc).clone();
    next.lanes.push(Lane::direct(id.clone(), "直连"));
    drop(doc);
    save_validated(state, next, "lane_ensure_direct").await?;
    Ok(id)
}

/// `provider_upsert`：同 id 覆盖、不存在则新增（幂等，不会写出重复条目）。
pub async fn upsert_provider(state: &AppState, mut provider: Provider) -> IpcResult<Provider> {
    // [docs/29 §2.2] `lane: "auto:<订阅>"` → 确保**这家服务商自己的**那条「自动」线路存在，
    // 并把**真实 id** 写回去。「当前挂的那条」从库里读（入参里这时还是标记，不是 id）。
    if let Some(sub) =
        provider.lane.as_ref().and_then(|l| l.as_str().strip_prefix(AUTO_LANE_PREFIX))
    {
        let sub = sub.to_string();
        let current =
            state.doc().providers.iter().find(|p| p.id == provider.id).and_then(|p| p.lane.clone());
        let provider_id = provider.id.as_str().to_string();
        let provider_name = provider.name.clone();
        provider.lane = Some(
            ensure_auto_lane(state, &provider_id, &provider_name, &sub, current.as_ref()).await?,
        );
    } else if provider.lane.as_ref().map(|l| l.as_str()) == Some(DIRECT_LANE_MARKER) {
        // [docs/29 §2.2] 选「直连」：那条 `kind: direct` 的线路可能还不存在，这里补出来
        provider.lane = Some(ensure_direct_lane(state).await?);
    }
    let mut doc = (*state.doc()).clone();
    match doc.providers.iter_mut().find(|x| x.id == provider.id) {
        Some(slot) => *slot = provider.clone(),
        None => doc.providers.push(provider.clone()),
    }
    save_validated(state, doc, "provider_upsert").await?;
    Ok(provider)
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn provider_upsert(
    state: State<'_, AppState>,
    provider: Provider,
) -> IpcResult<Provider> {
    upsert_provider(&state, provider).await
}

/// `channel_upsert`：同 id 覆盖、不存在则新增。
///
/// **能力校验**（[docs/17 §2]）：这条线路若会被**已有策略**与同 `model` 的其它线路
/// 归到一起，而它们的能力声明不一致 → 拒绝（TA-2007，带逐字段差异）。
///
/// 语义刻意**不是**"同 model 的所有线路必须一致"：那样用户就没法先建第一条、再加第二条。
/// 只有"已经有一条策略会把它们放在同一条路由后面"时才算矛盾。
pub async fn upsert_channel(
    state: &AppState,
    mut channel: ChannelConfig,
) -> IpcResult<ChannelConfig> {
    // [docs/28 §2] 线路**不再写"我是哪个模型的"**：关系在 `Model.lines` 上。
    // 所以无论调用方发来什么，遗留字段一律清空 —— 否则一个回传上来的老对象会把旧关系又写回去，
    // 而那份关系已经搬进 models 了，那就成了第二份真相源。
    channel.model = String::new();
    // [docs/46 §303] 这条线路**什么都没声明**时，按上游扫描存下来的原文补齐能力
    // （上游 → 软件保存的信息 → 线路能力 → 网关 `/v1/models` → 客户端，缺的一跳就是这里）。
    // 读不到 / 上游也没说 ⇒ 什么都不补（**不编**）。
    adopt_catalog_hints(state, &mut channel).await;
    let mut doc = (*state.doc()).clone();
    match doc.channels.iter_mut().find(|x| x.id == channel.id) {
        Some(slot) => *slot = channel.clone(),
        None => doc.channels.push(channel.clone()),
    }
    if let Some(detail) =
        channel_capability_mismatches(&doc, channel.id.as_str()).into_iter().next()
    {
        return Err(crate::commands::entity::capability_mismatch_ipc(detail));
    }
    save_validated(state, doc, "channel_upsert").await?;
    Ok(channel)
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn channel_upsert(
    state: State<'_, AppState>,
    channel: ChannelConfig,
) -> IpcResult<ChannelConfig> {
    upsert_channel(&state, channel).await
}

/// [docs/46 §303] 用**目录里那一行的上游原文**，补齐这条线路没声明过的能力。
///
/// * 上游名/服务商对不上、目录里没有这一行、原文里没有我们认得的字段 ⇒ 一个都不动（**不编**）；
/// * 读目录失败 ⇒ **不补**，按原样写（写线路不该因为目录读不到而失败），但记一条带错误码的日志。
async fn adopt_catalog_hints(state: &AppState, channel: &mut ChannelConfig) {
    if channel.upstream_model.trim().is_empty() {
        return;
    }
    let row = match state
        .model_catalog
        .get(channel.provider.as_str(), channel.upstream_model.as_str())
        .await
    {
        Ok(row) => row,
        Err(error) => {
            tracing::warn!(
                code = error.code(),
                "读模型目录失败：这次不按上游原文补能力，线路按原样写"
            );
            return;
        }
    };
    let Some(row) = row else {
        return;
    };
    let hints =
        ta_core::domain::upstream_hints::UpstreamModelHints::from_upstream_raw(&row.raw_json);
    let filled = hints.adopt_into(&mut channel.capabilities);
    if filled > 0 {
        // 计数写进正文：`[docs/14 §5]` 的白名单只管**字段名**，数字放消息里更省事也更好读
        tracing::info!(
            channel_id = %channel.id,
            provider_id = %channel.provider,
            upstream_model = %channel.upstream_model,
            "已按上游扫描原文补齐这条线路没声明的能力（上下文窗口/最大输出/模态/工具，共补了 {filled} 处）"
        );
    }
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

#[tauri::command(rename_all = "snake_case")]
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

    /// 一份**能过校验**的订阅（`url` 必须是真 URL：校验器会查）。
    fn sub_cfg(id: &str) -> ta_core::domain::Subscription {
        ta_core::domain::Subscription {
            id: ta_core::types::SubscriptionId::new(id),
            name: format!("机场-{id}"),
            url: "https://airport.example/sub".to_string(),
            user_agent: "clash-verge/v2.0.0".to_string(),
            format: ta_core::domain::SubscriptionFormat::Auto,
            interval_s: 6 * 3600,
            keep_versions: 3,
            filter: Default::default(),
            health_check: Default::default(),
            enabled: true,
            provider_key: None,
            disabled_nodes: Vec::new(),
            extra: serde_json::Map::new(),
        }
    }

    /// ★ 用户第 202 轮：**「自动」是每家服务商自己的**出口（选哪个节点也按服务商记）。
    ///
    /// 四条断言，每一条都对着一个真机上踩过的坑：
    /// ① 按**服务商**派生 id（`lane-<服务商>-auto`）；
    /// ② 同一个服务商重存 → **复用**，不许越用越多条；
    /// ③ 两家服务商各得一条**不同**的 lane（旧的"按订阅复用"会让两家共用一条 ⇒ 互相覆盖选择）；
    /// ④ 老配置（早先按订阅建的 `lane-<订阅>-auto`）**被继续沿用**，不因规则改了而多出一条、丢掉已选好的出口。
    /// [docs/46 §303] 新建线路时，按**上游扫描存下来的原文**补齐它没声明过的能力。
    ///
    /// 这是"服务商 → 软件（保存信息）→ 本机 `/v1/models` → 客户端"这条链上缺的那一跳：
    /// 原文我们逐字存着（真机上 113/135 行带 `token_limits`），但从没人读 ⇒ 线路能力永远是 0
    /// ⇒ 客户端拿不到上下文窗口。断言三件事：
    /// ① 目录里有这一行 ⇒ 没声明的字段被补上（用火山实测形状）；
    /// ② 用户**声明过的**字段一个都不许被改；
    /// ③ 目录里没有这一行 ⇒ 什么都不补（**不编**）。
    #[tokio::test]
    async fn channel_upsert_adopts_upstream_declared_capabilities() {
        let state = fresh_state("adopt-hints").await;
        let mut doc = ConfigDoc::default();
        doc.providers.push(Provider::new("p-vol", "火山", Protocol::Openai, "https://ark"));
        doc.credentials.push(ta_core::domain::Credential::new("c-1", "p-vol", "K", "${s:1}"));
        state.config.save(&doc).await.unwrap();
        state.set_doc(doc);

        // 目录里那一行：火山的实测形状（上下文 32768 / 输出 16384 / 只要文本 / 支持工具）
        state
            .model_catalog
            .upsert(&ta_store::ModelCatalogRow {
                provider_id: "p-vol".to_string(),
                upstream_model: "doubao-pro-32k".to_string(),
                suggested_id: "doubao-pro-32k".to_string(),
                raw_json: serde_json::json!({
                    "token_limits": { "context_window": 32768, "max_input_token_length": 32768,
                                      "max_output_token_length": 16384 },
                    "modalities": { "input_modalities": ["text"] },
                    "features": { "tools": { "function_calling": true } }
                }),
                seen_in_last_scan: true,
                first_seen_ms: 1,
                last_seen_ms: 1,
            })
            .await
            .expect("写目录行");

        // ① 没声明的线路：新建时自动补齐
        let created = upsert_channel(
            &state,
            ChannelConfig::new("ch-vol", "", "p-vol", "c-1", "doubao-pro-32k"),
        )
        .await
        .expect("建线路");
        assert_eq!(created.capabilities.context_tokens, 32_768, "上下文窗口要按上游补上");
        assert_eq!(created.capabilities.max_output_tokens, Some(16_384));
        assert!(created.capabilities.tools, "上游说支持工具 ⇒ 补成 true");
        assert_eq!(created.capabilities.modalities, vec!["text".to_string()]);
        // 落库的那一份也必须是补过的（不能只在返回值里好看）
        let stored = state
            .doc()
            .channels
            .iter()
            .find(|c| c.id.as_str() == "ch-vol")
            .cloned()
            .expect("库里要有这条");
        assert_eq!(stored.capabilities.context_tokens, 32_768);

        // ② 用户声明过的字段不许被改
        let mut declared =
            ChannelConfig::new("ch-vol-declared", "", "p-vol", "c-1", "doubao-pro-32k");
        declared.capabilities.context_tokens = 4_096;
        declared.capabilities.max_output_tokens = Some(1_024);
        let kept = upsert_channel(&state, declared).await.expect("建用户声明过的线路");
        assert_eq!(kept.capabilities.context_tokens, 4_096, "用户填过的不许动");
        assert_eq!(kept.capabilities.max_output_tokens, Some(1_024));

        // ③ 目录里没有这一行 ⇒ 一个字段都不补
        let unknown = upsert_channel(
            &state,
            ChannelConfig::new("ch-unknown", "", "p-vol", "c-1", "never-scanned"),
        )
        .await
        .expect("建没扫到过的线路");
        assert_eq!(unknown.capabilities.context_tokens, 0, "没扫到就不编");
        assert_eq!(unknown.capabilities.max_output_tokens, None);
        assert!(!unknown.capabilities.tools);
    }

    #[tokio::test]
    async fn auto_lane_is_per_provider_and_reuses_the_existing_one() {
        use ta_core::domain::{GroupType, Lane, LaneKind};

        let state = fresh_state("auto-per-provider").await;
        let mut doc = ConfigDoc::default();
        doc.proxy_subscriptions.push(sub_cfg("sub-a"));
        doc.providers.push(Provider::new("p-a", "A", Protocol::Openai, "https://a.example/v1"));
        doc.providers.push(Provider::new("p-b", "B", Protocol::Openai, "https://b.example/v1"));
        state.config.save(&doc).await.unwrap();
        state.set_doc(doc);

        let marker = |id: &str, name: &str| {
            let mut p = Provider::new(id, name, Protocol::Openai, "https://x.example/v1");
            p.lane = Some("auto:sub-a".into());
            p
        };

        // ①② A 选「自动」→ `lane-p-a-auto`；再存一次还是它
        let a = upsert_provider(&state, marker("p-a", "A")).await.expect("A 选自动");
        assert_eq!(a.lane.as_ref().map(|l| l.as_str()), Some("lane-p-a-auto"));
        let lanes_after_a = state.doc().lanes.len();
        let a_again = upsert_provider(&state, a.clone()).await.expect("A 再存");
        assert_eq!(a_again.lane.as_ref().map(|l| l.as_str()), Some("lane-p-a-auto"));
        assert_eq!(state.doc().lanes.len(), lanes_after_a, "幂等：不许越用越多条");

        // ③ B 也选「自动」→ 自己的那条
        let b = upsert_provider(&state, marker("p-b", "B")).await.expect("B 选自动");
        assert_eq!(b.lane.as_ref().map(|l| l.as_str()), Some("lane-p-b-auto"), "两家各有一条");
        {
            let doc = state.doc();
            assert_eq!(doc.lanes.len(), lanes_after_a + 1);
            for id in ["lane-p-a-auto", "lane-p-b-auto"] {
                let l = doc.lanes.iter().find(|x| x.id.as_str() == id).expect("那条要在");
                assert_eq!(l.kind, LaneKind::SubscriptionAuto, "种类不许换");
                assert_eq!(l.group_type, Some(GroupType::Select), "必须是显式 select");
            }
        }

        // ④ 老配置：A 当时挂的是早先那条 `lane-sub-a-auto` → 重存时**沿用**，不多建
        let mut legacy = ConfigDoc::default();
        legacy.proxy_subscriptions.push(sub_cfg("sub-a"));
        legacy.lanes.push(Lane::automatic("lane-sub-a-auto", "自动", "sub-a"));
        let mut pa = Provider::new("p-a", "A", Protocol::Openai, "https://a.example/v1");
        pa.lane = Some("lane-sub-a-auto".into());
        legacy.providers.push(pa);
        state.config.save(&legacy).await.unwrap();
        state.set_doc(legacy);

        let again = upsert_provider(&state, marker("p-a", "A")).await.expect("老配置重存");
        assert_eq!(
            again.lane.as_ref().map(|l| l.as_str()),
            Some("lane-sub-a-auto"),
            "老的那条沿用"
        );
        assert_eq!(
            state.doc().lanes.iter().filter(|l| l.kind == LaneKind::SubscriptionAuto).count(),
            1,
            "不许因为 id 规则改了而多建一条、把用户已经选好的出口丢掉"
        );
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
