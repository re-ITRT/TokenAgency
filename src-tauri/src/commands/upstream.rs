//! 上游探测类命令（[docs/19 §1.1–§1.3]）：`provider_test` / `model_import_from_provider` /
//! `model_apply_import`。
//!
//! # 为什么单独一个模块（而不是塞进 `entity.rs`）
//!
//! `entity.rs` 是"配置写命令"：每个函数都是"改内存快照 → [`save_validated`] → 返回"。
//! 本模块的三条命令是**另一件事**：它们要发一次真实的上游请求，再（可能）写配置。
//! 混在一起会让"哪些命令有网络副作用"变得不可见。
//!
//! # 出站：只有一条路
//!
//! 三条命令**都不自己发 HTTP**：探测走 `ta_gateway::probe::UpstreamProbe`，
//! 而它持有的是**网关那一个** `OutboundDialer`（从 [`AppState::gateway_deps`] 取），
//! 也就是与转发管线同一个拨号器 —— 线路策略（内核 lane listener）在那后面。
//! `src-tauri` 连 `reqwest` 都不依赖，所以"另开一条出站路径"在这里是**编译不过**的。
//!
//! 拿不到 dialer（网关还没装配）时**如实报错**（`TA-3002`）：一个"看起来成功了"的
//! 探测比一个明确的失败坏得多。
//!
//! # 语义分档（[docs/19 §1.1]，由 `probe.rs` 给出）
//!
//! `ok=false` **不等于**"服务商坏了"：
//! * `probe="models"` + `status=404/405/403` = "上游没有 `/models` 端点"（很多中转只有
//!   `/chat/completions`），note 里写清了这一档；界面**不许**把它渲染成"连接失败"；
//! * `probe="none"` + `status=0` = 一个响应都没拿到（线路不可用/连不通），`latency_ms`
//!   是到失败为止的**真实测量**；
//! * `latency_ms` 一律来自单调时钟（`probe.rs` 里测的），本模块**不修正、不美化**。
//!
//! # 鉴权头
//!
//! 用 `ta_gateway::passthrough::auth_headers`（**与转发路径同一份实现**）+ 凭据
//! `credential.secret` 经 `ta_secrets` 解析出的明文。命令层自己拼 `authorization` /
//! `x-api-key` 会立刻与转发路径漂移（同一件事两套实现）。
//!
//! 服务商**一把启用中的凭据都没有**时：[`probe_secret`] 如实报错。
//! 硬发一个空 `Bearer ` 会让上游答 401，那等于**用假鉴权制造假故障**，
//! 用户会去查自己的 key 而不是查"向导还没落库"。

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use tauri::State;

use ta_core::domain::{AuthScheme, ConfigDoc, DisplayConfig, Model, Provider};
use ta_core::error::TaError;
use ta_core::types::ProviderId;
use ta_gateway::passthrough;
use ta_gateway::pipeline::SecretResolver;
use ta_gateway::probe::{probe_lane, ProbeProviderRequest, UpstreamProbe};
use ta_gateway::traits::OutboundDialer;

use crate::commands::catalog::save_validated;
use crate::commands::config::ApplyResult;
use crate::commands::entity::applied;
use crate::commands::{IpcError, IpcResult};
use crate::gateway::SecretsAdapter;
use crate::import_preview::{PreviewModel, PreviewSkip};
use crate::state::AppState;

// ================================================================ DTO（线上契约）

/// `ProbeResult`（`bindings.ts` / [docs/19 §1.1]）。
///
/// **为什么这里有一个本地结构体**：`ta_gateway::probe::ProbeResult` 是**网关内部**类型
/// （`ta-gateway` 不依赖 specta，也不可能为它 derive `specta::Type` —— 那是 E5 的地盘），
/// 而 IPC 的返回类型必须能导出 TS。所以这里是**契约类型本身**（字段与 `bindings.ts`
/// 逐字段对齐），转换只有 [`ProbeResult::from_probe`] 一处 —— 不是"第二份实现"。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
pub struct ProbeResult {
    pub ok: bool,
    /// 一次请求都没发出去时是 **0**（不编一个 200）
    pub status: u16,
    /// 真实测量的耗时（单调时钟，毫秒）
    pub latency_ms: u32,
    /// `"models"` | `"chat"` | `"none"` —— 直接取 `ProbeKind::as_str()`。
    ///
    /// 用字符串而不是在这里再写一个枚举：那份取值表是 `probe.rs` 的
    /// （它的测试逐字钉住了 `"models"`），再抄一份就是第二个真相源。
    pub probe: String,
    /// 人话说明（可能带上游原文，**只进 message**）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl ProbeResult {
    fn from_probe(p: ta_gateway::probe::ProbeResult) -> Self {
        Self {
            ok: p.ok,
            status: p.status,
            latency_ms: p.latency_ms,
            probe: p.probe.as_str().to_string(),
            note: p.note,
        }
    }
}

/// `ImportPreview.models[]`（`bindings.ts`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
pub struct ImportModelRow {
    pub upstream_model: String,
    pub suggested_id: String,
    /// 扫描那一刻 `doc.models` 里已经有了（界面据此默认不勾选）
    pub exists: bool,
}

/// `ImportPreview.skipped[]`：派生不出合法 id 的上游模型 + 人话原因。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
pub struct ImportSkippedRow {
    pub upstream_model: String,
    pub reason: String,
}

/// 缓存里的行 → 线上契约的行（**形状转换只有这一处**，不是第二份定义）。
impl From<PreviewModel> for ImportModelRow {
    fn from(m: PreviewModel) -> Self {
        Self { upstream_model: m.upstream_model, suggested_id: m.suggested_id, exists: m.exists }
    }
}

impl From<PreviewSkip> for ImportSkippedRow {
    fn from(s: PreviewSkip) -> Self {
        Self { upstream_model: s.upstream_model, reason: s.reason }
    }
}

/// `ImportPreview`（`bindings.ts` / [docs/19 §1.2]）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
pub struct ImportPreview {
    /// 取回这次扫描结果的句柄（**进程内存**，5 分钟 TTL，不落库）
    pub preview_token: String,
    pub models: Vec<ImportModelRow>,
    pub skipped: Vec<ImportSkippedRow>,
}

