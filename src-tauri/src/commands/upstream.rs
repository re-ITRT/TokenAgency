//! 上游探测类命令：`provider_test` / `provider_echo` /
//! `model_import_from_provider` / `model_apply_import`。
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
//! # 语义分档（由 `probe.rs` 给出）
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

use ta_core::domain::capabilities::channel_capability_mismatches;
use ta_core::domain::{AuthScheme, ChannelConfig, ConfigDoc, Provider};
use ta_core::error::TaError;
// 补齐上游声明的能力前先校验（`validate` 由这个 trait 提供）
use ta_core::config::ConfigValidator;
use ta_core::types::{CredentialId, LaneId, Protocol, ProviderId};
use ta_gateway::passthrough;
use ta_gateway::pipeline::SecretResolver;
use ta_gateway::probe::{probe_lane, ProbeProviderRequest, UpstreamProbe};
use ta_gateway::traits::OutboundDialer;
use ta_store::ModelCatalogStore;

use crate::commands::catalog::save_validated;
use crate::commands::config::ApplyResult;
use crate::commands::entity::applied_with_channels;
use crate::commands::{IpcError, IpcResult};
use crate::gateway::SecretsAdapter;
use crate::import_preview::{PreviewModel, PreviewSkip};
use crate::state::AppState;

// ================================================================ DTO（线上契约）

/// `ProbeResult`（`bindings.ts` /）。
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

/// `ImportPreview`（`bindings.ts` /）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
pub struct ImportPreview {
    /// 取回这次扫描结果的句柄（**进程内存**，5 分钟 TTL，不落库）
    pub preview_token: String,
    pub models: Vec<ImportModelRow>,
    pub skipped: Vec<ImportSkippedRow>,
}

/// `ModelCatalogEntry`（`bindings.ts` /）：模型目录里的一行。
///
/// 目录是**观察记录**（"这家扫描到过哪些模型"，落 SQLite），`added` 才是用户意图
/// （配置里真有 `Model` + 通道）。两者混成一个字段就会让"扫描"等于"接入"，
/// 而用户明确要的是**扫了先记着、用的时候勾一下**。
///
/// `added` / `channel_id` / `channel_count` / `enabled` 都是**按当前配置现算**的
/// （不是目录里存的）：目录不该复制配置的状态，否则两边必然漂移。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
pub struct ModelCatalogEntry {
    pub provider_id: String,
    pub upstream_model: String,
    /// 扫描派生出来的逻辑名（接入时以它建 `Model`）
    pub suggested_id: String,
    /// **上游原样返回的对象：我们不改写、不补字段。**
    ///
    /// 它来自扫描时 `GET {base_url}/models` 里那一条（`data[i]`）的**完整** JSON ——
    /// 我们不认识的字段（`owned_by` / `created` / `permission` / 中转自造字段……）也都在，
    /// 键序也没被重排（`serde_json` 开了 `preserve_order`）。
    /// 用户要看的就是"上游真的声明了什么"；给模型填能力字段时它是**唯一**可核对的来源
    /// （：上游没给的字段一个都不编）。
    ///
    /// 上游那一条不是对象、或那次没记到 → `{}`（**绝不是**我们编出来的对象）。
    ///
    /// `#[specta(type = Any)]`：`serde_json::Value` 是递归类型，specta 展开它会产出无限 TS 类型
    /// （与 `IpcError.details` 同一个坑）。这里它真的是任意 JSON。
    #[specta(type = specta_typescript::Any)]
    pub raw: serde_json::Value,
    /// 上一次扫描返回过它吗（false = 行还在但这次没见到）
    pub seen_in_last_scan: bool,
    pub first_seen_ms: i64,
    pub last_seen_ms: i64,
    pub added: bool,
    /// 这条目录行对应的**全部**通道 id（同一上游模型可能挂在多份凭据下）。
    ///
    /// 数组而不是"第一条 + 条数"：取消勾选要**把这几个都停用**，只停第一条会让勾选框
    /// 下一帧自己弹回勾上（`enabled` 的语义是"有一个启用就算启用"）。
    pub channel_ids: Vec<String>,
    pub enabled: bool,
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
pub(crate) async fn probe_secret(
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

/// `provider_test({ id, model? }) -> ProbeResult`。
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

// ================================================================ provider_echo

/// 回显用的那张 1×1 PNG（base64）。**只为产生"有图"的形状**，不是用户数据。
const ECHO_IMAGE_PNG_B64: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";

/// 密钥类请求头：一律打码。
///
/// 界面上的头是给人看**结构**的（有没有带、带了哪个），不是拿来复制密钥的；
/// 回显把 `Authorization` 原文照搬出去，等于给"导出诊断"开了一条泄密缝。
fn is_secret_header(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.contains("authorization")
        || n.contains("api-key")
        || n.contains("apikey")
        || n.contains("cookie")
        || n.contains("token")
        || n.contains("secret")
        || n == "key"
}

fn redacted_headers(headers: &http::HeaderMap) -> std::collections::BTreeMap<String, String> {
    let mut out = std::collections::BTreeMap::new();
    for (name, value) in headers {
        let key = name.as_str().to_ascii_lowercase();
        let shown = if is_secret_header(&key) {
            "***".to_string()
        } else {
            value.to_str().map(str::to_string).unwrap_or_else(|_| "***".to_string())
        };
        out.insert(key, shown);
    }
    out
}

/// 只描述**结构**（键 → 类型名），不搬内容：界面上"形状"就该是形状。
fn value_shape(v: &serde_json::Value) -> serde_json::Value {
    use serde_json::{json, Value};
    match v {
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, x) in map {
                out.insert(k.clone(), value_shape(x));
            }
            Value::Object(out)
        }
        Value::Array(items) => match items.first() {
            Some(first) => json!({ "items": value_shape(first), "len": items.len() }),
            None => json!({ "items": "empty", "len": 0 }),
        },
        Value::String(_) => json!("string"),
        Value::Number(_) => json!("number"),
        Value::Bool(_) => json!("bool"),
        Value::Null => json!("null"),
    }
}

/// 回显用的**最小**请求：按服务商协议拼路径与体，返回 `(path, body)`。
///
/// 为什么是最小请求而不是重放上一次真实请求：回显要回答的是**形状对不上**
/// （字段在上游被吞/被改写），而重放用户请求会把一次诊断变成一次真实付费调用，
/// 还会把用户内容带进一个"给人看"的视图里。
fn echo_request(protocol: Protocol, model: &str, with_image: bool) -> (String, serde_json::Value) {
    use serde_json::json;
    let image_url = format!("data:image/png;base64,{ECHO_IMAGE_PNG_B64}");
    match protocol {
        Protocol::Anthropic => (
            "/messages".to_string(),
            json!({
                "model": model,
                "max_tokens": 1,
                "messages": [{
                    "role": "user",
                    "content": if with_image {
                        json!([
                            { "type": "text", "text": "ping" },
                            { "type": "image", "source": {
                                "type": "base64", "media_type": "image/png",
                                "data": ECHO_IMAGE_PNG_B64
                            }}
                        ])
                    } else {
                        json!([{ "type": "text", "text": "ping" }])
                    },
                }],
            }),
        ),
        Protocol::OpenaiResponses => (
            "/responses".to_string(),
            json!({
                "model": model,
                "max_output_tokens": 16,
                "input": [{
                    "role": "user",
                    "content": if with_image {
                        json!([
                            { "type": "input_text", "text": "ping" },
                            { "type": "input_image", "image_url": image_url }
                        ])
                    } else {
                        json!([{ "type": "input_text", "text": "ping" }])
                    },
                }],
            }),
        ),
        Protocol::Gemini => (
            format!("/models/{model}:generateContent"),
            json!({
                "contents": [{
                    "role": "user",
                    "parts": if with_image {
                        json!([
                            { "text": "ping" },
                            { "inline_data": { "mime_type": "image/png", "data": ECHO_IMAGE_PNG_B64 } }
                        ])
                    } else {
                        json!([{ "text": "ping" }])
                    },
                }],
            }),
        ),
        // Openai / Custom：同一套 OpenAI 兼容形状
        Protocol::Openai | Protocol::Custom => (
            "/chat/completions".to_string(),
            json!({
                "model": model,
                "max_tokens": 1,
                "stream": false,
                "messages": [{
                    "role": "user",
                    "content": if with_image {
                        json!([
                            { "type": "text", "text": "ping" },
                            { "type": "image_url", "image_url": { "url": image_url } }
                        ])
                    } else {
                        json!("ping")
                    },
                }],
            }),
        ),
    }
}

/// `provider_echo({ id, model, with_image }) -> EchoResult` 的返回。
///
/// 字段与 `bindings.ts` 的 `EchoResult` 逐字段对齐（`sent_*` = 我们发出去的，
/// `upstream_*` = 上游回来的），中间只有 [`echo_provider`] 一处转换。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, specta::Type)]
pub struct EchoResult {
    /// 我们发出去的请求头（**密钥类一律 `***`**）
    pub sent_headers: std::collections::BTreeMap<String, String>,
    /// 上游回来的响应头（同一条打码规则）
    pub upstream_headers: std::collections::BTreeMap<String, String>,
    /// 我们发出去的请求体**形状**（键 → 类型名，不含内容）
    #[specta(type = specta_typescript::Any)]
    pub sent_body_shape: serde_json::Value,
    /// 上游响应体：能解析成 JSON 就给对象，否则给**有界**的 `_raw` 片段
    /// `Value` 是递归类型：必须走 `Any`，否则绑定导出整体失败（见 `commands/mod.rs` 的说明）。
    #[specta(type = specta_typescript::Any)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream_body: Option<serde_json::Value>,
    /// 到拿到响应头为止的真实耗时（单调时钟）
    pub latency_ms: u32,
}

