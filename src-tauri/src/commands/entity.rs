//! 实体级**写命令**：provider / credential / model / channel / group / policy / pricing。
//!
//! 契约：`docs/10-interface-spec.md` §2.2（各命令的入参与返回），
//! 真源是 `src/shared/bindings.ts` 的 `CommandMap`。每个命令的 DTO 形状**逐字段对齐**它。
//!
//! # 写路径
//!
//! 所有写命令都走**同一个** [`save_validated`]（`catalog::save_validated`，`pub(crate)`）：
//!
//! ```text
//! 校验（TaConfigValidator）→ 不通过就带着字段路径返回，**一个字节都不写库**
//!   ↓ 通过
//! config.save（单事务整体替换）→ config.snapshot（留快照）→ set_doc（换内存热读快照）
//! ```
//!
//! 本模块**不再自带**这份实现：第 12 轮主管已把 `catalog::save_validated` 放开成 `pub(crate)`，
//! 实体写命令只有这一条写库顺序（两份会分叉，[docs/13 L21] 同族错误）。
//!
//! # 实体不存在
//!
//! `TaError`（`crates/ta-core/src/error.rs`）**没有"实体不存在"变体**：语义最接近的是
//! `MalformedRequest`（TA-1004，HTTP 400）。[`not_found`] 用它，并在消息里写清"哪个实体、
//! 哪个 id"。**不自己发明错误码**（新增码要同步 `docs/11 §5` 的总表）。
//! "被引用、禁止删除"有专门的 `TaError::Referenced`（TA-5002），所以 [`referenced`] 用它。
//!
//! # 网络类命令（本文件**不**实现，见汇报）
//!
//! `provider_test` / `provider_echo` / `model_import_from_provider` / `model_apply_import`
//! 需要真的发 HTTP 到上游。**唯一合规的出站通道**是 `ta_gateway` 的 `OutboundDialer`
//! （走 lane 端口 = 走线路策略）——它现在**够得到**了（`lib.rs` 的 `activate_gateway`
//! 已把 `GatewayDeps` 交给 `AppState::set_gateway`），但本文件仍不实现这 4 条：
//!
//! * `provider_echo` / `model_import_from_provider` 要**读上游响应体**
//!   （回显对比 / 解析 `/v1/models` 列表），而 `ta-gateway` 的 `collect_body`
//!   （`pipeline.rs:1079`）是私有的，`src-tauri` 也不依赖 `futures-util`
//!   （`Cargo.toml` 不在本工作包范围内）→ **读不了 body 就做不出诚实的回显与导入**；
//! * `model_apply_import` 依赖前一条产出的 `preview_token`（没有它就没有可应用的预览）；
//! * `provider_test` 本身可以只发一个状态探测（`dialer.execute` + 公共的
//!   `FullPassthrough::build_headers_with_body`），但"探测发什么请求、什么算 ok"
//!   需要主管裁决（`GET {base}/models` 在部分中转上是 404，会变成**假警报**；
//!   而真正的连通性验证要用 chat completion，那又回到"读响应体"的阻塞）。
//!
//! **不另开第二条出站路径**（那会绕过线路策略），这 4 条**不注册**。

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use tauri::State;

use ta_core::domain::{ChannelConfig, ChannelGroup, ConfigDoc, Credential, Model, Policy};
use ta_core::error::TaError;
use ta_core::pricing::PricingEngine;
use ta_core::types::{ChannelGroupId, StickyKey};
use ta_store::{SecretMetaStore, StickyStore};

use crate::commands::catalog::save_validated;
use crate::commands::config::ApplyResult;
use crate::commands::{IpcError, IpcResult};
use crate::state::AppState;

// ================================================================ 写路径

/// 命令层统一返回的 `ApplyResult`（形状与 `config::ApplyResult` 一致）。
///
/// `kernel_reload` **恒为 false**：网关/内核的重新加载是 G2/T2 的编排，
/// 命令层不假装已经 reload（`config_apply` 同款语义）。
fn applied() -> ApplyResult {
    ApplyResult { applied: true, restart_required: false, kernel_reload: false }
}

/// "你要改的东西不存在" —— 必须报错，**不能静默成功**。
///
/// 语义上更精确的是"没有这个实体"，但 `TaError` 现在没有那个变体 —— **不自己发明码**，
/// 用最接近的既有码（TA-1004），并在模块文档与本工作包的汇报里登记这个缺口。
fn not_found(what: &str, id: &str) -> IpcError {
    IpcError::from(TaError::MalformedRequest(format!(
        "{what}不存在：{id}（无法修改一个不存在的实体）"
    )))
}

/// "被引用，禁止删除" → `TaError::Referenced`（TA-5002，HTTP 409）。
///
/// `refs` 是**人话清单**（"通道 ch-a、定价条目 #2"），前端直接显示。
fn referenced(what: &str, refs: &[String]) -> IpcError {
    IpcError::from(TaError::Referenced {
        what: format!("{what}（引用它的地方：{}；要连带删除请传 force=true）", refs.join("、")),
    })
}

fn empty_list(field: &str) -> IpcError {
    IpcError::from(TaError::MalformedRequest(format!(
        "{field} 为空：这条命令是要改一批实体的，空清单只会写出一份一模一样的配置"
    )))
}

// ================================================================ 级联删除

/// 从通道组与策略里摘掉一组已删除的通道，返回**因此变空而被删掉的通道组 id**。
///
/// 为什么必须连组一起删：`channel_groups[i].members` 为空是**校验错误**
/// （`codes::EMPTY_MEMBERS`），留着一个空组会让整份配置都存不进去 ——
/// 那才是真正的"删不掉"，比多删一个组糟得多。
///
/// 同步处理策略对通道的三处引用（`use_channels` / `exclude_channels` / `pin_sticky`），
/// 否则 validate 会以 `not_found` 拒绝整份文档。
fn prune_channels(doc: &mut ConfigDoc, removed: &BTreeSet<String>) -> BTreeSet<String> {
    let mut emptied: BTreeSet<String> = BTreeSet::new();
    let mut kept: Vec<ChannelGroup> = Vec::with_capacity(doc.channel_groups.len());
    for mut g in doc.channel_groups.drain(..) {
        g.members.retain(|m| !removed.contains(m.as_str()));
        if g.members.is_empty() {
            emptied.insert(g.id.as_str().to_string());
        } else {
            kept.push(g);
        }
    }
    doc.channel_groups = kept;

    for p in doc.policies.iter_mut() {
        p.action.use_channels.retain(|c| !removed.contains(c.as_str()));
        p.action.exclude_channels.retain(|c| !removed.contains(c.as_str()));
        if p.action.pin_sticky.as_ref().is_some_and(|c| removed.contains(c.as_str())) {
            p.action.pin_sticky = None;
        }
        if p.action.use_group.as_ref().is_some_and(|g| emptied.contains(g.as_str())) {
            p.action.use_group = None;
        }
    }
    emptied
}

/// 摘掉策略里对已删除通道组的引用（`action.use_group` → `None`）。
fn prune_group_refs(doc: &mut ConfigDoc, removed: &BTreeSet<String>) {
    for p in doc.policies.iter_mut() {
        if p.action.use_group.as_ref().is_some_and(|g| removed.contains(g.as_str())) {
            p.action.use_group = None;
        }
    }
}

// ================================================================ DTO：额度状态

/// `docs/10 §2.2` / `bindings.ts` 的 `QuotaState`（**不是** `ta-store` 的 `QuotaStateRow`：
/// 那个是库的行，字段与线上契约不同）。
#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct QuotaState {
    pub credential_id: String,
    /// `ok` | `soft` | `exhausted` | `unknown`（`QuotaStateKind` 的线上写法）
    pub state: String,
    #[serde(default)]
    pub used_tokens: u64,
    #[serde(default)]
    pub used_requests: u64,
    #[serde(default)]
    pub used_cost: f64,
    #[serde(default)]
    pub reported_remaining: Option<f64>,
    #[serde(default)]
    pub window_start_ms: i64,
    #[serde(default)]
    pub window_end_ms: Option<i64>,
    #[serde(default)]
    pub updated_at_ms: i64,
}

impl QuotaState {
    fn from_core(s: &ta_core::quota::QuotaState) -> Self {
        Self {
            credential_id: s.credential_id.as_str().to_string(),
            state: ta_store::quota_backend::kind_str(s.kind).to_string(),
            used_tokens: s.used_tokens,
            used_requests: s.used_requests,
            used_cost: s.used_cost,
            reported_remaining: s.reported_remaining,
            window_start_ms: s.window_start_ms,
            window_end_ms: s.window_end_ms,
            updated_at_ms: s.updated_at_ms,
        }
    }

    fn from_row(r: &ta_store::QuotaStateRow) -> Self {
        Self {
            credential_id: r.credential_id.clone(),
            state: r.state.clone(),
            used_tokens: r.used_tokens.max(0) as u64,
            used_requests: r.used_requests.max(0) as u64,
            used_cost: r.used_cost,
            reported_remaining: r.reported_remaining,
            window_start_ms: r.window_start,
            window_end_ms: if r.window_end == 0 { None } else { Some(r.window_end) },
            updated_at_ms: r.updated_at,
        }
    }
}

// ================================================================ provider