// ================================================================ 公共前置：出站通道与凭据

/// 唯一出站通道。拿不到就**如实报错**，绝不假装探测成功。
fn outbound_dialer(state: &AppState) -> IpcResult<std::sync::Arc<dyn OutboundDialer>> {
    match state.gateway_deps() {
        Some(deps) => Ok(deps.dialer.clone()),
        None => Err(IpcError::from(TaError::LaneUnavailable {
            lane: "（网关尚未装配：命令层拿不到唯一出站通道，探测不会假装成功）".to_string(),
        })),
    }
}

/// 取服务商；不存在 → `TA-1004`（`entity.rs` 同款口径：`TaError` 没有"实体不存在"变体，
/// 用最接近的既有码，消息里写清是哪个 id）。
fn provider_of<'a>(doc: &'a ConfigDoc, id: &ProviderId) -> IpcResult<&'a Provider> {
    doc.providers.iter().find(|p| &p.id == id).ok_or_else(|| {
        IpcError::from(TaError::MalformedRequest(format!(
            "服务商不存在：{id}（先保存服务商与密钥，再做上游探测）"
        )))
    })
}

/// 这一次探测该带哪把密钥。
///
/// * `auth.scheme: none` → **不需要密钥**（`passthrough::rewrite_auth` 对这条腿什么都不写，
///   也不会追加 query），返回空串占位，**不碰密钥仓库**；
/// * 其它 scheme → 取该服务商**第一条启用中的凭据**，按 `credential.secret` 的引用去解析明文。
///   `ta_secrets` 的解析失败原样上报（[`TaError::Secret`]）：拿空 key 去打上游会得到 401，
///   那会让用户去查自己的 key，而真因是"钥匙串里没有这个条目"。
async fn probe_secret(
    doc: &ConfigDoc,
    provider: &Provider,
    secrets: &dyn SecretResolver,
) -> IpcResult<String> {
    if matches!(provider.auth.scheme, AuthScheme::None) {
        return Ok(String::new());
    }
    let Some(cred) =
        doc.credentials.iter().filter(|c| c.provider == provider.id).find(|c| c.enabled)
    else {
        return Err(IpcError::from(TaError::MalformedRequest(format!(
            "服务商 {} 还没有启用中的凭据：探测没法带鉴权（先保存服务商与密钥）",
            provider.id
        ))));
    };
    let plaintext = secrets.resolve(&cred.secret).await?;
    Ok(plaintext)
}

// ================================================================ provider_test

/// 实现体（测试直接调它，避开 `State<'_, AppState>` 的构造）。
///
/// 顺序是有意的：**先证明服务商存在、再拿到出站通道、最后才解析密钥** ——
/// 每一步失败都对应一句能读懂的话，且都不会发出半个请求。
pub async fn test_provider(
    state: &AppState,
    secrets: &dyn SecretResolver,
    id: &str,
    model: Option<&str>,
) -> IpcResult<ProbeResult> {
    let doc = state.doc();
    let provider_id = ProviderId::new(id);
    let provider = provider_of(&doc, &provider_id)?;
    let dialer = outbound_dialer(state)?;
    let lane = probe_lane(&doc, &provider_id);
    let secret = probe_secret(&doc, provider, secrets).await?;

    let outcome = UpstreamProbe::new(dialer)
        .probe_provider(ProbeProviderRequest {
            lane: &lane,
            base_url: &provider.base_url,
            // 与转发路径**同一份**鉴权头实现（见模块文档）
            headers: passthrough::auth_headers(Some(provider), &secret),
            // `auth.scheme: query` 时密钥只能进 URL（`apply_query_auth` 对别的 scheme 原样返回）
            query_auth: Some((&provider.auth, secret.as_str())),
            model,
        })
        .await;

    tracing::debug!(
        provider_id = %provider_id,
        lane_id = %lane,
        status = outcome.status,
        elapsed_ms = outcome.latency_ms,
        truncated = outcome.truncated,
        "上游探测完成（probe 分档由界面按 probe 字段渲染）"
    );
    Ok(ProbeResult::from_probe(outcome))
}

/// `provider_test({ id, model? }) -> ProbeResult`（[docs/19 §1.1]）。
#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn provider_test(
    state: State<'_, AppState>,
    id: String,
    model: Option<String>,
) -> IpcResult<ProbeResult> {
    let secrets = SecretsAdapter::with_defaults();
    test_provider(&state, &secrets, &id, model.as_deref()).await
}

// ================================================================ model_import_from_provider

/// 一条上游模型名 → 建议的逻辑模型 id。
///
/// 规则**就这么四条**（[docs/19 §1.2] 要求"简单规则 + 写在注释里"，不做智能改名）：
///
/// 1. 取最后一段路径：`openai/gpt-4o` → `gpt-4o`（中转爱把厂商前缀写进名字里，
///    而 `channel.model` 是我们对外暴露的逻辑名，带厂商前缀没有意义）；
/// 2. 去掉**紧贴末尾**的一段日期：`-2024-08-06` / `_2024-08-06` / `-2024.08.06` / `-20240806`
///    （分隔符可混）。**只去一次**：`gpt-4o-2024-08-06` → `gpt-4o`，
///    而 `gpt-4o-2024-08-06-preview` **不动**（日期后面还有东西，说明它不是版本后缀）；
/// 3. 规范化：只保留 `[A-Za-z0-9._:-]`，其余字符换成 `-`，再去掉首尾的 `-` `.` `:`；
/// 4. 结果为空 → `Err(人话原因)`，进 `skipped`。**绝不硬凑**一个 id
///    （凑出来的 id 会变成一个用户没要过的模型）。
fn derive_suggested_id(upstream: &str) -> std::result::Result<String, String> {
    let tail = upstream.rsplit('/').next().unwrap_or(upstream).trim();
    if tail.is_empty() {
        return Err("上游名是空的（只有路径前缀），派生不出模型 id".to_string());
    }
    let sanitized: String =
        strip_trailing_date(tail)
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '-') {
                    c
                } else {
                    '-'
                }
            })
            .collect();
    let trimmed = sanitized.trim_matches(|c| matches!(c, '-' | '.' | ':'));
    if trimmed.is_empty() {
        return Err(format!(
            "由 `{upstream}` 派生不出非空的模型 id（去掉日期与非法字符后什么都不剩）"
        ));
    }
    Ok(trimmed.to_string())
}