/// 实现体（测试直接调它，避开 `State<'_, AppState>` 的构造）。
///
/// 出站**只有一条路**：`UpstreamProbe` 持有网关那一个 `OutboundDialer`，
/// 也就是与转发管线同一个拨号器 —— 线路策略在那后面。这里不另开 client。
pub async fn echo_provider(
    state: &AppState,
    secrets: &dyn SecretResolver,
    id: &str,
    model: &str,
    with_image: bool,
) -> IpcResult<EchoResult> {
    let doc = state.doc();
    let provider_id = ProviderId::new(id);
    let provider = provider_of(&doc, &provider_id)?;
    let dialer = outbound_dialer(state)?;
    let lane = probe_lane(&doc, &provider_id);
    let secret = probe_secret(&doc, provider, secrets).await?;

    // 先记下"发出去的头"，再补 Content-Type（补的这个不是秘密，一起记）
    let mut headers = passthrough::auth_headers(Some(provider), &secret);
    headers.insert(http::header::CONTENT_TYPE, http::HeaderValue::from_static("application/json"));
    let sent_headers = redacted_headers(&headers);

    let (path, payload) = echo_request(provider.protocol, model, with_image);
    let sent_body_shape = value_shape(&payload);
    // `auth.scheme: query` 时密钥只能进 URL（对别的 scheme 原样返回）
    let url = passthrough::apply_query_auth(
        &passthrough::join_upstream_url(&provider.base_url, &path, None),
        &provider.auth,
        secret.as_str(),
    );
    let body = serde_json::to_vec(&payload).map_err(|e| {
        IpcError::new("TA-3003", "tokenagency_internal", format!("拼回显请求体失败：{e}"))
    })?;

    let outcome = UpstreamProbe::new(dialer)
        .fetch(&lane, "POST", &url, headers, bytes::Bytes::from(body))
        .await
        .map_err(|e| {
            IpcError::new(
                "TA-3002",
                "tokenagency_upstream_unavailable",
                format!("回显请求没拿到任何响应（线路不可用或连不通）：{e}"),
            )
        })?;

    let upstream_body = if outcome.body.is_empty() {
        None
    } else {
        Some(match serde_json::from_slice::<serde_json::Value>(&outcome.body) {
            Ok(v) => v,
            Err(_) => serde_json::json!({
                "_raw": String::from_utf8_lossy(&outcome.body).chars().take(2000).collect::<String>(),
                "_truncated": outcome.truncated,
            }),
        })
    };

    tracing::debug!(
        provider_id = %provider_id,
        lane_id = %lane,
        status = outcome.status,
        elapsed_ms = outcome.latency_ms,
        "字段回显完成（响应体只在界面显示，不进日志）"
    );

    Ok(EchoResult {
        sent_headers,
        upstream_headers: redacted_headers(&outcome.headers),
        sent_body_shape,
        upstream_body,
        latency_ms: outcome.latency_ms,
    })
}

/// `provider_echo({ id, model, with_image }) -> EchoResult`。
///
/// 回答"谁吞了字段"：把**我们发的**与**上游回的**摆在一起逐字段看。
#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn provider_echo(
    state: State<'_, AppState>,
    id: String,
    model: String,
    with_image: bool,
) -> IpcResult<EchoResult> {
    let model = model.trim().to_string();
    if model.is_empty() {
        return Err(IpcError::new(
            "TA-3004",
            "tokenagency_invalid_args",
            "回显必须给一个模型名（空名发出去只会得到上游的 400）",
        ));
    }
    let secrets = SecretsAdapter::with_defaults();
    echo_provider(&state, &secrets, &id, &model, with_image).await
}

// ================================================================ model_import_from_provider

/// 一条上游模型名 → 建议的逻辑模型 id。
///
/// 规则**就这么四条**（要求"简单规则 + 写在注释里"，不做智能改名）：
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

/// 一次扫描解析出来的一行：**预览行**（给向导）+ **上游原文**（给目录）。
///
/// 为什么不把 `raw` 塞进 [`PreviewModel`]：预览是**进程内存、5 分钟就没了**的东西
/// （它服务"这次就勾着加"），而原文要**落 SQLite**（：扫过就记得住）。
/// 两件事生命周期不同，混成一个结构会让"预览过期"顺手把原文也丢掉。
struct ScannedModel {
    /// 派生结果（上游名 + 建议逻辑名 + 此刻是否已存在）
    preview: PreviewModel,
    /// `data[i]` 的**完整**对象，一字不改（`ta_gateway::probe::UpstreamModelEntry::raw`）
    raw: serde_json::Value,
}

/// 实现体（测试直接调它）。
///
/// 清单**只来自上游**：`list_upstream_models` 在任何非 2xx / 形状不对 / 体被截断时都返回
/// `Err`（`probe.rs` 的约定），所以这里没有"用状态码编一份清单"的余地。
///
/// 用的是 [`UpstreamProbe::list_upstream_models_with_raw`]：与只取名字那条**同一条腿、
/// 同一套错误口径**（后者现在是它的薄包装），多的只是把每条的上游原文一起带回来 ——
/// 目录要记的正是"上游给了什么"，不是我们派生出来的名字。
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

    let upstream_entries = UpstreamProbe::new(dialer)
        .list_upstream_models_with_raw(
            &lane,
            &provider.base_url,
            passthrough::auth_headers(Some(provider), &secret),
            Some((&provider.auth, secret.as_str())),
        )
        .await?;

    // 空串/纯空白的前缀 = 没给过滤条件（免得 `""` 变成"全都要"之外的第三种语义）
    let prefix = prefix_filter.map(str::trim).filter(|p| !p.is_empty());

    let mut scanned: Vec<ScannedModel> = Vec::new();
    let mut skipped: Vec<PreviewSkip> = Vec::new();
    for entry in upstream_entries {
        if prefix.is_some_and(|p| !entry.id.starts_with(p)) {
            continue;
        }
        match derive_suggested_id(&entry.id) {
            Ok(suggested_id) => {
                // 同一个建议 id 的多条上游模型**都保留**（上游确实有两份，藏起来就是隐瞒）；
                // 应用时后一条会因为"此刻已存在"被跳过，所以不会写出重复模型。
                let exists = model_exists(&doc, &suggested_id);
                scanned.push(ScannedModel {
                    preview: PreviewModel {
                        // 身份用网关规范化的 `id`（trim 过）。原文里的 `id` 保留上游原样
                        // （可能带前后空白）—— 那是**两个字段**，不是同一个东西。
                        upstream_model: entry.id,
                        suggested_id,
                        exists,
                    },
                    raw: entry.raw,
                });
            }
            // 派生不出的进 skipped 并带原因 —— **不硬凑**一个 id
            Err(reason) => skipped.push(PreviewSkip { upstream_model: entry.id, reason }),
        }
    }

    let models: Vec<PreviewModel> = scanned.iter().map(|s| s.preview.clone()).collect();

    // 条数写进 message（`§5` 的白名单里没有"计数"字段，不乱借一个名字）
    tracing::debug!(
        provider_id = %provider_id,
        "上游模型清单已解析：{} 条候选、{} 条派生不出 id（清单只来自上游，空结果就是如实空结果）",
        models.len(),
        skipped.len()
    );

    // 扫描 = **全量记录**（不是接入）：把这次的清单写进模型目录。
    //
    // 顺序刻意放在"返回预览之前"：目录落不下来就不算扫描成功 —— 那就如实报错，
    // 而不是回一个"扫到了"的预览、用户关掉向导后什么都没有（那正是本轮要修的缺陷）。
    persist_model_catalog(state, &provider_id, &scanned).await?;

    // 预览放**进程内存**（5 分钟 TTL，不落库）：它讲的是"上游现在有什么"。
    // 目录（上面）与预览是**两件事**：目录活得久、预览只服务"这次就勾着加"。
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

/// `model_import_from_provider({ provider_id, prefix_filter? }) -> ImportPreview`。
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

// ================================================================ 模型目录