/// `provider_delete`：无引用直接删；有引用必须 `force=true`，`force` 时**级联**删除
/// 该服务商名下的凭据与通道（以及它们在通道组/策略/定价里的引用）。
pub async fn delete_provider(state: &AppState, id: &str, force: bool) -> IpcResult<()> {
    let mut doc = (*state.doc()).clone();
    if !doc.providers.iter().any(|p| p.id.as_str() == id) {
        return Err(not_found("服务商", id));
    }

    let cred_ids: BTreeSet<String> = doc
        .credentials
        .iter()
        .filter(|c| c.provider.as_str() == id)
        .map(|c| c.id.as_str().to_string())
        .collect();
    let ch_ids: BTreeSet<String> = doc
        .channels
        .iter()
        .filter(|c| c.provider.as_str() == id || cred_ids.contains(c.credential.as_str()))
        .map(|c| c.id.as_str().to_string())
        .collect();
    let policy_refs: Vec<String> = doc
        .policies
        .iter()
        .filter(|p| p.r#match.provider_in.iter().any(|x| x.as_str() == id))
        .map(|p| format!("策略 {}", p.id))
        .collect();
    let pricing_refs: Vec<String> = doc
        .pricing
        .entries
        .iter()
        .enumerate()
        .filter(|(_, e)| {
            e.r#match.provider.as_ref().is_some_and(|p| p.as_str() == id)
                || e.r#match.credential.as_ref().is_some_and(|c| cred_ids.contains(c.as_str()))
        })
        .map(|(i, _)| format!("定价条目 #{i}"))
        .collect();
    let fallback_ref = doc.gateway.fallback_provider.as_ref().is_some_and(|p| p.as_str() == id);

    if !force {
        let mut refs: Vec<String> = Vec::new();
        if !cred_ids.is_empty() {
            refs.push(format!(
                "{} 份凭据（{}）",
                cred_ids.len(),
                cred_ids.iter().cloned().collect::<Vec<_>>().join(", ")
            ));
        }
        if !ch_ids.is_empty() {
            refs.push(format!(
                "{} 条通道（{}）",
                ch_ids.len(),
                ch_ids.iter().cloned().collect::<Vec<_>>().join(", ")
            ));
        }
        refs.extend(policy_refs);
        refs.extend(pricing_refs);
        if fallback_ref {
            refs.push("gateway.fallback_provider".to_string());
        }
        if !refs.is_empty() {
            return Err(referenced("服务商", &refs));
        }
    }

    // ---- 级联（force 或本来就没有引用）
    doc.providers.retain(|p| p.id.as_str() != id);
    doc.credentials.retain(|c| c.provider.as_str() != id);
    let emptied = prune_channels(&mut doc, &ch_ids);
    doc.channels.retain(|c| !ch_ids.contains(c.id.as_str()));
    prune_group_refs(&mut doc, &emptied);
    for p in doc.policies.iter_mut() {
        p.r#match.provider_in.retain(|x| x.as_str() != id);
    }
    doc.pricing.entries.retain(|e| {
        !e.r#match.provider.as_ref().is_some_and(|p| p.as_str() == id)
            && !e.r#match.credential.as_ref().is_some_and(|c| cred_ids.contains(c.as_str()))
    });
    if fallback_ref {
        doc.gateway.fallback_provider = None;
    }

    save_validated(state, doc, "provider_delete").await
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn provider_delete(state: State<'_, AppState>, id: String, force: bool) -> IpcResult<()> {
    delete_provider(&state, &id, force).await
}

// ================================================================ credential

/// `credential_upsert`：同 id 覆盖、不存在则新增（幂等）。
///
/// **只写引用**：`credential.secret` 必须是 `"${secret:name}"` / `"${env:VAR}"`，
/// 明文由校验层/`ta-secrets` 拦截 —— 明文 key 永远不进 SQLite。
pub async fn upsert_credential(state: &AppState, credential: Credential) -> IpcResult<Credential> {
    let mut doc = (*state.doc()).clone();
    match doc.credentials.iter_mut().find(|x| x.id == credential.id) {
        Some(slot) => *slot = credential.clone(),
        None => doc.credentials.push(credential.clone()),
    }
    save_validated(state, doc, "credential_upsert").await?;
    Ok(credential)
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn credential_upsert(
    state: State<'_, AppState>,
    credential: Credential,
) -> IpcResult<Credential> {
    upsert_credential(&state, credential).await
}

/// `credential_delete`：无引用直接删；有引用必须 `force=true`。
///
/// **不删钥匙串条目**：一条 `${secret:name}` 可能被多份凭据共用（也可能属于用户别处的配置），
/// 删凭据时顺手删 key 会静默毁掉另一份还能用的配置。条目留在 `ta-secrets` 里，
/// 用户可以显式管理（`secrets_*` 那批命令的职责）。
pub async fn delete_credential(state: &AppState, id: &str, force: bool) -> IpcResult<()> {
    let mut doc = (*state.doc()).clone();
    if !doc.credentials.iter().any(|c| c.id.as_str() == id) {
        return Err(not_found("凭据", id));
    }

    let ch_ids: BTreeSet<String> = doc
        .channels
        .iter()
        .filter(|c| c.credential.as_str() == id)
        .map(|c| c.id.as_str().to_string())
        .collect();
    let pricing_refs: Vec<String> = doc
        .pricing
        .entries
        .iter()
        .enumerate()
        .filter(|(_, e)| e.r#match.credential.as_ref().is_some_and(|c| c.as_str() == id))
        .map(|(i, _)| format!("定价条目 #{i}"))
        .collect();

    if !force {
        let mut refs: Vec<String> = Vec::new();
        if !ch_ids.is_empty() {
            refs.push(format!(
                "{} 条通道（{}）",
                ch_ids.len(),
                ch_ids.iter().cloned().collect::<Vec<_>>().join(", ")
            ));
        }
        refs.extend(pricing_refs);
        if !refs.is_empty() {
            return Err(referenced("凭据", &refs));
        }
    }

    doc.credentials.retain(|c| c.id.as_str() != id);
    let emptied = prune_channels(&mut doc, &ch_ids);
    doc.channels.retain(|c| c.credential.as_str() != id);
    prune_group_refs(&mut doc, &emptied);
    doc.pricing
        .entries
        .retain(|e| !e.r#match.credential.as_ref().is_some_and(|c| c.as_str() == id));

    save_validated(state, doc, "credential_delete").await
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn credential_delete(
    state: State<'_, AppState>,
    id: String,
    force: bool,
) -> IpcResult<()> {
    delete_credential(&state, &id, force).await
}

/// `credential_set_enabled`：只翻转一个凭据的启用状态。
pub async fn set_credential_enabled(state: &AppState, id: &str, enabled: bool) -> IpcResult<()> {
    let mut doc = (*state.doc()).clone();
    match doc.credentials.iter_mut().find(|c| c.id.as_str() == id) {
        Some(slot) => slot.enabled = enabled,
        None => return Err(not_found("凭据", id)),
    }
    save_validated(state, doc, "credential_set_enabled").await
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn credential_set_enabled(
    state: State<'_, AppState>,
    id: String,
    enabled: bool,
) -> IpcResult<()> {
    set_credential_enabled(&state, &id, enabled).await
}

// ================================================================ credential_set_secret

/// `credential_set_secret` 的实现体：**注入式 store**，测试用 `ta_secrets::mock::MockStore`，
/// 生产用 [`default_secret_store`] —— 测试绝不碰真钥匙串。
///
/// 硬约束（本工作包铁律 1）：
/// * **绝不回显明文**（返回值是 `()`，错误消息里也不带值）；
/// * **绝不打日志**（`tracing` 里只出现凭据 id 与**脱敏后的 hint**）；
/// * **不进配置文件**（`credential.secret` 仍是 `${secret:name}` 引用，明文只进 `ta-secrets`）；
/// * 明文只经 `ta_secrets::SecretString` 的路径？—— 这里不必构造 `SecretString`：
///   store 的 `set` 收 `&str`，我们**不把它交给任何会格式化它的东西**。
pub async fn set_credential_secret_with(
    state: &AppState,
    store: &dyn ta_secrets::SecretStore,
    id: &str,
    plaintext: &str,
) -> IpcResult<()> {
    let doc = state.doc();
    let Some(cred) = doc.credentials.iter().find(|c| c.id.as_str() == id) else {
        return Err(not_found("凭据", id));
    };

    if plaintext.trim().is_empty() {
        return Err(IpcError::from(TaError::Secret(
            "要写入的密钥为空（空 key 会让这条凭据永远 401）".to_string(),
        )));
    }

    // `credential.secret` 决定条目名。明文/写坏的引用在这里被 `ta-secrets` 拦下
    // （`TaError::PlaintextSecret` TA-5003 / `Secret`），**错误消息不回显引用原文**。
    let name = match ta_secrets::resolve_ref(&cred.secret, store) {
        Ok(ta_secrets::SecretRef::Keyring { name }) => name,
        Ok(ta_secrets::SecretRef::Env { var }) => {
            return Err(IpcError::from(TaError::Secret(format!(
                "该凭据用的是环境变量引用（{var}）：环境变量是只读来源，写不进去。\
                 要在这里存 key，请先把 credential.secret 改成 ${{secret:条目名}}"
            ))))
        }
        Err(e) => return Err(IpcError::from(e)),
    };

    store.set(&name, plaintext).await?;
    // 元数据只留脱敏提示（表里根本没有 value 列）：`sk-a***` 这种。
    let hint = ta_secrets::mask_secret(plaintext);
    state.secret_meta.upsert(&name, &hint, state.clock.now_ms()).await?;

    // 日志里**只有条目名与脱敏提示**，没有值。
    tracing::info!(credential_id = id, entry = %name, hint = %hint, "凭据密钥已写入密钥仓库");
    Ok(())
}

/// 生产链：**钥匙串 → 便携文件 → 环境变量**（与 `gateway::SecretsAdapter::with_defaults`
/// 同一条链、同一套降级：钥匙串不可用时降到"便携文件 + 环境变量"并留 error 日志，
/// 而不是退回空实现 —— 空实现会让用户看到"我的 key 坏了"，与真因（钥匙串打不开）完全不同）。
fn default_secret_store() -> ta_secrets::CompositeStore {
    match ta_secrets::CompositeStore::with_defaults() {
        Ok(store) => store,
        Err(e) => {
            tracing::error!(
                code = e.code(),
                error = %e,
                "密钥优先级链初始化失败，降级为环境变量与便携文件"
            );
            let portable = ta_secrets::PortableStore::open(
                ta_secrets::portable::default_path()
                    .unwrap_or_else(|_| std::path::PathBuf::from("secrets.enc")),
            );
            ta_secrets::CompositeStore::portable_mode(portable)
        }
    }
}

/// `credential_set_secret`：把明文写进 `ta-secrets` 的密钥仓库（**不入库**）。
#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn credential_set_secret(
    state: State<'_, AppState>,
    id: String,
    plaintext: String,
) -> IpcResult<()> {
    let store = default_secret_store();
    set_credential_secret_with(&state, &store, &id, &plaintext).await
}

// ================================================================ credential_clear_quota

/// `credential_clear_quota`：用户手动"我充值了 / 这是新周期"。
///
/// 走既有账本（`ta_core::quota::QuotaLedgerImpl` + `QuotaObservation::ManualReset`）：
/// 清计数、解除 hard_exhausted 与 invalid、把状态推到 `ok`，然后**照常落库**
/// （`SqliteQuotaBackend` 的内存快照立刻可见，后台任务写 `quota_state` 表）。
pub async fn clear_credential_quota(state: &AppState, id: &str) -> IpcResult<QuotaState> {
    if !state.doc().credentials.iter().any(|c| c.id.as_str() == id) {
        return Err(not_found("凭据", id));
    }
    // C 源（手工额度）的真源是配置表：配置刚被改过时先把快照刷一遍，
    // 否则 `recompute` 会拿旧的 limit/threshold 算这次清零后的状态。
    state.quota.refresh_quota_configs(&state.db).await?;

    let ledger =
        ta_core::quota::QuotaLedgerImpl::with_clock(state.quota.clone(), state.clock.clone());
    let cred = ta_core::types::CredentialId::new(id);
    ta_core::quota::QuotaLedger::observe(
        &ledger,
        &cred,
        ta_core::quota::QuotaObservation::ManualReset,
    );
    Ok(QuotaState::from_core(&ta_core::quota::QuotaLedger::state(&ledger, &cred)))
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn credential_clear_quota(
    state: State<'_, AppState>,
    id: String,
) -> IpcResult<QuotaState> {
    clear_credential_quota(&state, &id).await
}

// ================================================================ channel

/// `channel_delete`（契约里**没有** `force`）。
///
/// 删除的判据分两类，**不静默改路由**：
///
/// * **死引用 → 顺手摘掉**（不改任何路由语义）：策略 `exclude_channels` 里点名的通道
///   （排除一个已不存在的通道本来就是 no-op）、以及"缩水后仍有成员"的通道组里的成员位。
/// * **会改路由 → 拒绝**（`TA-5002`）：策略 `use_channels` / `pin_sticky` **点名选中**了它，
///   或者某个策略通过一个**只剩它一个成员**的通道组依赖它（删掉通道 → 组空 → 组必须删 →
///   策略的 `use_group` 只能清空，那就是实打实的路由变化）。
///
/// 拒绝时消息里列出**具体是哪条策略的哪个字段**，用户先改策略再删。
pub async fn delete_channel(state: &AppState, id: &str) -> IpcResult<()> {
    let mut doc = (*state.doc()).clone();
    if !doc.channels.iter().any(|c| c.id.as_str() == id) {
        return Err(not_found("通道", id));
    }

    // 删掉它之后会变成空组的那些组（成员列表里只有它）
    let would_empty: BTreeSet<String> = doc
        .channel_groups
        .iter()
        .filter(|g| {
            g.members.iter().any(|m| m.as_str() == id) && g.members.iter().all(|m| m.as_str() == id)
        })
        .map(|g| g.id.as_str().to_string())
        .collect();

    let mut blocking: Vec<String> = Vec::new();
    for p in doc.policies.iter() {
        if p.action.use_channels.iter().any(|c| c.as_str() == id) {
            blocking.push(format!("策略 {} 的 action.use_channels", p.id));
        }
        if p.action.pin_sticky.as_ref().is_some_and(|c| c.as_str() == id) {
            blocking.push(format!("策略 {} 的 action.pin_sticky", p.id));
        }
        if let Some(g) = p.action.use_group.as_ref().filter(|g| would_empty.contains(g.as_str())) {
            blocking.push(format!("策略 {} 通过通道组 {g} 依赖它（该组只有这一个成员）", p.id));
        }
    }
    if !blocking.is_empty() {
        return Err(referenced("通道", &blocking));
    }

    let removed: BTreeSet<String> = [id.to_string()].into_iter().collect();
    let emptied = prune_channels(&mut doc, &removed);
    prune_group_refs(&mut doc, &emptied);
    doc.channels.retain(|c| c.id.as_str() != id);

    save_validated(state, doc, "channel_delete").await
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn channel_delete(state: State<'_, AppState>, id: String) -> IpcResult<()> {
    delete_channel(&state, &id).await
}

/// `channel_set_tags`：对一批通道做**增删标签**（`add` 去重追加、`remove` 先摘掉）。
///
/// 一个 id 不存在就整体报错（不做半成功）。两类清单都空也是错误：
/// "什么都没改但报成功"是用户最没法排查的一类结果。
pub async fn set_channel_tags(
    state: &AppState,
    channel_ids: &[String],
    add: &[String],
    remove: &[String],
) -> IpcResult<ApplyResult> {
    if channel_ids.is_empty() {
        return Err(empty_list("channel_ids"));
    }
    if add.is_empty() && remove.is_empty() {
        return Err(empty_list("add / remove"));
    }
    let mut doc = (*state.doc()).clone();
    let missing: Vec<String> = channel_ids
        .iter()
        .filter(|id| !doc.channels.iter().any(|c| c.id.as_str() == id.as_str()))
        .cloned()
        .collect();
    if !missing.is_empty() {
        return Err(not_found("通道", &missing.join(", ")));
    }

    for id in channel_ids {
        if let Some(c) = doc.channels.iter_mut().find(|c| c.id.as_str() == id.as_str()) {
            c.tags.retain(|t| !remove.iter().any(|r| r == t));
            for t in add {
                if !c.tags.iter().any(|x| x == t) {
                    c.tags.push(t.clone());
                }
            }
        }
    }
    save_validated(state, doc, "channel_set_tags").await?;
    Ok(applied())
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn channel_set_tags(
    state: State<'_, AppState>,
    channel_ids: Vec<String>,
    add: Vec<String>,
    remove: Vec<String>,
) -> IpcResult<ApplyResult> {
    set_channel_tags(&state, &channel_ids, &add, &remove).await
}

/// `channel_bulk_assign_lane`：把一批通道**钉到同一条线路**上。
///
/// `lane_id` 必须是 `doc.lanes` 里真实存在的线路：写一个不存在的 id 会让校验器
/// 以 `channels[i].lane not_found` 拒绝整份文档 —— 那是个"看起来成功、其实哪条都没改"
/// 的坑，所以这里先给出**指名道姓**的报错。
pub async fn bulk_assign_lane(
    state: &AppState,
    channel_ids: &[String],
    lane_id: &str,
) -> IpcResult<ApplyResult> {
    if channel_ids.is_empty() {
        return Err(empty_list("channel_ids"));
    }
    let mut doc = (*state.doc()).clone();
    if !doc.lanes.iter().any(|l| l.id.as_str() == lane_id) {
        return Err(not_found("线路", lane_id));
    }
    let missing: Vec<String> = channel_ids
        .iter()
        .filter(|id| !doc.channels.iter().any(|c| c.id.as_str() == id.as_str()))
        .cloned()
        .collect();
    if !missing.is_empty() {
        return Err(not_found("通道", &missing.join(", ")));
    }

    for id in channel_ids {
        if let Some(c) = doc.channels.iter_mut().find(|c| c.id.as_str() == id.as_str()) {
            c.lane = Some(ta_core::types::LaneId::new(lane_id));
        }
    }
    save_validated(state, doc, "channel_bulk_assign_lane").await?;
    Ok(applied())
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn channel_bulk_assign_lane(
    state: State<'_, AppState>,
    channel_ids: Vec<String>,
    lane_id: String,
) -> IpcResult<ApplyResult> {
    bulk_assign_lane(&state, &channel_ids, &lane_id).await
}

// ---------------------------------------------------------------- channel_matrix

/// `MatrixView.models[]`（`bindings.ts`）。
#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct ModelRow {
    pub model_id: String,
    /// `Model` 结构里**没有**独立的显示名字段（只有 `display.group` / `display.order`），
    /// 所以这里如实用 `id` —— 不给一个前端会当成"用户起的名字"的假值。
    pub display_name: String,
    /// 没有任何一条定价条目/内置价覆盖它（且它至少有一条通道时才算，见 [`channel_matrix`]）
    #[serde(default)]
    pub unpriced: bool,
}

/// `MatrixView.credentials[]`。
#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct CredentialCol {
    pub credential_id: String,
    pub label: String,
    pub provider_name: String,
    pub quota: QuotaState,
}

/// `MatrixView.cells[]`。一个通道一行。
#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct Cell {
    /// **规范模型 id**（`channel.model` 可能写的是别名，这里解析回 id 以便与 `models[]` 对齐）
    pub model_id: String,
    pub credential_id: String,
    #[serde(default)]
    pub channel_id: Option<String>,
    /// 生效线路（`channel.lane ?? credential.lane ?? provider.lane ?? net.default_lane`）；
    /// 全都没配 → `None`（= 直连，见 `domain::behavior::resolve_lane`）
    #[serde(default)]
    pub lane_id: Option<String>,
    #[serde(default)]
    pub upstream_model: Option<String>,
    #[serde(default)]
    pub priority: Option<i32>,
    #[serde(default)]
    pub weight: Option<u32>,
    #[serde(default)]
    pub tags: Vec<String>,
    pub enabled: bool,
}

/// `MatrixView`（`bindings.ts` / [10 §2.2]）。
#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct MatrixView {
    pub models: Vec<ModelRow>,
    pub credentials: Vec<CredentialCol>,
    pub cells: Vec<Cell>,
}

/// `channel_matrix`：模型 × 凭据 × 通道的**只读**视图（纯计算，不查业务表之外的存储）。
///
/// 数据全部来自已有的真源：配置热读快照（模型/通道/凭据/线路）+ 额度行
/// （`usage.list_quota_states`）+ **同一个**定价引擎（`PricingEngineImpl`，
/// 判定"未定价"用的是引擎自己那份口径，不是这里另写一套）。
pub async fn build_channel_matrix(state: &AppState) -> IpcResult<MatrixView> {
    let doc = state.doc();
    let quota_rows = state.usage.list_quota_states().await.unwrap_or_default();
    let engine = ta_core::pricing::PricingEngineImpl::new(&doc.pricing);

    let mut models: Vec<ModelRow> = Vec::with_capacity(doc.models.len());
    for m in doc.models.iter() {
        let chans: Vec<&ChannelConfig> =
            doc.channels.iter().filter(|c| m.matches(&c.model)).collect();
        // 没有任何通道的模型不打"未定价"标：它现在不参与计费，标红了只会成为噪音
        // （与校验器 `unpriced_model` 警告的口径一致：只对"有通道"的模型提醒）。
        let unpriced = !chans.is_empty()
            && chans
                .iter()
                .all(|c| !engine.resolve(&c.model, &c.provider, &c.credential).is_priced());
        models.push(ModelRow { model_id: m.id.clone(), display_name: m.id.clone(), unpriced });
    }

    let credentials: Vec<CredentialCol> = doc
        .credentials
        .iter()
        .map(|c| {
            let provider_name = doc
                .providers
                .iter()
                .find(|p| p.id == c.provider)
                .map(|p| p.name.clone())
                // 服务商不存在是配置错误（校验器会报），这里如实回 id 而不是编一个名字
                .unwrap_or_else(|| c.provider.as_str().to_string());
            let quota = match quota_rows.iter().find(|r| r.credential_id == c.id.as_str()) {
                Some(row) => QuotaState::from_row(row),
                // 从未观测过：全 0 + `unknown`（不是"可用"也不是"耗尽"）
                None => QuotaState {
                    credential_id: c.id.as_str().to_string(),
                    state: "unknown".to_string(),
                    used_tokens: 0,
                    used_requests: 0,
                    used_cost: 0.0,
                    reported_remaining: None,
                    window_start_ms: 0,
                    window_end_ms: None,
                    updated_at_ms: 0,
                },
            };
            CredentialCol {
                credential_id: c.id.as_str().to_string(),
                label: c.label.clone(),
                provider_name,
                quota,
            }
        })
        .collect();

    let mut cells: Vec<Cell> = Vec::with_capacity(doc.channels.len());
    for ch in doc.channels.iter() {
        let canon =
            doc.find_model(&ch.model).map(|m| m.id.clone()).unwrap_or_else(|| ch.model.clone());
        let cred = doc.credentials.iter().find(|c| c.id == ch.credential);
        let provider = doc.providers.iter().find(|p| p.id == ch.provider);
        let lane_id = ch.effective_lane(cred, provider, &doc.net).map(|l| l.as_str().to_string());
        cells.push(Cell {
            model_id: canon,
            credential_id: ch.credential.as_str().to_string(),
            channel_id: Some(ch.id.as_str().to_string()),
            lane_id,
            upstream_model: Some(ch.upstream_model.clone()),
            priority: Some(ch.priority),
            weight: Some(ch.weight),
            tags: ch.tags.clone(),
            enabled: ch.enabled,
        });
    }

    Ok(MatrixView { models, credentials, cells })
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn channel_matrix(state: State<'_, AppState>) -> IpcResult<MatrixView> {
    build_channel_matrix(&state).await
}

// ================================================================ group

/// `group_upsert`：同 id 覆盖、不存在则新增（幂等）。
pub async fn upsert_group(state: &AppState, group: ChannelGroup) -> IpcResult<ChannelGroup> {
    let mut doc = (*state.doc()).clone();
    match doc.channel_groups.iter_mut().find(|x| x.id == group.id) {
        Some(slot) => *slot = group.clone(),
        None => doc.channel_groups.push(group.clone()),
    }
    save_validated(state, doc, "group_upsert").await?;
    Ok(group)
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn group_upsert(
    state: State<'_, AppState>,
    group: ChannelGroup,
) -> IpcResult<ChannelGroup> {
    upsert_group(&state, group).await
}

/// `group_delete`（契约里**没有** `force`）：策略正在用它 → **拒绝**。
///
/// 为什么这里不学 `channel_delete` 顺手把 `use_group` 清成 `None`：那是一次**路由语义变化**
/// （"用这个组" → "不限定组"），不是删掉一个死引用。契约又没给 `force`，
/// 所以只能拒绝并列出用了它的策略，用户改完策略再删。
pub async fn delete_group(state: &AppState, id: &str) -> IpcResult<()> {
    let mut doc = (*state.doc()).clone();
    if !doc.channel_groups.iter().any(|g| g.id.as_str() == id) {
        return Err(not_found("通道组", id));
    }
    let users: Vec<String> = doc
        .policies
        .iter()
        .filter(|p| p.action.use_group.as_ref().is_some_and(|g| g.as_str() == id))
        .map(|p| format!("策略 {} 的 action.use_group", p.id))
        .collect();
    if !users.is_empty() {
        return Err(referenced("通道组", &users));
    }

    doc.channel_groups.retain(|g| g.id.as_str() != id);
    save_validated(state, doc, "group_delete").await
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn group_delete(state: State<'_, AppState>, id: String) -> IpcResult<()> {
    delete_group(&state, &id).await
}

/// `group_set_sticky`：替换某个通道组的粘性配置（`enabled` / `ttl_h` / `key`）。
pub async fn set_group_sticky(
    state: &AppState,
    id: &str,
    sticky: ta_core::domain::StickyConfig,
) -> IpcResult<()> {
    let mut doc = (*state.doc()).clone();
    match doc.channel_groups.iter_mut().find(|g| g.id.as_str() == id) {
        Some(slot) => slot.sticky = sticky,
        None => return Err(not_found("通道组", id)),
    }
    save_validated(state, doc, "group_set_sticky").await
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn group_set_sticky(
    state: State<'_, AppState>,
    id: String,
    sticky: ta_core::domain::StickyConfig,
) -> IpcResult<()> {
    set_group_sticky(&state, &id, sticky).await
}

/// `group_release_sticky`：清粘性。给了 `key` 只解绑那一条，否则**清掉整个组**的绑定。
///
/// 用既有的 `SqliteStickyStore`（`release` / `release_group`），不自己碰 `sticky` 表。
/// 组不存在 → 报错（不静默成功）：删掉的组不会有粘性，"清一个不存在的组的粘性"是调用方写错了。
pub async fn release_group_sticky(
    state: &AppState,
    group_id: &str,
    key: Option<String>,
) -> IpcResult<()> {
    if !state.doc().channel_groups.iter().any(|g| g.id.as_str() == group_id) {
        return Err(not_found("通道组", group_id));
    }
    let group = ChannelGroupId::new(group_id);
    let removed = match key {
        Some(k) => state.sticky.release(&StickyKey::new(k), &group).await?,
        None => state.sticky.release_group(&group).await?,
    };
    tracing::info!(group_id, released = removed, "已清理会话粘性绑定");
    Ok(())
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn group_release_sticky(
    state: State<'_, AppState>,
    group_id: String,
    key: Option<String>,
) -> IpcResult<()> {
    release_group_sticky(&state, &group_id, key).await
}

// ================================================================ model

/// `ModelPatch`（`bindings.ts`：`Partial<Pick<Model, "expose" | "aliases" | "display">>`）。
///
/// 三个字段都是 `Option`：**没给的字段不动**（`null`/缺省都不等于"清空"）。
#[derive(Debug, Clone, Default, Serialize, Deserialize, specta::Type)]
pub struct ModelPatch {
    #[serde(default)]
    pub expose: Option<bool>,
    #[serde(default)]
    pub aliases: Option<Vec<String>>,
    #[serde(default)]
    pub display: Option<ta_core::domain::DisplayConfig>,
}

/// `model_upsert`：同 id 覆盖、不存在则新增（幂等）。
pub async fn upsert_model(state: &AppState, model: Model) -> IpcResult<Model> {
    let mut doc = (*state.doc()).clone();
    match doc.models.iter_mut().find(|x| x.id == model.id) {
        Some(slot) => *slot = model.clone(),
        None => doc.models.push(model.clone()),
    }
    save_validated(state, doc, "model_upsert").await?;
    Ok(model)
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn model_upsert(state: State<'_, AppState>, model: Model) -> IpcResult<Model> {
    upsert_model(&state, model).await
}

/// `model_delete`：无引用直接删；有引用必须 `force=true`。
///
/// `force` 时的级联：删掉挂在它下面的**全部通道**（`channel.model` 可以写 id 或任一别名）、
/// 删掉以它为 model 的**通道组**（通道组的 `model` 悬空是校验错误），
/// 再摘掉策略里对已删通道/组的引用。
///
/// **定价条目不动**：校验器不要求 `pricing.entries[].match.model` 指向存在的模型
/// （它是 glob），照着删会误删一条通配条目。删模型后这些条目自然变成不命中的死条目，
/// 用户可以在成本中心看到并自行清理。
pub async fn delete_model(state: &AppState, id: &str, force: bool) -> IpcResult<()> {
    let mut doc = (*state.doc()).clone();
    let Some(model) = doc.models.iter().find(|m| m.id == id).cloned() else {
        return Err(not_found("模型", id));
    };
    let names: BTreeSet<String> = model.names().map(|n| n.to_string()).collect();

    let ch_ids: BTreeSet<String> = doc
        .channels
        .iter()
        .filter(|c| names.contains(c.model.as_str()))
        .map(|c| c.id.as_str().to_string())
        .collect();
    let group_ids: Vec<String> = doc
        .channel_groups
        .iter()
        .filter(|g| names.contains(g.model.as_str()))
        .map(|g| g.id.as_str().to_string())
        .collect();

    if !force {
        let mut refs: Vec<String> = Vec::new();
        if !ch_ids.is_empty() {
            refs.push(format!(
                "{} 条通道（{}）",
                ch_ids.len(),
                ch_ids.iter().cloned().collect::<Vec<_>>().join(", ")
            ));
        }
        if !group_ids.is_empty() {
            refs.push(format!("{} 个通道组（{}）", group_ids.len(), group_ids.join(", ")));
        }
        if !refs.is_empty() {
            return Err(referenced("模型", &refs));
        }
    }

    doc.models.retain(|m| m.id != id);
    let mut removed_groups = prune_channels(&mut doc, &ch_ids);
    doc.channels.retain(|c| !names.contains(c.model.as_str()));
    for g in doc.channel_groups.iter().filter(|g| names.contains(g.model.as_str())) {
        removed_groups.insert(g.id.as_str().to_string());
    }
    doc.channel_groups.retain(|g| !names.contains(g.model.as_str()));
    prune_group_refs(&mut doc, &removed_groups);

    save_validated(state, doc, "model_delete").await
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn model_delete(state: State<'_, AppState>, id: String, force: bool) -> IpcResult<()> {
    delete_model(&state, &id, force).await
}

/// `model_bulk_update`：对一批模型打同一个补丁（`patch` 里没给的字段保持原样）。
///
/// **要么全改、要么全不改**：任何一个 id 不存在就整体报错（不做"跳过不存在的"那种
/// 静默半成功 —— 那会让用户以为改上了）。
pub async fn bulk_update_models(
    state: &AppState,
    ids: &[String],
    patch: &ModelPatch,
) -> IpcResult<ApplyResult> {
    if ids.is_empty() {
        return Err(empty_list("ids"));
    }
    let mut doc = (*state.doc()).clone();
    let missing: Vec<String> =
        ids.iter().filter(|id| !doc.models.iter().any(|m| m.id == **id)).cloned().collect();
    if !missing.is_empty() {
        return Err(not_found("模型", &missing.join(", ")));
    }

    for id in ids {
        if let Some(m) = doc.models.iter_mut().find(|m| m.id == *id) {
            if let Some(expose) = patch.expose {
                m.expose = expose;
            }
            if let Some(aliases) = &patch.aliases {
                m.aliases = aliases.clone();
            }
            if let Some(display) = &patch.display {
                m.display = display.clone();
            }
        }
    }
    save_validated(state, doc, "model_bulk_update").await?;
    Ok(applied())
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn model_bulk_update(
    state: State<'_, AppState>,
    ids: Vec<String>,
    patch: ModelPatch,
) -> IpcResult<ApplyResult> {
    bulk_update_models(&state, &ids, &patch).await
}

// ================================================================ policy

/// `policy_upsert`：同 id 覆盖、不存在则新增（幂等）。
///
/// `priority` 数字小的先求值、首个命中即生效（[02 §2.8]）——那一列由
/// [`reorder_policies`] 负责，这里不动它（用户手填的优先级要原样保留）。
pub async fn upsert_policy(state: &AppState, policy: Policy) -> IpcResult<Policy> {
    let mut doc = (*state.doc()).clone();
    match doc.policies.iter_mut().find(|x| x.id == policy.id) {
        Some(slot) => *slot = policy.clone(),
        None => doc.policies.push(policy.clone()),
    }
    save_validated(state, doc, "policy_upsert").await?;
    Ok(policy)
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn policy_upsert(state: State<'_, AppState>, policy: Policy) -> IpcResult<Policy> {
    upsert_policy(&state, policy).await
}

/// `policy_delete`：删一条策略。
///
/// 没有任何东西引用策略 id（`PolicyAction` 引用的都是**通道/通道组/线路**），
/// 所以不需要 `force`、也没有级联。
pub async fn delete_policy(state: &AppState, id: &str) -> IpcResult<()> {
    let mut doc = (*state.doc()).clone();
    if !doc.policies.iter().any(|p| p.id.as_str() == id) {
        return Err(not_found("策略", id));
    }
    doc.policies.retain(|p| p.id.as_str() != id);
    save_validated(state, doc, "policy_delete").await
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn policy_delete(state: State<'_, AppState>, id: String) -> IpcResult<()> {
    delete_policy(&state, &id).await
}

/// `policy_reorder`：按 `ordered_ids` 的顺序**重写全部** priority。
///
/// 两条约定（都不是发明的错误码，是把"什么算合法请求"写清楚）：
///
/// 1. `ordered_ids` 必须是**当前全部策略 id 的一个排列** —— 少了、多了、重复了都报
///    `TA-1004` 并列出差在哪。允许"只排一部分"会得到一个**其余策略顺序取决于声明顺序**
///    的结果（`config_store` 里 `ord` 列还在，但那不是用户看到的那张表），
///    而那正是这个命令要消灭的不确定性。
/// 2. 新 priority 是 `(下标 + 1) * 10`（10 / 20 / 30 …）：留出插入空隙，
///    并且**天然不会撞号**（同优先级会让求值顺序取决于声明顺序，校验器为此专门告警）。
pub async fn reorder_policies(state: &AppState, ordered_ids: &[String]) -> IpcResult<()> {
    let mut doc = (*state.doc()).clone();
    let current: BTreeSet<String> =
        doc.policies.iter().map(|p| p.id.as_str().to_string()).collect();
    let mut given: BTreeSet<String> = BTreeSet::new();
    let mut dup: Vec<String> = Vec::new();
    for id in ordered_ids {
        if !given.insert(id.clone()) {
            dup.push(id.clone());
        }
    }
    let unknown: Vec<String> = given.difference(&current).cloned().collect();
    let missing: Vec<String> = current.difference(&given).cloned().collect();
    if !dup.is_empty() || !unknown.is_empty() || !missing.is_empty() {
        let mut detail: Vec<String> = Vec::new();
        if !dup.is_empty() {
            detail.push(format!("重复给了：{}", dup.join(", ")));
        }
        if !unknown.is_empty() {
            detail.push(format!("不存在的策略：{}", unknown.join(", ")));
        }
        if !missing.is_empty() {
            detail.push(format!("漏掉了：{}", missing.join(", ")));
        }
        return Err(IpcError::from(TaError::MalformedRequest(format!(
            "ordered_ids 必须是当前全部策略 id 的一个排列（{}）；{}",
            current.len(),
            detail.join("；")
        ))));
    }

    let mut reordered: Vec<Policy> = Vec::with_capacity(ordered_ids.len());
    for (i, id) in ordered_ids.iter().enumerate() {
        let mut p = doc
            .policies
            .iter()
            .find(|p| p.id.as_str() == id.as_str())
            .cloned()
            .expect("上面已证明每个 id 都存在");
        p.priority = (i as i32 + 1) * 10;
        reordered.push(p);
    }
    doc.policies = reordered;
    save_validated(state, doc, "policy_reorder").await
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn policy_reorder(state: State<'_, AppState>, ordered_ids: Vec<String>) -> IpcResult<()> {
    reorder_policies(&state, &ordered_ids).await
}

/// `policy_set_enabled`：只翻转一条策略的启用状态（求值只看 `enabled` 的策略）。
pub async fn set_policy_enabled(state: &AppState, id: &str, enabled: bool) -> IpcResult<()> {
    let mut doc = (*state.doc()).clone();
    match doc.policies.iter_mut().find(|p| p.id.as_str() == id) {
        Some(slot) => slot.enabled = enabled,
        None => return Err(not_found("策略", id)),
    }
    save_validated(state, doc, "policy_set_enabled").await
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn policy_set_enabled(
    state: State<'_, AppState>,
    id: String,
    enabled: bool,
) -> IpcResult<()> {
    set_policy_enabled(&state, &id, enabled).await
}

// ---------------------------------------------------------------- policy_simulate

/// `SimulateInput`（[10 §2.2] / `bindings.ts`）。
///
/// 前四个字段在 TS 里是**必填**的，所以这里也不给 `#[serde(default)]` ——
/// 少传一个字段应当在反序列化时就失败，而不是被悄悄当成 `false` / `0`
/// （模拟器的全部价值就是"输入什么就按什么算"）。
#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct SimulateInput {
    pub model: String,
    pub has_image: bool,
    pub stream: bool,
    pub est_input_tokens: u64,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub session_key: Option<String>,
    #[serde(default)]
    pub now: Option<i64>,
}

impl SimulateInput {
    /// 转成 `ta-core` 的模拟器输入。
    ///
    /// ta-core 那一版还有四个"运行态注入"字段（quota_remaining / latency_ms /
    /// channel_state / lane_health，TS 契约里没有）——这里**不编**：留空 = 冷启动视角
    /// （与 `simulate_input` 的纯函数语义一致：同一输入必得同一结果）。
    fn to_core(&self) -> ta_core::routing::SimulateInput {
        ta_core::routing::SimulateInput {
            model: self.model.clone(),
            has_image: self.has_image,
            stream: self.stream,
            est_input_tokens: self.est_input_tokens,
            headers: self.headers.clone(),
            session_key: self.session_key.clone(),
            now: self.now,
            ..Default::default()
        }
    }
}

/// `SimulateResult.matched_policy`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, specta::Type)]
pub struct SimulatePolicyRef {
    pub id: String,
    pub name: String,
}

/// `SimulateResult.candidates[]`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, specta::Type)]
pub struct SimulateCandidate {
    pub channel_id: String,
    pub ok: bool,
    #[serde(default)]
    pub reason: Option<String>,
}

/// `SimulateResult.chosen`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, specta::Type)]
pub struct SimulateChosen {
    pub channel_id: String,
    pub sticky_hit: bool,
    #[serde(default)]
    pub sticky_key: Option<String>,
}

/// `SimulateResult.lane`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, specta::Type)]
pub struct SimulateLane {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub node: Option<String>,
    #[serde(default)]
    pub latency_ms: Option<u32>,
}

/// `SimulateResult.eliminated[]`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, specta::Type)]
pub struct SimulateEliminated {
    pub channel_id: String,
    pub reason: String,
}

/// `SimulateResult`（[10 §2.2] / `bindings.ts`）。
///
/// 为什么要一份自己的 DTO：`ta_core::routing::simulate::SimulateResult` **没有**
/// `specta::Type`（`ta-core` 只在契约类型上挂 `feature = "specta"`），
/// 直接当命令返回值会导不出 TS 绑定。字段与形状**逐字段对齐** `bindings.ts`，
/// 转换就是一次直译（[`SimulateResult::from_core`]）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, specta::Type)]
pub struct SimulateResult {
    #[serde(default)]
    pub matched_policy: Option<SimulatePolicyRef>,
    pub candidates: Vec<SimulateCandidate>,
    #[serde(default)]
    pub chosen: Option<SimulateChosen>,
    pub failover_chain: Vec<String>,
    #[serde(default)]
    pub lane: Option<SimulateLane>,
    /// 恒为 true（除 model / 鉴权头外，发出的请求与客户端逐字段一致）
    pub outgoing_equals_client: bool,
    pub eliminated: Vec<SimulateEliminated>,
}

impl SimulateResult {
    fn from_core(r: &ta_core::routing::SimulateResult) -> Self {
        Self {
            matched_policy: r
                .matched_policy
                .as_ref()
                .map(|p| SimulatePolicyRef { id: p.id.clone(), name: p.name.clone() }),
            candidates: r
                .candidates
                .iter()
                .map(|c| SimulateCandidate {
                    channel_id: c.channel_id.clone(),
                    ok: c.ok,
                    reason: c.reason.clone(),
                })
                .collect(),
            chosen: r.chosen.as_ref().map(|c| SimulateChosen {
                channel_id: c.channel_id.clone(),
                sticky_hit: c.sticky_hit,
                sticky_key: c.sticky_key.clone(),
            }),
            failover_chain: r.failover_chain.clone(),
            lane: r.lane.as_ref().map(|l| SimulateLane {
                id: l.id.clone(),
                name: l.name.clone(),
                node: l.node.clone(),
                latency_ms: l.latency_ms,
            }),
            outgoing_equals_client: r.outgoing_equals_client,
            eliminated: r
                .eliminated
                .iter()
                .map(|e| SimulateEliminated {
                    channel_id: e.channel_id.clone(),
                    reason: e.reason.clone(),
                })
                .collect(),
        }
    }
}

/// `policy_simulate`：**"如果现在来这样一个请求，会怎么走？"**
///
/// 计算全部在 `ta_core::routing::simulate::simulate_input`（它就是
/// `Router::decide` + `Router::select` 的同一条代码路径，且每次新建运行时状态
/// → **同一输入必得同一结果**）。命令层只做 DTO 直译，不重写任何判定。
pub async fn simulate_policy(state: &AppState, input: &SimulateInput) -> IpcResult<SimulateResult> {
    let doc = state.doc();
    let core = ta_core::routing::simulate_input(&doc, &input.to_core());
    Ok(SimulateResult::from_core(&core))
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn policy_simulate(
    state: State<'_, AppState>,
    input: SimulateInput,
) -> IpcResult<SimulateResult> {
    simulate_policy(&state, &input).await
}

// ================================================================ pricing

/// `ResolvedPrice`（[10 §2.2] / `bindings.ts`）。
///
/// `ta_core::pricing::ResolvedPrice` 没有 `specta::Type`，所以这里直译一份。
/// **刻意不暴露** `divisor` / `entry_index`：契约里没有它们，
/// 而且 `divisor` 是内部计价口径（费率已按 `unit` 归一），前端拿到只会误用。
#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct ResolvedPrice {
    pub rates: BTreeMap<String, f64>,
    pub multiplier: f64,
    pub currency: String,
    /// `builtin` | `manual` | `imported` | `none`（`none` = 未定价，成本记 0）
    pub source: String,
    #[serde(default)]
    pub plan_fee: Option<ta_core::domain::PlanFee>,
}

/// 把解析结果里的套餐费还原成契约形状的 `PlanFee`。
///
/// `PlanFeeResolved` 只有 `amount` / `currency`（没有 `window` —— 窗口是配置条目上的字段）。
/// 所以窗口从**命中的那条配置条目**取（`entry_index`）；取不到时用
/// `PlanFee::default()` 的窗口（月）——**只在这一个不一致场景下**才会走到。
fn plan_fee_of(
    doc: &ConfigDoc,
    fee: &ta_core::pricing::PlanFeeResolved,
    entry_index: i64,
) -> ta_core::domain::PlanFee {
    let window = usize::try_from(entry_index)
        .ok()
        .and_then(|i| doc.pricing.entries.get(i))
        .and_then(|e| e.plan_fee.as_ref())
        .map(|f| f.window)
        .unwrap_or_default();
    ta_core::domain::PlanFee {
        amount: fee.amount,
        currency: fee.currency.clone(),
        window,
        ..Default::default()
    }
}

fn source_str(kind: ta_core::pricing::PriceSourceKind) -> &'static str {
    use ta_core::pricing::PriceSourceKind as K;
    match kind {
        K::Manual => "manual",
        K::Imported => "imported",
        K::Builtin => "builtin",
        K::None => "none",
    }
}

fn resolved_to_dto(doc: &ConfigDoc, p: &ta_core::pricing::ResolvedPrice) -> ResolvedPrice {
    ResolvedPrice {
        rates: p.rates.iter().map(|(k, v)| (k.as_key().to_string(), *v)).collect(),
        multiplier: p.multiplier,
        currency: p.currency.clone(),
        source: source_str(p.source).to_string(),
        plan_fee: p.plan_fee.as_ref().map(|f| plan_fee_of(doc, f, p.entry_index)),
    }
}

/// `pricing_list`：配置里的定价条目（**按声明顺序**，与引擎"同档内首个命中生效"一致）。
#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn pricing_list(
    state: State<'_, AppState>,
) -> IpcResult<Vec<ta_core::domain::PriceEntry>> {
    Ok(state.doc().pricing.entries.clone())
}

/// `pricing_upsert`：**按 `match` 判同一性**（定价条目没有 id）——
/// `match`（model / provider / credential）完全相同的条目被**覆盖**，否则追加。
///
/// 为什么不按下标：`pricing_delete(index)` 用的是下标，但下标会随别的增删漂移；
/// 让"改价"依赖前端记住的下标，就是让一次误标改错一条价。
pub async fn upsert_price(
    state: &AppState,
    entry: ta_core::domain::PriceEntry,
) -> IpcResult<ta_core::domain::PriceEntry> {
    let mut doc = (*state.doc()).clone();
    match doc.pricing.entries.iter_mut().find(|e| same_match(&e.r#match, &entry.r#match)) {
        Some(slot) => *slot = entry.clone(),
        None => doc.pricing.entries.push(entry.clone()),
    }
    save_validated(state, doc, "pricing_upsert").await?;
    Ok(entry)
}

fn same_match(a: &ta_core::domain::PriceMatch, b: &ta_core::domain::PriceMatch) -> bool {
    a.model == b.model && a.provider == b.provider && a.credential == b.credential
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn pricing_upsert(
    state: State<'_, AppState>,
    entry: ta_core::domain::PriceEntry,
) -> IpcResult<ta_core::domain::PriceEntry> {
    upsert_price(&state, entry).await
}

/// `pricing_delete`：按下标删一条（契约就是这么定的：`index: number`）。
///
/// 越界必须报错：静默忽略会得到"界面上少了一行、其实一条都没删"。
pub async fn delete_price(state: &AppState, index: usize) -> IpcResult<()> {
    let mut doc = (*state.doc()).clone();
    if index >= doc.pricing.entries.len() {
        return Err(IpcError::from(TaError::MalformedRequest(format!(
            "定价条目下标越界：{index}（当前只有 {} 条）",
            doc.pricing.entries.len()
        ))));
    }
    doc.pricing.entries.remove(index);
    save_validated(state, doc, "pricing_delete").await
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn pricing_delete(state: State<'_, AppState>, index: u32) -> IpcResult<()> {
    delete_price(&state, index as usize).await
}

/// `pricing_resolve`：按 **manual > imported > builtin（配置）> 内置静态表** 解析一条模型的价格。
///
/// `provider` / `credential` 缺省 = **不限定**（只命中不限定这两者的条目 + 内置表），
/// **不替调用方猜一个 provider 出来**：猜错会给出一个看似精确的错价。
pub async fn resolve_price(
    state: &AppState,
    model: &str,
    provider: Option<String>,
    credential: Option<String>,
) -> IpcResult<ResolvedPrice> {
    let doc = state.doc();
    let engine = ta_core::pricing::PricingEngineImpl::new(&doc.pricing);
    let provider = ta_core::types::ProviderId::new(provider.unwrap_or_default());
    let credential = ta_core::types::CredentialId::new(credential.unwrap_or_default());
    Ok(resolved_to_dto(&doc, &engine.resolve(model, &provider, &credential)))
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn pricing_resolve(
    state: State<'_, AppState>,
    model: String,
    provider: Option<String>,
    credential: Option<String>,
) -> IpcResult<ResolvedPrice> {
    resolve_price(&state, &model, provider, credential).await
}

/// `pricing_import_builtin`：把内置官方价表**拷进配置**，用户可以接着手改。
///
/// * `source` 用 `imported`（而不是 `builtin`）：引擎的 `imported` 档就是"导入的表"，
///   与内置静态表分属两档，用户能一眼看出这些价是从哪来的；
/// * **每条都写明 `unit: per_1m` 与 `currency: USD`**（`extra.unit` / `currency` 字段）：
///   内置表的数字是"每 1M token / 美元"，而配置的默认单位是用户可以改的
///   —— 不钉死这两项，用户把 `pricing.unit` 改成 per_1k 之后，导入的价会静默差 1000 倍；
/// * **已有同 `match.model` 的条目就跳过**：不覆盖用户已经手改过的价；
/// * 幂等：再点一次不会翻倍。
pub async fn import_builtin_prices(state: &AppState) -> IpcResult<ApplyResult> {
    let mut doc = (*state.doc()).clone();
    let existing: BTreeSet<String> =
        doc.pricing.entries.iter().filter_map(|e| e.r#match.model.clone()).collect();

    let mut added = 0usize;
    for bp in ta_core::pricing::builtin::BUILTIN_PRICES {
        if existing.contains(bp.pattern) {
            continue;
        }
        let mut entry = ta_core::domain::PriceEntry {
            r#match: ta_core::domain::PriceMatch::model(bp.pattern),
            rates: bp.rates.iter().map(|(k, v)| ((*k).to_string(), *v)).collect(),
            multiplier: 1.0,
            currency: Some(ta_core::pricing::builtin::BUILTIN_CURRENCY.to_string()),
            plan_fee: None,
            source: ta_core::domain::PriceSource::Imported,
            effective_from: None,
            extra: Default::default(),
        };
        entry.extra.insert("unit".to_string(), serde_json::json!("per_1m"));
        doc.pricing.entries.push(entry);
        added += 1;
    }
    tracing::info!(added, total = doc.pricing.entries.len(), "已导入内置价目表");
    save_validated(state, doc, "pricing_import_builtin").await?;
    Ok(applied())
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn pricing_import_builtin(state: State<'_, AppState>) -> IpcResult<ApplyResult> {
    import_builtin_prices(&state).await
}

// ================================================================ 测试

#[cfg(test)]
mod tests {
    use super::*;

    use ta_core::config::ConfigValidator;
    use ta_core::domain::{
        ChannelConfig, Model, Policy, PriceEntry, PriceMatch, PriceSource, Provider,
    };
    use ta_core::quota::QuotaBackend;
    use ta_core::types::{ChannelId, Protocol};
    use ta_secrets::mock::MockStore;
    use ta_secrets::SecretStore;
    use ta_store::ConfigStore;

    /// 一个**真的库**（临时目录里的 SQLite）。
    ///
    /// 写命令的测试必须真的落库再读回来 —— 只断言"函数返回 Ok"证明不了"写进去了"。
    async fn fresh_state(tag: &str) -> AppState {
        let dir = std::env::temp_dir().join(format!("ta-ent-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("建临时目录");
        AppState::bootstrap(dir.join("db.sqlite")).await.expect("开库")
    }

    /// 种一份**引用完整**的最小配置（校验器查引用完整性，缺一个就存不进去）。
    ///
    /// 形状：1 服务商 / 1 凭据 / 1 模型 / 1 通道 / 1 通道组 / 1 条默认策略 / 1 条定价。
    async fn seeded(tag: &str) -> AppState {
        let state = fresh_state(tag).await;
        let mut doc = ConfigDoc::default();
        doc.providers.push(Provider::new("p-a", "A", Protocol::Openai, "https://a.example/v1"));
        doc.credentials.push(Credential::new("c-a", "p-a", "A", "${secret:a}"));
        doc.models.push(Model::new("m-a"));
        let ch = ChannelConfig::new("ch-a", "m-a", "p-a", "c-a", "upstream-a");
        doc.channels.push(ch);
        doc.channel_groups.push(ChannelGroup::new("g-a", "m-a", ["ch-a"]));
        doc.policies.push(
            Policy::new("pol-a", "默认").with_match(ta_core::domain::PolicyMatch::match_all()),
        );
        let mut price = PriceEntry::new(PriceMatch::model("m-a").with_credential("c-a"));
        price.source = PriceSource::Manual;
        price.rates.insert("input".into(), 1.0);
        doc.pricing.entries.push(price);
        state.config.save(&doc).await.expect("种配置");
        state.set_doc(doc);
        state
    }

    fn cred_exists(state: &AppState, id: &str) -> bool {
        state.doc().credentials.iter().any(|c| c.id.as_str() == id)
    }

    /// 测试用：改一份配置并**走同一条写路径**落库（校验 → save → set_doc）。
    ///
    /// 存在的意义：本批测试要造出"策略点名用了某个通道/组"的前置条件，
    /// 而 `upsert_policy` 属于后面的批次 —— 直接用同一条写路径造前置，
    /// 比"先跑半截真实命令"更不容易把测试变成互相依赖。
    async fn rewrite_doc(state: &AppState, f: impl FnOnce(&mut ConfigDoc)) {
        let mut doc = (*state.doc()).clone();
        f(&mut doc);
        let report = ta_core::config::TaConfigValidator.validate(&doc);
        assert!(report.ok, "测试前置配置本身必须合法：{:?}", report.errors);
        state.config.save(&doc).await.expect("save");
        state.set_doc(doc);
    }

    #[tokio::test]
    async fn model_upsert_and_delete_cascade_like_the_docs_say() {
        let state = seeded("model-crud").await;

        // upsert 新增 + 读回
        let m2 = Model::new("m-b").with_aliases(["alias-b"]);
        upsert_model(&state, m2.clone()).await.expect("新增模型");
        let back = state.config.load().await.expect("load");
        assert!(back.models.iter().any(|m| m.id == "m-b"), "写进去的必须读得回来");

        // 同 id 覆盖：别名改掉，不新增
        let mut m2b = m2.clone();
        m2b.aliases = vec!["alias-b2".to_string()];
        upsert_model(&state, m2b).await.expect("覆盖");
        let back = state.config.load().await.expect("load");
        assert_eq!(back.models.iter().filter(|m| m.id == "m-b").count(), 1);
        assert_eq!(
            back.models.iter().find(|m| m.id == "m-b").map(|m| m.aliases.clone()),
            Some(vec!["alias-b2".to_string()])
        );

        // m-a 有通道 + 通道组引用 → 未 force 必须拒绝且不改配置
        let err = delete_model(&state, "m-a", false).await.expect_err("有引用必须拒绝");
        assert_eq!(err.code, "TA-5002");
        assert!(state.doc().models.iter().any(|m| m.id == "m-a"), "被拒绝时不许改配置");

        // force → 通道 + 通道组一起走，策略对组的引用摘掉
        delete_model(&state, "m-a", true).await.expect("force 级联");
        let after = state.config.load().await.expect("load");
        assert!(!after.models.iter().any(|m| m.id == "m-a"));
        assert!(after.channels.iter().all(|c| c.model != "m-a"), "挂载的通道必须一起删");
        assert!(
            after.channel_groups.iter().all(|g| g.model != "m-a"),
            "以它为 model 的组必须一起删"
        );
        assert!(after.policies.iter().all(|p| p.action.use_group.is_none()), "策略引用要摘掉");

        // 别名也能命中（"模型"的删除范围是 id + 全部别名）
        assert!(delete_model(&state, "m-nope", false).await.is_err());
    }

    #[tokio::test]
    async fn model_bulk_update_patches_all_or_nothing() {
        let state = seeded("model-bulk").await;
        upsert_model(&state, Model::new("m-b")).await.expect("加第二个模型");

        let patch = ModelPatch { expose: Some(false), aliases: None, display: None };
        let r =
            bulk_update_models(&state, &["m-a".into(), "m-b".into()], &patch).await.expect("批量");
        assert!(
            r.applied && !r.restart_required && !r.kernel_reload,
            "ApplyResult 语义要与 config_apply 一致"
        );
        let back = state.config.load().await.expect("load");
        for id in ["m-a", "m-b"] {
            assert!(!back.models.iter().find(|m| m.id == id).expect("在").expose);
        }

        // 只改一个：别的模型一个字段都不许动
        let one = ModelPatch { aliases: Some(vec!["alias-a".to_string()]), ..Default::default() };
        bulk_update_models(&state, &["m-a".into()], &one).await.expect("单改");
        let back = state.config.load().await.expect("load");
        assert_eq!(
            back.models.iter().find(|m| m.id == "m-a").map(|m| m.aliases.clone()),
            Some(vec!["alias-a".to_string()])
        );
        assert_eq!(
            back.models.iter().find(|m| m.id == "m-b").map(|m| m.aliases.is_empty()),
            Some(true),
            "没被点名的模型不许被动"
        );

        // 别名撞上另一个模型的 id → 校验器拒绝（TA-5001，字段路径精确到 models[i]），且不改配置。
        // 注意这里的 code 是 `duplicate_id` 而不是 `ambiguous_alias`：把 m-a 的别名设成 `m-b`
        // 时，索引里 `m-b` 先被 m-a 的别名占位，轮到 m-b 自己的 id 就撞成"id 重复"。
        // 两种 code 都是"解析结果不确定"，都属 TA-5001 —— 断言只钉"必须拒绝 + 带出路径"。
        let dup = ModelPatch { aliases: Some(vec!["m-b".to_string()]), ..Default::default() };
        let err =
            bulk_update_models(&state, &["m-a".into()], &dup).await.expect_err("撞名必须拒绝");
        assert_eq!(err.code, "TA-5001");
        let details = err.details.expect("details");
        let text = details.to_string();
        assert!(
            text.contains("duplicate_id") || text.contains("ambiguous_alias"),
            "要带出字段路径与 code：{text}"
        );
        assert!(text.contains("models[1]"), "字段路径要指向冲突的那个模型：{text}");
        assert_eq!(
            state.config.load().await.expect("load").models[0].aliases,
            vec!["alias-a".to_string()],
            "被拒绝的补丁不许落库"
        );

        // 有一个 id 不存在 → 整体不改（不是"改掉存在的那几个"）
        let err =
            bulk_update_models(&state, &["m-a".into(), "m-nope".into()], &ModelPatch::default())
                .await
                .expect_err("未知 id 必须整体失败");
        assert_eq!(err.code, "TA-1004");
        assert!(err.message.contains("m-nope"));
        // 空清单也是一个错误，不是"成功但什么都没做"
        assert!(bulk_update_models(&state, &[], &ModelPatch::default()).await.is_err());
    }

    #[tokio::test]
    async fn channel_delete_prunes_dead_refs_but_refuses_when_it_changes_routing() {
        let state = seeded("ch-del").await;
        // ① 策略点名用它（use_channels）→ 拒绝，且配置没被动
        rewrite_doc(&state, |d| {
            d.policies[0].action.use_channels = vec![ChannelId::new("ch-a")];
        })
        .await;
        let err = delete_channel(&state, "ch-a").await.expect_err("点名选中的通道不许删");
        assert_eq!(err.code, "TA-5002");
        assert!(err.message.contains("use_channels"), "{}", err.message);
        assert!(state.doc().channels.iter().any(|c| c.id.as_str() == "ch-a"));

        // ② 摘掉点名 → 但策略还通过"只剩 ch-a 一个成员"的组依赖它 → 仍然拒绝
        rewrite_doc(&state, |d| {
            d.policies[0].action.use_channels.clear();
            d.policies[0].action.use_group = Some(ChannelGroupId::new("g-a"));
        })
        .await;
        let err = delete_channel(&state, "ch-a").await.expect_err("组只剩它 → 不许删");
        assert_eq!(err.code, "TA-5002");
        assert!(err.message.contains("g-a"), "{}", err.message);

        // ③ 策略不再依赖组，只在 exclude_channels 里点名（死引用）→ 删得掉，且排除项被顺手摘掉
        rewrite_doc(&state, |d| {
            d.policies[0].action.use_group = None;
            d.policies[0].action.exclude_channels = vec![ChannelId::new("ch-a")];
        })
        .await;
        delete_channel(&state, "ch-a").await.expect("死引用 → 可以删");
        let after = state.config.load().await.expect("load");
        assert!(after.channels.is_empty());
        assert!(after.policies[0].action.exclude_channels.is_empty(), "对已删通道的排除项必须摘掉");
        assert!(after.channel_groups.is_empty(), "成员被删空 → 空组一起删");

        // ④ 未知 id
        assert_eq!(delete_channel(&state, "ch-nope").await.expect_err("未知 id").code, "TA-1004");
    }

    #[tokio::test]
    async fn channel_set_tags_and_bulk_lane_write_then_read_back() {
        let state = seeded("ch-bulk").await;
        // 种一条真实线路（通道的 lane 必须指向存在的线路，否则校验器拒绝）
        let mut doc = (*state.doc()).clone();
        doc.lanes.push(ta_core::domain::Lane::direct("lane-direct", "直连（本机网络）"));
        state.config.save(&doc).await.expect("种线路");
        state.set_doc(doc);

        let r = set_channel_tags(&state, &["ch-a".into()], &["vision".into(), "cheap".into()], &[])
            .await
            .expect("加标签");
        assert!(r.applied);
        // 重复 add 不产生重复项
        set_channel_tags(&state, &["ch-a".into()], &["cheap".into()], &["vision".into()])
            .await
            .expect("再去重");
        let back = state.config.load().await.expect("load");
        assert_eq!(back.channels[0].tags, vec!["cheap".to_string()], "写进去的必须读得回来");

        let r = bulk_assign_lane(&state, &["ch-a".into()], "lane-direct").await.expect("钉线路");
        assert!(r.applied);
        assert_eq!(
            state.config.load().await.expect("load").channels[0].lane.as_ref().map(|l| l.as_str()),
            Some("lane-direct")
        );

        // 不存在的线路 / 不存在的通道 / 空清单，都要如实报错且不写库
        let err =
            bulk_assign_lane(&state, &["ch-a".into()], "lane-nope").await.expect_err("线路不存在");
        assert_eq!(err.code, "TA-1004");
        assert!(bulk_assign_lane(&state, &["ch-nope".into()], "lane-direct").await.is_err());
        assert!(bulk_assign_lane(&state, &[], "lane-direct").await.is_err());
        assert!(set_channel_tags(&state, &["ch-nope".into()], &["x".into()], &[]).await.is_err());
        assert!(set_channel_tags(&state, &[], &["x".into()], &[]).await.is_err());
        assert!(
            set_channel_tags(&state, &["ch-a".into()], &[], &[]).await.is_err(),
            "两类清单都空 = 什么都没改"
        );
    }

    #[tokio::test]
    async fn group_crud_sticky_and_release() {
        let state = seeded("group-crud").await;
        // 策略正在用 g-a → 不许删
        rewrite_doc(&state, |d| {
            d.policies[0].action.use_group = Some(ChannelGroupId::new("g-a"));
        })
        .await;
        let err = delete_group(&state, "g-a").await.expect_err("策略在用 → 拒绝");
        assert_eq!(err.code, "TA-5002");
        assert!(err.message.contains("pol-a"), "{}", err.message);

        // 摘掉引用 → 删得掉
        rewrite_doc(&state, |d| d.policies[0].action.use_group = None).await;
        delete_group(&state, "g-a").await.expect("可以删");
        assert!(state.config.load().await.expect("load").channel_groups.is_empty());

        // upsert 覆盖（同 id 不新增）
        upsert_group(&state, ChannelGroup::new("g-b", "m-a", ["ch-a"])).await.expect("加组");
        let mut g = ChannelGroup::new("g-b", "m-a", ["ch-a"]);
        g.selector = ta_core::domain::Selector::Cheapest;
        upsert_group(&state, g).await.expect("覆盖");
        let back = state.config.load().await.expect("load");
        assert_eq!(back.channel_groups.len(), 1);
        assert_eq!(back.channel_groups[0].selector, ta_core::domain::Selector::Cheapest);

        // 粘性配置（落库可读）
        let sticky = ta_core::domain::StickyConfig {
            enabled: true,
            ttl_h: 12,
            key: vec!["header:x-session-id".to_string()],
            ..Default::default()
        };
        set_group_sticky(&state, "g-b", sticky.clone()).await.expect("设置粘性");
        let back = state.config.load().await.expect("load");
        assert!(back.channel_groups[0].sticky.enabled);
        assert_eq!(back.channel_groups[0].sticky.ttl_h, 12);

        // 真实粘性表：先写两条绑定，再按 key 解绑 / 整组清空
        let group = ChannelGroupId::new("g-b");
        let ch = ChannelId::new("ch-a");
        state.sticky.put(&StickyKey::new("k1"), &group, &ch, 60_000, 0).await.expect("put k1");
        state.sticky.put(&StickyKey::new("k2"), &group, &ch, 60_000, 0).await.expect("put k2");
        release_group_sticky(&state, "g-b", Some("k1".into())).await.expect("解绑一条");
        assert!(state.sticky.get(&StickyKey::new("k1"), &group, 10).await.expect("get").is_none());
        assert!(state.sticky.get(&StickyKey::new("k2"), &group, 10).await.expect("get").is_some());
        release_group_sticky(&state, "g-b", None).await.expect("清整组");
        assert!(state.sticky.get(&StickyKey::new("k2"), &group, 10).await.expect("get").is_none());
        assert!(state.sticky.list(10, 10).await.expect("list").is_empty());

        // 不存在的组 / 不存在的通道组设置粘性
        assert!(release_group_sticky(&state, "g-nope", None).await.is_err());
        assert!(set_group_sticky(&state, "g-nope", sticky).await.is_err());
        assert!(delete_group(&state, "g-nope").await.is_err());
    }

    #[tokio::test]
    async fn channel_matrix_joins_models_credentials_and_channels() {
        let state = seeded("matrix").await;
        // 加一条别名通道，验证 cell 把别名解析回规范模型 id
        let mut doc = (*state.doc()).clone();
        doc.models[0].aliases = vec!["alias-a".to_string()];
        let ch2 = ChannelConfig::new("ch-alias", "alias-a", "p-a", "c-a", "upstream-alias");
        doc.channels.push(ch2);
        state.config.save(&doc).await.expect("种别名通道");
        state.set_doc(doc);

        let m = build_channel_matrix(&state).await.expect("矩阵");
        assert_eq!(m.models.len(), 1);
        assert_eq!(m.models[0].model_id, "m-a");
        assert_eq!(m.models[0].display_name, "m-a");
        assert!(!m.models[0].unpriced, "种子里给 m-a+c-a 配了 manual 价，不该被标未定价");

        assert_eq!(m.credentials.len(), 1);
        assert_eq!(m.credentials[0].credential_id, "c-a");
        assert_eq!(m.credentials[0].label, "A");
        assert_eq!(m.credentials[0].provider_name, "A");
        assert_eq!(
            m.credentials[0].quota.state, "unknown",
            "从未观测 → unknown，不是 ok 也不是 exhausted"
        );

        assert_eq!(m.cells.len(), 2);
        for cell in &m.cells {
            assert_eq!(cell.model_id, "m-a", "别名要解析回规范 id，否则前端 join 不上 models[]");
            assert_eq!(cell.credential_id, "c-a");
            assert_eq!(cell.priority, Some(100));
            assert_eq!(cell.weight, Some(10));
            assert!(cell.lane_id.is_none(), "五级线路都没配 → None（直连），不编一个字符串");
            assert!(cell.enabled);
        }
        assert_eq!(m.cells[0].channel_id.as_deref(), Some("ch-a"));
        assert_eq!(m.cells[1].upstream_model.as_deref(), Some("upstream-alias"));

        // 未定价的模型要被标出来（用引擎的口径：没有任何条目/内置价覆盖）
        let mut doc = (*state.doc()).clone();
        doc.pricing.entries.clear();
        let ch3 = ChannelConfig::new("ch-local", "m-local", "p-a", "c-a", "llama-local");
        doc.models.push(Model::new("m-local"));
        doc.channels.push(ch3);
        state.config.save(&doc).await.expect("种未定价模型");
        state.set_doc(doc);
        let m = build_channel_matrix(&state).await.expect("矩阵");
        let local = m.models.iter().find(|r| r.model_id == "m-local").expect("在");
        assert!(local.unpriced, "既没配价也没内置价的模型必须被标未定价");
    }

    /// routing 场景（模拟器用）：两条通道 + 一个组 + 两条策略。
    ///
    /// 形状照抄 `ta-core/src/routing/simulate.rs` 单测里那份 YAML 的语义：
    /// `pol-skip`（priority 5，带图请求排除 `no-vision`）先于 `pol-default`（priority 1000）。
    async fn seeded_routing(tag: &str) -> AppState {
        let state = fresh_state(tag).await;
        let mut doc = ConfigDoc::default();
        doc.providers.push(Provider::new("p-a", "A", Protocol::Openai, "https://a.example/v1"));
        doc.providers.push(Provider::new("p-b", "B", Protocol::Openai, "https://b.example/v1"));
        doc.credentials.push(Credential::new("c-a", "p-a", "A", "${secret:a}"));
        doc.credentials.push(Credential::new("c-b", "p-b", "B", "${secret:b}"));
        doc.models.push(Model::new("m-g"));
        doc.channels.push(
            ChannelConfig::new("ch-a", "m-g", "p-a", "c-a", "up-a")
                .with_priority(10)
                .with_weight(100)
                .with_tags(["official"]),
        );
        doc.channels.push(
            ChannelConfig::new("ch-b", "m-g", "p-b", "c-b", "up-b")
                .with_priority(20)
                .with_weight(50)
                .with_tags(["no-vision"]),
        );
        doc.channel_groups.push(ChannelGroup::new("g-g", "m-g", ["ch-a", "ch-b"]).with_sticky(
            ta_core::domain::StickyConfig {
                enabled: true,
                ttl_h: 12,
                key: vec!["header:x-session-id".to_string()],
                ..Default::default()
            },
        ));
        doc.policies.push(
            Policy::new("pol-skip", "带图请求跳过不识图的通道").with_priority(5).with_match(
                ta_core::domain::PolicyMatch { has_image: Some(true), ..Default::default() },
            ),
        );
        doc.policies.last_mut().expect("刚 push").action.exclude_tag = Some("no-vision".into());
        doc.policies.push(
            Policy::new("pol-default", "默认策略")
                .with_priority(1000)
                .with_match(ta_core::domain::PolicyMatch::match_all()),
        );
        doc.policies.last_mut().expect("刚 push").action.use_group =
            Some(ta_core::types::ChannelGroupId::new("g-g"));
        state.config.save(&doc).await.expect("种路由配置");
        state.set_doc(doc);
        state
    }

    fn sim_input(has_image: bool) -> SimulateInput {
        SimulateInput {
            model: "m-g".into(),
            has_image,
            stream: true,
            est_input_tokens: 1500,
            headers: [("x-session-id".to_string(), "abc".to_string())].into_iter().collect(),
            session_key: Some("abc".into()),
            now: Some(1_699_920_000_000),
        }
    }

    #[tokio::test]
    async fn pricing_upsert_delete_and_resolve_use_the_shared_engine() {
        let state = seeded("pricing-crud").await;
        // 种子里的条目（match: m-a + c-a，manual，input=1.0）
        let list = state.doc().pricing.entries.clone();
        assert_eq!(list.len(), 1);

        // 同 match → 覆盖（不新增）；换成一条更贵的价
        let mut same = PriceEntry::new(PriceMatch::model("m-a").with_credential("c-a"));
        same.source = PriceSource::Manual;
        same.rates.insert("input".into(), 9.0);
        same.rates.insert("output".into(), 27.0);
        upsert_price(&state, same).await.expect("覆盖");
        let back = state.config.load().await.expect("load");
        assert_eq!(back.pricing.entries.len(), 1, "同 match 必须覆盖，不是追加");
        assert_eq!(back.pricing.entries[0].rates.get("input"), Some(&9.0));

        // 不同 match → 追加
        let mut other = PriceEntry::new(PriceMatch::model("m-b"));
        other.source = PriceSource::Manual;
        other.rates.insert("input".into(), 2.0);
        upsert_price(&state, other).await.expect("追加");
        assert_eq!(state.config.load().await.expect("load").pricing.entries.len(), 2);

        // resolve 走的是同一个引擎：manual > 内置表
        let r = resolve_price(&state, "m-a", None, Some("c-a".into())).await.expect("解析");
        assert_eq!(r.source, "manual");
        assert_eq!(r.rates.get("input"), Some(&9.0));
        assert!((r.multiplier - 1.0).abs() < 1e-12);
        assert_eq!(r.currency, "USD", "条目没写币种 → 回退到 pricing.currency");

        // 没配 provider/credential → 不限定（m-b 的条目就是不限定的）
        let r = resolve_price(&state, "m-b", None, None).await.expect("解析");
        assert_eq!(r.source, "manual");
        assert_eq!(r.rates.get("input"), Some(&2.0));

        // 完全没配过的模型 → 落到内置静态表
        let r = resolve_price(&state, "gpt-4o", None, None).await.expect("解析");
        assert_eq!(r.source, "builtin");
        assert_eq!(r.rates.get("input"), Some(&2.5));
        assert_eq!(r.rates.get("output"), Some(&10.0));

        // 内置表也不认识 → none（未定价，**不猜**）
        let r = resolve_price(&state, "totally-local-model", None, None).await.expect("解析");
        assert_eq!(r.source, "none");
        assert!(r.rates.is_empty());

        // 删除：按下标，越界报错且不写库
        let err = delete_price(&state, 5).await.expect_err("越界必须报错");
        assert_eq!(err.code, "TA-1004");
        assert!(err.message.contains("越界"), "{}", err.message);
        delete_price(&state, 1).await.expect("删第 2 条");
        let back = state.config.load().await.expect("load");
        assert_eq!(back.pricing.entries.len(), 1);
        assert_eq!(back.pricing.entries[0].r#match.model.as_deref(), Some("m-a"));
    }

    #[tokio::test]
    async fn pricing_import_builtin_is_idempotent_and_keeps_the_user_price() {
        let state = seeded("pricing-import").await;
        // 先把种子那条改成 manual，并且再加一条与内置表同 pattern 的条目（用户已手改过）
        rewrite_doc(&state, |d| {
            let mut mine = PriceEntry::new(PriceMatch::model("gpt-4o*"));
            mine.source = PriceSource::Manual;
            mine.rates.insert("input".into(), 0.01);
            d.pricing.entries.push(mine);
        })
        .await;

        let r = import_builtin_prices(&state).await.expect("导入");
        assert!(r.applied);
        let after = state.config.load().await.expect("load");
        let imported =
            after.pricing.entries.iter().filter(|e| e.source == PriceSource::Imported).count();
        assert!(imported >= 5, "内置表要真的被拷进来，实际 {imported} 条");
        // 用户的 gpt-4o* 条目**没有被覆盖**
        let mine = after
            .pricing
            .entries
            .iter()
            .find(|e| e.r#match.model.as_deref() == Some("gpt-4o*"))
            .expect("在");
        assert_eq!(mine.source, PriceSource::Manual);
        assert_eq!(mine.rates.get("input"), Some(&0.01));
        // 导入的条目钉死了单位与币种（否则用户改 pricing.unit 会静默差 1000 倍）
        let g = after
            .pricing
            .entries
            .iter()
            .find(|e| e.r#match.model.as_deref() == Some("gpt-4o-mini*"))
            .expect("内置表里有 gpt-4o-mini");
        assert_eq!(g.currency.as_deref(), Some("USD"));
        assert_eq!(g.extra.get("unit"), Some(&serde_json::json!("per_1m")));

        // 幂等：再导一次条数不变
        let before = after.pricing.entries.len();
        import_builtin_prices(&state).await.expect("再导一次");
        assert_eq!(
            state.config.load().await.expect("load").pricing.entries.len(),
            before,
            "第二次导入必须一条都不加"
        );

        // 导入的价真的参与解析（source = imported，数值与内置表一致）
        let r = resolve_price(&state, "claude-sonnet-4-5", None, None).await.expect("解析");
        assert_eq!(r.source, "imported");
        assert_eq!(r.rates.get("input"), Some(&3.0));
        assert_eq!(r.rates.get("output"), Some(&15.0));
    }

    #[tokio::test]
    async fn policy_upsert_reorder_delete_and_enabled_are_persisted() {
        let state = seeded_routing("pol-crud").await;

        // upsert 新增 + 覆盖
        upsert_policy(&state, Policy::new("pol-x", "临时").with_priority(500))
            .await
            .expect("加策略");
        assert!(state
            .config
            .load()
            .await
            .expect("load")
            .policies
            .iter()
            .any(|p| p.id.as_str() == "pol-x"));
        let mut dup = Policy::new("pol-x", "临时改名").with_priority(500);
        dup.enabled = false;
        upsert_policy(&state, dup).await.expect("覆盖");
        let back = state.config.load().await.expect("load");
        assert_eq!(back.policies.iter().filter(|p| p.id.as_str() == "pol-x").count(), 1);
        assert!(!back.policies.iter().find(|p| p.id.as_str() == "pol-x").expect("在").enabled);

        // reorder：必须给全量排列，优先级按下标重写
        let err = reorder_policies(&state, &["pol-skip".into()]).await.expect_err("少给了必须报错");
        assert_eq!(err.code, "TA-1004");
        assert!(err.message.contains("漏掉"), "{}", err.message);
        let err =
            reorder_policies(&state, &["pol-skip".into(), "pol-default".into(), "pol-nope".into()])
                .await
                .expect_err("给了不存在的必须报错");
        assert!(err.message.contains("不存在"), "{}", err.message);
        let err =
            reorder_policies(&state, &["pol-skip".into(), "pol-skip".into(), "pol-default".into()])
                .await
                .expect_err("重复给必须报错");
        assert!(err.message.contains("重复"), "{}", err.message);

        reorder_policies(&state, &["pol-x".into(), "pol-default".into(), "pol-skip".into()])
            .await
            .expect("重排");
        let back = state.config.load().await.expect("load");
        assert_eq!(
            back.policies.iter().map(|p| (p.id.as_str(), p.priority)).collect::<Vec<_>>(),
            vec![("pol-x", 10), ("pol-default", 20), ("pol-skip", 30)],
            "顺序 = 给定顺序，priority = (下标+1)*10（要落库）"
        );

        // set_enabled
        set_policy_enabled(&state, "pol-skip", false).await.expect("停用");
        assert!(!state.config.load().await.expect("load").policies[2].enabled);
        assert!(set_policy_enabled(&state, "pol-nope", true).await.is_err());

        // delete（策略不被任何东西引用 → 直接删）
        delete_policy(&state, "pol-x").await.expect("删");
        assert_eq!(state.config.load().await.expect("load").policies.len(), 2);
        assert_eq!(delete_policy(&state, "pol-nope").await.expect_err("未知 id").code, "TA-1004");
    }

    #[tokio::test]
    async fn policy_simulate_explains_the_route_and_stays_deterministic() {
        let state = seeded_routing("pol-sim").await;

        // 无图：默认策略命中，首选高优先级通道
        let plain = simulate_policy(&state, &sim_input(false)).await.expect("模拟");
        assert_eq!(
            plain.matched_policy.as_ref().map(|p| (p.id.as_str(), p.name.as_str())),
            Some(("pol-default", "默认策略"))
        );
        assert_eq!(plain.chosen.as_ref().map(|c| c.channel_id.as_str()), Some("ch-a"));
        assert_eq!(plain.chosen.as_ref().map(|c| c.sticky_hit), Some(false), "冷启动视角");
        assert_eq!(plain.chosen.as_ref().and_then(|c| c.sticky_key.as_deref()), Some("abc"));
        assert_eq!(plain.failover_chain, vec!["ch-a".to_string(), "ch-b".to_string()]);
        assert!(plain.outgoing_equals_client);
        assert_eq!(plain.candidates.len(), 2);
        assert!(plain.candidates.iter().all(|c| c.ok));
        assert!(plain.eliminated.is_empty());
        assert!(plain.lane.is_none(), "没配任何 lane → 直连，不编一条线路出来");

        // 带图：前一条策略先把 no-vision 通道淘汰掉，并给出机器可读的原因
        let vision = simulate_policy(&state, &sim_input(true)).await.expect("模拟");
        assert_eq!(
            vision.matched_policy.as_ref().map(|p| p.id.as_str()),
            Some("pol-skip"),
            "priority 小的先求值、首个命中即生效"
        );
        assert_eq!(vision.chosen.as_ref().map(|c| c.channel_id.as_str()), Some("ch-a"));
        assert_eq!(
            vision.eliminated,
            vec![SimulateEliminated { channel_id: "ch-b".into(), reason: "tagged_out".into() }]
        );
        assert_eq!(vision.failover_chain, vec!["ch-a".to_string()], "被淘汰的不进失败转移链");

        // 确定性：同一输入连跑 20 次结果一致（纯函数语义）
        for i in 0..20 {
            assert_eq!(
                simulate_policy(&state, &sim_input(true)).await.expect("模拟"),
                vision,
                "第 {i} 次结果与第 1 次不一致"
            );
        }

        // 全部停用 → 没有候选，如实返回"选不出来"，不是随便挑一条
        rewrite_doc(&state, |d| {
            for c in d.channels.iter_mut() {
                c.enabled = false;
            }
        })
        .await;
        let none = simulate_policy(&state, &sim_input(false)).await.expect("模拟");
        assert!(none.chosen.is_none());
        assert!(none.failover_chain.is_empty());
        assert!(none.eliminated.iter().all(|e| e.reason == "disabled"));
    }

    #[tokio::test]
    async fn credential_upsert_writes_then_reads_back_and_stays_idempotent() {
        let state = seeded("cred-upsert").await;
        let c = Credential::new("c-new", "p-a", "新套餐", "${secret:b}");
        upsert_credential(&state, c.clone()).await.expect("首次 upsert");

        // 内存快照
        assert!(cred_exists(&state, "c-new"));
        // **真读库**
        let from_db = state.config.load().await.expect("load");
        assert!(
            from_db.credentials.iter().any(|x| x.id.as_str() == "c-new"),
            "写进去的必须读得回来"
        );

        // 同 id 再 upsert（改标签）→ 覆盖而不是新增
        let mut renamed = c.clone();
        renamed.label = "改名后".to_string();
        upsert_credential(&state, renamed).await.expect("再次 upsert");
        let after = state.config.load().await.expect("load");
        assert_eq!(after.credentials.iter().filter(|x| x.id.as_str() == "c-new").count(), 1);
        assert_eq!(
            after.credentials.iter().find(|x| x.id.as_str() == "c-new").map(|x| x.label.clone()),
            Some("改名后".to_string())
        );

        // 引用不存在的服务商 → 校验器拒绝，一个字节都不写库（"宁可不写，也不写非法配置"）
        let bad = Credential::new("c-bad", "p-nope", "坏", "${secret:b}");
        let err = upsert_credential(&state, bad).await.expect_err("引用不存在的服务商必须拒绝");
        assert_eq!(err.code, "TA-5001");
        assert!(!cred_exists(&state, "c-bad"), "被拒绝的实体不许留在内存快照里");
        assert!(
            !state
                .config
                .load()
                .await
                .expect("load")
                .credentials
                .iter()
                .any(|x| x.id.as_str() == "c-bad"),
            "被拒绝的实体不许落库"
        );
    }

    #[tokio::test]
    async fn credential_set_enabled_persists_is_reversible_and_rejects_unknown_id() {
        let state = seeded("cred-enable").await;
        set_credential_enabled(&state, "c-a", false).await.expect("停用");
        assert!(!state.doc().credentials[0].enabled, "内存快照要立刻反映");
        assert!(!state.config.load().await.expect("load").credentials[0].enabled, "必须落库");
        set_credential_enabled(&state, "c-a", true).await.expect("再启用");
        assert!(state.config.load().await.expect("load").credentials[0].enabled, "可逆");

        let err =
            set_credential_enabled(&state, "c-nope", false).await.expect_err("未知 id 必须报错");
        assert!(!err.message.is_empty());
        assert_eq!(err.code, "TA-1004", "缺'实体不存在'专用码，当前用 MalformedRequest");
    }

    #[tokio::test]
    async fn credential_delete_refuses_when_referenced_then_force_cascades() {
        let state = seeded("cred-del").await;
        // 有通道引用 → 拒绝，且**配置没被动过**
        let err = delete_credential(&state, "c-a", false).await.expect_err("有引用必须拒绝");
        assert_eq!(err.code, "TA-5002");
        assert!(cred_exists(&state, "c-a"), "被拒绝时不许改配置");
        assert_eq!(state.config.load().await.expect("load").channels.len(), 1, "库里也不许变");

        // force → 凭据 + 通道 + 空掉的组 + 引用它的定价条目一起走
        delete_credential(&state, "c-a", true).await.expect("force 级联");
        let after = state.config.load().await.expect("load");
        assert!(!after.credentials.iter().any(|c| c.id.as_str() == "c-a"));
        assert!(after.channels.is_empty(), "名下的通道必须跟着删（否则悬空引用校验不过）");
        assert!(after.channel_groups.is_empty(), "成员被删空 → 空组必须一起删（EMPTY_MEMBERS）");
        assert!(after.policies[0].action.use_group.is_none(), "策略对已删组的引用要摘掉");
        assert!(
            after.pricing.entries.is_empty(),
            "引用被删凭据的定价条目必须一起删（校验器查 credential 引用）"
        );

        // 未知 id
        let err = delete_credential(&state, "c-nope", false).await.expect_err("未知 id 必须报错");
        assert_eq!(err.code, "TA-1004");
    }

    #[tokio::test]
    async fn provider_delete_counts_refs_and_cascades_with_force() {
        let state = seeded("prov-del").await;
        let err = delete_provider(&state, "p-a", false).await.expect_err("有引用必须拒绝");
        assert_eq!(err.code, "TA-5002");
        assert!(err.message.contains("凭据"), "报错要说清被什么引用：{}", err.message);
        assert!(err.message.contains("通道"));

        delete_provider(&state, "p-a", true).await.expect("force 级联");
        let after = state.config.load().await.expect("load");
        assert!(after.providers.is_empty());
        assert!(after.credentials.is_empty(), "该服务商名下的凭据一起走");
        assert!(after.channels.is_empty());
        assert!(after.channel_groups.is_empty());

        let err = delete_provider(&state, "p-nope", false).await.expect_err("未知 id 必须报错");
        assert_eq!(err.code, "TA-1004");
    }

    #[tokio::test]
    async fn credential_set_secret_writes_to_store_and_never_echoes_plaintext() {
        let state = seeded("cred-secret").await;
        let store = MockStore::new();
        // 形态像密钥的**假值**：标 `ta-allow-fake-secret`，按 gate 的既定机制放行（docs/02 §5）
        let plaintext = "sk-test-0123456789abcdef"; // ta-allow-fake-secret

        set_credential_secret_with(&state, &store, "c-a", plaintext).await.expect("写入");

        // ① 真的写进了注入的 store（条目名来自 `${secret:a}`）
        assert_eq!(store.get("a").await.expect("get").as_deref(), Some(plaintext));

        // ② 元数据只留脱敏提示
        let meta = state.secret_meta.get("a").await.expect("get meta").expect("有条目");
        assert!(!meta.hint.is_empty());
        assert!(!meta.hint.contains("0123456789"), "hint 泄漏了密钥主体：{}", meta.hint);

        // ③ 明文**没有**进配置（真源仍只有引用），也没有进任何返回/错误消息
        let doc_json = serde_json::to_string(&*state.doc()).expect("serialize");
        assert!(!doc_json.contains("0123456789"), "明文不得出现在配置里");
        assert_eq!(
            state.doc().credentials[0].secret,
            "${secret:a}",
            "credential.secret 必须保持引用形态"
        );

        // ④ 未知凭据 / 环境变量引用 / 空明文都要如实报错
        assert!(set_credential_secret_with(&state, &store, "c-nope", plaintext).await.is_err());
        let env_cred = Credential::new("c-env", "p-a", "env", "${env:OPENAI_API_KEY}");
        upsert_credential(&state, env_cred).await.expect("加一份 env 凭据");
        let err = set_credential_secret_with(&state, &store, "c-env", plaintext)
            .await
            .expect_err("env 引用不能写");
        assert!(err.message.contains("环境变量"), "{}", err.message);
        assert!(set_credential_secret_with(&state, &store, "c-a", "   ").await.is_err());
    }

    #[tokio::test]
    async fn credential_clear_quota_resets_counters_and_persists() {
        let state = seeded("cred-clear").await;
        // 先造一份"已用掉一些"的账本记录（走既有后端，模拟真实运行态）
        let cred = ta_core::types::CredentialId::new("c-a");
        let mut rec = ta_core::quota::QuotaRecord::new(&cred);
        rec.state.used_tokens = 12_345;
        rec.state.used_requests = 7;
        rec.state.used_cost = 1.5;
        rec.state.kind = ta_core::quota::QuotaStateKind::Exhausted;
        rec.hard_exhausted = true;
        state.quota.put(&rec);
        state.quota.flush().await.expect("落库");

        let view = clear_credential_quota(&state, "c-a").await.expect("清零");
        assert_eq!(view.credential_id, "c-a");
        assert_eq!(view.used_tokens, 0);
        assert_eq!(view.used_requests, 0);
        assert_eq!(view.used_cost, 0.0);
        assert_eq!(view.state, "ok", "手动清零 = 一次观测，状态从 unknown/exhausted 进 ok");

        // 真落库（等后台任务把这次写入刷完再读库）
        state.quota.flush().await.expect("flush");
        let row = state.usage.quota_state("c-a").await.expect("query").expect("有行");
        assert_eq!(row.used_tokens, 0);
        assert!(!row.hard_exhausted, "手动清零要解除硬性耗尽");
        assert_eq!(row.state, "ok");

        let err = clear_credential_quota(&state, "c-nope").await.expect_err("未知 id");
        assert_eq!(err.code, "TA-1004");
    }
}