/// 去掉末尾的一段日期（见 [`derive_suggested_id`] 的规则 2）。按**字符**切，不按字节切。
fn strip_trailing_date(s: &str) -> &str {
    let n = s.chars().count();
    let is_digit = |c: char| c.is_ascii_digit();
    let is_sep = |c: char| matches!(c, '-' | '_' | '.');

    // `YYYYMMDD`（8 位数字）
    if n >= 9 {
        let tail: Vec<char> = s.chars().skip(n - 8).collect();
        if tail.iter().all(|c| is_digit(*c)) && s.chars().nth(n - 9).is_some_and(is_sep) {
            return drop_last_chars(s, 9);
        }
    }
    // `YYYY-MM-DD`（10 字符，分隔符可混）
    if n >= 11 {
        let tail: Vec<char> = s.chars().skip(n - 10).collect();
        if s.chars().nth(n - 11).is_some_and(is_sep)
            && tail[..4].iter().all(|c| is_digit(*c))
            && is_sep(tail[4])
            && tail[5..7].iter().all(|c| is_digit(*c))
            && is_sep(tail[7])
            && tail[8..].iter().all(|c| is_digit(*c))
        {
            return drop_last_chars(s, 11);
        }
    }
    s
}

/// 去掉末尾 `k` 个字符（按字符，不按字节）。
fn drop_last_chars(s: &str, k: usize) -> &str {
    match s.char_indices().rev().nth(k - 1) {
        Some((idx, _)) => &s[..idx],
        None => "",
    }
}

/// 上游模型名是否在 `doc.models` 里已经存在（`id` 或任一别名，与 `channel.model` 的解析同口径）。
fn model_exists(doc: &ConfigDoc, suggested_id: &str) -> bool {
    doc.find_model(suggested_id).is_some()
}

/// 实现体（测试直接调它）。
///
/// 清单**只来自上游**：`list_upstream_models` 在任何非 2xx / 形状不对 / 体被截断时都返回
/// `Err`（`probe.rs` 的约定），所以这里没有"用状态码编一份清单"的余地。
pub async fn import_models_from_provider(
    state: &AppState,
    secrets: &dyn SecretResolver,
    provider_id: &str,
    prefix_filter: Option<&str>,
) -> IpcResult<ImportPreview> {
    let doc = state.doc();
    let provider_id = ProviderId::new(provider_id);
    let provider = provider_of(&doc, &provider_id)?;
    let dialer = outbound_dialer(state)?;
    let lane = probe_lane(&doc, &provider_id);
    let secret = probe_secret(&doc, provider, secrets).await?;

    let upstream_ids = UpstreamProbe::new(dialer)
        .list_upstream_models(
            &lane,
            &provider.base_url,
            passthrough::auth_headers(Some(provider), &secret),
            Some((&provider.auth, secret.as_str())),
        )
        .await?;

    // 空串/纯空白的前缀 = 没给过滤条件（免得 `""` 变成"全都要"之外的第三种语义）
    let prefix = prefix_filter.map(str::trim).filter(|p| !p.is_empty());

    let mut models: Vec<PreviewModel> = Vec::new();
    let mut skipped: Vec<PreviewSkip> = Vec::new();
    for upstream in upstream_ids {
        if prefix.is_some_and(|p| !upstream.starts_with(p)) {
            continue;
        }
        match derive_suggested_id(&upstream) {
            Ok(suggested_id) => {
                // 同一个建议 id 的多条上游模型**都保留**（上游确实有两份，藏起来就是隐瞒）；
                // 应用时后一条会因为"此刻已存在"被跳过，所以不会写出重复模型。
                let exists = model_exists(&doc, &suggested_id);
                models.push(PreviewModel { upstream_model: upstream, suggested_id, exists });
            }
            // 派生不出的进 skipped 并带原因 —— **不硬凑**一个 id
            Err(reason) => skipped.push(PreviewSkip { upstream_model: upstream, reason }),
        }
    }

    // 条数写进 message（`§5` 的白名单里没有"计数"字段，不乱借一个名字）
    tracing::debug!(
        provider_id = %provider_id,
        "上游模型清单已解析：{} 条候选、{} 条派生不出 id（清单只来自上游，空结果就是如实空结果）",
        models.len(),
        skipped.len()
    );

    // 预览放**进程内存**（5 分钟 TTL，不落库）：它讲的是"上游现在有什么"。
    let preview_token = state.import_previews.put(
        provider_id.as_str(),
        models.clone(),
        skipped.clone(),
        state.clock.now_ms(),
    );
    Ok(ImportPreview {
        preview_token,
        models: models.into_iter().map(ImportModelRow::from).collect(),
        skipped: skipped.into_iter().map(ImportSkippedRow::from).collect(),
    })
}

/// `model_import_from_provider({ provider_id, prefix_filter? }) -> ImportPreview`（[docs/19 §1.2]）。
#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn model_import_from_provider(
    state: State<'_, AppState>,
    provider_id: String,
    prefix_filter: Option<String>,
) -> IpcResult<ImportPreview> {
    let secrets = SecretsAdapter::with_defaults();
    import_models_from_provider(&state, &secrets, &provider_id, prefix_filter.as_deref()).await
}

// ================================================================ model_apply_import