/// 扫描结果 → 目录：逐条 upsert，然后**以本次为准**清掉本次没返回的。
///
/// 唯一不删的例外：**还接着通道**的那些行（`keep`）—— 用户明确接过它们，
/// 删掉会让界面出现"有一条通道、但模型区看不到这个模型"的鬼状态。
/// 它们保留并标成「本次未返回」（`seen_in_last_scan = 0`），让用户自己决定。
///
/// **没有"手动添加"**（用户要求）：目录里的每一行都必须来自一次真实扫描 ——
/// 手填一行会让"目录 = 上游真实提供了什么"这条不变量失效，而自动化全建在它上面。
///
/// 每行连同**上游原文对象**一起落库：`raw` 就是 `data[i]` 本身，
/// 我们不挑字段、不补字段。派生不出的（`skipped`）**不进目录**：
/// 它连"接入"都做不到，记下来只会变成一行点不动的东西。
async fn persist_model_catalog(
    state: &AppState,
    provider_id: &ProviderId,
    models: &[ScannedModel],
) -> IpcResult<()> {
    let now = state.clock.now_ms();
    // ① 这家全部标成"本次没见过"（下面 upsert 会把本次返回的标回 1）
    state.model_catalog.mark_all_unseen(provider_id.as_str()).await?;

    // ② 本次返回的逐条写进去。`first_seen_ms` 只在**第一次**写入时作数
    //    （store 的 upsert 在冲突时保留它），所以"首次见到"这个事实不会被重扫抹掉。
    for m in models {
        state
            .model_catalog
            .upsert(&ta_store::ModelCatalogRow {
                provider_id: provider_id.as_str().to_string(),
                upstream_model: m.preview.upstream_model.clone(),
                suggested_id: m.preview.suggested_id.clone(),
                // 上游原文：**原样**（store 里那一列只接受对象；不是对象就落 `{}`，不补字段）
                raw_json: m.raw.clone(),
                seen_in_last_scan: true,
                first_seen_ms: now,
                last_seen_ms: now,
            })
            .await?;
    }

    // ②.5 **上游声明过的能力 → 补进还没声明的线路**。
    //
    //     链路：上游 `/models` →（上面刚存的）原文 → **这一步** → 线路能力 → 网关 `/v1/models`
    //     → 客户端（DeepSeek Harness 的「获取可用模型」那一栏）。少了这一步，客户端永远是空的
    //     （用户原话：服务商 → 软件（软件保存信息）↔ 本机访问「没有打通」）。
    //     只补**没声明**的字段（用户填过的绝不覆盖）；一处都没补就不写库、不留快照。
    let mut doc = (*state.doc()).clone();
    let mut filled = 0usize;
    for m in models {
        let hints = ta_core::domain::upstream_hints::UpstreamModelHints::from_upstream_raw(&m.raw);
        filled += doc.adopt_upstream_hints(
            provider_id.as_str(),
            m.preview.upstream_model.as_str(),
            &hints,
        );
    }
    if filled > 0 {
        // 补齐**不能**把整次扫描搞崩：上游声明互不一致时（同一模型两条线路的上下文不同），
        // 「能力一致才可路由」这条校验本来就会拦下整份配置 —— 那就**如实跳过这次补齐**并记日志；
        // 目录照旧写好，界面上该看到的"哪几条线路能力不一致"也照样看得到。
        let report = ta_core::config::TaConfigValidator.validate(&doc);
        if report.ok {
            crate::commands::catalog::save_validated(state, doc, "provider_scan:capabilities")
                .await?;
            tracing::info!(
                provider_id = %provider_id,
                "已把上游声明的能力（上下文窗口/最大输出/模态/工具）补进未声明的线路，共补了 {filled} 处"
            );
        } else {
            tracing::warn!(
                provider_id = %provider_id,
                "上游声明的能力与既有线路的声明不一致：这次不补（补了会 {filled} 处、校验报 {} 处问题）",
                report.errors.len()
            );
        }
    }

    // ③ 清理：本次没返回、但**没有通道**的行删掉。
    //    keep 列表 = 这家服务商所有通道用到的上游名（接入过的东西不因一次扫描消失）。
    let doc = state.doc();
    let keep: Vec<String> = doc
        .channels
        .iter()
        .filter(|c| c.provider == *provider_id && !c.upstream_model.is_empty())
        .map(|c| c.upstream_model.clone())
        .collect();
    let dropped = state.model_catalog.prune_unseen(provider_id.as_str(), &keep).await?;
    if !dropped.is_empty() {
        tracing::debug!(
            provider_id = %provider_id,
            "本次没返回 {} 个已记录的模型，已从目录里清掉（：以本次为准）",
            dropped.len()
        );
    }
    Ok(())
}

/// 目录行 → 线上契约行：`added` 那几项**按当前配置现算**。
///
/// 为什么不存进目录：目录是"上游提供过什么"的**观察**，`added` 是"用户接没接"的
/// **意图**。存进目录就是第二份真相源，配置一改（用户在通道页删了那条通道）两边立刻漂移。
fn catalog_entry(doc: &ConfigDoc, row: &ta_store::ModelCatalogRow) -> ModelCatalogEntry {
    let provider = ProviderId::new(row.provider_id.as_str());
    let wired: Vec<&ChannelConfig> = doc
        .channels
        .iter()
        .filter(|c| c.provider == provider && c.upstream_model == row.upstream_model)
        .collect();
    ModelCatalogEntry {
        provider_id: row.provider_id.clone(),
        upstream_model: row.upstream_model.clone(),
        suggested_id: row.suggested_id.clone(),
        // 上游原文**原样出去**：命令层一个字段都不加、不改（那是 `catalog_entry` 的职责边界，
        // 也是"我们不改写、不补字段"这条承诺的落点）。
        raw: row.raw_json.clone(),
        seen_in_last_scan: row.seen_in_last_scan,
        first_seen_ms: row.first_seen_ms,
        last_seen_ms: row.last_seen_ms,
        added: !wired.is_empty(),
        channel_ids: wired.iter().map(|c| c.id.as_str().to_string()).collect(),
        enabled: wired.iter().any(|c| c.enabled),
    }
}

/// 实现体（测试直接调它）：这家服务商的模型目录 + "接入了没有"。
///
/// 服务商不存在 → 如实报错，**不返回一个空目录** —— 空目录的意思是"扫过、什么都没有"，
/// 那是另一件事（把"服务商没了"渲染成"没有模型"就是在骗人）。
pub async fn model_catalog_entries(
    state: &AppState,
    provider_id: &str,
) -> IpcResult<Vec<ModelCatalogEntry>> {
    let pid = ProviderId::new(provider_id);
    let doc = state.doc();
    provider_of(&doc, &pid)?;
    let rows = state.model_catalog.list(provider_id).await?;
    Ok(rows.iter().map(|r| catalog_entry(&doc, r)).collect())
}

/// `provider_model_catalog({ provider_id }) -> ModelCatalogEntry[]`。
#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn provider_model_catalog(
    state: State<'_, AppState>,
    provider_id: String,
) -> IpcResult<Vec<ModelCatalogEntry>> {
    model_catalog_entries(&state, &provider_id).await
}

/// 实现体（测试直接调它）：把目录里的一条**接到**这家服务商上。
///
/// 三件事：建 `Model`（缺才建）→ 建一条**启用中**的通道 → 落库。
/// 逻辑名**只从目录行取**（入参只有上游名）：界面没有机会传一个与目录不一致的名字。
///
/// 幂等：这家已经有一条同上游名的通道时，一个字节都不写，`applied=true`。
pub async fn attach_catalog_model(
    state: &AppState,
    provider_id: &str,
    upstream_model: &str,
) -> IpcResult<ApplyResult> {
    let pid = ProviderId::new(provider_id);
    let mut doc = (*state.doc()).clone();
    provider_of(&doc, &pid)?;

    let Some(row) = state.model_catalog.get(provider_id, upstream_model).await? else {
        return Err(IpcError::from(TaError::MalformedRequest(format!(
            "模型目录里没有 {upstream_model} 这一条：先扫描一次。\
             （目录只记真实扫描到的模型 —— 手填一个上游没提供的模型会让自动化建立在不存在的模型上）"
        ))));
    };

    // 已经接入过（这家下已有同上游名的通道）→ 幂等返回，不写库
    if doc.channels.iter().any(|c| c.provider == pid && c.upstream_model == row.upstream_model) {
        return Ok(applied_with_channels(0));
    }

    // 走到这里就真要建通道了：这时才要求"至少有一份启用中的凭据"
    let Some(credential) = pick_credential(&doc, &pid) else {
        return Err(no_credential(&pid));
    };

    // **只建线路，不建模型** —— 用户原话："创建完服务商和线路之后，
    // 模型是不自动创建的，是等待用户去分配的"。模型由用户在模型页把线路分配给某些模型时创建，
    // 所以这里**绝不** `doc.models.push(...)`：那样会凭空造出一个"用户没要过、也没有线路"
    // 的模型（它还会因为 0 条线路而无法路由）。
    //
    // 线路 id 用**上游模型名**派生（与前端 `channelIdFor` 新的口径一致）：线路的身份是
    // "服务商 × 凭据 × 上游模型"，逻辑名只是扫描给出的**建议**。
    let channel = ChannelConfig::new(
        channel_id_for(pid.as_str(), credential.as_str(), &row.upstream_model),
        // 遗留字段留空：关系在 `Model.lines` 上
        "",
        pid.clone(),
        credential,
        row.upstream_model.clone(),
    );
    let channel_id = channel.id.clone();
    doc.channels.push(channel);

    // 与通道页**手动建通道**同一条能力规则：绝不静默造一对能力声明矛盾的线路。
    // （`model_apply_import` 那条批量路径的宽松行为是既有事实， 已登记差异。）
    if let Some(detail) =
        channel_capability_mismatches(&doc, channel_id.as_str()).into_iter().next()
    {
        return Err(crate::commands::entity::capability_mismatch_ipc(detail));
    }

    save_validated(state, doc, "provider_model_attach").await?;
    tracing::info!(
        provider_id = %pid,
        upstream_model = %row.upstream_model,
        "扫描条目已建成线路（**没有**创建模型：模型由用户在模型页分配）：新建 1 条线路"
    );
    Ok(applied_with_channels(1))
}

/// `provider_model_attach({ provider_id, upstream_model }) -> ApplyResult`。
#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn provider_model_attach(
    state: State<'_, AppState>,
    provider_id: String,
    upstream_model: String,
) -> IpcResult<ApplyResult> {
    attach_catalog_model(&state, &provider_id, &upstream_model).await
}

// ================================================================ provider_lines_create_all

/// `provider_lines_create_all` 的结果。
#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct LinesCreateAllResult {
    /// 这次**真新建**了几条线路（0 = 目录里每一条都早就有线路了，一条都没写）
    pub created: u32,
    /// 上游目录里一共有多少条（列表里会显示多少行；`created <= total`）
    pub total: u32,
}