/// 实现体（测试直接调它）。
///
/// **只写 `selected` 里、且此刻还不在配置里的那些**（重复写没有意义）。
/// 写库走的是**唯一**那条写路径 [`save_validated`] —— 与 `model_upsert` 同一份
/// "校验 → 事务写库 → 留快照 → 换内存快照"。
pub async fn apply_model_import(
    state: &AppState,
    preview_token: &str,
    selected: &[String],
) -> IpcResult<ApplyResult> {
    // token 未知或超过 5 分钟 → 让用户重新扫描（预览不落库，所以没有"从库里找回来"这条路）
    let Some(entry) = state.import_previews.get(preview_token, state.clock.now_ms()) else {
        return Err(IpcError::from(TaError::MalformedRequest(
            "预览已过期，请重新扫描（扫描结果只在内存里保留 5 分钟，也不落库）".to_string(),
        )));
    };

    if selected.is_empty() {
        return Err(IpcError::from(TaError::MalformedRequest(
            "selected 为空：这条命令是要写一批模型的，空清单只会得到一次什么都没发生的成功"
                .to_string(),
        )));
    }

    // 调用方只能勾这次扫描给出来的 id；给了别的说明它与后端不同步（**点名**，不静默忽略）
    let known: BTreeSet<&str> = entry.models.iter().map(|m| m.suggested_id.as_str()).collect();
    let unknown: Vec<&String> = selected.iter().filter(|id| !known.contains(id.as_str())).collect();
    if !unknown.is_empty() {
        let names: Vec<&str> = unknown.iter().map(|s| s.as_str()).collect();
        return Err(IpcError::from(TaError::MalformedRequest(format!(
            "selected 里有这次扫描没给出的模型 id：{}（预览与调用方不同步，请重新扫描）",
            names.join(", ")
        ))));
    }

    let wanted: BTreeSet<&str> = selected.iter().map(String::as_str).collect();
    let mut doc = (*state.doc()).clone();
    let mut added = 0usize;
    for row in entry.models.iter() {
        if !wanted.contains(row.suggested_id.as_str()) {
            continue;
        }
        // 以**此刻**的配置为准重新判一次（扫描到应用之间配置可能已经变了 ——
        // 另一个入口刚加过同一个模型时，这里就不该再写第二条）
        if model_exists(&doc, &row.suggested_id) {
            continue;
        }
        doc.models.push(Model {
            id: row.suggested_id.clone(),
            aliases: Vec::new(),
            display: DisplayConfig::default(),
            expose: true,
            extra: serde_json::Map::new(),
        });
        added += 1;
    }

    // 勾的全是"已经有了"：这时**不写库**（写一份一模一样的配置只会多一个快照，
    // 而用户的意图已经满足了）；`applied` 仍然为真，语义是"这条命令成功了，没有要改的"。
    if added == 0 {
        return Ok(applied());
    }

    save_validated(state, doc, "model_apply_import").await?;
    tracing::info!(
        provider_id = %entry.provider_id,
        "模型导入已写入配置：本次新增 {} 条",
        added
    );
    Ok(applied())
}

/// `model_apply_import({ preview_token, selected }) -> ApplyResult`（[docs/19 §1.2]）。
#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn model_apply_import(
    state: State<'_, AppState>,
    preview_token: String,
    selected: Vec<String>,
) -> IpcResult<ApplyResult> {
    apply_model_import(&state, &preview_token, &selected).await
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::VecDeque;
    use std::net::SocketAddr;
    use std::sync::{Arc, Mutex};

    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use ta_core::types::{CredentialId, LaneId};
    use ta_gateway::pipeline::StaticSecrets;
    use ta_store::ConfigStore;

    use crate::import_preview::PREVIEW_TTL_MS;

    // ---------------------------------------------------------------- 假上游（真的监听 TCP）

    /// 一条脚本化的响应。
    #[derive(Clone)]
    struct Reply {
        status: u16,
        body: String,
        delay_ms: u64,
    }

    impl Reply {
        fn json(status: u16, body: impl Into<String>) -> Self {
            Self { status, body: body.into(), delay_ms: 0 }
        }

        /// 故意慢答：用来证明 `latency_ms` 是**真测出来的**，不是编的 0。
        fn delayed(mut self, ms: u64) -> Self {
            self.delay_ms = ms;
            self
        }
    }

    /// **真的在本机监听一个 HTTP 上游**：探测是网络代码，用 mock dialer 自证等于什么都没测。
    struct MockUpstream {
        addr: SocketAddr,
        seen: Arc<Mutex<Vec<String>>>,
    }

    impl MockUpstream {
        fn base_url(&self) -> String {
            format!("http://{}", self.addr)
        }

        fn base_url_with_version(&self) -> String {
            format!("http://{}/v1", self.addr)
        }

        /// 上游收到的**请求头原文**（逐条核对"发出去的是什么"）。
        fn seen(&self) -> Vec<String> {
            self.seen.lock().expect("seen 锁").clone()
        }
    }

    async fn spawn_upstream(replies: Vec<Reply>) -> MockUpstream {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("绑定假上游");
        let addr = listener.local_addr().expect("本地地址");
        let queue = Arc::new(Mutex::new(VecDeque::from(replies)));
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

        let server_queue = Arc::clone(&queue);
        let server_seen = Arc::clone(&seen);
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else { return };
                // 探测只有 GET/POST-无体（本测试不解析 body），读到请求头结束即可
                let mut acc: Vec<u8> = Vec::new();
                let mut buf = vec![0u8; 4096];
                while let Ok(n) = socket.read(&mut buf).await {
                    if n == 0 {
                        break;
                    }
                    acc.extend_from_slice(&buf[..n]);
                    if acc.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                server_seen
                    .lock()
                    .expect("seen 锁")
                    .push(String::from_utf8_lossy(&acc).into_owned());

                let reply = server_queue
                    .lock()
                    .expect("队列锁")
                    .pop_front()
                    .unwrap_or(Reply::json(500, "{}"));
                if reply.delay_ms > 0 {
                    tokio::time::sleep(std::time::Duration::from_millis(reply.delay_ms)).await;
                }
                let head = format!(
                    "HTTP/1.1 {} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    reply.status,
                    reply.body.len()
                );
                let _ = socket.write_all(head.as_bytes()).await;
                let _ = socket.write_all(reply.body.as_bytes()).await;
                let _ = socket.flush().await;
                let _ = socket.shutdown().await;
            }
        });

        MockUpstream { addr, seen }
    }

    fn models_body(ids: &[&str]) -> String {
        let data: Vec<serde_json::Value> =
            ids.iter().map(|id| serde_json::json!({ "id": id, "object": "model" })).collect();
        serde_json::json!({ "object": "list", "data": data }).to_string()
    }

    // ---------------------------------------------------------------- 夹具：真库 + 真配置 + 真 dialer

    /// 一个**真的库**（临时目录里的 SQLite）+ 一个装着真 dialer 的 `AppState`。
    async fn fixture(tag: &str, config: serde_json::Value) -> AppState {
        let dir = std::env::temp_dir().join(format!("ta-up-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("建临时目录");
        let state = AppState::bootstrap(dir.join("db.sqlite")).await.expect("开库");

        let doc: ConfigDoc = serde_json::from_value(config).expect("夹具配置必须能解析");
        state.config.save(&doc).await.expect("写夹具配置");
        state.set_doc(doc);

        // 唯一出站通道：真的 `KernelDialer`（空 PortMap → 只有 `direct` lane 能出去）
        state.set_gateway(crate::gateway::test_gateway_deps(&state));
        state
    }

    /// 一个服务商 + 一把默认凭据（id 固定 `p1-default`，密钥引用固定 `${secret:p1-default}`）。
    fn config_with(base_url: &str, extra: serde_json::Value) -> serde_json::Value {
        let mut config = serde_json::json!({
            "providers": [{
                "id": "p1",
                "name": "P1",
                "protocol": "openai",
                "base_url": base_url,
            }],
            "credentials": [{
                "id": "p1-default",
                "provider": "p1",
                "label": "默认",
                "secret": "${secret:p1-default}",
            }],
        });
        let map = config.as_object_mut().expect("夹具是对象");
        for (k, v) in extra.as_object().expect("extra 是对象") {
            map.insert(k.clone(), v.clone());
        }
        config
    }

    fn secrets() -> StaticSecrets {
        StaticSecrets::new([("${secret:p1-default}", "sk-命令层测试")])
    }

    // ---------------------------------------------------------------- provider_test

    /// 2xx + 形状正确 → `ok=true, probe="models"`，且**延迟是真测出来的**（不是编的 0）。
    #[tokio::test]
    async fn provider_test_reports_models_ok_and_sends_the_credential_header() {
        // 上游故意慢答 120ms：`latency_ms` 必须反映它（probe.rs 的单调时钟测的是 TTFB）
        let upstream =
            spawn_upstream(vec![Reply::json(200, models_body(&["m-1"])).delayed(120)]).await;
        let state =
            fixture("test-ok", config_with(&upstream.base_url(), serde_json::json!({}))).await;

        let result =
            test_provider(&state, &secrets(), "p1", None).await.expect("上游 200，命令不该报错");

        assert!(result.ok, "{result:?}");
        assert_eq!(result.status, 200);
        assert_eq!(result.probe, "models");
        assert!(
            result.latency_ms >= 100,
            "真延迟 120ms，报出来 {}ms 说明这个数是编的",
            result.latency_ms
        );
        assert!(result.note.is_none(), "通了就不该编一个 note：{:?}", result.note);

        let head = upstream.seen()[0].clone();
        assert!(
            head.contains("authorization: Bearer sk-命令层测试"),
            "探测必须带上凭据解析出来的密钥（与转发路径同一份 auth_headers）：{head}"
        );
        assert!(head.starts_with("GET /models"), "{head}");
    }

    /// **上游 404 那一档**（[docs/19 §1.1] 的核心裁决）：这是"没有 `/models` 端点"，
    /// **不是**"连不通" —— `probe` 必须是 `models`、note 必须说清这一档。
    #[tokio::test]
    async fn provider_test_404_is_a_missing_models_endpoint_not_a_connection_failure() {
        let upstream = spawn_upstream(vec![Reply::json(404, r#"{"error":"no such route"}"#)]).await;
        let state =
            fixture("test-404", config_with(&upstream.base_url(), serde_json::json!({}))).await;

        let result = test_provider(&state, &secrets(), "p1", None).await.expect("404 不是命令错误");

        assert!(!result.ok);
        assert_eq!(result.status, 404);
        assert_eq!(result.probe, "models", "请求发出去了、上游答了 404 —— 这一档不是 none");
        let note = result.note.clone().unwrap_or_default();
        assert!(note.contains("没有 /models 端点"), "note 必须说清是哪一档：{note}");
        assert!(note.contains("不是连不通"), "{note}");
        assert!(note.contains("no such route"), "上游原文要带出来：{note}");
    }

    /// 线路不可用 → `probe="none"`、`status=0`，**不冒充**"上游没有端点"。
    #[tokio::test]
    async fn provider_test_on_an_unreachable_lane_reports_probe_none() {
        let upstream = spawn_upstream(vec![Reply::json(200, models_body(&["m-1"]))]).await;
        let state = fixture(
            "test-lane",
            config_with(
                &upstream.base_url(),
                serde_json::json!({
                    "lanes": [{ "id": "lane-jp", "name": "日本", "kind": "subscription_auto" }],
                    "net": { "default_lane": "lane-jp" },
                }),
            ),
        )
        .await;

        let result =
            test_provider(&state, &secrets(), "p1", None).await.expect("线路不可用不是命令错误");

        assert!(!result.ok);
        assert_eq!(result.status, 0, "一个响应都没拿到，status 只能是 0");
        assert_eq!(result.probe, "none");
        let note = result.note.clone().unwrap_or_default();
        assert!(note.contains("连不通"), "{note}");
        assert!(!note.contains("没有 /models 端点"), "连不通不许报成'没有端点'：{note}");
        assert!(upstream.seen().is_empty(), "请求根本没发出去，上游不该看到任何东西");
    }

    /// 网关还没装配 → **如实报错**（`TA-3002`），不许返回一个"看起来正常"的探测结果。
    #[tokio::test]
    async fn provider_test_without_the_gateway_refuses_instead_of_pretending() {
        let upstream = spawn_upstream(vec![Reply::json(200, models_body(&["m-1"]))]).await;
        let state =
            fixture("test-nogw", config_with(&upstream.base_url(), serde_json::json!({}))).await;
        // 把出站通道摘掉（等价于"启动编排还没走到装配网关"）
        *state.gateway.write().expect("写锁") = None;

        let err = test_provider(&state, &secrets(), "p1", None)
            .await
            .expect_err("拿不到出站通道必须报错");
        assert_eq!(err.code, "TA-3002");
        assert!(err.message.contains("出站通道"), "{}", err.message);
    }

    /// 服务商不存在 → `TA-1004`，且**没发任何请求**。
    #[tokio::test]
    async fn provider_test_for_an_unknown_provider_is_an_honest_error() {
        let upstream = spawn_upstream(vec![Reply::json(200, models_body(&["m-1"]))]).await;
        let state =
            fixture("test-noprov", config_with(&upstream.base_url(), serde_json::json!({}))).await;

        let err = test_provider(&state, &secrets(), "没有这个服务商", None)
            .await
            .expect_err("服务商不存在必须报错");
        assert_eq!(err.code, "TA-1004");
        assert!(err.message.contains("服务商不存在"), "{}", err.message);
        assert!(upstream.seen().is_empty());
    }

    /// 一把启用中的凭据都没有 → 如实报错，**不发请求**（发出去只会得到 401，那是假故障）。
    #[tokio::test]
    async fn provider_test_without_a_credential_refuses_instead_of_faking_a_401() {
        let upstream = spawn_upstream(vec![Reply::json(200, models_body(&["m-1"]))]).await;
        let mut config = config_with(&upstream.base_url(), serde_json::json!({}));
        config["credentials"][0]["enabled"] = serde_json::json!(false);
        let state = fixture("test-nocred", config).await;

        let err = test_provider(&state, &secrets(), "p1", None)
            .await
            .expect_err("没有启用中的凭据必须报错");
        assert_eq!(err.code, "TA-1004");
        assert!(err.message.contains("还没有启用中的凭据"), "{}", err.message);
        assert!(upstream.seen().is_empty(), "没有密钥就不该发请求");
    }

    // ---------------------------------------------------------------- model_import_from_provider

    /// 扫描：只认上游 `data[].id`，派生建议 id，标出已存在的，空结果如实说空。
    #[tokio::test]
    async fn import_scans_the_upstream_list_and_marks_what_already_exists() {
        let upstream = spawn_upstream(vec![Reply::json(
            200,
            models_body(&["gpt-4o", "gpt-4o-2024-08-06", "openai/o3-mini", "deepseek-chat"]),
        )])
        .await;
        let state = fixture(
            "import-ok",
            config_with(
                &upstream.base_url_with_version(),
                serde_json::json!({ "models": [{ "id": "gpt-4o" }] }),
            ),
        )
        .await;

        let preview = import_models_from_provider(&state, &secrets(), "p1", None)
            .await
            .expect("上游 200，扫描不该报错");

        let rows: Vec<(String, String, bool)> = preview
            .models
            .iter()
            .map(|m| (m.upstream_model.clone(), m.suggested_id.clone(), m.exists))
            .collect();
        assert_eq!(
            rows,
            vec![
                ("gpt-4o".to_string(), "gpt-4o".to_string(), true),
                ("gpt-4o-2024-08-06".to_string(), "gpt-4o".to_string(), true),
                ("openai/o3-mini".to_string(), "o3-mini".to_string(), false),
                ("deepseek-chat".to_string(), "deepseek-chat".to_string(), false),
            ],
            "{rows:?}"
        );
        assert!(preview.skipped.is_empty(), "{:?}", preview.skipped);
        assert!(!preview.preview_token.is_empty());

        let head = upstream.seen()[0].clone();
        assert!(head.starts_with("GET /v1/models"), "base_url 里的 /v1 不许被吃掉：{head}");
        assert!(head.contains("authorization: Bearer sk-命令层测试"), "{head}");
    }

    /// `prefix_filter` 真的过滤（按上游名的前缀），且前缀过滤后仍然只来自后端。
    #[tokio::test]
    async fn import_applies_the_prefix_filter() {
        let upstream = spawn_upstream(vec![Reply::json(
            200,
            models_body(&["gpt-4o", "gpt-4o-mini", "claude-3-5-sonnet"]),
        )])
        .await;
        let state =
            fixture("import-prefix", config_with(&upstream.base_url(), serde_json::json!({})))
                .await;

        let preview = import_models_from_provider(&state, &secrets(), "p1", Some("gpt-"))
            .await
            .expect("扫描成功");

        let got: Vec<&str> = preview.models.iter().map(|m| m.upstream_model.as_str()).collect();
        assert_eq!(got, vec!["gpt-4o", "gpt-4o-mini"], "前缀过滤之后不许混进别的");
    }

    /// 派生不出合法 id 的上游模型 → 进 `skipped` 带原因，**不硬凑**一个 id。
    #[tokio::test]
    async fn import_skips_names_it_cannot_derive_a_legal_id_from() {
        let upstream =
            spawn_upstream(vec![Reply::json(200, models_body(&["!!!", "ok-model"]))]).await;
        let state =
            fixture("import-skip", config_with(&upstream.base_url(), serde_json::json!({}))).await;

        let preview =
            import_models_from_provider(&state, &secrets(), "p1", None).await.expect("扫描成功");

        assert_eq!(preview.models.len(), 1, "{:?}", preview.models);
        assert_eq!(preview.models[0].suggested_id, "ok-model");
        assert_eq!(preview.skipped.len(), 1, "{:?}", preview.skipped);
        assert_eq!(preview.skipped[0].upstream_model, "!!!");
        assert!(!preview.skipped[0].reason.is_empty(), "skip 必须带原因");
    }

    /// 上游 404 → **报错**，绝不把状态码编成一份清单（`probe.rs` 的约定）。
    #[tokio::test]
    async fn import_reports_a_404_instead_of_inventing_a_model_list() {
        let upstream = spawn_upstream(vec![Reply::json(404, "no such route")]).await;
        let state =
            fixture("import-404", config_with(&upstream.base_url(), serde_json::json!({}))).await;

        let err = import_models_from_provider(&state, &secrets(), "p1", None)
            .await
            .expect_err("404 绝不能变成清单");
        assert_eq!(err.code, "TA-1004");
        assert!(!err.message.is_empty());
    }

    // ---------------------------------------------------------------- model_apply_import

    /// 只写 `selected` 里、且**此刻**还不在配置里的那些模型。
    #[tokio::test]
    async fn apply_import_writes_only_the_selected_models() {
        let upstream = spawn_upstream(vec![Reply::json(
            200,
            models_body(&["gpt-4o", "o3-mini", "deepseek-chat"]),
        )])
        .await;
        let state = fixture(
            "apply-selected",
            config_with(
                &upstream.base_url(),
                serde_json::json!({ "models": [{ "id": "gpt-4o" }] }),
            ),
        )
        .await;

        let preview =
            import_models_from_provider(&state, &secrets(), "p1", None).await.expect("扫描成功");
        let result = apply_model_import(
            &state,
            &preview.preview_token,
            &["o3-mini".to_string(), "deepseek-chat".to_string()],
        )
        .await
        .expect("应用导入应当成功");

        assert!(result.applied);
        let ids: Vec<String> = state.doc().models.iter().map(|m| m.id.clone()).collect();
        assert_eq!(
            ids,
            vec!["gpt-4o".to_string(), "o3-mini".to_string(), "deepseek-chat".to_string()]
        );
    }

    /// 勾了"已存在"的行不会写出重复模型（`exists=true` 的默认不勾，但用户可能手滑全选）。
    #[tokio::test]
    async fn apply_import_never_duplicates_an_existing_model() {
        let upstream = spawn_upstream(vec![Reply::json(200, models_body(&["gpt-4o"]))]).await;
        let state = fixture(
            "apply-dup",
            config_with(
                &upstream.base_url(),
                serde_json::json!({ "models": [{ "id": "gpt-4o" }] }),
            ),
        )
        .await;

        let preview =
            import_models_from_provider(&state, &secrets(), "p1", None).await.expect("扫描成功");
        assert!(preview.models[0].exists, "已存在的必须标出来");

        let result = apply_model_import(&state, &preview.preview_token, &["gpt-4o".to_string()])
            .await
            .expect("应用导入应当成功");
        assert!(result.applied);
        assert_eq!(state.doc().models.len(), 1, "重复写没有意义：库里不该出现第二条 gpt-4o");
    }

    /// 同一个 token 可以分几次把扫描结果加进来（**取用不消耗**，5 分钟内有效）。
    #[tokio::test]
    async fn apply_import_can_be_called_again_with_the_same_token() {
        let upstream =
            spawn_upstream(vec![Reply::json(200, models_body(&["a-model", "b-model"]))]).await;
        let state =
            fixture("apply-twice", config_with(&upstream.base_url(), serde_json::json!({}))).await;

        let preview =
            import_models_from_provider(&state, &secrets(), "p1", None).await.expect("扫描成功");
        apply_model_import(&state, &preview.preview_token, &["a-model".to_string()])
            .await
            .expect("第一次导入");
        apply_model_import(&state, &preview.preview_token, &["b-model".to_string()])
            .await
            .expect("第二次导入（同一个 token）");

        let ids: Vec<String> = state.doc().models.iter().map(|m| m.id.clone()).collect();
        assert_eq!(ids, vec!["a-model".to_string(), "b-model".to_string()]);
    }

    /// token 未知/过期 → `TA-1004`，消息说清"预览已过期，请重新扫描"。
    #[tokio::test]
    async fn apply_import_with_an_unknown_token_asks_for_a_rescan() {
        let upstream = spawn_upstream(vec![Reply::json(200, models_body(&["m-1"]))]).await;
        let state =
            fixture("apply-bad-token", config_with(&upstream.base_url(), serde_json::json!({})))
                .await;

        let err = apply_model_import(&state, "pv_从没见过的", &["m-1".to_string()])
            .await
            .expect_err("未知 token 必须报错");
        assert_eq!(err.code, "TA-1004");
        assert!(err.message.contains("预览已过期，请重新扫描"), "{}", err.message);
        assert!(state.doc().models.is_empty(), "报错就不许写库");
    }

    /// 预览**超过 TTL** 后同样按"过期"处理（不是"没有这把 token"这种含糊说法）。
    ///
    /// 直接往缓存里塞一份"5 分钟之前存下的"预览：等价于用户把向导放了很久才点「添加选中」。
    #[tokio::test]
    async fn apply_import_for_an_expired_preview_asks_for_a_rescan() {
        let state =
            fixture("apply-expired", config_with("http://127.0.0.1:1", serde_json::json!({})))
                .await;
        let stale_now = state.clock.now_ms() - PREVIEW_TTL_MS - 1;
        let token = state.import_previews.put(
            "p1",
            vec![PreviewModel {
                upstream_model: "m-1".to_string(),
                suggested_id: "m-1".to_string(),
                exists: false,
            }],
            Vec::new(),
            stale_now,
        );

        let err = apply_model_import(&state, &token, &["m-1".to_string()])
            .await
            .expect_err("过期预览必须报错");
        assert_eq!(err.code, "TA-1004");
        assert!(err.message.contains("预览已过期，请重新扫描"), "{}", err.message);
        assert!(state.doc().models.is_empty(), "报错就不许写库");
    }

    /// 空 `selected` 是**调用方写错了**（"什么都没改但报成功"最难排查）→ 报错。
    #[tokio::test]
    async fn apply_import_with_an_empty_selection_is_a_malformed_request() {
        let upstream = spawn_upstream(vec![Reply::json(200, models_body(&["m-1"]))]).await;
        let state =
            fixture("apply-empty", config_with(&upstream.base_url(), serde_json::json!({}))).await;

        let preview =
            import_models_from_provider(&state, &secrets(), "p1", None).await.expect("扫描成功");
        let err = apply_model_import(&state, &preview.preview_token, &[])
            .await
            .expect_err("空清单必须报错");
        assert_eq!(err.code, "TA-1004");
        assert!(state.doc().models.is_empty());
    }

    /// 选了**不在这次预览里**的 id → 报错并点名（调用方与后端不同步，不许静默忽略）。
    #[tokio::test]
    async fn apply_import_rejects_ids_that_are_not_in_the_preview() {
        let upstream = spawn_upstream(vec![Reply::json(200, models_body(&["m-1"]))]).await;
        let state =
            fixture("apply-unknown", config_with(&upstream.base_url(), serde_json::json!({})))
                .await;

        let preview =
            import_models_from_provider(&state, &secrets(), "p1", None).await.expect("扫描成功");
        let err = apply_model_import(&state, &preview.preview_token, &["凭空捏造的".to_string()])
            .await
            .expect_err("预览里没有的 id 必须报错");
        assert_eq!(err.code, "TA-1004");
        assert!(err.message.contains("凭空捏造的"), "要点名是哪个 id：{}", err.message);
        assert!(state.doc().models.is_empty());
    }

    // ---------------------------------------------------------------- 派生规则（纯函数）

    /// 派生规则**逐条**钉住（规则变了必须留下修改痕迹）。
    #[test]
    fn suggested_id_derivation_rules() {
        let cases = [
            ("gpt-4o", "gpt-4o"),
            // 只去末尾**一次**日期
            ("gpt-4o-2024-08-06", "gpt-4o"),
            ("claude-3-5-sonnet-20241022", "claude-3-5-sonnet"),
            ("model_2024.08.06", "model"),
            // 日期后面还有东西 → 不动（那不是版本后缀）
            ("gpt-4o-2024-08-06-preview", "gpt-4o-2024-08-06-preview"),
            // 厂商路径前缀去掉
            ("openai/o3-mini", "o3-mini"),
            // 非法字符规范化 + 去掉首尾分隔符
            ("Qwen/Qwen2.5 7B!", "Qwen2.5-7B"),
        ];
        for (upstream, expected) in cases {
            assert_eq!(
                derive_suggested_id(upstream).as_deref(),
                Ok(expected),
                "上游名 `{upstream}` 的派生结果不对"
            );
        }

        // 派生不出 id 的必须**如实报错**，而不是凑一个
        for bad in ["!!!", "/", "openai/", "   "] {
            assert!(derive_suggested_id(bad).is_err(), "`{bad}` 派生不出合法 id，必须进 skipped");
        }
    }

    /// `exists` 判定与 `channel.model` 的解析同口径：**别名也算已存在**。
    #[test]
    fn exists_uses_the_same_id_or_alias_resolution_as_channels() {
        let doc: ConfigDoc = serde_json::from_value(serde_json::json!({
            "models": [{ "id": "gpt-4o", "aliases": ["gpt4o"] }]
        }))
        .expect("配置能解析");
        assert!(model_exists(&doc, "gpt-4o"));
        assert!(model_exists(&doc, "gpt4o"), "别名也算已存在");
        assert!(!model_exists(&doc, "gpt-4o-mini"));
    }

    /// 夹具自证：`provider_of` 找到的真是那份配置里的服务商。
    #[test]
    fn provider_lookup_is_not_a_stub() {
        let doc: ConfigDoc =
            serde_json::from_value(config_with("http://127.0.0.1:1", serde_json::json!({})))
                .expect("配置能解析");
        let found = provider_of(&doc, &ProviderId::new("p1")).expect("p1 必须找得到");
        assert_eq!(found.base_url, "http://127.0.0.1:1");
        assert!(provider_of(&doc, &ProviderId::new("p2")).is_err());
    }

    /// 夹具自证：`CredentialId` / `LaneId` 的构造没写错（防止上面的断言对着空配置成立）。
    #[test]
    fn fixture_ids_are_the_ones_the_commands_look_for() {
        assert_eq!(CredentialId::new("p1-default").as_str(), "p1-default");
        assert_eq!(LaneId::new("lane-jp").as_str(), "lane-jp");
        let doc: ConfigDoc = serde_json::from_value(config_with("http://x", serde_json::json!({})))
            .expect("配置能解析");
        assert_eq!(doc.credentials[0].id.as_str(), "p1-default");
        assert_eq!(doc.credentials[0].provider.as_str(), "p1");
    }

    /// `Model` 的构造必须给出全部字段（`Model` 没有 `Default`）——
    /// 这条测试与 `apply_import` 里的写法共用同一份字段清单。
    #[test]
    fn new_model_carries_only_the_id_the_contract_requires() {
        let model = Model {
            id: "m".to_string(),
            aliases: Vec::new(),
            display: DisplayConfig::default(),
            expose: true,
            extra: serde_json::Map::new(),
        };
        let back: Model =
            serde_json::from_value(serde_json::to_value(&model).unwrap()).expect("能往返");
        assert_eq!(back.id, "m");
        assert!(back.expose);
    }

    /// 日志里没有密钥：源码级断言（`docs/14 §11`：日志文件是要被分享出去的）。
    #[test]
    fn the_source_never_logs_a_secret() {
        let src = include_str!("upstream.rs");
        let production = src.split("#[cfg(test)]").next().unwrap_or(src);
        for needle in ["secret = %", "plaintext = %", "{plaintext}", "{secret}", "key = %"] {
            assert!(!production.contains(needle), "密钥不许进日志/结构化字段：{needle}");
        }
    }

    /// 出站只有一条路：生产段里不许出现第二条（`reqwest` 之类）。
    #[test]
    fn the_production_slice_has_no_second_egress_path() {
        let src = include_str!("upstream.rs");
        let production = src.split("#[cfg(test)]").next().unwrap_or(src);
        for forbidden in ["reqwest::", "TcpStream", "hyper::Client", "ureq::"] {
            assert!(
                !production.contains(forbidden),
                "`{forbidden}` 出现在命令层 = 绕过线路策略的第二条出站路径"
            );
        }
        assert!(production.contains("UpstreamProbe::new("), "探测必须走网关的 UpstreamProbe");
    }
}