/// 「添加服务商就把**所有**线路建出来」（用户第 81 轮的重构）：
/// 把上游目录里的每一条都建成线路，**默认关闭**（`enabled: false`）。
///
/// 之后**只用滑块控制启用 / 关闭**（`channel_set_enabled` / `channel_bulk_set_enabled`），
/// 于是向导与详情页都能只画**一个列表**：上游原文 + 网络代理 + 启用开关 ——
/// "没有添加的线路"也在这个列表里、也能设网络代理（它就是 `enabled: false` 的那一条）。
///
/// 幂等：已经有线路的条目**一个字节都不动**（不覆盖用户改过的 lane / enabled / 优先级），
/// 只补缺的；一条都没补时**不写库**（写一份一模一样的配置只会多一个快照）。
///
/// 仍然**不建模型**（硬规则 1）：模型由用户在模型页把线路分配给它时才出现。
pub async fn create_all_provider_lines(
    state: &AppState,
    provider_id: &str,
    lane: Option<LaneId>,
) -> IpcResult<LinesCreateAllResult> {
    let pid = ProviderId::new(provider_id);
    let mut doc = (*state.doc()).clone();
    provider_of(&doc, &pid)?;
    let rows = state.model_catalog.list(provider_id).await?;
    let total = rows.len() as u32;
    if rows.is_empty() {
        return Err(IpcError::from(TaError::MalformedRequest(format!(
            "{provider_id} 的模型目录还是空的（一条上游模型都没记到）：先扫描一次，\
             再谈「把线路都建出来」—— 目录里没有的东西这里不凭空造"
        ))));
    }

    // 真正要建之前才要求"至少有一份启用中的凭据"（与 `provider_model_attach` 同一条纪律）
    let credential = pick_credential(&doc, &pid);
    let mut created: u32 = 0;
    for row in &rows {
        if doc.channels.iter().any(|c| c.provider == pid && c.upstream_model == row.upstream_model)
        {
            continue; // 已经有了：不动它
        }
        let Some(credential) = credential.clone() else {
            return Err(no_credential(&pid));
        };
        let mut channel = ChannelConfig::new(
            channel_id_for(pid.as_str(), credential.as_str(), &row.upstream_model),
            // 遗留字段留空：关系在 `Model.lines` 上
            "",
            pid.clone(),
            credential,
            row.upstream_model.clone(),
        );
        // 线路**不再逐条写在通道上**（那个字段已删除）：走哪个节点由
        // **服务商**决定。入参给的默认代理在下面统一写到 provider 上。
        // ★ **默认关闭**：新建出来的线路都不参与路由，等用户用滑块逐条（或全选）打开。
        channel.enabled = false;
        doc.channels.push(channel);
        created += 1;
    }

    // 入参给的默认代理写在**服务商**上（通道级 lane 已删除）
    apply_default_lane_to_provider(&mut doc, &pid, lane.as_ref());

    if created == 0 {
        // 幂等：没有要补的，不写库（`created: 0` 如实说"这次一条都没建"）
        return Ok(LinesCreateAllResult { created: 0, total });
    }

    save_validated(state, doc, "provider_lines_create_all").await?;
    // 字段名只能用 白名单里的（`status` = 本次真新建几条，`attempt` = 目录里共几条）
    tracing::info!(
        provider_id = %pid,
        status = created,
        attempt = total,
        "上游目录里的线路已全部建出（**默认关闭**，等用户用滑块启用）；**没有**创建模型"
    );
    Ok(LinesCreateAllResult { created, total })
}

/// `provider_lines_create_all({ provider_id, lane? }) -> LinesCreateAllResult`。
#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn provider_lines_create_all(
    state: State<'_, AppState>,
    provider_id: String,
    lane: Option<LaneId>,
) -> IpcResult<LinesCreateAllResult> {
    create_all_provider_lines(&state, &provider_id, lane).await
}

// ================================================================ model_apply_import

/// 通道 id 的**一段**净化（与前端 `src/pages/channels/model.ts` 的 `idSlug` 同一套）：
/// trim → 小写 → 非 `[a-z0-9]` 一律变 `-` → 合并连续横线 → 去掉首尾横线。
///
/// 用 `char::to_lowercase()`（Unicode 小写）而不是 `to_ascii_lowercase()`：JS 的
/// `String.prototype.toLowerCase()` 就是前者，两者在非 ASCII 字母上会分叉
/// （`İ` 的小写是 `i` + 组合符）。id 本来就是 slug，但派生法必须逐字一致。
pub(crate) fn id_slug(raw: &str) -> String {
    let mut buf = String::with_capacity(raw.len());
    for c in raw.trim().chars() {
        for lowered in c.to_lowercase() {
            if lowered.is_ascii_lowercase() || lowered.is_ascii_digit() {
                buf.push(lowered);
            } else {
                buf.push('-');
            }
        }
    }
    buf.split('-').filter(|p| !p.is_empty()).collect::<Vec<_>>().join("-")
}

/// 派生通道 id：`ch-<provider>-<credential>-<model>`。
///
/// **必须与前端 `channelIdFor` 逐字一致**：这个 id 是"同一条线"的稳定指纹，
/// 两边派生法一旦分叉，界面按 A 判定"还没有这条线"、后端按 B 建出另一条，
/// 用户就会看到重复线路。空段（净化后为空）照前端的做法丢掉。
fn channel_id_for(provider: &str, credential: &str, model: &str) -> String {
    let parts = [id_slug(provider), id_slug(credential), id_slug(model)];
    let mut out = String::from("ch");
    for p in parts.iter().filter(|p| !p.is_empty()) {
        out.push('-');
        out.push_str(p);
    }
    out
}

/// 这次导入该把新通道挂到哪份凭据上（选择顺序）：
/// `<provider>-default`（向导第 1 步建的那一份）→ 该服务商**第一条启用中**的凭据。
///
/// 两步都要求 `enabled`：把通道接到一份停用的凭据上，等于建了一条永远不会有流量的线
/// —— 那正是"界面上看着配好了、实际不通"的假成功。一条启用中的都没有 → `None`，
/// 调用方在**真要建通道**的那一刻如实报 `TA-1004`，
/// **绝不**静默建一条没有凭据的线路。
fn pick_credential(doc: &ConfigDoc, provider: &ProviderId) -> Option<CredentialId> {
    let pool: Vec<&ta_core::domain::Credential> =
        doc.credentials.iter().filter(|c| c.provider == *provider && c.enabled).collect();
    let default_id = format!("{provider}-default");
    pool.iter().find(|c| c.id.as_str() == default_id).or_else(|| pool.first()).map(|c| c.id.clone())
}

/// 一份启用中的凭据都没有时的**人话**错误（`TA-1004`）。
fn no_credential(provider: &ProviderId) -> IpcError {
    IpcError::from(TaError::MalformedRequest(format!(
        "服务商 {provider} 还没有凭据可用（一份都没有，或者全都停用了）：先回第 1 步填密钥、\
         保存凭据，再回来点「添加」——绝不静默建一条没有凭据的线路"
    )))
}

/// 实现体（测试直接调它）。
///
/// 一次导入做**两件事**：把模型写进 `doc.models`（没有才建），
/// 并把它**接到**这个服务商上（建 `ch-<provider>-<credential>-<model>` 这条通道）。
/// 只写模型不建通道，就是第 71 轮的真机缺陷：列表从 `channel_matrix` 派生，
/// 没有通道 → 上面永远空、模型永远显示"未配置"。
///
/// 写库走的是**唯一**那条写路径 [`save_validated`]：校验 → 事务写库 → 留快照 → 换内存快照。
///
/// **幂等**：同一次预览应用两次，第二次既不建模型也不建通道，**且不写库**
/// （写一份一模一样的配置只会多一个快照）；`applied` 仍为真 —— 语义是"命令成功了，没有要改的"。
pub async fn apply_model_import(
    state: &AppState,
    preview_token: &str,
    selected: &[String],
    lane: Option<LaneId>,
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

    // 服务商必须还在：新线路要挂在它下面（不在就报"服务商不存在"，不写出一个悬空引用）
    let provider_id = ProviderId::new(entry.provider_id.as_str());
    provider_of(&doc, &provider_id)?;

    let mut channels_created = 0u32;
    // 凭据**按需**解析（只在真要建线路时才要求有凭据）：全是"已经有线路"的幂等调用
    // 不该因为后来密钥被删掉就报错。
    let mut chosen_credential: Option<CredentialId> = None;

    for row in entry.models.iter() {
        if !wanted.contains(row.suggested_id.as_str()) {
            continue;
        }

        // **只建线路，不建模型**（用户原话："模型是不自动创建的，是等待用户去分配的"）。
        // 所以这里不再找/建 `Model`、也不写 `Model.default_lane`：模型由用户在模型页
        // 把这些线路分配给它时创建。勾一个候选 = 建一条线路。
        //
        // 判重按 `(provider, credential, 上游模型名)` 这个三元组（原意：
        // 三条里任何一条不同就是**另一条线**）—— 线路的身份现在就是这一组，不再含逻辑名。
        let credential = chosen_credential.clone().or_else(|| pick_credential(&doc, &provider_id));

        let already_wired = credential.as_ref().is_some_and(|cred| {
            doc.channels.iter().any(|c| {
                c.provider == provider_id
                    && &c.credential == cred
                    && c.upstream_model == row.upstream_model
            })
        });
        if already_wired {
            continue;
        }

        // 走到这里就是**真要建一条线路**了：这时才要求"至少有一份启用中的凭据"，
        // 一份都没有就如实报错 —— 绝不静默建一条没有凭据的线路。
        let Some(credential) = credential else {
            return Err(no_credential(&provider_id));
        };
        if chosen_credential.is_none() {
            chosen_credential = Some(credential.clone());
        }

        let mut channel = ChannelConfig::new(
            // 线路 id 用**上游模型名**派生（与前端 `channelIdFor` 同口径）：线路的身份是
            // "服务商 × 凭据 × 上游模型"，扫描给出的逻辑名只是**建议**。
            channel_id_for(provider_id.as_str(), credential.as_str(), &row.upstream_model),
            // 遗留字段留空：关系在 `Model.lines` 上
            "",
            provider_id.clone(),
            credential,
            row.upstream_model.clone(),
        );
        // 线路**不再逐条写在通道上**（那个字段已删除）：走哪个节点由
        // **服务商**决定。入参给的默认代理在下面统一写到 provider 上。
        channel.enabled = false;
        doc.channels.push(channel);
        channels_created += 1;
    }

    // 勾的全是"已经有了"：这时**不写库**（写一份一模一样的配置只会多一个快照，
    // 而用户的意图已经满足了）；`applied` 仍然为真，语义是"这条命令成功了，没有要改的"。
    if channels_created == 0 {
        return Ok(applied_with_channels(0));
    }

    // 入参给的默认代理写在**服务商**上（通道级 lane 已删除）
    apply_default_lane_to_provider(&mut doc, &provider_id, lane.as_ref());

    save_validated(state, doc, "model_apply_import").await?;
    tracing::info!(
        provider_id = %entry.provider_id,
        "扫描结果已写入配置：本次新建 {} 条线路（**没有**创建模型：模型由用户在模型页分配）",
        channels_created
    );
    Ok(applied_with_channels(channels_created))
}

/// 把"新建线路时选的默认网络代理"写到**服务商**上。
///
/// 通道级的 `lane` 字段已删除（用户裁决："线路级别的分配逻辑我们已经放弃了，
/// 取而代之的，**每个服务商统一一个节点**"），所以这两个导入命令的 `lane` 入参
/// 唯一能落的地方就是**服务商**。
///
/// 语义如实说明：这会改**这家服务商的所有线路**（不只是这次新建的那些）——
/// 那正是"每个服务商一个节点"的意思。`None` = 不动（保持服务商当前的选择）。
fn apply_default_lane_to_provider(
    doc: &mut ConfigDoc,
    provider: &ProviderId,
    lane: Option<&LaneId>,
) {
    let Some(lane) = lane else { return };
    if let Some(p) = doc.providers.iter_mut().find(|p| &p.id == provider) {
        p.lane = Some(lane.clone());
    }
}

/// `model_apply_import({ preview_token, selected, lane? }) -> ApplyResult`。
#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn model_apply_import(
    state: State<'_, AppState>,
    preview_token: String,
    selected: Vec<String>,
    lane: Option<LaneId>,
) -> IpcResult<ApplyResult> {
    apply_model_import(&state, &preview_token, &selected, lane).await
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::VecDeque;
    use std::net::SocketAddr;
    use std::sync::{Arc, Mutex};

    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use ta_core::domain::Model;
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

    /// 一条**带上游自造字段**的清单体：证明目录记的是上游原文，而不是我们挑出来的字段。
    ///
    /// 里面 `object` / `owned_by` / `created` / `permission` / `context_length` 与
    /// `vendor_custom` 这些**我们一个都没建模** —— 它们必须原样出现在 `raw` 里。
    fn models_body_verbatim() -> String {
        serde_json::json!({
            "object": "list",
            "data": [{
                "id": "gpt-4o",
                "object": "model",
                "owned_by": "vendor-x",
                "created": 1712345678_i64,
                "permission": [{ "id": "perm-1", "allow_create_engine": false }],
                "context_length": 128000,
                "vendor_custom": { "tier": "pro", "tags": [1, 2, 3] }
            }]
        })
        .to_string()
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

    /// **上游 404 那一档**（核心裁决）：这是"没有 `/models` 端点"，
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
            None,
        )
        .await
        .expect("应用导入应当成功");

        assert!(result.applied);
        // 勾选 = 建**线路**：`Model` 一条都不许建（模型由用户在模型页分配）。
        // 夹具里那份 `gpt-4o` 模型是**配置里本来就有的**，不算这次建的 —— 所以断言的是
        // "没有**新增**模型"（数量与夹具一致）。
        assert_eq!(state.doc().models.len(), 1, "创建线路不许顺带建模型");
        assert_eq!(state.doc().models[0].id, "gpt-4o");
        let ups: Vec<String> =
            state.doc().channels.iter().map(|c| c.upstream_model.clone()).collect();
        assert_eq!(ups, vec!["o3-mini".to_string(), "deepseek-chat".to_string()]);
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

        let result =
            apply_model_import(&state, &preview.preview_token, &["gpt-4o".to_string()], None)
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
        apply_model_import(&state, &preview.preview_token, &["a-model".to_string()], None)
            .await
            .expect("第一次导入");
        apply_model_import(&state, &preview.preview_token, &["b-model".to_string()], None)
            .await
            .expect("第二次导入（同一个 token）");

        // 建的是线路（上游模型名就是线路的身份），不是模型
        let ups: Vec<String> =
            state.doc().channels.iter().map(|c| c.upstream_model.clone()).collect();
        assert_eq!(ups, vec!["a-model".to_string(), "b-model".to_string()]);
    }

    /// token 未知/过期 → `TA-1004`，消息说清"预览已过期，请重新扫描"。
    #[tokio::test]
    async fn apply_import_with_an_unknown_token_asks_for_a_rescan() {
        let upstream = spawn_upstream(vec![Reply::json(200, models_body(&["m-1"]))]).await;
        let state =
            fixture("apply-bad-token", config_with(&upstream.base_url(), serde_json::json!({})))
                .await;

        let err = apply_model_import(&state, "pv_从没见过的", &["m-1".to_string()], None)
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

        let err = apply_model_import(&state, &token, &["m-1".to_string()], None)
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
        let err = apply_model_import(&state, &preview.preview_token, &[], None)
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
        let err =
            apply_model_import(&state, &preview.preview_token, &["凭空捏造的".to_string()], None)
                .await
                .expect_err("预览里没有的 id 必须报错");
        assert_eq!(err.code, "TA-1004");
        assert!(err.message.contains("凭空捏造的"), "要点名是哪个 id：{}", err.message);
        assert!(state.doc().models.is_empty());
    }

    /// 「添加模型」= 建模型 + **建线路**：导入之后模型真的接在这个服务商上。
    ///
    /// 这是第 71 轮真机缺陷的回归测试：只往 `doc.models` 塞一行的话，第 2 步的
    /// "已添加的模型"（从 `channel_matrix` 派生）永远是空的，模型也永远显示"未配置"。
    #[tokio::test]
    async fn apply_import_wires_each_model_to_the_provider_with_the_chosen_lane() {
        let upstream =
            spawn_upstream(vec![Reply::json(200, models_body(&["gpt-4o-2024-08-06", "o3-mini"]))])
                .await;
        let state = fixture(
            "apply-wired",
            config_with(
                &upstream.base_url(),
                serde_json::json!({
                    "lanes": [{ "id": "lane-direct", "name": "直连", "kind": "direct" }],
                }),
            ),
        )
        .await;

        let preview =
            import_models_from_provider(&state, &secrets(), "p1", None).await.expect("扫描成功");
        let result = apply_model_import(
            &state,
            &preview.preview_token,
            &["gpt-4o".to_string(), "o3-mini".to_string()],
            Some(LaneId::new("lane-direct")),
        )
        .await
        .expect("应用导入应当成功");

        assert!(result.applied);
        assert_eq!(result.channels_created, 2, "两个模型各接了一条线");

        let doc = state.config.load().await.expect("load");
        // 线路 id 用**上游模型名**派生（线路的身份 = 服务商 × 凭据 × 上游模型）
        let wired = doc
            .channels
            .iter()
            .find(|c| c.id.as_str() == "ch-p1-p1-default-gpt-4o-2024-08-06")
            .unwrap_or_else(|| {
                panic!(
                    "导入必须建出 ch-p1-p1-default-gpt-4o-2024-08-06，实际的通道：{:?}",
                    doc.channels
                )
            });
        assert_eq!(wired.model, "", "遗留字段必须留空（关系在 Model.lines 上）");
        assert!(doc.models.is_empty(), "建线路不许顺带建模型");
        assert_eq!(wired.provider.as_str(), "p1");
        assert_eq!(wired.credential.as_str(), "p1-default");
        assert_eq!(
            wired.upstream_model, "gpt-4o-2024-08-06",
            "上游名要用扫描行的那一个（不是派生出来的逻辑 id）"
        );
        // 入参给的默认代理写在**服务商**上（通道级 lane 字段已删除）
        assert_eq!(
            doc.providers
                .iter()
                .find(|p| p.id.as_str() == "p1")
                .and_then(|p| p.lane.as_ref())
                .map(LaneId::as_str),
            Some("lane-direct"),
            "导入时选的默认代理必须落到服务商上"
        );
        assert!(
            doc.channels.iter().any(|c| c.id.as_str() == "ch-p1-p1-default-o3-mini"),
            "勾选的每个模型都要接上：{:?}",
            doc.channels
        );

        // 模型侧**不写任何东西**：`default_lane` 那套"给模型选代理"的语义已经作废
        // （代理是线路自己的属性），所以这里断言"没有因为这次导入而多出模型"。
        assert!(
            doc.models.is_empty(),
            "创建线路不许顺带建模型，更不该去改模型的默认线：{:?}",
            doc.models
        );
    }

    /// **幂等**：同一次预览应用两次，第二次不重复建模型/通道，**且不写库**。
    #[tokio::test]
    async fn apply_import_twice_creates_nothing_and_writes_nothing() {
        let upstream = spawn_upstream(vec![Reply::json(200, models_body(&["m-1"]))]).await;
        let state =
            fixture("apply-idem", config_with(&upstream.base_url(), serde_json::json!({}))).await;

        let preview =
            import_models_from_provider(&state, &secrets(), "p1", None).await.expect("扫描成功");
        let first = apply_model_import(&state, &preview.preview_token, &["m-1".to_string()], None)
            .await
            .expect("第一次导入");
        assert_eq!(first.channels_created, 1, "第一次要真接上一条线");

        // 快照条数是"有没有写过库"的硬证据：save_validated 每次写库都会留一个快照
        let snapshots_before = state.config.list_snapshots(50).await.expect("列快照").len();
        let second = apply_model_import(&state, &preview.preview_token, &["m-1".to_string()], None)
            .await
            .expect("第二次导入");
        assert!(second.applied, "没有要改的不等于命令失败");
        assert_eq!(second.channels_created, 0, "第二次不该再建线");
        assert_eq!(state.doc().channels.len(), 1, "线路也不该重复");
        assert_eq!(
            state.config.list_snapshots(50).await.expect("列快照").len(),
            snapshots_before,
            "没有任何改动时**不许写库**（多一个快照就是多写了一次）"
        );
    }

    /// 一份凭据都没有 → `TA-1004`，消息说清"先回第 1 步填密钥"，**且库没变**
    /// （绝不静默建一条没有凭据的线路）。
    #[tokio::test]
    async fn apply_import_without_a_credential_refuses_with_ta_1004() {
        let upstream = spawn_upstream(vec![Reply::json(200, models_body(&["m-1"]))]).await;
        let state =
            fixture("apply-nocred", config_with(&upstream.base_url(), serde_json::json!({}))).await;

        // 先真的扫一次（扫描需要一份启用中的凭据），拿到 token 之后再模拟"凭据被删掉"
        let preview =
            import_models_from_provider(&state, &secrets(), "p1", None).await.expect("扫描成功");
        let mut doc = (*state.doc()).clone();
        doc.credentials.clear();
        state.config.save(&doc).await.expect("save");
        state.set_doc(doc);

        let err = apply_model_import(&state, &preview.preview_token, &["m-1".to_string()], None)
            .await
            .expect_err("没有凭据必须报错");
        assert_eq!(err.code, "TA-1004");
        assert!(err.message.contains("还没有凭据"), "{}", err.message);
        assert!(err.message.contains("第 1 步"), "要说清去哪一步补：{}", err.message);

        assert!(state.doc().models.is_empty(), "报错就不许在内存里留下半个模型");
        assert!(state.doc().channels.is_empty(), "报错就不许留下通道");
        let back = state.config.load().await.expect("load");
        assert!(back.models.is_empty(), "报错就不许写库");
        assert!(back.channels.is_empty(), "报错就不许写库");
    }

    /// 凭据选择顺序：`<provider>-default` 优先；它停用时退到第一条启用中的。
    #[tokio::test]
    async fn apply_import_picks_the_default_credential_first_then_the_first_enabled() {
        let upstream = spawn_upstream(vec![Reply::json(200, models_body(&["m-1"]))]).await;
        let mut config = config_with(&upstream.base_url(), serde_json::json!({}));
        config["credentials"] = serde_json::json!([
            {
                "id": "p1-default",
                "provider": "p1",
                "label": "默认",
                "secret": "${secret:p1-default}",
                "enabled": false,
            },
            {
                "id": "p1-second",
                "provider": "p1",
                "label": "第二把",
                "secret": "${secret:p1-second}",
            },
        ]);
        let state = fixture("apply-cred-order", config).await;
        let secrets = StaticSecrets::new([
            ("${secret:p1-default}", "sk-命令层测试"),
            ("${secret:p1-second}", "sk-第二把"),
        ]);

        let preview =
            import_models_from_provider(&state, &secrets, "p1", None).await.expect("扫描成功");
        let result = apply_model_import(&state, &preview.preview_token, &["m-1".to_string()], None)
            .await
            .expect("应用导入");

        assert_eq!(result.channels_created, 1);
        let doc = state.config.load().await.expect("load");
        assert_eq!(
            doc.channels[0].credential.as_str(),
            "p1-second",
            "停用的 <provider>-default 不该被选中（接到停用凭据上等于建了一条没有流量的线）"
        );
        assert_eq!(
            doc.channels[0].id.as_str(),
            "ch-p1-p1-second-m-1",
            "通道 id 里的凭据段必须就是被选中的那一份"
        );
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

    /// 通道 id 的派生法**必须与前端 `channelIdFor`（`src/pages/channels/model.ts`）逐字一致**
    /// ：两边一旦分叉，界面会说"还没有这条线"、后端却建出另一条。
    #[test]
    fn channel_id_derivation_matches_the_frontend() {
        let cases = [
            (("p1", "p1-default", "gpt-4o"), "ch-p1-p1-default-gpt-4o"),
            // 非 [a-z0-9] 一律变 '-'，连续横线合并，去首尾横线
            (("OpenAI 官方", "key #1", "gpt-4o"), "ch-openai-key-1-gpt-4o"),
            (("p", "c", "Qwen2.5 7B!"), "ch-p-c-qwen2-5-7b"),
            // 净化后为空的段照前端的做法丢掉（`filter(part => part !== "")`）
            (("p", "***", "m"), "ch-p-m"),
        ];
        for ((provider, credential, model), expected) in cases {
            assert_eq!(
                channel_id_for(provider, credential, model),
                expected,
                "`{provider}` / `{credential}` / `{model}` 的派生结果不对"
            );
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

    /// `Model` 的构造必须给出全部字段（`Model` 没有 `Default`）。
    #[test]
    fn new_model_carries_only_the_id_the_contract_requires() {
        let model = Model {
            id: "m".to_string(),
            aliases: Vec::new(),
            display: ta_core::domain::DisplayConfig::default(),
            expose: true,
            //：模型级总开关。停用 = 不列出 + 拒绝请求。
            enabled: true,
            //：线路归属在模型这一侧（`lines`），关系由模型页"分配"时建立
            lines: Vec::new(),
            extra: serde_json::Map::new(),
        };
        let back: Model =
            serde_json::from_value(serde_json::to_value(&model).unwrap()).expect("能往返");
        assert_eq!(back.id, "m");
        assert!(back.expose);
        assert!(back.enabled);
    }

    /// 日志里没有密钥：源码级断言（：日志文件是要被分享出去的）。
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

    // ================================================================ 模型目录
    //
    // 用户第 77 轮的原话是"一旦扫描，全部记录下来（当然不是添加）……直接把那个没勾的勾上就行了"。
    // 所以这一组测试钉住的正是那两件事：**扫描 ≠ 接入**，以及**扫过的东西留得住**。

    /// 扫描 = **全量记录**：能派生出逻辑名的都进目录，派生不出的（`skipped`）不进；
    /// 而且**配置里一个模型/通道都没多**（"记录"不是"接入"）。
    #[tokio::test]
    async fn scan_records_every_model_but_wires_nothing() {
        // `////` 派生不出非空 id（规则 4）→ 进 skipped
        let upstream = spawn_upstream(vec![Reply::json(
            200,
            models_body(&["vendor/gpt-4o-2024-08-06", "claude-3-5-sonnet", "////"]),
        )])
        .await;
        let state =
            fixture("cat-scan", config_with(&upstream.base_url(), serde_json::json!({}))).await;

        let preview =
            import_models_from_provider(&state, &secrets(), "p1", None).await.expect("上游 200");

        assert_eq!(preview.models.len(), 2, "能派生逻辑名的都记下来：{preview:?}");
        assert_eq!(preview.skipped.len(), 1, "派生不出的进 skipped：{preview:?}");

        let rows = state.model_catalog.list("p1").await.expect("读目录");
        assert_eq!(rows.len(), 2, "两条候选都要落库（含没勾的那条）");
        assert_eq!(rows[0].upstream_model, "claude-3-5-sonnet");
        // 日期后缀被剥掉（派生规则），并且存的就是这个派生结果
        assert_eq!(rows[1].upstream_model, "vendor/gpt-4o-2024-08-06");
        assert_eq!(rows[1].suggested_id, "gpt-4o");
        assert!(rows.iter().all(|r| r.seen_in_last_scan), "本次都见到了");

        // ★ 扫描**不是**接入：配置里一条模型、一条通道都没有
        let doc = state.doc();
        assert!(doc.models.is_empty(), "扫描不许往配置里写模型");
        assert!(doc.channels.is_empty(), "扫描不许建通道");

        // 目录读出来时，`added` 全为 false（按当前配置现算）
        let entries = model_catalog_entries(&state, "p1").await.expect("读目录视图");
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().all(|e| !e.added && e.channel_ids.is_empty() && !e.enabled));
    }

    /// **扫描记录的是上游原文**（用户第 77 轮："一旦扫描，全部记录下来"）：
    /// 我们不认识的字段（`owned_by` / `created` / `permission` / `context_length` / 中转自造字段）
    /// 必须原字段、原键序地出现在 `provider_model_catalog` 的 `raw` 里，命令层**一个字段都不加、不改**。
    ///
    /// 这是"用户能看到上游真的声明了什么"与"给模型填能力时有可核对的来源"的唯一落点：
    /// 只在内存里留一个 id，两件事都做不到，只能靠猜 —— 那就是编字段。
    #[tokio::test]
    async fn scan_keeps_the_upstream_object_verbatim_and_returns_it_unchanged() {
        let upstream = spawn_upstream(vec![Reply::json(200, models_body_verbatim())]).await;
        let state =
            fixture("cat-raw", config_with(&upstream.base_url(), serde_json::json!({}))).await;

        import_models_from_provider(&state, &secrets(), "p1", None).await.expect("上游 200");

        // 期望值**直接取自我们发给上游的那份清单体**（不手抄一份，免得断言对着抄错的东西成立）
        let body: serde_json::Value = serde_json::from_str(&models_body_verbatim()).unwrap();
        let expected = body["data"][0].clone();

        let entries = model_catalog_entries(&state, "p1").await.expect("读目录视图");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].upstream_model, "gpt-4o");
        assert_eq!(entries[0].raw, expected, "上游原文必须逐字段相同：{entries:?}");
        assert_eq!(
            serde_json::to_string(&entries[0].raw).unwrap(),
            serde_json::to_string(&expected).unwrap(),
            "文本也逐字相同：键序没被重排、字段没被丢也没被补"
        );

        // 逐条点名几个"我们没建模"的字段，免得上面的断言其实对着一个空对象也成立
        let obj = entries[0].raw.as_object().expect("raw 必须是对象");
        assert_eq!(obj.get("owned_by").and_then(|v| v.as_str()), Some("vendor-x"));
        assert_eq!(obj.get("created").and_then(|v| v.as_i64()), Some(1712345678));
        assert_eq!(obj.get("context_length").and_then(|v| v.as_i64()), Some(128000));
        assert!(obj.contains_key("permission"), "嵌套的 permission 也要在：{obj:?}");
        assert_eq!(
            obj.get("vendor_custom")
                .and_then(|v| v.get("tags"))
                .and_then(|v| v.as_array())
                .map(|a| a.len()),
            Some(3),
            "中转自造的嵌套结构原样留着：{obj:?}"
        );
        // `id` 在派生列之外**还在原文里**（两个字段，不是同一个东西）
        assert_eq!(obj.get("id").and_then(|v| v.as_str()), Some("gpt-4o"));

        // 扫描仍然**不是**接入：配置里一条模型、一条通道都没有
        assert!(state.doc().models.is_empty() && state.doc().channels.is_empty());
    }

    /// 上游清单里有一条**没有 `id`** → 整次扫描如实报错（形状不对 = `TA-2005`），
    /// **不**给它编一个名字记进目录："记不到"与"编一个"是两件事。
    #[tokio::test]
    async fn scan_refuses_a_list_entry_without_an_id_instead_of_inventing_one() {
        let upstream = spawn_upstream(vec![Reply::json(
            200,
            r#"{"object":"list","data":[{"object":"model","owned_by":"v"},{"id":"ok-model"}]}"#,
        )])
        .await;
        let state =
            fixture("cat-no-id", config_with(&upstream.base_url(), serde_json::json!({}))).await;

        let err = import_models_from_provider(&state, &secrets(), "p1", None)
            .await
            .expect_err("缺 id 的清单形状不对：不许当成正常清单");
        assert_eq!(err.code, "TA-2005");
        assert!(
            state.model_catalog.list("p1").await.unwrap().is_empty(),
            "扫描失败就不许往目录里写半行（更不许给缺 id 的那条编一个名字）"
        );
    }

    /// 重扫**刷新原文**：上游给这条模型加了字段，目录要跟着它走
    /// （原文记的是"最后一次观测"，不是"第一次见到就冻住"的东西；冻住的那个是 `first_seen_ms`）。
    #[tokio::test]
    async fn rescan_refreshes_the_upstream_object() {
        let first = spawn_upstream(vec![Reply::json(200, models_body(&["m-1"]))]).await;
        let state =
            fixture("cat-raw-refresh", config_with(&first.base_url(), serde_json::json!({}))).await;
        import_models_from_provider(&state, &secrets(), "p1", None).await.expect("第一次扫描");
        assert_eq!(
            model_catalog_entries(&state, "p1").await.unwrap()[0].raw,
            serde_json::json!({ "id": "m-1", "object": "model" }),
            "第一次扫到的原文就是上游给的那份"
        );

        // 第二次：**同一条上游模型**，上游这次多给了字段
        let rich = serde_json::json!({
            "object": "list",
            "data": [{ "id": "m-1", "object": "model", "owned_by": "vendor-x", "context_length": 128000 }]
        })
        .to_string();
        let second = spawn_upstream(vec![Reply::json(200, rich.clone())]).await;
        let mut doc = (*state.doc()).clone();
        doc.providers[0].base_url = second.base_url();
        state.config.save(&doc).await.expect("改 base_url");
        state.set_doc(doc);
        import_models_from_provider(&state, &secrets(), "p1", None).await.expect("第二次扫描");

        let body: serde_json::Value = serde_json::from_str(&rich).unwrap();
        let entries = model_catalog_entries(&state, "p1").await.unwrap();
        assert_eq!(entries.len(), 1, "同一条上游模型还是一条（不是又记一行）");
        assert_eq!(entries[0].raw, body["data"][0].clone(), "原文要刷新成上游这次给的那份");
        assert!(
            entries[0].raw.as_object().unwrap().contains_key("context_length"),
            "新字段要跟着记下来：{:?}",
            entries[0].raw
        );
    }

    /// 重新扫描**以本次为准**：本次没返回的扫描项从目录里清掉；
    /// 但**已经接入的**那一条要保留（否则界面上会出现"有通道、模型区却看不到这个模型"）。
    #[tokio::test]
    async fn rescan_prunes_unseen_but_keeps_what_is_wired() {
        let first =
            spawn_upstream(vec![Reply::json(200, models_body(&["a-model", "b-model", "c-model"]))])
                .await;
        let state =
            fixture("cat-prune", config_with(&first.base_url(), serde_json::json!({}))).await;
        import_models_from_provider(&state, &secrets(), "p1", None).await.expect("第一次扫描");
        assert_eq!(state.model_catalog.list("p1").await.unwrap().len(), 3);

        // 把 b 接进来（用户勾了它）——此后它就是"用户意图"，扫描无权把它抹掉
        attach_catalog_model(&state, "p1", "b-model").await.expect("接入 b");
        assert_eq!(state.doc().channels.len(), 1);

        // 第二次扫描：上游只返回 a（b、c 都不见了）
        let second = spawn_upstream(vec![Reply::json(200, models_body(&["a-model"]))]).await;
        let mut doc = (*state.doc()).clone();
        doc.providers[0].base_url = second.base_url();
        state.config.save(&doc).await.expect("改 base_url");
        state.set_doc(doc);

        import_models_from_provider(&state, &secrets(), "p1", None).await.expect("第二次扫描");

        let rows = state.model_catalog.list("p1").await.expect("读目录");
        let names: Vec<&str> = rows.iter().map(|r| r.upstream_model.as_str()).collect();
        assert_eq!(names, vec!["a-model", "b-model"], "c 该被清掉；b 因为接了通道要留住");
        let b = rows.iter().find(|r| r.upstream_model == "b-model").expect("b 在");
        assert!(!b.seen_in_last_scan, "留住的那条要标成'本次未返回'，让用户自己决定");
        assert!(rows.iter().find(|r| r.upstream_model == "a-model").unwrap().seen_in_last_scan);
        // 接入的事实**没有被扫描动过**
        assert_eq!(state.doc().channels.len(), 1);
    }

    /// 勾一下 = **只建一条线路**（：**不建模型**，模型由用户在模型页分配）；再勾一次幂等。
    #[tokio::test]
    async fn attach_creates_only_a_line_then_is_idempotent() {
        let upstream =
            spawn_upstream(vec![Reply::json(200, models_body(&["vendor/gpt-4o-2024-08-06"]))])
                .await;
        let state =
            fixture("cat-attach", config_with(&upstream.base_url(), serde_json::json!({}))).await;
        import_models_from_provider(&state, &secrets(), "p1", None).await.expect("扫描");

        let result =
            attach_catalog_model(&state, "p1", "vendor/gpt-4o-2024-08-06").await.expect("接入");
        assert!(result.applied);
        assert_eq!(result.channels_created, 1, "真新建了一条线路");

        let doc = state.doc();
        assert!(doc.models.is_empty(), "勾一下**不许**顺带建模型（用户要自己去模型页分配）");
        assert_eq!(doc.channels.len(), 1);
        assert_eq!(doc.channels[0].upstream_model, "vendor/gpt-4o-2024-08-06");
        assert_eq!(doc.channels[0].model, "", "遗留字段必须留空（关系在 Model.lines 上）");
        assert_eq!(doc.channels[0].credential.as_str(), "p1-default");
        assert!(doc.channels[0].enabled, "接入就是**能用**：线路必须是启用状态");
        // 线路 id 用**上游模型名**派生（线路的身份 = 服务商 × 凭据 × 上游模型）
        assert_eq!(doc.channels[0].id.as_str(), "ch-p1-p1-default-vendor-gpt-4o-2024-08-06");

        // 目录视图如实说"已接入"，并给出线路 id（取消勾选/移除都要用它）
        let entries = model_catalog_entries(&state, "p1").await.expect("读目录视图");
        assert_eq!(entries.len(), 1);
        assert!(entries[0].added);
        assert!(entries[0].enabled);
        assert_eq!(
            entries[0].channel_ids,
            vec!["ch-p1-p1-default-vendor-gpt-4o-2024-08-06".to_string()]
        );

        // 幂等：再勾一次不写库、不新建（`channels_created = 0`）
        let again = attach_catalog_model(&state, "p1", "vendor/gpt-4o-2024-08-06")
            .await
            .expect("再接入一次");
        assert!(again.applied);
        assert_eq!(again.channels_created, 0);
        assert_eq!(state.doc().channels.len(), 1, "不许建第二条");
    }

    ///「添加服务商就把**所有**线路建出来」：目录里每条都建，**默认关闭**，
    /// 幂等且**绝不覆盖**用户已经调过的那条线路。
    #[tokio::test]
    async fn create_all_provider_lines_builds_everything_disabled_and_is_idempotent() {
        let upstream = spawn_upstream(vec![Reply::json(
            200,
            models_body(&["vendor/a", "vendor/b", "vendor/c"]),
        )])
        .await;
        let state =
            fixture("lines-create-all", config_with(&upstream.base_url(), serde_json::json!({})))
                .await;
        import_models_from_provider(&state, &secrets(), "p1", None).await.expect("扫描");

        let result = create_all_provider_lines(&state, "p1", None).await.expect("全部建出");
        assert_eq!(result.created, 3, "目录里 3 条就建 3 条");
        assert_eq!(result.total, 3);

        {
            let doc = state.doc();
            assert!(doc.models.is_empty(), "建线路**不许**顺带建模型");
            assert_eq!(doc.channels.len(), 3);
            for c in &doc.channels {
                assert!(!c.enabled, "新建的线路**默认关闭**（之后只用滑块启用）: {}", c.id);
                assert_eq!(c.model, "", "遗留字段留空");
            }
        }

        // 用户调过其中一条（启用 + 钉一条 lane）→ 再扫一次**不许**把它改回去
        {
            let mut doc = (*state.doc()).clone();
            doc.channels[0].enabled = true;
            state.config.save(&doc).await.expect("存");
            state.set_doc(doc);
        }
        let again = create_all_provider_lines(&state, "p1", None).await.expect("再建一次");
        assert_eq!(again.created, 0, "已经全有了 → 一条都不补");
        assert_eq!(again.total, 3);
        let doc = state.doc();
        assert_eq!(doc.channels.len(), 3, "不许重复建");
        assert!(doc.channels[0].enabled, "用户调过的那条必须原样保留");
    }

    /// 目录还是空的 → **如实报错点名**（不凭空造线路）；没有启用中的凭据 → 也不建。
    #[tokio::test]
    async fn create_all_provider_lines_refuses_an_empty_catalog_and_missing_credential() {
        let upstream = spawn_upstream(vec![Reply::json(200, models_body(&["vendor/a"]))]).await;
        let state = fixture(
            "lines-create-all-bad",
            config_with(&upstream.base_url(), serde_json::json!({})),
        )
        .await;

        // ① 还没扫描 → 目录空 → TA-1004，并说清"先扫描一次"
        let err = create_all_provider_lines(&state, "p1", None).await.expect_err("空目录");
        assert_eq!(err.code, "TA-1004");
        assert!(err.message.contains("目录"), "{}", err.message);
        assert!(state.doc().channels.is_empty(), "被拒绝时一条线路都不许建");

        // ② 有目录、但没有启用中的凭据 → 同样不建（绝不静默建一条没有凭据的线路）
        import_models_from_provider(&state, &secrets(), "p1", None).await.expect("扫描");
        {
            let mut doc = (*state.doc()).clone();
            for c in doc.credentials.iter_mut() {
                c.enabled = false;
            }
            state.config.save(&doc).await.expect("停用凭据");
            state.set_doc(doc);
        }
        let err = create_all_provider_lines(&state, "p1", None).await.expect_err("没有凭据");
        assert_eq!(err.code, "TA-1004");
        assert!(state.doc().channels.is_empty(), "没有凭据时一条线路都不许建");
    }

    /// 目录里没有这一条 → `TA-1004`（**"先扫描一次"**），绝不凭一个上游名凭空建通道。
    #[tokio::test]
    async fn attach_refuses_a_model_that_the_catalog_does_not_have() {
        let upstream = spawn_upstream(vec![]).await;
        let state =
            fixture("cat-attach-miss", config_with(&upstream.base_url(), serde_json::json!({})))
                .await;

        let err = attach_catalog_model(&state, "p1", "never-scanned")
            .await
            .expect_err("目录里没有这一条时必须报错");
        // `IpcError` 没有 `Display`（它是要序列化给前端的结构体），断言看它给用户的那句话
        assert!(err.message.contains("先扫描一次"), "要说清怎么才能成功：{}", err.message);
        assert!(state.doc().channels.is_empty(), "报错就不许有副作用");
    }

    /// 一份启用中的凭据都没有 → `TA-1004`（"先去配密钥"），而不是静默建一条没有凭据的线。
    #[tokio::test]
    async fn attach_needs_an_enabled_credential() {
        let upstream = spawn_upstream(vec![]).await;
        let mut config = config_with(&upstream.base_url(), serde_json::json!({}));
        config["credentials"][0]["enabled"] = serde_json::json!(false);
        let state = fixture("cat-attach-nocred", config).await;
        // 目录直接种一行（**扫描本身**就要求有启用中的凭据：`probe_secret` 会先报 TA-1004，
        // 所以这里不能靠扫描来造数据 —— 测的是"接入"这一步的凭据要求）。
        state
            .model_catalog
            .upsert(&ta_store::ModelCatalogRow {
                provider_id: "p1".to_string(),
                upstream_model: "m-1".to_string(),
                suggested_id: "m-1".to_string(),
                // 这条用例测的是"接入"那一步的凭据要求，与原文无关；
                // 没有原文就是 `{}`（不编一个上游没给过的对象）。
                raw_json: serde_json::json!({}),
                seen_in_last_scan: true,
                first_seen_ms: 1,
                last_seen_ms: 1,
            })
            .await
            .expect("种一行目录");

        let err =
            attach_catalog_model(&state, "p1", "m-1").await.expect_err("没有可用凭据必须报错");
        assert!(err.message.contains("凭据"), "{}", err.message);
        assert!(state.doc().channels.is_empty());
    }

    /// 服务商不存在时**不返回一个空目录**："服务商没了"与"扫过但没有模型"是两件事。
    #[tokio::test]
    async fn catalog_of_an_unknown_provider_is_an_error_not_an_empty_list() {
        let upstream = spawn_upstream(vec![]).await;
        let state =
            fixture("cat-missing", config_with(&upstream.base_url(), serde_json::json!({}))).await;

        assert!(model_catalog_entries(&state, "p1").await.is_ok(), "夹具自证：p1 是存在的");
        assert!(model_catalog_entries(&state, "nope").await.is_err(), "不存在的服务商要报错");
    }

    // ---------------------------------------------------------------- provider_echo

    /// 核心裁决：回显报出**我们真发了什么**，而回显里的密钥**必须是 `***`**。
    #[tokio::test]
    async fn echo_reports_the_shape_we_sent_and_redacts_the_key() {
        let upstream = spawn_upstream(vec![Reply::json(
            200,
            r#"{"choices":[{"message":{"role":"assistant","content":"pong"}}]}"#,
        )])
        .await;
        let state =
            fixture("echo-ok", config_with(&upstream.base_url(), serde_json::json!({}))).await;

        let out = echo_provider(&state, &secrets(), "p1", "m-1", true)
            .await
            .expect("上游 200，回显不该报错");

        // ① 真发出去的是 POST /chat/completions，且带上了凭据解出来的密钥
        let head = upstream.seen()[0].clone();
        assert!(head.starts_with("POST /chat/completions"), "{head}");
        assert!(head.contains("authorization: Bearer sk-命令层测试"), "{head}");

        // ② 回显里的头**必须打码**：这是"给人看的结构"，不是导出密钥
        assert_eq!(out.sent_headers.get("authorization").map(String::as_str), Some("***"));
        let dump = format!("{:?}", out.sent_headers);
        assert!(!dump.contains("sk-命令层测试"), "回显把密钥漏出来了：{dump}");

        // ③ 形状：带图时 content 是**两项**（text + image_url），不是一句字符串
        assert_eq!(out.sent_body_shape["max_tokens"], serde_json::json!("number"));
        assert_eq!(
            out.sent_body_shape["messages"]["items"]["content"]["len"],
            serde_json::json!(2)
        );

        // ④ 上游响应体原样带回来（诊断要看的就是它）
        assert_eq!(
            out.upstream_body.as_ref().expect("有响应体")["choices"][0]["message"]["content"],
            "pong"
        );
        // ⑤ 延迟是真测的（这里上游不延迟，只断言字段存在且是数）
        assert!(out.latency_ms < 60_000);
    }

    /// 没有启用中的凭据 ⇒ 如实报错，**不硬发一个空 Bearer**（那会制造假 401）。
    #[tokio::test]
    async fn echo_without_a_credential_refuses_instead_of_faking_a_401() {
        let upstream = spawn_upstream(vec![Reply::json(200, "{}")]).await;
        let mut cfg = config_with(&upstream.base_url(), serde_json::json!({}));
        cfg["credentials"] = serde_json::json!([]);
        let state = fixture("echo-nocred", cfg).await;

        let err = echo_provider(&state, &secrets(), "p1", "m-1", false)
            .await
            .expect_err("没有凭据必须报错");
        assert!(!err.message.is_empty(), "{err:?}");
        assert!(upstream.seen().is_empty(), "报错前不该发出任何请求：{:?}", upstream.seen());
    }

    /// 不认识的服务商 ⇒ 人话错误。
    #[tokio::test]
    async fn echo_for_an_unknown_provider_is_an_honest_error() {
        let upstream = spawn_upstream(vec![Reply::json(200, "{}")]).await;
        let state =
            fixture("echo-unknown", config_with(&upstream.base_url(), serde_json::json!({}))).await;

        let err = echo_provider(&state, &secrets(), "nope", "m-1", false)
            .await
            .expect_err("服务商不存在必须报错");
        assert!(!err.message.is_empty(), "{err:?}");
    }
}
