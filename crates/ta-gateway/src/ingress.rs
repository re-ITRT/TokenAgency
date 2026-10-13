//! 入站：axum 路由、本地密钥、RPM 限流。**工作包 G1 负责实现**。
//!
//! 路由表见 。**只绑 127.0.0.1**。
//!
//! ## G1 的职责边界
//!
//! G1 把请求**接进来**（步骤 1–2：解析 / request_id / 原始头体快照 / 本地密钥 / 限流 /
//! body 上限），并提供**定点改写**的纯函数 [`build_prepared_request`]，产出结构化的
//! [`PreparedRequest`]。路由决策与分发（步骤 3–12）由 G2 实现 [`RequestPipeline`]。
//!
//! 这样切分的好处：全透传（本项目最核心的承诺）在 G1 就被钉死并可独立测试；
//! G2 只需要"选通道 → 调 `build_prepared_request` → 发出去"，没有空间再去重建头部。
//!
//! ## 与文档的顺序差异（显式记录）
//!
//!  把"记录原始头/体快照"放在第 1 步、鉴权放在第 2 步。实现上是
//! **先鉴权再读 body**：一个带着 32MB body 的未授权请求不该先被读进内存。
//! 对合法请求这条差异不可观测（快照内容完全一致）。

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post};
use axum::Router;
use bytes::{Bytes, BytesMut};
use http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};

use ta_core::clock::Clock;
use ta_core::domain::{ChannelConfig, ConfigDoc, Model, Provider};
use ta_core::error::{FieldError, Result, TaError};
use ta_core::types::{ChannelId, LaneId, Protocol};
use ta_secrets::{classify_literal, is_reference, PlaintextKind};

use crate::passthrough::{self, HeaderDiff, PassthroughPolicy};
use crate::pipeline::SecretResolver;
use crate::GatewayDeps;

// ================================================================ 端点

/// 路由表里的端点类别。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Endpoint {
    ChatCompletions,
    Responses,
    Messages,
    Embeddings,
    ImagesGenerations,
    Audio,
    Files,
    ModelsList,
    ModelGet,
    Healthz,
    /// `ANY /*` 通配透传
    Wildcard,
}

impl Endpoint {
    pub fn as_str(&self) -> &'static str {
        match self {
            Endpoint::ChatCompletions => "chat_completions",
            Endpoint::Responses => "responses",
            Endpoint::Messages => "messages",
            Endpoint::Embeddings => "embeddings",
            Endpoint::ImagesGenerations => "images_generations",
            Endpoint::Audio => "audio",
            Endpoint::Files => "files",
            Endpoint::ModelsList => "models_list",
            Endpoint::ModelGet => "model_get",
            Endpoint::Healthz => "healthz",
            Endpoint::Wildcard => "wildcard",
        }
    }

    /// 该端点**默认**属于哪个协议（真正以上游 provider.protocol 为准）。
    pub fn protocol(&self) -> Protocol {
        match self {
            Endpoint::Messages => Protocol::Anthropic,
            Endpoint::Responses => Protocol::OpenaiResponses,
            _ => Protocol::Openai,
        }
    }

    pub fn is_wildcard(&self) -> bool {
        matches!(self, Endpoint::Wildcard)
    }
}

/// 纯函数路由分类。**与 [`router`] 注册的路由一一对应**（有测试守住两者不漂移）。
pub fn classify(method: &Method, path: &str) -> Endpoint {
    match (method.as_str(), path) {
        ("POST", "/v1/chat/completions") => Endpoint::ChatCompletions,
        ("POST", "/v1/responses") => Endpoint::Responses,
        ("POST", "/v1/messages") => Endpoint::Messages,
        ("POST", "/v1/embeddings") => Endpoint::Embeddings,
        ("POST", "/v1/images/generations") => Endpoint::ImagesGenerations,
        ("POST", "/v1/files") => Endpoint::Files,
        ("POST", rest) if rest.starts_with("/v1/audio/") => Endpoint::Audio,
        ("GET", "/v1/models") => Endpoint::ModelsList,
        // **不带 `/v1` 的 `/models` 也必须是本地清单**。
        //
        // 真机现象（用户第 325 轮）："选 OPENAI 会直接穿透拿到服务商的所有模型"。
        // 根因就是这里：`classify` 只认 `/v1/models`，而客户端把 base_url 填成
        // `http://127.0.0.1:<port>`（不带 `/v1`，OpenAI SDK 很常见的写法）时会请求
        // `GET /models` → 落进 `fallback(any(handle))` → **原样透传上游** →
        // 端回**服务商自己的完整模型清单**（还带着上游的真实模型名）。
        //
        // Anthropic 协议的客户端不发这个请求，所以"看起来正常" —— 差别不在协议分支，
        // 而在**路径是否恰好命中**。
        ("GET", "/models") => Endpoint::ModelsList,
        ("GET", rest) if rest.starts_with("/v1/models/") && rest.len() > "/v1/models/".len() => {
            Endpoint::ModelGet
        }
        ("GET", rest) if rest.starts_with("/models/") && rest.len() > "/models/".len() => {
            Endpoint::ModelGet
        }
        ("GET", "/healthz") => Endpoint::Healthz,
        _ => Endpoint::Wildcard,
    }
}

/// 从路径里猜模型名（通配路径用，如 `/v1beta/models/gemini-2.5-pro:generateContent`）。
pub fn model_from_path(path: &str) -> Option<String> {
    let marker = "/models/";
    let start = path.find(marker)? + marker.len();
    let segment = path[start..].split(['/', ':']).next().unwrap_or("");
    if segment.is_empty() {
        None
    } else {
        Some(segment.to_string())
    }
}

// ================================================================ 请求快照与 hints

/// 客户端原始请求的快照（**未经过任何改写**），供 trace 与 `diff_headers` 自检使用。
#[derive(Debug, Clone)]
pub struct RequestSnapshot {
    pub request_id: String,
    pub method: Method,
    pub path: String,
    pub query: Option<String>,
    pub endpoint: Endpoint,
    pub headers: HeaderMap,
    pub body: Bytes,
    pub received_at_ms: i64,
}

impl RequestSnapshot {
    /// 只读 hints（步骤 3）：路由需要知道的几个字段。**不修改请求**。
    pub fn hints(&self) -> BodyHints {
        body_hints(&self.body)
    }

    /// 模型名候选：`x-tokenagency-model` 头 → body 的 `model` → 通配路径里的模型名。
    pub fn model_hint(&self) -> Option<String> {
        if let Some(name) = header_str(&self.headers, "x-tokenagency-model") {
            if !name.trim().is_empty() {
                return Some(name.trim().to_string());
            }
        }
        if let Some(model) = self.hints().model {
            return Some(model);
        }
        model_from_path(&self.path)
    }

    /// 客户端是否要求流式（只读；G2 的流式转发据此决定）。
    pub fn is_stream(&self) -> bool {
        self.hints().stream
    }
}

/// 路由只需知道的三个字段（步骤 3）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BodyHints {
    pub model: Option<String>,
    pub stream: bool,
    pub has_image: bool,
}

/// 从 body 里读 hints。**解析失败一律当作"读不到"**，绝不因为读不了 hints 而拒绝请求。
pub fn body_hints(body: &[u8]) -> BodyHints {
    let Ok(value) = serde_json::from_slice::<Value>(body) else {
        return BodyHints::default();
    };
    BodyHints {
        model: value.get("model").and_then(Value::as_str).map(str::to_string),
        stream: value.get("stream").and_then(Value::as_bool).unwrap_or(false),
        has_image: contains_image_part(&value),
    }
}

/// 递归找图片块（openai `image_url` / responses `input_image` / anthropic `image`）。
fn contains_image_part(value: &Value) -> bool {
    match value {
        Value::Array(items) => items.iter().any(contains_image_part),
        Value::Object(map) => {
            if let Some(kind) = map.get("type").and_then(Value::as_str) {
                if matches!(kind, "image_url" | "input_image" | "image" | "image_base64") {
                    return true;
                }
            }
            map.values().any(contains_image_part)
        }
        _ => false,
    }
}

// ================================================================ PreparedRequest

/// 已经完成**定点改写**、准备发往上游的一次请求（G1 的产出，G2 的输入）。
#[derive(Debug)]
pub struct PreparedRequest {
    pub request_id: String,
    pub endpoint: Endpoint,
    /// 客户端请求所属协议（由端点推断）
    pub client_protocol: Protocol,
    /// 上游协议（由通道的 `provider` 决定）。
    /// **两者不同 → G2/G3 必须走 IR 路径**（同协议走 RAW，零改动零核对）。
    pub protocol: Protocol,
    /// 客户端发来的逻辑模型名（未改动）
    pub model: Option<String>,
    pub upstream_model: String,
    pub stream: bool,
    pub has_image: bool,
    pub channel_id: ChannelId,
    pub lane_id: Option<LaneId>,
    pub upstream_url: String,
    /// 要发给上游的头（整体转发 + 4 处定点修改）
    pub headers: HeaderMap,
    /// 要发给上游的 body（原字节 + 定点 patch）
    pub body: Bytes,
    /// 自检：正常必须为空
    pub header_diff: HeaderDiff,
    /// 失败转移链（G2 的 `prepare` 填：`channel_id` 之外的后续候选，按顺序）。
    /// G1 的 `build_prepared_request` 恒填空表 —— 它不做路由决策。
    pub failover_chain: Vec<ChannelId>,
    // 这里曾有 `lane_override: Option<LaneId>`（策略级全局线路覆盖，G2 从 RouteDecision 抄过来）。
    //
    //（第 322 轮）整条删除：线路分配只剩**服务商级**一处，`RouteDecision` 里那个
    // 字段也删了，所以这里再没有任何东西可抄 —— 留着就是一段恒为 `None` 的死管线。
    /// 客户端原始请求（trace 用；body 与 headers 是 `Bytes`/`HeaderMap`，克隆很便宜）
    pub snapshot: RequestSnapshot,
}

/// [`build_prepared_request`] 的入参（字段多，避免位置参数错位）。
pub struct PrepareInput<'a> {
    pub cfg: &'a ConfigDoc,
    pub policy: &'a dyn PassthroughPolicy,
    pub snapshot: &'a RequestSnapshot,
    pub channel: &'a ChannelConfig,
    /// 通道引用的服务商；缺失说明配置有洞（返回明确错误，不静默降级）
    pub provider: Option<&'a Provider>,
    /// 该通道凭据的明文 key（只在这里用一次，**绝不落库、绝不进日志**）
    pub secret: &'a str,
    pub lane_id: Option<LaneId>,
}

/// 跨协议时，**上游要打的端点路径**由目标协议的语义决定。
///
/// 不能把客户端的路径原样带过去：客户端发 `/v1/messages`（Anthropic）而后端是 OpenAI 兼容的
/// 中转站时，`POST /v1/messages` 必然 404 —— 那是把"协议不匹配"伪装成"上游没有这个接口"。
/// Gemini 的端点**路径里带模型名**，所以它不能只返回一个常量。
/// 流式还换一个方法名（`:streamGenerateContent?alt=sse`）——
/// 把 `alt=sse` 放进路径而不是查询串，是因为查询串那一格还要留给"调用方显式指定的 query"。
fn gemini_path(upstream_model: &str, stream: bool) -> String {
    let model = upstream_model.trim();
    if stream {
        format!("/v1beta/models/{model}:streamGenerateContent?alt=sse")
    } else {
        format!("/v1beta/models/{model}:generateContent")
    }
}

pub fn upstream_path_for(target: Protocol, upstream_model: &str, stream: bool) -> String {
    match target {
        Protocol::Anthropic => "/v1/messages".to_string(),
        Protocol::Openai | Protocol::Custom => "/v1/chat/completions".to_string(),
        Protocol::OpenaiResponses => "/v1/responses".to_string(),
        Protocol::Gemini => gemini_path(upstream_model, stream),
    }
}

/// **G1 的核心纯函数**：把"客户端原始请求 + 选定通道"变成"发往上游的请求"。
///
/// 只做契约允许的事：`model` 定点 patch、鉴权头替换、`host`/`content-length` 重算、
/// hop-by-hop 剥离、以及用户显式配置的 `ChannelConfig::transform`。
///
///（第 326 轮）**外加跨协议中转**：客户端协议与上游协议不同时，body 走
/// `adapt` 的 IR 单向通道（客户端协议 → IR → 上游协议），路径换成目标协议的端点。
/// 这一层原来写好了却**从来没有被调用** —— 于是网关一直原样透传客户端 body，
/// 真机表现就是"客户端协议必须等于上游协议，否则必然 400"，用户被迫"把服务商切回
/// OPENAI 才能输入"。
pub fn build_prepared_request(input: PrepareInput<'_>) -> Result<PreparedRequest> {
    let channel = input.channel;
    let snapshot = input.snapshot;

    let Some(provider) = input.provider else {
        return Err(TaError::Internal(format!(
            "通道 {} 引用的服务商 {} 不存在（配置有洞）",
            channel.id, channel.provider
        )));
    };

    let client_protocol = snapshot.endpoint.protocol();
    // **只在两边都有适配器时才转**（`adapt::can_translate`）。
    //
    // 不能写成"协议不同就转"：`Protocol::OpenaiResponses` / `Gemini` 目前**没有适配器**
    // （`adapter_for` 返回 None），硬转会得到一个 `CapabilityLoss` —— 而在此之前
    // `POST /v1/responses` 发往 OpenAI 上游是能原样透传的。接手一个改动不该顺手把
    // 原本能走的路变成 415：**能转就转（openai-chat ↔ anthropic），不能转就保持原样**。
    let cross_protocol = client_protocol != provider.protocol
        && crate::adapt::can_translate(client_protocol, provider.protocol)
        // 第 326 轮起**流式也转**：响应方向由 `adapt::session` 逐帧翻译
        // （切帧 → IR → 目标协议），所以请求方向不再因为 `stream: true` 而区别对待。
        ;

    // 路径：`transform.path` 覆盖客户端路径（少数上游路径不一致的场景）
    let stream = snapshot.hints().stream;
    let path: String = channel
        .transform
        .path
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
            if cross_protocol {
                upstream_path_for(provider.protocol, &channel.upstream_model, stream)
            } else {
                snapshot.path.clone()
            }
        });
    let mut upstream_url =
        passthrough::join_upstream_url(&provider.base_url, &path, snapshot.query.as_deref());
    upstream_url = passthrough::apply_query_auth(&upstream_url, &provider.auth, input.secret);
    // **把"我们实际打给上游的地址"记进日志**（去掉查询串：`auth.scheme: query`
    // 的密钥就在那里）。这一行是上一次排查时最缺的东西 —— 当时只能靠 base_url + 客户端路径手算，
    // 才看出 `…/api/coding/v3/v1/chat/completions` 这种拼错的路径（上游回 404 且无响应体，
    // 客户端只看到 `404 status code (no body)`）。字段名走 message，不加白名单外的新字段。
    if let Some((origin, _)) = upstream_url.split_once('?') {
        tracing::info!(
            channel_id = %channel.id,
            provider_id = %channel.provider,
            "上游目标（已去掉查询串）{origin}"
        );
    } else {
        tracing::info!(
            channel_id = %channel.id,
            provider_id = %channel.provider,
            "上游目标 {upstream_url}"
        );
    }

    // **跨协议：body 先走 IR 中转**（客户端协议 → IR → 上游协议）。
    //
    // 放在 `patch_body` **之前**：`translate_request` 已经按 `channel.upstream_model` 编码好了
    // 模型名，紧接着的 `patch_body` 看到 `model` 已经等于上游名 → 不动它（幂等）。
    // 空 loss 的路径（同协议）**一个字节都不动**：走下面的 `snapshot.body` 原样透传。
    let translated = if cross_protocol {
        Some(
            crate::adapt::translate_request(
                client_protocol,
                provider.protocol,
                &channel.upstream_model,
                &snapshot.body,
                // 有损时的处置是**网关级**设置（`GatewayConfig.passthrough.on_loss`）：
                // "这次转换能不能接受损失"是全局策略，不是某一家上游的属性。
                input.cfg.gateway.passthrough.on_loss,
            )?
            .body,
        )
    } else {
        None
    };
    let body_for_upstream: &[u8] = translated.as_deref().unwrap_or(&snapshot.body);

    let patched = input.policy.patch_body(body_for_upstream, channel)?;
    let mut headers = input.policy.build_headers(
        input.cfg,
        &snapshot.headers,
        channel,
        input.secret,
        &upstream_url,
    );
    // trait 的 build_headers 拿不到 body，这里补上"按实际 body 重算 content-length"
    passthrough::set_content_length(&mut headers, patched.len());
    let header_diff = input.policy.diff_headers(&snapshot.headers, &headers);

    let hints = snapshot.hints();
    Ok(PreparedRequest {
        request_id: snapshot.request_id.clone(),
        endpoint: snapshot.endpoint,
        client_protocol: snapshot.endpoint.protocol(),
        protocol: provider.protocol,
        // 把客户端发来的名字**归一成配置里的规范模型 id**（`id` 与任一 `alias` 都认）。
        //
        // 为什么必须归一、且必须**只在这一处**做：下游有三处都要用**同一个**名字 ——
        // ① 定价（`PriceMatch.model` 匹配的是「对外模型名」）、
        // ② 用量表的 `model` 列、③ 成本中心按模型的分组。
        // 不归一的话，同一个模型会因为客户端发了别名而**算不出价**（显示"未定价"），
        // 并且在成本中心被**拆成两行**（`m-a` 与 `alias-a` 各占一行、各算一半钱）。
        //
        // 查不到就**照原样保留**客户端发的名字：那属于"请求了一个没配置的模型"，
        // 此时编一个规范 id 出来才是错的（网关要如实记下它收到了什么）。
        model: hints
            .model
            .as_deref()
            .and_then(|name| input.cfg.find_model(name))
            .map(|m| m.id.clone())
            .or(hints.model),
        upstream_model: channel.upstream_model.clone(),
        stream: hints.stream,
        has_image: hints.has_image,
        channel_id: channel.id.clone(),
        lane_id: input.lane_id,
        upstream_url,
        headers,
        body: Bytes::from(patched),
        header_diff,
        failover_chain: Vec::new(),
        snapshot: snapshot.clone(),
    })
}

// ================================================================ 管线 trait（G2 实现）

/// 请求管线。G2 实现它（路由决策 + 分发 + 流式 + 计量 + trace）。
#[async_trait]
pub trait RequestPipeline: Send + Sync {
    /// 步骤 3–8：只读 hints → 路由决策 → 选通道 → 定点改写。
    async fn prepare(&self, snapshot: &RequestSnapshot) -> Result<PreparedRequest>;

    /// 步骤 9–12：分发（SSE 零缓冲转发）+ 计量 + trace。
    ///
    /// 铁律：**已向客户端写出第一个字节后绝不重试**（只记 `stream_broken`）。
    async fn execute(&self, prepared: PreparedRequest) -> Result<Response>;
}

// ================================================================ 内核探针

/// 内核状态（`KernelStatus.state`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KernelState {
    Stopped,
    Starting,
    Ready,
    Restarting,
    Failed,
    Unknown,
}

impl KernelState {
    pub fn as_str(&self) -> &'static str {
        match self {
            KernelState::Stopped => "stopped",
            KernelState::Starting => "starting",
            KernelState::Ready => "ready",
            KernelState::Restarting => "restarting",
            KernelState::Failed => "failed",
            KernelState::Unknown => "unknown",
        }
    }
}

/// 只读的内核状态探针（`/healthz` 与 `GET /status` 用）。
///
/// 与 `AdminBackend` 的分工：探针只**读**状态，运维动作（重启等）走 backend。
pub trait KernelProbe: Send + Sync {
    fn state(&self) -> KernelState;
    fn version(&self) -> Option<String> {
        None
    }
    fn current_node(&self) -> Option<String> {
        None
    }
}

/// 内核还没接进来时的探针：**诚实地报 `unknown`**，不假装 ready。
#[derive(Debug, Default, Clone, Copy)]
pub struct UnknownKernel;

impl KernelProbe for UnknownKernel {
    fn state(&self) -> KernelState {
        KernelState::Unknown
    }
}

// ================================================================ RPM 限流

/// 本机 RPM 限流器（固定窗口）。
///
/// 键是"loopback"：网关只绑 127.0.0.1，所有客户端都在本机，所以**进程级总量**
/// 才是诚实的语义（本机保护，防止某个脚本把自家网关打爆）。
#[derive(Debug, Default)]
pub struct RpmLimiter {
    window: Mutex<Window>,
}

#[derive(Debug, Clone, Copy, Default)]
struct Window {
    started_ms: i64,
    count: u32,
}

impl RpmLimiter {
    pub fn new() -> Self {
        Self::default()
    }

    /// 记一次请求。超限返回 `Err(retry_after_s)`；`limit == 0` 视为未配置。
    ///
    /// 锁中毒（某次请求 panic）时取回内部值继续用 —— 否则一次 panic 会让网关永久 429。
    pub fn check(&self, now_ms: i64, limit: u32, window_ms: i64) -> std::result::Result<(), u64> {
        if limit == 0 {
            return Ok(());
        }
        let window_ms = window_ms.max(1);
        let mut guard = match self.window.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if now_ms.saturating_sub(guard.started_ms) >= window_ms {
            guard.started_ms = now_ms;
            guard.count = 0;
        }
        guard.count = guard.count.saturating_add(1);
        if guard.count > limit {
            let elapsed = now_ms.saturating_sub(guard.started_ms);
            let remaining_ms = window_ms.saturating_sub(elapsed).max(0);
            return Err((remaining_ms as u64).div_ceil(1000).max(1));
        }
        Ok(())
    }
}

// ================================================================ 状态与装配

/// 入站选项。**RPM 上限没有配置字段**（`GatewayConfig` 里没有 `rpm_limit`），
/// 所以它是构造参数而不是从 `gateway.extra` 里偷偷读 —— 静默不生效的配置
/// 比编译失败更危险（L10）。
#[derive(Debug, Clone, Copy)]
pub struct IngressOptions {
    /// `None` / `Some(0)` = 关闭本机限流
    pub rpm_limit: Option<u32>,
    pub rpm_window_ms: i64,
}

impl Default for IngressOptions {
    fn default() -> Self {
        Self { rpm_limit: None, rpm_window_ms: 60_000 }
    }
}

/// 网关运行状态（给 `app_status` / 托盘读）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GatewayRunState {
    Running,
    Paused,
}

/// **暂停/恢复闸门**（托盘与状态栏的"暂停网关"按钮）。
///
/// 语义（与主管逐条对齐）：
/// 1. 暂停 = **不再接受新的请求**（在途请求**不取消** —— 那是 G2③̶ 的语义，两者正交）；
/// 2. **配置与热重载不受影响**：这里只是一个准入开关，不碰 `ArcSwap<ConfigDoc>`；
/// 3. 状态可被外部读到（[`GatewayGate::state`]），`/healthz` **仍然可用**
///    ——  paused 必须是"我按了暂停"，不能显示成"网关失联"。
///
/// 它挂在 [`IngressState`] 上（**唯一装配路径**），不是第二个服务入口：
/// 在 ingress 外侧再包一层就是"第二条请求路径"， 明令禁止。
#[derive(Debug, Default)]
pub struct GatewayGate {
    paused: std::sync::atomic::AtomicBool,
}

impl GatewayGate {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn pause(&self) {
        self.paused.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn resume(&self) {
        self.paused.store(false, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn state(&self) -> GatewayRunState {
        if self.paused.load(std::sync::atomic::Ordering::SeqCst) {
            GatewayRunState::Paused
        } else {
            GatewayRunState::Running
        }
    }

    pub fn is_paused(&self) -> bool {
        self.state() == GatewayRunState::Paused
    }
}

/// 暂停期间拒绝新请求用的错误。
///
/// 用 的专用码 **`TA-1005 tokenagency_gateway_paused`（503，retryable=false）**：
/// - **不可重试**是关键：客户端拿到它会把话抛给用户，而**不会对着暂停的网关猛重试**
///   （`TA-1002`/429 或 `TA-3001`/`TA-3002` 都是 retryable，语义上就错了）；
/// - 与"内核未就绪 / 线路不可用"（也都是 503）靠 `code` 与 `type` 区分 ——
///   用户看到的必须是"**我自己按了暂停**"，不是"中转站坏了"。
fn gateway_paused_error() -> TaError {
    TaError::GatewayPaused
}

/// 入站运行时依赖。
pub struct IngressState {
    pub cfg: Arc<arc_swap::ArcSwap<ConfigDoc>>,
    pub clock: Arc<dyn Clock>,
    pub passthrough: Arc<dyn PassthroughPolicy>,
    /// 引用解析器：`gateway.local_key` 允许写成 `"${secret:name}"` / `"${env:VAR}"`
    /// （默认配置就是），所以鉴权前必须解析。与通道凭据共用同一个解析器 ——
    /// 同一套引用语法有两份实现，迟早会漂移。
    pub secrets: Arc<dyn SecretResolver>,
    /// API key 验票接缝。默认件是 [`NoApiKeys`]：一把 key 都没有 =
    /// 不做多 key 鉴权。装配点见 `RuntimeParts::with_api_keys`。
    pub api_keys: Arc<dyn ApiKeyVerifier>,
    pub pipeline: Arc<dyn RequestPipeline>,
    pub kernel: Arc<dyn KernelProbe>,
    /// 暂停/恢复闸门（`app_pause_gateway` / `app_resume_gateway` 的接缝）。
    pub gate: Arc<GatewayGate>,
    pub rpm: RpmLimiter,
    pub options: IngressOptions,
}

impl IngressState {
    pub fn new(
        cfg: Arc<arc_swap::ArcSwap<ConfigDoc>>,
        clock: Arc<dyn Clock>,
        passthrough: Arc<dyn PassthroughPolicy>,
        secrets: Arc<dyn SecretResolver>,
        pipeline: Arc<dyn RequestPipeline>,
        kernel: Arc<dyn KernelProbe>,
        options: IngressOptions,
    ) -> Self {
        Self {
            cfg,
            clock,
            passthrough,
            secrets,
            api_keys: Arc::new(NoApiKeys),
            pipeline,
            kernel,
            gate: Arc::new(GatewayGate::new()),
            rpm: RpmLimiter::new(),
            options,
        }
    }

    /// 装上 API key 验票器（`build_runtime` 把 `RuntimeParts::api_keys` 传进来）。
    pub fn with_api_keys(mut self, api_keys: Arc<dyn ApiKeyVerifier>) -> Self {
        self.api_keys = api_keys;
        self
    }

    /// 用调用方持有的闸门装配（`build_runtime` 用它把同一个 `Arc` 交给运行时与 `app_*` 命令）。
    pub fn with_gate(mut self, gate: Arc<GatewayGate>) -> Self {
        self.gate = gate;
        self
    }

    /// 从 `lib.rs` 的依赖包装配（T2 用）。
    pub fn from_deps(
        deps: &Arc<GatewayDeps>,
        passthrough: Arc<dyn PassthroughPolicy>,
        secrets: Arc<dyn SecretResolver>,
        pipeline: Arc<dyn RequestPipeline>,
        kernel: Arc<dyn KernelProbe>,
        options: IngressOptions,
    ) -> Self {
        Self::new(
            deps.config.clone(),
            deps.clock.clone(),
            passthrough,
            secrets,
            pipeline,
            kernel,
            options,
        )
    }
}

/// 构建 axum 路由（路由表）。
///
/// 方法不匹配由 axum 自己挡（405）；`ANY /*` 由 `fallback` 承担。
pub fn router(state: Arc<IngressState>) -> Router {
    Router::new()
        .route("/v1/chat/completions", post(handle))
        .route("/v1/responses", post(handle))
        .route("/v1/messages", post(handle))
        .route("/v1/embeddings", post(handle))
        .route("/v1/images/generations", post(handle))
        .route("/v1/audio/{*rest}", post(handle))
        .route("/v1/files", post(handle))
        .route("/v1/models", get(handle))
        .route("/v1/models/{id}", get(handle))
        // 不带 `/v1` 的形态也本地作答（否则会原样透传上游，把服务商的
        // 完整模型清单端给客户端 —— 就是用户报的"穿透"）。
        .route("/models", get(handle))
        .route("/models/{id}", get(handle))
        .route("/healthz", get(handle))
        .fallback(any(handle))
        .with_state(state)
}

/// 绑定监听端口。**只接受 loopback**（I1：永不监听 `0.0.0.0`）。
///
/// `localhost` 会被**归一化**成 `127.0.0.1`（人就是会写 `localhost`，让它变成
/// 一个"启动失败"的坑不值得）。注意这是**归一化而不是信任**：归一化之后仍然
/// 走同一套 loopback 检查，I1 不受影响。
pub fn parse_loopback_addr(addr: &str) -> Result<SocketAddr> {
    let trimmed = addr.trim();
    let normalized = normalize_localhost(trimmed);
    let socket: SocketAddr = normalized
        .parse()
        .map_err(|e| TaError::Internal(format!("监听地址不合法：{addr}（{e}）")))?;
    if !socket.ip().is_loopback() {
        return Err(TaError::Internal(format!(
            "只允许绑定 loopback（127.0.0.1 / ::1），拒绝 {socket}：本产品永不监听 0.0.0.0"
        )));
    }
    Ok(socket)
}

/// `localhost:8787` → `127.0.0.1:8787`（大小写不敏感）。
/// `localhostevil:80` 这类会被拼成非法地址而**解析失败** —— 失败的姿势是安全的。
fn normalize_localhost(addr: &str) -> String {
    const NAME: &str = "localhost";
    if addr.len() >= NAME.len() && addr[..NAME.len()].eq_ignore_ascii_case(NAME) {
        return format!("127.0.0.1{}", &addr[NAME.len()..]);
    }
    addr.to_string()
}

pub async fn bind_loopback(addr: &str) -> Result<tokio::net::TcpListener> {
    let socket = parse_loopback_addr(addr)?;
    tokio::net::TcpListener::bind(socket)
        .await
        .map_err(|e| TaError::Internal(format!("绑定 {socket} 失败：{e}")))
}

/// 起网关（监听地址取 `gateway.listen`）。
pub async fn serve(state: Arc<IngressState>) -> Result<()> {
    let listen = state.cfg.load().gateway.listen.clone();
    let listener = bind_loopback(&listen).await?;
    serve_on(listener, state).await
}

pub async fn serve_on(listener: tokio::net::TcpListener, state: Arc<IngressState>) -> Result<()> {
    axum::serve(listener, router(state))
        .await
        .map_err(|e| TaError::Internal(format!("网关服务退出：{e}")))
}

// ================================================================ 请求处理

async fn handle(State(state): State<Arc<IngressState>>, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    let request_id = new_request_id();
    let endpoint = classify(&parts.method, parts.uri.path());

    // `/healthz`：不走上游、不校验密钥
    if endpoint == Endpoint::Healthz {
        return healthz_response(&state, &request_id);
    }

    // 一切真实请求都在这一个 span 里处理（见 `handle_request` 的文档）：
    // 之后**任何模块**打的日志都自动带上 `request_id`，不用每处重复写一遍。
    handle_request(state, parts, body, request_id, endpoint).await
}

/// 请求处理主体。**唯一的作用是给整条路径套一个带 `request_id` 的 span。**
///
/// 要求"拿一个 `request_id` 能筛出它经过的全部日志行，含跨 crate 与后台分叉"。
/// 靠"每个日志点都手写 `request_id = %rid`"是守不住的：新加一行忘了写就静默失效。
/// 用 `#[instrument]` 把 span 挂在**函数**上，`tracing` 会在每次 poll 时正确进入/退出
/// （不是 `span.enter()` 那种跨 `.await` 的错法），于是：
/// - `ingress` / `pipeline` / `dispatch` 里的日志**自动**带上 `request_id`；
/// - 后台分叉（`tokio::spawn`）要用 `Span::current()` 显式带过去 —— 那是有意识的动作。
///
/// `skip_all` + `fields(request_id = ...)`：请求体/头**绝不**进 span（那是用户数据）。
#[tracing::instrument(
    name = "request",
    skip_all,
    fields(request_id = %request_id)
)]
async fn handle_request(
    state: Arc<IngressState>,
    parts: http::request::Parts,
    body: Body,
    request_id: String,
    endpoint: Endpoint,
) -> Response {
    let path = parts.uri.path().to_string();
    let query = parts.uri.query().map(str::to_string);
    let cfg = state.cfg.load_full();
    let now_ms = state.clock.now_ms();

    //：**全部** `x-tokenagency-*` 都受 `gateway.debug_headers` 控制。
    // 出口只有一个，所以在这里统一收口 —— 逐个 return 去记"这个要不要加"迟早会漏一个。
    // 注意：**错误信封 body 里的 `request_id` 不受影响**（那是内容，不是头），
    // 关掉调试头只是让"对未知头敏感的客户端"看不见它们。
    let finish = |mut response: Response| -> Response {
        if !cfg.gateway.debug_headers {
            // HeaderMap 没有 retain，先收集名字再删（不能边遍历边改）
            let ours: Vec<http::HeaderName> = response
                .headers()
                .keys()
                .filter(|name| name.as_str().starts_with("x-tokenagency-"))
                .cloned()
                .collect();
            for name in ours {
                response.headers_mut().remove(&name);
            }
        }
        response
    };

    let limit = cfg.gateway.max_body_mb.saturating_mul(1024 * 1024);

    // 暂停闸门：**新请求一律拒绝**，在途的不受影响（那是 G2③ 的取消语义，别混在一起）。
    // 放在鉴权**之前**：暂停时不必再做解析/校验，也少一次钥匙串访问；
    // `/healthz` 已经在上面返回了 —— 托盘必须能显示"已暂停"，而不是"失联"。
    if state.gate.is_paused() {
        return finish(error_response(&gateway_paused_error(), &request_id));
    }

    // 先看声明的长度：超限的大 body 根本不必读进内存
    if let Some(size) = declared_length(&parts.headers) {
        if size > limit {
            return finish(error_response(&TaError::BodyTooLarge { size, limit }, &request_id));
        }
    }
    // 鉴权：本地 key / 库里的 API key。
    // 身份目前没有下游消费者（命中即记账在 `ApiKeyVerifier` 实现内部完成），返回它是为了让
    // "这条请求是哪把 key 打进来的"有一个**唯一**的落点 —— 只有这一层看得到客户端 token。
    // 要把它写进请求日志/快照得先登记字段名（那不属于本轮）。
    let _identity = match check_local_key(
        &cfg,
        &parts.headers,
        state.secrets.as_ref(),
        state.api_keys.as_ref(),
    )
    .await
    {
        Ok(identity) => identity,
        Err(err) => return finish(error_response(&err, &request_id)),
    };

    // **下游 API 家族开关**（用户第 326 轮："是打开关闭形式，按需求勾选"）。
    //
    // 三件事是刻意的：
    // * 关掉的端点**当场明确报错**，绝不转发上游 —— 客户端配错却收到一个来自上游的
    //   404/400 时，用户从错误里**看不出**是自己少勾了一个开关，那是最难查的一类问题；
    // * 放在**鉴权之后**：没通过鉴权的调用者不该从错误文案里探到我们的配置状态；
    // * 放在暂停闸门之后：暂停是"现在一律不接单"，比"这个家族关着"更外层。
    if let Some(switch) = disabled_downstream(&cfg, endpoint) {
        return finish(error_response(
            &TaError::MalformedRequest(format!(
                "这个 API 家族当前是关着的（{switch}）。\
                 到 设置 → 服务 → 「对外提供的 API」把它打开，或者改用已打开的端点。"
            )),
            &request_id,
        ));
    }
    if let Some(limit) = state.options.rpm_limit {
        if let Err(retry_after_s) = state.rpm.check(now_ms, limit, state.options.rpm_window_ms) {
            let mut response = error_response(&TaError::RateLimited, &request_id);
            if let Ok(value) = HeaderValue::from_str(&retry_after_s.to_string()) {
                response.headers_mut().insert(header::RETRY_AFTER, value);
            }
            return finish(response);
        }
    }
    let body = match read_body_limited(body, limit).await {
        Ok(body) => body,
        Err(err) => return finish(error_response(&err, &request_id)),
    };

    //：`GET /v1/models` 与 `GET /v1/models/{id}` **不透传上游** ——
    // 鉴权 / 限流 / body 上限都过了之后，在"选线路"之前直接用我们自己的模型回答。
    // 位置是刻意的：**没跳过任何鉴权**，也不再选 lane、不发任何上游请求。
    if matches!(endpoint, Endpoint::ModelsList | Endpoint::ModelGet) {
        return finish(models_response(&cfg, endpoint, &path, now_ms, &request_id));
    }

    let snapshot = RequestSnapshot {
        request_id: request_id.clone(),
        method: parts.method,
        path,
        query,
        endpoint,
        headers: parts.headers,
        body,
        received_at_ms: now_ms,
    };

    let prepared = match state.pipeline.prepare(&snapshot).await {
        Ok(prepared) => prepared,
        Err(err) => {
            // 失败也要能在日志里按码+ID 查到（：报 TaError 必带 code）
            tracing::warn!(
                code = err.code(),
                retryable = err.retryable(),
                status = err.http_status(),
                error = %err,
                "请求在网关自身被拒"
            );
            return finish(error_response(&err, &request_id));
        }
    };
    match state.pipeline.execute(prepared).await {
        Ok(response) => finish(with_request_id(response, &request_id)),
        Err(err) => {
            tracing::warn!(
                code = err.code(),
                retryable = err.retryable(),
                status = err.http_status(),
                error = %err,
                "请求在网关自身被拒（分发/失败链）"
            );
            finish(error_response(&err, &request_id))
        }
    }
}

/// 生成请求 ID（`req_` 前缀 + nanoid，对应 trace）。
pub fn new_request_id() -> String {
    format!("req_{}", nanoid::nanoid!(12))
}

/// 按 `gateway.max_body_mb` 读取 body，超限即 413（`TA-1003`）。
///
/// 自己累加而不是用 `to_bytes(body, limit)`：这样能给出**真实**超出的字节数，
/// 并且错误信封里的 `details` 才有意义。
pub(crate) async fn read_body_limited(mut body: Body, limit: usize) -> Result<Bytes> {
    let mut buf = BytesMut::new();
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|e| TaError::Internal(format!("读取请求体失败：{e}")))?;
        let Ok(data) = frame.into_data() else { continue };
        let projected = buf.len().saturating_add(data.len());
        if projected > limit {
            return Err(TaError::BodyTooLarge { size: projected, limit });
        }
        buf.extend_from_slice(&data);
    }
    Ok(buf.freeze())
}

fn declared_length(headers: &HeaderMap) -> Option<usize> {
    header_str(headers, header::CONTENT_LENGTH.as_str())?.trim().parse().ok()
}

// ================================================================ 鉴权 / 错误信封

/// 这次请求以谁的身份通过鉴权。
///
/// 客户端送来的 token **只有这一层看得到**（`src-tauri` 的 `SecretResolver`、`GatewayDeps`
/// 都拿不到它），所以"这条请求是哪把 API key 打进来的"必须在鉴权那一刻定下来 ——
/// 它是 `api_key_usage` 归属的唯一依据。
///
/// 名字与 `src-tauri/src/apikey_auth.rs` 里计划的那个类型**同名同形**：
/// 应用层那个本地类型应当删掉改用这一个，否则就是第二份契约（一致时看不出来、漂移时静默）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyIdentity {
    /// 本地密钥（`gateway.local_key`）通过，或压根没配置鉴权
    Local,
    /// 命中库里某把**启用中**的 API key
    ApiKey(String),
}

/// 账本里 `gateway.local_key` 的固定 `key_id`（与 `src-tauri/src/apikey_auth.rs` 的
/// `LOCAL_KEY_ID` 同值：这是同一个口径，不是两套）。
pub const LOCAL_KEY_ID: &str = "local";

impl KeyIdentity {
    /// **直接写进 `api_key_usage.key_id` 的那个值**：本地身份是 `"local"`，其余是 key 的 id。
    pub fn key_id(&self) -> &str {
        match self {
            KeyIdentity::Local => LOCAL_KEY_ID,
            KeyIdentity::ApiKey(id) => id,
        }
    }
}

/// API key 校验接缝（多 key 鉴权）。
///
/// ## 为什么是接缝，而不是网关自己去查库
///
/// key 的存储、启用状态与"命中即记账"都在应用层（`src-tauri/src/apikey_auth.rs` 的
/// `ApiKeyAuthenticator`，它才拿得到 store 与用量表）。网关只负责**在唯一的鉴权点上**问它一句。
/// 这样"多 key"不会长成第二条鉴权路径 —— 一旦网关自己也去查库，两条路径的启用/删除语义
/// 迟早会不一致（"停用了还能用"这种洞就是这么来的）。
#[async_trait]
pub trait ApiKeyVerifier: Send + Sync {
    /// 库里是否**至少有一把启用中的 key**。
    ///
    /// 这是"`local_key` 没配时还要不要强制鉴权"的判据：一把都没有 → **保持既有行为**
    /// （不鉴权），否则每个既有用户升级后都会突然全部 401。
    async fn any_enabled(&self) -> Result<bool>;

    /// 命中启用中的访问密钥 → `Ok(Some(key_id))`；未命中 → `Ok(None)`。
    ///
    /// `Err` 只用于"查不动"（库故障/解密失败）—— 调用方**必须 fail closed**：
    /// 返回 `Err` 让请求失败，绝不退回"放行"。
    async fn verify(&self, token: &str) -> Result<Option<String>>;
}

/// 一把 API key 都没有时的实现（`IngressState::new` 的默认件）。
///
/// **默认件必须是"谁也不放行"**，而不是"都放行"：默认放行会让任何忘了装配的调用点
/// 静默变成不鉴权 ——— 那正是最不该有的失败方向。
pub struct NoApiKeys;

#[async_trait]
impl ApiKeyVerifier for NoApiKeys {
    async fn any_enabled(&self) -> Result<bool> {
        Ok(false)
    }

    async fn verify(&self, _token: &str) -> Result<Option<String>> {
        Ok(None)
    }
}

/// 鉴权：`gateway.local_key`（可为空）与库里的 API key。
///
/// ## 判定规则（主管定稿）
///
/// | 现状 | 行为 |
/// | --- | --- |
/// | `local_key` 为空 **且** 一把启用中的 key 都没有 | **不鉴权**（保持既有行为） |
/// | `local_key` 非空 **或** 有启用中的 key | 必须命中 ①本地 key 或 ②某把启用中的 api key，否则 **401 `TA-1001`** |
///
/// 注意是 **TA-1001 `unauthorized`（401，不可重试）**，不是 TA-1002：后者是 429 限流，
/// 客户端看到会**猛重试**一把根本没被接受的 key。
///
/// ## 为什么必须先解析引用
///
/// `gateway.local_key` 允许写成 `"${secret:name}"` / `"${env:VAR}"`，**而且默认配置就是**
/// `"${secret:gateway-local-key}"`（`ta-core/config/defaults.rs`）。客户端发来的是**明文**，
/// 所以"拿引用字面量当密码比"会让默认配置下的每一个请求都 401 —— 用户看到的是
/// "我的 key 错了"，而真正的原因是我们没去解析。
///
/// ## 三条不许退化的边界
///
/// 1. 引用解析失败 → **响亮地失败**（原样返回解析错误，TA-9001）。绝不退化成"按字面量比"，
///    那等于把引用本身当成密钥。
/// 2. 引用**写坏了**（`"${secret:}"` 这类，`is_reference` 为假但形似引用）→ 配置错误（TA-5001），
///    也不当明文密码。这正是 的通用原则：能提前判定就自己判定，选响亮的一侧。
/// 3. 解析出来是空串 → 配置错误。否则"钥匙串里存了个空值"会变成"空 key 即可通过"的放行洞。
///
/// 另一条：`any_enabled()` 只在 `local_key` 为空时才问 —— 本地密钥已经能定身份时，
/// 不必为每个请求多打一次库（库故障也不该连带打死本地 key，见
/// `the_local_key_path_does_not_touch_the_key_store`）。
pub async fn check_local_key(
    cfg: &ConfigDoc,
    headers: &HeaderMap,
    secrets: &dyn SecretResolver,
    keys: &dyn ApiKeyVerifier,
) -> Result<KeyIdentity> {
    let spec = cfg.gateway.local_key.as_deref().map(str::trim).filter(|key| !key.is_empty());
    let presented = bearer_token(headers).or_else(|| header_str(headers, "x-api-key"));

    let Some(spec) = spec else {
        // 没配本地密钥：只有库里确实有启用中的 key 时才强制鉴权；一把都没有 = 保持既有行为。
        // `any_enabled()` 出错 → `?` 直接失败（fail closed），不退回"放行"。
        if !keys.any_enabled().await? {
            return Ok(KeyIdentity::Local);
        }
        let Some(token) = presented.map(str::trim).filter(|token| !token.is_empty()) else {
            return Err(TaError::Unauthorized);
        };
        return match keys.verify(token).await? {
            Some(key_id) => Ok(KeyIdentity::ApiKey(key_id)),
            None => Err(TaError::Unauthorized),
        };
    };

    let expected = if is_reference(spec) {
        match secrets.resolve(spec).await {
            Ok(value) => value,
            Err(err) => {
                tracing::error!(
                    path = "gateway.local_key",
                    code = err.code(),
                    retryable = err.retryable(),
                    error = %err,
                    "本地密钥引用解析失败，网关无法鉴权（不降级为按字面量比较）"
                );
                return Err(err);
            }
        }
    } else if matches!(classify_literal(spec), Some(PlaintextKind::MalformedRef)) {
        tracing::error!(
            path = "gateway.local_key",
            reason = "引用写坏了（不是合法的 ${secret:name} / ${env:VAR}）",
            "本地密钥配置错误，拒绝一切请求（fail closed）"
        );
        return Err(TaError::ConfigInvalid {
            errors: vec![FieldError {
                path: "gateway.local_key".into(),
                code: "malformed_reference".into(),
                message: "本地密钥写成了非法引用，合法形式只有 ${secret:条目名} 与 ${env:变量名}"
                    .into(),
            }],
        });
    } else {
        spec.to_string()
    };

    if expected.trim().is_empty() {
        tracing::error!(
            path = "gateway.local_key",
            reason = "解析结果是空串",
            "本地密钥解析结果为空，拒绝一切请求（空 key 不是「不校验」）"
        );
        return Err(TaError::ConfigInvalid {
            errors: vec![FieldError {
                path: "gateway.local_key".into(),
                code: "empty_after_resolve".into(),
                message: "本地密钥解析后为空；空值不等于「不校验」，请清空该字段或填入真实密钥"
                    .into(),
            }],
        });
    }

    match presented {
        Some(key) if ct_eq(key.trim().as_bytes(), expected.as_bytes()) => Ok(KeyIdentity::Local),
        // 本地密钥没命中 → 再看库里的访问密钥；两条腿都命中不了才 401。
        // `verify()` 出错 → `?` 直接失败（fail closed）。
        Some(key) => match keys.verify(key.trim()).await? {
            Some(key_id) => Ok(KeyIdentity::ApiKey(key_id)),
            None => Err(TaError::Unauthorized),
        },
        None => Err(TaError::Unauthorized),
    }
}

pub(crate) fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    let value = header_str(headers, header::AUTHORIZATION.as_str())?;
    let (scheme, token) = value.split_once(' ')?;
    scheme.eq_ignore_ascii_case("bearer").then(|| token.trim())
}

fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name)?.to_str().ok()
}

/// 常量时间比较：本地密钥不该因为逐字节提前返回而泄漏长度/前缀信息。
pub(crate) fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// 这个端点所属的**下游 API 家族**被关掉了吗？返回要打开的开关名。
///
/// 只覆盖"用户能勾的三个家族"：其它端点（`/v1/models`、embeddings、audio、files、
/// wildcard）**没有开关**，一律照旧放行 —— `/v1/models` 尤其不能关，
/// 它是客户端发现我们能提供什么模型的唯一入口。
fn disabled_downstream(
    cfg: &ta_core::domain::ConfigDoc,
    endpoint: Endpoint,
) -> Option<&'static str> {
    let d = &cfg.gateway.downstream;
    match endpoint {
        Endpoint::ChatCompletions if !d.openai_chat => Some("gateway.downstream.openai_chat"),
        Endpoint::Responses if !d.openai_responses => Some("gateway.downstream.openai_responses"),
        Endpoint::Messages if !d.anthropic_messages => {
            Some("gateway.downstream.anthropic_messages")
        }
        _ => None,
    }
}

///  的错误信封。**只有网关自身判断失败才用它**，上游错误一律原样透传。
pub fn error_response(err: &TaError, request_id: &str) -> Response {
    let status =
        StatusCode::from_u16(err.http_status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let body = json!({
        "error": {
            "type": err.type_str(),
            "message": err.to_string(),
            "code": err.code(),
            "retryable": err.retryable(),
            "request_id": request_id,
            "trace_hint": format!("ta trace {request_id}"),
            "details": error_details(err),
        }
    });
    with_request_id((status, axum::Json(body)).into_response(), request_id)
}

fn error_details(err: &TaError) -> Value {
    match err {
        TaError::BodyTooLarge { size, limit } => json!({ "size": size, "limit": limit }),
        TaError::ConfigInvalid { errors } => serde_json::to_value(errors).unwrap_or(Value::Null),
        TaError::RateLimited => json!({ "scope": "loopback", "window": "1m" }),
        _ => Value::Null,
    }
}

/// 所有响应都带上 `x-tokenagency-request-id`（其余 `x-tokenagency-*` 由 G4 注入）。
pub(crate) fn with_request_id(mut response: Response, request_id: &str) -> Response {
    if !response.headers().contains_key("x-tokenagency-request-id") {
        if let Ok(value) = HeaderValue::from_str(request_id) {
            response.headers_mut().insert("x-tokenagency-request-id", value);
        }
    }
    response
}

fn healthz_response(state: &IngressState, request_id: &str) -> Response {
    let body = json!({ "status": "ok", "kernel": state.kernel.state().as_str() });
    with_request_id((StatusCode::OK, axum::Json(body)).into_response(), request_id)
}

// ================================================================ `/v1/models`：本地作答

/// `GET /v1/models` 的响应体：**我们自己的模型**，一条记录一个模型。
///
/// **为什么不再透传上游**：用户原话「返回的肯定是我们"模型"界面定义的每个模型的信息，
/// 字段来自我们创建的信息」。上游只认识**它自己**有什么模型，不知道我们对外暴露的逻辑模型
/// （一个逻辑模型背后可以有多条线路、多个上游）：透传会把上游的内部模型名当成我们的答案，
/// 同一个逻辑模型在不同上游还会返回互相矛盾的两份清单。
///
/// **口径（都是显式选择，不是顺带发生的事）**：
/// * **一个已配置模型一条记录**，`id` = 它的规范 id（[`Model::id`]）。别名是**被接受的名字**
///   （`/v1/models/{别名}` 与 `chat_completions` 的 `model` 字段都认），但**不**在 `data` 里
///   各占一条 —— 否则客户端会把同一个模型算成两个模型。
/// * `expose = false` 的模型**不出现**（[`Model::new`] 的注释写的就是"`expose = true` 才出现在
///   `/v1/models`"），它的 id/别名在这里也 404 —— 不然"隐藏"是假的。
/// * 字段**只来自我们的配置**，一个都不编（：上游没给的字段一个都不编）。
///   上下文窗口 / 最大输出**只在我们的线路真的声明了**的时候才带（见 [`model_info`]，
///   名字按客户端实际读的那两个键）；价格仍然一个都不带（我们没存价格）。
fn models_list_body(cfg: &ConfigDoc, now_ms: i64) -> Value {
    let data: Vec<Value> = cfg
        .models
        .iter()
        .filter(|m| m.enabled && m.expose)
        .map(|m| model_entry(cfg, m, now_ms))
        .collect();
    json!({ "object": "list", "data": data })
}

/// 按 id 或别名找**对外暴露**的模型（`None` = 不是我们的模型 → 调用方给 404）。
///
/// 路径段先按原样比（别名里的 `+` 是字面量，路径语义里不是空格），再退一步比百分号解码后的
/// 形式（`openai%2Fgpt-4o` 这种编码过的别名）。
fn exposed_model<'a>(cfg: &'a ConfigDoc, name: &str) -> Option<&'a Model> {
    let decoded = passthrough::percent_decode(name);
    cfg.models.iter().find(|m| {
        m.enabled && m.expose && (m.matches(name) || (decoded != name && m.matches(&decoded)))
    })
}

/// 该模型**公开出来的**能力数字 —— 直接取 那份**唯一口径**
/// （`ConfigDoc::public_model_info`）：网关与模型页「查看模型信息」面板用的是同一个函数，
/// 不会出现"客户端看到的"和"界面显示的"两套数字。
///
/// **为什么以前一个都不带**：用户原话「上下文信息那些根本没有透传过来」。客户端（DeepSeek Harness）
/// 的「获取可用模型」读的正是 `context_window` / `max_output_tokens`
/// （在它的 `readListing` 里逐字核过：`context_window` 之后才看 `context_length` / `max_input_tokens`；
/// `max_output_tokens` 之后才看 `maxTokens` / `max_tokens`）。
fn model_info(cfg: &ConfigDoc, model: &Model) -> (Option<u64>, Option<u64>) {
    let info = cfg.public_model_info(model);
    (info.context_window, info.max_output_tokens)
}

/// `/v1/models` 里一条记录的**唯一**构造点。
///
/// * `created`：**本次回答的时刻**（`now_ms / 1000`）。我们的配置里不存模型的创建时间，
///   而 OpenAI 客户端要求这个整数字段 —— 给响应时刻是唯一不编造"创建时间"的选择。
/// * `owned_by`：见 [`owned_by`]。
/// * `context_window` / `max_output_tokens`：见 [`model_info`]（**没声明就不出现**）。
fn model_entry(cfg: &ConfigDoc, model: &Model, now_ms: i64) -> Value {
    let mut entry = json!({
        "id": model.id,
        "object": "model",
        "created": now_ms / 1000,
        "owned_by": owned_by(cfg, model),
    });
    let (context_window, max_output_tokens) = model_info(cfg, model);
    if let Some(map) = entry.as_object_mut() {
        if let Some(v) = context_window {
            map.insert("context_window".to_string(), json!(v));
        }
        if let Some(v) = max_output_tokens {
            map.insert("max_output_tokens".to_string(), json!(v));
        }
    }
    entry
}

/// `owned_by` = 该模型**第一条线路**所属服务商的 `id`（：线路归服务商）。
///
/// 用 `Provider.id`（稳定标识）而不是 `name`（可改的显示名），也**不编**一个我们没有的机构名；
/// 没有可解析的线路（悬空/空 `lines`，校验器本会拦下）时退化成我们自己的名字字面量
/// `"tokenagency"`。
fn owned_by(cfg: &ConfigDoc, model: &Model) -> String {
    model
        .lines
        .first()
        .and_then(|id| cfg.channel(id.as_str()))
        .and_then(|channel| cfg.provider(channel.provider.as_str()))
        .map(|provider| provider.id.as_str().to_string())
        .unwrap_or_else(|| "tokenagency".to_string())
}

/// `/v1/models` 与 `/v1/models/{id}` 的本地响应。
///
/// **不选线路、不发任何上游请求**：调用点在鉴权 / 限流 / body 上限之后、"选线路"之前。
fn models_response(
    cfg: &ConfigDoc,
    endpoint: Endpoint,
    path: &str,
    now_ms: i64,
    request_id: &str,
) -> Response {
    if endpoint == Endpoint::ModelsList {
        let body = models_list_body(cfg, now_ms);
        return with_request_id((StatusCode::OK, axum::Json(body)).into_response(), request_id);
    }

    // `classify` 保证了 `/v1/models/` 前缀 + 非空 id 段；取不到就按"不是我们的模型"处理。
    let name = path.strip_prefix("/v1/models/").unwrap_or_default();
    match exposed_model(cfg, name) {
        Some(model) => with_request_id(
            (StatusCode::OK, axum::Json(model_entry(cfg, model, now_ms))).into_response(),
            request_id,
        ),
        // 不是我们的 id/别名 → 404，错误信封仍是 OpenAI 形状（`error.message/type/code`）
        None => error_response(&TaError::ModelNotFound(name.to_string()), request_id),
    }
}

// ================================================================ 测试

#[cfg(test)]
mod tests {
    use super::*;
    use crate::passthrough::{canonical_headers, tests::golden_fixture, FullPassthrough};
    use axum::body::Body;
    use ta_core::clock::ManualClock;
    use tower::ServiceExt;

    const SECRET: &str = "sk-relay-a-secret";

    fn test_cfg() -> ConfigDoc {
        let fixture = golden_fixture();
        let mut doc = ConfigDoc::default();
        doc.providers.push(fixture.provider);
        doc.channels.push(fixture.channel);
        doc
    }

    /// 固定响应的 mock pipeline：把"上游会收到什么"原样回显，供测试断言。
    struct EchoPipeline {
        cfg: ConfigDoc,
        policy: Arc<dyn PassthroughPolicy>,
    }

    #[async_trait]
    impl RequestPipeline for EchoPipeline {
        async fn prepare(&self, snapshot: &RequestSnapshot) -> Result<PreparedRequest> {
            let fixture = golden_fixture();
            build_prepared_request(PrepareInput {
                cfg: &self.cfg,
                policy: self.policy.as_ref(),
                snapshot,
                channel: &fixture.channel,
                provider: Some(&fixture.provider),
                secret: SECRET,
                lane_id: None,
            })
        }

        async fn execute(&self, prepared: PreparedRequest) -> Result<Response> {
            let view = json!({
                "request_id": prepared.request_id,
                "endpoint": prepared.endpoint.as_str(),
                "protocol": prepared.protocol,
                "client_protocol": prepared.client_protocol,
                "upstream_url": prepared.upstream_url,
                "upstream_model": prepared.upstream_model,
                "channel_id": prepared.channel_id.as_str(),
                "stream": prepared.stream,
                "has_image": prepared.has_image,
                "header_diff_empty": prepared.header_diff.is_empty(),
                "headers": canonical_headers(&prepared.headers),
                "body": serde_json::from_slice::<Value>(&prepared.body).unwrap_or(Value::Null),
            });
            Ok((StatusCode::OK, axum::Json(view)).into_response())
        }
    }

    fn state_with(
        cfg: ConfigDoc,
        options: IngressOptions,
        kernel: KernelState,
    ) -> Arc<IngressState> {
        state_with_secrets(
            cfg,
            options,
            kernel,
            Arc::new(crate::pipeline::StaticSecrets::default()),
        )
    }

    fn state_with_secrets(
        cfg: ConfigDoc,
        options: IngressOptions,
        kernel: KernelState,
        secrets: Arc<dyn SecretResolver>,
    ) -> Arc<IngressState> {
        Arc::new(ingress_state_with_secrets(cfg, options, kernel, secrets))
    }

    /// 未包 `Arc` 的装配（要调 `with_api_keys` 这种消费 `self` 的构造器时用）。
    fn ingress_state_with_secrets(
        cfg: ConfigDoc,
        options: IngressOptions,
        kernel: KernelState,
        secrets: Arc<dyn SecretResolver>,
    ) -> IngressState {
        struct FixedKernel(KernelState);
        impl KernelProbe for FixedKernel {
            fn state(&self) -> KernelState {
                self.0
            }
        }
        let clock = Arc::new(ManualClock::new(1_700_000_000_000));
        IngressState::new(
            Arc::new(arc_swap::ArcSwap::from_pointee(cfg.clone())),
            clock,
            FullPassthrough::shared(),
            secrets,
            Arc::new(EchoPipeline { cfg, policy: FullPassthrough::shared() }),
            Arc::new(FixedKernel(kernel)),
            options,
        )
    }

    fn state() -> Arc<IngressState> {
        state_with(test_cfg(), IngressOptions::default(), KernelState::Ready)
    }

    /// `/v1/models` 的夹具：**两个**模型，覆盖"两条线路 / 一条线路 / 隐藏"三种形态。
    ///
    /// * `gpt-4o`：两个别名，两条线路（`ch-openai` 在前 → `owned_by` 应是它那个服务商 `p-openai`）；
    /// * `claude-sonnet-4-5`：一条线路；
    /// * `m-hidden`：`expose = false` —— 清单与 `/v1/models/{id}` 里都不许出现。
    fn models_cfg() -> ConfigDoc {
        let mut doc = ConfigDoc::default();
        doc.providers.push(Provider::new("p-openai", "OpenAI 中转", Protocol::Openai, "http://a"));
        doc.providers.push(Provider::new(
            "p-anthropic",
            "Anthropic",
            Protocol::Anthropic,
            "http://b",
        ));
        doc.credentials
            .push(ta_core::domain::Credential::new("c-openai", "p-openai", "K", "${s:1}"));
        doc.credentials.push(ta_core::domain::Credential::new(
            "c-anthropic",
            "p-anthropic",
            "K",
            "${s:2}",
        ));
        // `ch-openai-2` 刻意排在前面，证明 `owned_by` 取的是**配置顺序的第一条**线路
        doc.channels.push(ChannelConfig::new(
            "ch-openai-2",
            "",
            "p-anthropic",
            "c-anthropic",
            "gpt-4o-b",
        ));
        doc.channels.push(ChannelConfig::new("ch-openai", "", "p-openai", "c-openai", "gpt-4o"));
        doc.channels.push(ChannelConfig::new(
            "ch-claude",
            "",
            "p-anthropic",
            "c-anthropic",
            "claude",
        ));
        // 线路**声明**的能力 —— `/v1/models` 要把它透传给客户端：
        // `gpt-4o` 的两条线路声明不同（200k/128k、8192/16384）⇒ 取**最小值**（不夸大）；
        // `claude` 那条**什么都不声明** ⇒ 那两个键必须**不出现**（不编 0、不猜）。
        {
            let two = doc.channels.iter_mut().find(|c| c.id.as_str() == "ch-openai-2").unwrap();
            two.capabilities.context_tokens = 200_000;
            two.capabilities.max_output_tokens = Some(8_192);
            let one = doc.channels.iter_mut().find(|c| c.id.as_str() == "ch-openai").unwrap();
            one.capabilities.context_tokens = 128_000;
            one.capabilities.max_output_tokens = Some(16_384);
        }
        doc.models.push(
            Model::new("gpt-4o")
                .with_aliases(["gpt-4o-2024-11-20", "openai/gpt-4o"])
                .with_lines(["ch-openai-2", "ch-openai"]),
        );
        doc.models.push(Model::new("claude-sonnet-4-5").with_lines(["ch-claude"]));
        doc.models.push(Model {
            expose: false,
            lines: vec![ChannelId::new("ch-claude")],
            ..Model::new("m-hidden")
        });
        // **模型级总开关关掉**的模型：`expose` 还是 true，
        // 但它必须既不进清单、也不可通过 `/v1/models/{id}` 取到。
        doc.models.push(Model {
            enabled: false,
            lines: vec![ChannelId::new("ch-claude")],
            ..Model::new("m-off")
        });
        doc
    }

    fn models_state() -> Arc<IngressState> {
        state_with(models_cfg(), IngressOptions::default(), KernelState::Ready)
    }

    ///：`/v1/models` 的**本地**答案形状与口径。
    ///
    /// 断言的是完整 `data` 数组（逐字段），因为"多一条 / 少一条 / 多一个字段"都是这次改动的
    /// 核心契约；`created` 用固定时钟的 `1_700_000_000` 钉住。
    #[tokio::test]
    async fn models_list_is_answered_locally_from_our_own_config() {
        let (status, headers, value) =
            send(models_state(), json_request("GET", "/v1/models", b"")).await;
        assert_eq!(status, StatusCode::OK);
        assert!(headers.contains_key("x-tokenagency-request-id"));
        assert_eq!(value["object"], "list");
        assert_eq!(
            value["data"],
            json!([
                {
                    "id": "gpt-4o",
                    "object": "model",
                    "created": 1_700_000_000i64,
                    // 第一条线路 `ch-openai-2` → 服务商 `p-anthropic`（不是按 id 猜出来的）
                    "owned_by": "p-anthropic",
                    // 线路声明的能力透传给客户端；两条线路不同 ⇒ 取最小值
                    "context_window": 128_000,
                    "max_output_tokens": 8_192,
                },
                {
                    "id": "claude-sonnet-4-5",
                    "object": "model",
                    "created": 1_700_000_000i64,
                    "owned_by": "p-anthropic",
                    // 这条线路什么都没声明 ⇒ 两个键都不出现（不编 0）
                },
            ]),
            "一个已配置模型一条记录；别名不各占一条；expose=false 不出现；字段只来自配置：{value}"
        );
        // 字段集固定：没声明的模型**不许**有上下文/输出字段；声明过的模型**必须**有
        let first = value["data"][0].as_object().expect("一条记录是对象");
        assert_eq!(
            first.keys().collect::<Vec<_>>(),
            ["id", "object", "created", "owned_by", "context_window", "max_output_tokens"],
            "声明过的能力要透传（名字按客户端实际读的键）"
        );
        let second = value["data"][1].as_object().expect("一条记录是对象");
        assert_eq!(
            second.keys().collect::<Vec<_>>(),
            ["id", "object", "created", "owned_by"],
            "没声明就不许编字段"
        );
    }

    ///：**模型级开关关掉 = 彻底停用**，不是"藏起来"。
    ///
    /// 两件一起验，缺一不可：
    /// * 清单里没有它（`enabled=false` 即使 `expose=true` 也不列）；
    /// * 按名字取它 → **404 `ModelNotFound`**，而不是"查得到但不给用"。
    ///
    /// 只验第一条会让"藏起来但照旧能打"蒙混过关 —— 那正是用户说的"频繁删除与创建"
    /// 想避免的坑：关掉就该等于不存在。
    #[tokio::test]
    async fn disabled_model_is_neither_listed_nor_resolvable() {
        let state = models_state();
        let (status, _, value) = send(state.clone(), json_request("GET", "/v1/models", b"")).await;
        assert_eq!(status, StatusCode::OK);
        let ids: Vec<&str> = value["data"]
            .as_array()
            .expect("data 是数组")
            .iter()
            .filter_map(|m| m["id"].as_str())
            .collect();
        assert!(!ids.contains(&"m-off"), "停用的模型不该出现在清单里：{ids:?}");

        let (status, _, value) = send(state, json_request("GET", "/v1/models/m-off", b"")).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "停用的模型按名字取必须 404：{value}");
        assert_eq!(
            value["error"]["type"], "tokenagency_model_not_found",
            "必须是「模型不存在」，不能是别的错：{value}"
        );
    }

    /// 反向：`expose=false` 但**启用**的模型仍旧"藏起来可调用"（既有语义，没被这一轮改掉）。
    ///
    /// 这条是上面那条的对照组 —— 两个开关语义不同，别把它们混成一个。
    #[tokio::test]
    async fn hidden_but_enabled_model_stays_unlisted_and_unresolvable_by_name() {
        let (status, _, value) =
            send(models_state(), json_request("GET", "/v1/models/m-hidden", b"")).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "expose=false 也不该被 /v1/models/{{id}} 取到");
        assert_eq!(value["error"]["type"], "tokenagency_model_not_found");
    }

    ///：`/v1/models/{id}` 与清单**同一条记录**（能力字段也一起给）。
    #[tokio::test]
    async fn model_get_carries_the_same_declared_capabilities() {
        let (status, _, value) =
            send(models_state(), json_request("GET", "/v1/models/claude-sonnet-4-5", b"")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(value["context_window"], json!(null), "没声明就是没有：{value}");
        assert_eq!(value["max_output_tokens"], json!(null));

        let (status, _, value) =
            send(models_state(), json_request("GET", "/v1/models/gpt-4o", b"")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(value["context_window"], json!(128_000));
        assert_eq!(value["max_output_tokens"], json!(8_192));
    }

    /// 别名是**被接受的名字**：`/v1/models/{id}` 认它，但 `data` 里只有规范 id 一条。
    #[tokio::test]
    async fn model_get_answers_for_ids_and_aliases_alike() {
        let state = models_state();
        let expected = json!({
            "id": "gpt-4o",
            "object": "model",
            "created": 1_700_000_000i64,
            "owned_by": "p-anthropic",
            "context_window": 128_000,
            "max_output_tokens": 8_192,
        });
        for name in ["gpt-4o", "gpt-4o-2024-11-20", "openai/gpt-4o", "openai%2Fgpt-4o"] {
            let (status, _, value) =
                send(state.clone(), json_request("GET", &format!("/v1/models/{name}"), b"")).await;
            assert_eq!(status, StatusCode::OK, "{name} 应当命中");
            assert_eq!(value, expected, "{name} 应当返回同一条记录");
        }
    }

    /// 不是我们的 id/别名 → **404 + OpenAI 形状的错误信封**（不是空对象、也不是编一条记录）。
    #[tokio::test]
    async fn model_get_404s_with_an_openai_shaped_error_for_unknown_names() {
        let (status, _, value) =
            send(models_state(), json_request("GET", "/v1/models/gpt-3.5-turbo", b"")).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(value["error"]["type"], "tokenagency_model_not_found");
        assert_eq!(value["error"]["code"], "TA-2001");
        let message = value["error"]["message"].as_str().expect("要有 message");
        assert!(message.contains("gpt-3.5-turbo"), "错误里要点名是哪个名字：{message}");

        // `expose = false` 的模型在清单里被隐藏 → 按 id 也 404（"隐藏"不能是假的）
        let (status, _, value) =
            send(models_state(), json_request("GET", "/v1/models/m-hidden", b"")).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(value["error"]["code"], "TA-2001");
    }

    /// `/v1/models` **不再走上游**：`models_list` 零请求、零线路选择。
    ///
    /// 手法：把网关接到一个**真的会记账的 dialer** 上（替身在本文件的测试模块里实现，
    /// 因为 `pipeline.rs` 的替身是私有的），然后打 `/v1/models` —— 断言 `calls() == 0`；
    /// 对照组打 `/v1/chat/completions`，证明"这台 dialer 真会记账"
    /// （否则 `calls() == 0` 什么都证明不了）。
    #[tokio::test]
    async fn models_list_is_answered_locally_and_never_dials_upstream() {
        struct CountingDialer {
            calls: Arc<std::sync::atomic::AtomicU32>,
        }
        #[async_trait]
        impl crate::dispatch::OutboundDialer for CountingDialer {
            async fn execute(
                &self,
                _lane: &LaneId,
                _req: crate::dispatch::UpstreamRequest,
            ) -> Result<crate::dispatch::UpstreamResponse> {
                self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(crate::dispatch::UpstreamResponse {
                    status: StatusCode::OK,
                    headers: HeaderMap::new(),
                    body: crate::dispatch::byte_stream_from_bytes(Bytes::from_static(b"{}")),
                })
            }

            async fn health(&self, lane: &LaneId) -> crate::dispatch::HealthReport {
                crate::dispatch::HealthReport {
                    lane_id: lane.clone(),
                    ok: true,
                    latency_ms: None,
                    error: None,
                }
            }
        }

        /// 永远给一条候选 `ch-a`（对照组要能真的走到出站那一步）。
        struct AlwaysChA;
        impl ta_core::routing::PolicyEngine for AlwaysChA {
            fn decide(
                &self,
                _hints: &ta_core::routing::RouteHints,
                _ctx: &ta_core::routing::RouteContext,
            ) -> ta_core::routing::RouteDecision {
                ta_core::routing::RouteDecision {
                    candidates: vec![ChannelId::new("ch-openai")],
                    selector: ta_core::domain::Selector::BalancedSticky,
                    on_no_candidate: ta_core::domain::NoCandidateAction::Error,
                    ..Default::default()
                }
            }
        }
        struct FirstCandidate;
        impl ta_core::routing::ChannelSelector for FirstCandidate {
            fn select(
                &self,
                decision: &ta_core::routing::RouteDecision,
                _hints: &ta_core::routing::RouteHints,
                _ctx: &ta_core::routing::RouteContext,
            ) -> Option<ta_core::routing::Selection> {
                Some(ta_core::routing::Selection {
                    channel: decision.candidates.first()?.clone(),
                    sticky_hit: false,
                    sticky_key: None,
                    chain: decision.candidates.clone(),
                })
            }
        }
        struct SilentSink;
        #[async_trait]
        impl crate::pipeline::AttemptSink for SilentSink {
            async fn on_attempt(&self, _r: crate::pipeline::AttemptRecord) {}
        }

        let calls = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let cfg = models_cfg();
        let cfg_arc = Arc::new(arc_swap::ArcSwap::from_pointee(cfg.clone()));
        let clock = Arc::new(ManualClock::new(1_700_000_000_000));
        // ★ 对照组要真的走到"出站"那一步，就得让 `prepare` 能把 `${s:1}` 解析出来 ——
        //   否则它在选完通道之后、dial 之前就失败了，`calls()==0` 会变成一句空话
        //   （这不是"没出站"，是"压根没走到出站"）。
        let secrets: Arc<dyn crate::pipeline::SecretResolver> =
            Arc::new(crate::pipeline::StaticSecrets::new([
                ("${s:1}", "sk-openai"),
                ("${s:2}", "sk-anthropic"),
            ]));
        let pipeline =
            Arc::new(crate::pipeline::Pipeline::new(Arc::new(crate::pipeline::PipelineDeps {
                cfg: cfg_arc.clone(),
                clock: clock.clone(),
                planner: Arc::new(crate::runtime::PolicySelectorPlanner::new(
                    cfg_arc.clone(),
                    Arc::new(AlwaysChA),
                    Arc::new(FirstCandidate),
                )),
                secrets: secrets.clone(),
                dialer: Arc::new(CountingDialer { calls: calls.clone() }),
                passthrough: FullPassthrough::shared(),
                sink: Arc::new(SilentSink),
                meter: None,
            })));
        struct ReadyKernel;
        impl KernelProbe for ReadyKernel {
            fn state(&self) -> KernelState {
                KernelState::Ready
            }
        }
        let state = Arc::new(IngressState::new(
            cfg_arc,
            clock,
            FullPassthrough::shared(),
            secrets,
            pipeline,
            Arc::new(ReadyKernel),
            IngressOptions::default(),
        ));

        let (status, _, value) = send(state.clone(), json_request("GET", "/v1/models", b"")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(value["data"].as_array().map(Vec::len), Some(2));
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "models_list 不许有任何上游请求"
        );

        // **不带 `/v1` 的 `/models` 也必须本地作答。**
        //
        // 这条是用户第 325 轮"OpenAI 会直接穿透拿到服务商的所有模型"的**反回归闸门**：
        // 客户端把 base_url 填成不带 `/v1` 的形态时会请求 `GET /models`，
        // 以前它落进 `fallback` 被原样透传上游 → 端回服务商自己的完整清单
        //（而且带着上游的真实模型名，等于把上游目录泄露给客户端）。
        let (status, _, value) = send(state.clone(), json_request("GET", "/models", b"")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            value["data"].as_array().map(Vec::len),
            Some(2),
            "不带 /v1 也必须返回**我们自己配置的**模型清单"
        );
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "GET /models 也不许碰上游（以前这里是穿透）"
        );

        // 对照组：同一条装配打 chat_completions → 确实会出站（证明这台 dialer 会记账）
        let body = serde_json::to_vec(&json!({"model": "gpt-4o", "messages": []})).unwrap();
        let (_, _, _) = send(state, json_request("POST", "/v1/chat/completions", &body)).await;
        assert!(
            calls.load(std::sync::atomic::Ordering::SeqCst) > 0,
            "对照组必须真的出站，否则 calls()==0 什么都没证明"
        );
    }

    /// 设置页关掉的**下游 API 家族**必须当场明确报错。
    ///
    /// 用户第 326 轮的需求 2："可以在设置页面选择我们下游是统一成 OPENAI 的两种还是
    /// Anthropic 的" —— 补一句"是打开关闭形式，按需求勾选"。这条钉两件事：
    ///
    /// ① 关掉的端点返回**明确**错误，且错误里点出**是哪个开关**（否则用户只知道"不通了"）；
    /// ② **另一个家族不受影响** —— 三个开关是**独立**的，不是"选一个"。
    #[tokio::test]
    async fn disabled_downstream_family_is_refused_and_other_families_still_work() {
        let mut cfg = models_cfg();
        cfg.gateway.downstream.anthropic_messages = false;
        let state = state_with(cfg, IngressOptions::default(), KernelState::Ready);

        let (status, _, value) =
            send(state.clone(), json_request("POST", "/v1/messages", b"{}")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "关掉的消息端点必须明确拒绝");
        let message = value["error"]["message"].as_str().unwrap_or_default();
        assert!(
            message.contains("anthropic_messages"),
            "错误里必须点出是哪个开关，否则用户只能猜：{value}"
        );

        // OpenAI 家族**不该**被同一个开关连带关掉
        let body = serde_json::to_vec(&json!({"model": "gpt-4o", "messages": []})).unwrap();
        let (_, _, other) = send(state, json_request("POST", "/v1/chat/completions", &body)).await;
        assert!(
            !other.to_string().contains("是关着的"),
            "chat/completions 不该被 anthropic_messages 的开关连带关掉：{other}"
        );
    }

    /// 跨协议时**上游要打的端点路径**由目标协议决定（§2）。
    ///
    /// 三件事各自都会错（错一个就是必然 404/400）：
    /// ① 不能把客户端的路径原样带过去（把 `/v1/messages` 打给 OpenAI 端点）；
    /// ② Responses 有自己的端点 `/v1/responses`，不是 chat/completions；
    /// ③ **Gemini 的模型名在路径里**，而且流式换一个方法名 —— 它给不出常量。
    #[test]
    fn upstream_path_follows_the_target_protocol() {
        // 非 Gemini：常量端点
        assert_eq!(upstream_path_for(Protocol::Anthropic, "whatever", false), "/v1/messages");
        assert_eq!(upstream_path_for(Protocol::Openai, "whatever", true), "/v1/chat/completions");
        assert_eq!(
            upstream_path_for(Protocol::OpenaiResponses, "whatever", false),
            "/v1/responses"
        );
        // Gemini：**模型名进路径**，流式换方法 + `alt=sse`
        assert_eq!(
            upstream_path_for(Protocol::Gemini, "gemini-2.5-pro", false),
            "/v1beta/models/gemini-2.5-pro:generateContent"
        );
        assert_eq!(
            upstream_path_for(Protocol::Gemini, "gemini-2.5-pro", true),
            "/v1beta/models/gemini-2.5-pro:streamGenerateContent?alt=sse"
        );
    }

    /// 鉴权**没有**因为本地回答而被绕过：`local_key` 配了却带错 key → 401，而不是拿到清单。
    #[tokio::test]
    async fn models_list_still_requires_the_local_key() {
        let mut cfg = models_cfg();
        cfg.gateway.local_key = Some("sk-local".into());
        let state = state_with(cfg, IngressOptions::default(), KernelState::Ready);

        let (status, _, value) = send(state.clone(), json_request("GET", "/v1/models", b"")).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(value["error"]["code"], "TA-1001");

        let mut request = json_request("GET", "/v1/models", b"");
        request
            .headers_mut()
            .insert(header::AUTHORIZATION, HeaderValue::from_static("Bearer sk-local"));
        let (status, _, value) = send(state, request).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(value["object"], "list");
    }

    async fn send(state: Arc<IngressState>, request: Request) -> (StatusCode, HeaderMap, Value) {
        let response = router(state).oneshot(request).await.expect("路由必须能处理请求");
        let status = response.status();
        let headers = response.headers().clone();
        let bytes =
            axum::body::to_bytes(response.into_body(), 8 * 1024 * 1024).await.expect("响应体可读");
        let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, headers, value)
    }

    fn json_request(method: &str, uri: &str, body: &[u8]) -> Request {
        Request::builder()
            .method(method)
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_vec()))
            .expect("构造请求")
    }

    // ---------------------------------------------------------- 路由表

    #[test]
    fn classify_covers_the_whole_route_table() {
        let table = [
            ("POST", "/v1/chat/completions", Endpoint::ChatCompletions),
            ("POST", "/v1/responses", Endpoint::Responses),
            ("POST", "/v1/messages", Endpoint::Messages),
            ("POST", "/v1/embeddings", Endpoint::Embeddings),
            ("POST", "/v1/images/generations", Endpoint::ImagesGenerations),
            ("POST", "/v1/files", Endpoint::Files),
            ("POST", "/v1/audio/speech", Endpoint::Audio),
            ("POST", "/v1/audio/transcriptions", Endpoint::Audio),
            ("POST", "/v1/audio/some-future-endpoint", Endpoint::Audio),
            ("GET", "/v1/models", Endpoint::ModelsList),
            ("GET", "/v1/models/gpt-4o", Endpoint::ModelGet),
            // 不带 `/v1` 的形态：**必须本地作答**，否则会被透传上游
            ("GET", "/models", Endpoint::ModelsList),
            ("GET", "/models/gpt-4o", Endpoint::ModelGet),
            ("GET", "/healthz", Endpoint::Healthz),
            ("ANY", "/some/unknown/path", Endpoint::Wildcard),
            ("GET", "/v1beta/models/gemini-2.5-pro:generateContent", Endpoint::Wildcard),
            ("DELETE", "/v1/files/file-abc", Endpoint::Wildcard),
        ];
        for (method, path, expected) in table {
            let method = if method == "ANY" {
                Method::PATCH
            } else {
                Method::from_bytes(method.as_bytes()).unwrap()
            };
            assert_eq!(classify(&method, path), expected, "{method} {path}");
        }
        // 方法不匹配 → 通配（真正的 405 由 axum 的路由挡）
        assert_eq!(classify(&Method::GET, "/v1/chat/completions"), Endpoint::Wildcard);
        // `/v1/models/` 没有 id 段：axum 的 `{id}` 不匹配空段 → 落到通配
        assert_eq!(classify(&Method::GET, "/v1/models/"), Endpoint::Wildcard);
    }

    #[test]
    fn model_from_path_extracts_gemini_style_names() {
        assert_eq!(
            model_from_path("/v1beta/models/gemini-2.5-pro:generateContent").unwrap(),
            "gemini-2.5-pro"
        );
        assert_eq!(model_from_path("/v1/models/gpt-4o").unwrap(), "gpt-4o");
        assert!(model_from_path("/v1/unknown/thing").is_none());
        assert!(model_from_path("/models/").is_none());
    }

    /// **转发的**每一条注册路由都真的走到管线（`prepare` 能拿到它、端点分类正确）。
    ///
    /// `/v1/models` 与 `/v1/models/{id}` **不在**这个清单里： 起它们由网关
    /// **本地作答**（见 `models_list_is_answered_locally_and_never_dials_upstream`），
    /// 故意不再进管线 —— 这里把它们排掉不是"漏了"，是防止有人把本地回答改回透传还看不出来。
    #[tokio::test]
    async fn every_forwarded_route_reaches_the_pipeline() {
        let state = state();
        let body = serde_json::to_vec(&json!({"model": "claude-sonnet-4-5"})).unwrap();
        for (method, uri, expected) in [
            ("POST", "/v1/chat/completions", "chat_completions"),
            ("POST", "/v1/responses", "responses"),
            ("POST", "/v1/messages", "messages"),
            ("POST", "/v1/embeddings", "embeddings"),
            ("POST", "/v1/images/generations", "images_generations"),
            ("POST", "/v1/files", "files"),
            ("POST", "/v1/audio/speech", "audio"),
            ("PATCH", "/v1/unknown/future/endpoint", "wildcard"),
            ("POST", "/v1/some/future/endpoint", "wildcard"),
            ("DELETE", "/api/deep/path?x=1", "wildcard"),
        ] {
            let (status, _, value) = send(state.clone(), json_request(method, uri, &body)).await;
            assert_eq!(status, StatusCode::OK, "{method} {uri} 应当命中路由");
            assert_eq!(value["endpoint"], expected, "{method} {uri}");
        }
        // 已注册路径 + 错误方法 → axum 的 405（比悄悄走通配透传更符合 HTTP 语义）
        let (status, _, _) = send(state, json_request("GET", "/v1/messages", &body)).await;
        assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    }

    #[tokio::test]
    async fn healthz_does_not_touch_upstream() {
        let (status, headers, value) = send(state(), json_request("GET", "/healthz", b"")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(value, json!({"status": "ok", "kernel": "ready"}));
        assert!(headers.contains_key("x-tokenagency-request-id"));
    }

    #[tokio::test]
    async fn healthz_reports_unknown_kernel_without_pretending() {
        let state = state_with(test_cfg(), IngressOptions::default(), KernelState::Unknown);
        let (_, _, value) = send(state, json_request("GET", "/healthz", b"")).await;
        assert_eq!(value["kernel"], "unknown");
    }

    // ---------------------------------------------------------- DoD：golden 端到端

    #[tokio::test]
    async fn golden_request_is_faithful_end_to_end_through_the_router() {
        let fixture = golden_fixture();
        let raw_body = serde_json::to_vec(&fixture.body).unwrap();
        // **同协议**：这一条测"逐字节透传"，跨协议会被真的翻译掉。
        // 跨协议的端到端路径由 `models_list_is_answered_locally...` 同族的用例与
        // `pipeline::cross_protocol_request_body_is_translated_not_passed_through` 覆盖。
        let mut builder = Request::builder().method("POST").uri("/v1/chat/completions");
        for (name, value) in fixture.headers.iter() {
            builder = builder.header(name, value);
        }
        let request = builder.body(Body::from(raw_body.clone())).expect("构造请求");

        let (status, _, echoed) = send(state(), request).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(echoed["header_diff_empty"], Value::Bool(true), "自检必须为空");

        // 上游收到的头：除 4 处例外，逐个头与客户端一致
        let upstream_headers = &echoed["headers"];
        for (name, values) in canonical_headers(&fixture.headers) {
            if crate::passthrough::is_hop_by_hop(&name)
                || ["host", "content-length", "authorization"].contains(&name.as_str())
            {
                continue;
            }
            assert_eq!(
                upstream_headers[&name],
                json!(values),
                "{name} 必须逐字段一致（agent 标识就是靠这条不丢）"
            );
        }
        assert_eq!(upstream_headers["host"], json!(["relay.example.com"]));
        assert_eq!(upstream_headers["authorization"], json!([format!("Bearer {SECRET}")]));
        assert_eq!(
            upstream_headers["content-length"],
            json!([golden_expected_body_len(&raw_body).to_string()]),
            "content-length 必须按实际 body 重算"
        );
        for gone in ["connection", "keep-alive", "proxy-connection"] {
            assert!(upstream_headers.get(gone).is_none(), "{gone} 必须剥离");
        }

        // 上游收到的 body：只改 model
        let mut expected_body = fixture.body.clone();
        expected_body["model"] = json!("claude-sonnet-4-5-20250929");
        assert_eq!(echoed["body"], expected_body);
        assert_eq!(echoed["upstream_model"], "claude-sonnet-4-5-20250929");
        assert_eq!(echoed["endpoint"], "chat_completions");
        // **同协议 → RAW 零改动透传**，这正是本测试要钉的东西（逐字节 faithful）。
        //
        // 跨协议会被**真的翻译**，那由
        // `pipeline::cross_protocol_request_body_is_translated_not_passed_through` 覆盖。
        // 原来这里写着"客户端 anthropic / 上游 openai → 必须走 IR 路径"，但断言的是
        // **原样透传** —— 因为那条 IR 路径当时根本没被调用。夹具与注释现在都改齐了。
        assert_eq!(echoed["client_protocol"], "openai");
        assert_eq!(echoed["protocol"], "openai");
        assert_eq!(echoed["has_image"], Value::Bool(true), "带图请求必须被识别（只读 hint）");
        assert_eq!(echoed["upstream_url"], "https://relay.example.com/v1/chat/completions");
        assert_eq!(echoed["channel_id"], "ch-relay-a");
    }

    /// patch 只改 model，body 其余部分原样 → 长度变化只来自 model 字符串
    fn golden_expected_body_len(original: &[u8]) -> usize {
        let mut expected = serde_json::from_slice::<Value>(original).unwrap();
        expected["model"] = json!("claude-sonnet-4-5-20250929");
        serde_json::to_vec(&expected).unwrap().len()
    }

    // ---------------------------------------------------------- 本地密钥

    #[tokio::test]
    async fn local_key_accepts_correct_key_and_rejects_wrong_or_missing() {
        let mut cfg = test_cfg();
        cfg.gateway.local_key = Some("local-secret".into());
        let state = state_with(cfg, IngressOptions::default(), KernelState::Ready);
        let body = b"{\"model\":\"claude-sonnet-4-5\"}";

        // 正确（Authorization: Bearer）
        let request = Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header(header::AUTHORIZATION, "Bearer local-secret")
            .body(Body::from(body.to_vec()))
            .unwrap();
        let (status, _, _) = send(state.clone(), request).await;
        assert_eq!(status, StatusCode::OK);

        // 正确（x-api-key）
        let request = Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("x-api-key", "local-secret")
            .body(Body::from(body.to_vec()))
            .unwrap();
        let (status, _, _) = send(state.clone(), request).await;
        assert_eq!(status, StatusCode::OK);

        // 错误
        let request = Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header(header::AUTHORIZATION, "Bearer wrong")
            .body(Body::from(body.to_vec()))
            .unwrap();
        let (status, _, value) = send(state.clone(), request).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(value["error"]["code"], "TA-1001");
        assert_eq!(value["error"]["type"], "tokenagency_unauthorized");
        assert_eq!(value["error"]["retryable"], Value::Bool(false));

        // 缺失
        let (status, _, value) =
            send(state, json_request("POST", "/v1/chat/completions", body)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(value["error"]["code"], "TA-1001");
    }

    /// D4 守卫：`gateway.local_key` 允许写成引用 —— **默认配置就是**
    /// `"${secret:gateway-local-key}"`（`ta-core/config/defaults.rs`）。
    /// 客户端发来的是**解析后的明文**；如果拿引用字面量当密码比，默认配置下人人 401。
    ///
    /// 这条测试**先于修复存在**，当时的实现直接比字面量，第一个断言是红的。
    #[tokio::test]
    async fn local_key_reference_is_resolved_before_comparison() {
        let mut cfg = test_cfg();
        cfg.gateway.local_key = Some("${secret:gateway-local-key}".into());
        let secrets = Arc::new(crate::pipeline::StaticSecrets::new([(
            "${secret:gateway-local-key}",
            "resolved-local-key",
        )]));
        let state = state_with_secrets(cfg, IngressOptions::default(), KernelState::Ready, secrets);
        let body = b"{\"model\":\"claude-sonnet-4-5\"}";

        // 解析后的明文 → 必须通过
        let request = Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header(header::AUTHORIZATION, "Bearer resolved-local-key")
            .body(Body::from(body.to_vec()))
            .unwrap();
        let (status, _, value) = send(state.clone(), request).await;
        assert_eq!(status, StatusCode::OK, "local_key 是引用时必须先解析再用：{value}");

        // 引用字面量本身**不是**密码 → 必须拒绝
        let request = Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header(header::AUTHORIZATION, "Bearer ${secret:gateway-local-key}")
            .body(Body::from(body.to_vec()))
            .unwrap();
        let (status, _, _) = send(state, request).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "引用字面量不许被当成密码");
    }

    /// 解析不到的引用 → 响亮地失败（TA-9001），**绝不退化成"按字面量比"**。
    ///
    /// 退化的后果正是 D4 想避免的：用户看到"你的 key 错了"，而真实原因是钥匙串里没这条。
    #[tokio::test]
    async fn unresolvable_local_key_reference_fails_loudly() {
        let mut cfg = test_cfg();
        cfg.gateway.local_key = Some("${secret:gateway-local-key}".into());
        // 空解析器：这个引用必然解析不到
        let state = state_with(cfg, IngressOptions::default(), KernelState::Ready);
        let body = b"{\"model\":\"claude-sonnet-4-5\"}";

        // 拿引用字面量当密码 —— 旧实现会放行，这里必须不是 200
        let request = Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header(header::AUTHORIZATION, "Bearer ${secret:gateway-local-key}")
            .body(Body::from(body.to_vec()))
            .unwrap();
        let (status, _, value) = send(state, request).await;
        assert_ne!(status, StatusCode::OK, "解析不到就不能放行：{value}");
        assert_eq!(value["error"]["code"], "TA-9001", "解析失败是 TA-9001（不是 401）：{value}");
        assert_ne!(value["error"]["type"], "tokenagency_unauthorized", "别把配置问题报成密钥错");
    }

    /// 写坏的引用（`"${secret:}"`）是**配置错误**，既不放行、也不当明文密码。
    #[tokio::test]
    async fn malformed_local_key_reference_is_a_config_error() {
        let mut cfg = test_cfg();
        cfg.gateway.local_key = Some("${secret:}".into());
        let state = state_with(cfg, IngressOptions::default(), KernelState::Ready);
        let body = b"{\"model\":\"claude-sonnet-4-5\"}";

        let request = Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header(header::AUTHORIZATION, "Bearer ${secret:}")
            .body(Body::from(body.to_vec()))
            .unwrap();
        let (status, _, value) = send(state, request).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{value}");
        assert_eq!(value["error"]["code"], "TA-5001", "{value}");
    }

    /// 引用解析成**空串**不是"不校验"：否则钥匙串里存个空值就成了放行洞。
    #[tokio::test]
    async fn local_key_resolving_to_empty_is_not_a_free_pass() {
        let mut cfg = test_cfg();
        cfg.gateway.local_key = Some("${secret:gateway-local-key}".into());
        let secrets =
            Arc::new(crate::pipeline::StaticSecrets::new([("${secret:gateway-local-key}", "   ")]));
        let state = state_with_secrets(cfg, IngressOptions::default(), KernelState::Ready, secrets);
        let body = b"{\"model\":\"claude-sonnet-4-5\"}";

        // 不带任何密钥 → 不许通过（"解析成空"不等于"未配置"）
        let (status, _, value) =
            send(state.clone(), json_request("POST", "/v1/chat/completions", body)).await;
        assert_ne!(status, StatusCode::OK, "空 key 不能放行：{value}");
        assert_eq!(value["error"]["code"], "TA-5001", "{value}");

        // 直接发空 Bearer → 也不许通过
        let request = Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header(header::AUTHORIZATION, "Bearer ")
            .body(Body::from(body.to_vec()))
            .unwrap();
        let (status, _, value) = send(state, request).await;
        assert_ne!(status, StatusCode::OK, "{value}");
        assert_eq!(value["error"]["code"], "TA-5001", "{value}");
    }

    #[tokio::test]
    async fn empty_local_key_means_no_auth() {
        let mut cfg = test_cfg();
        cfg.gateway.local_key = Some("   ".into());
        let state = state_with(cfg, IngressOptions::default(), KernelState::Ready);
        let (status, _, _) =
            send(state, json_request("POST", "/v1/chat/completions", b"{\"model\":\"m\"}")).await;
        assert_eq!(status, StatusCode::OK, "空白 local_key 等价于未配置");
    }

    #[test]
    fn ct_eq_compares_content_not_just_length() {
        assert!(ct_eq(b"secret", b"secret"));
        assert!(!ct_eq(b"secret", b"secreT"));
        assert!(!ct_eq(b"secret", b"secret-longer"));
        assert!(ct_eq(b"", b""));
    }

    // ---------------------------------------------------------- API key 鉴权接缝

    /// 测试用验票器：**想让它怎么答就怎么答**（命中表 / 库故障 / 有没有启用中的 key）。
    struct FakeApiKeys {
        /// `any_enabled()` 的答案
        enabled: bool,
        /// (token, key_id) 命中表
        hits: Vec<(&'static str, &'static str)>,
        /// 模拟"库查不动"
        broken: bool,
        /// 记录 `any_enabled()` 被问了几次（证明本地 key 那条腿不该打库）
        asked: Mutex<u32>,
    }

    impl FakeApiKeys {
        fn new(enabled: bool, hits: Vec<(&'static str, &'static str)>) -> Arc<Self> {
            Arc::new(Self { enabled, hits, broken: false, asked: Mutex::new(0) })
        }

        fn broken() -> Arc<Self> {
            Arc::new(Self { enabled: false, hits: Vec::new(), broken: true, asked: Mutex::new(0) })
        }

        fn any_enabled_calls(&self) -> u32 {
            *self.asked.lock().expect("计数锁")
        }
    }

    #[async_trait]
    impl ApiKeyVerifier for FakeApiKeys {
        async fn any_enabled(&self) -> Result<bool> {
            *self.asked.lock().expect("计数锁") += 1;
            if self.broken {
                return Err(TaError::Store("库挂了".into()));
            }
            Ok(self.enabled)
        }

        async fn verify(&self, token: &str) -> Result<Option<String>> {
            if self.broken {
                return Err(TaError::Store("库挂了".into()));
            }
            Ok(self.hits.iter().find(|(t, _)| *t == token).map(|(_, id)| id.to_string()))
        }
    }

    fn state_with_keys(cfg: ConfigDoc, keys: Arc<dyn ApiKeyVerifier>) -> Arc<IngressState> {
        Arc::new(
            ingress_state_with_secrets(
                cfg,
                IngressOptions::default(),
                KernelState::Ready,
                Arc::new(crate::pipeline::StaticSecrets::default()),
            )
            .with_api_keys(keys),
        )
    }

    fn bearer(token: &str) -> Request {
        Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .body(Body::from(b"{\"model\":\"m\"}".to_vec()))
            .unwrap()
    }

    fn no_auth() -> Request {
        json_request("POST", "/v1/chat/completions", b"{\"model\":\"m\"}")
    }

    /// 回归：**一把 key 都没有、local_key 也没配** → 保持既有行为（不鉴权）。
    /// 这条要是红了，每个既有用户升级后都会突然全部 401。
    #[tokio::test]
    async fn no_local_key_and_no_enabled_api_keys_still_means_no_auth() {
        let mut cfg = test_cfg();
        cfg.gateway.local_key = None;
        let keys = FakeApiKeys::new(false, vec![]);
        let state = state_with_keys(cfg.clone(), keys.clone());

        let (status, _, value) = send(state, no_auth()).await;
        assert_eq!(status, StatusCode::OK, "{value}");
        assert_eq!(keys.any_enabled_calls(), 1, "这个判据必须真的问过库，不能靠默认值");

        // 身份也得说清是"本地/未鉴权"
        let identity = check_local_key(
            &cfg,
            &HeaderMap::new(),
            Arc::new(crate::pipeline::StaticSecrets::default()).as_ref(),
            keys.as_ref(),
        )
        .await
        .expect("没有 key 就没有必须命中的凭据");
        assert_eq!(identity, KeyIdentity::Local);
        assert_eq!(identity.key_id(), "local", "账本里的 key_id 就是 \"local\"");
    }

    /// 库里一旦有启用中的 key，**凭据就是必须的** —— 哪怕 local_key 没配。
    #[tokio::test]
    async fn an_enabled_api_key_makes_a_credential_mandatory() {
        let mut cfg = test_cfg();
        cfg.gateway.local_key = None;
        let keys = FakeApiKeys::new(true, vec![("sk-good", "key-1")]);
        let state = state_with_keys(cfg.clone(), keys.clone());

        let (status, _, value) = send(state.clone(), no_auth()).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{value}");
        assert_eq!(
            value["error"]["code"], "TA-1001",
            "鉴权失败是 TA-1001；TA-1002 是 429 限流：{value}"
        );
        assert_eq!(
            value["error"]["retryable"],
            Value::Bool(false),
            "重试一把没被接受的 key 毫无意义"
        );

        // 没命中的 token 同样 401（不泄漏"哪把 key 不存在"）
        let (status, _, value) = send(state, bearer("sk-nope")).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{value}");
        assert_eq!(value["error"]["code"], "TA-1001");
    }

    /// 命中启用中的 api key → 通过，且身份带着**该 key 的 id**（`api_key_usage` 归属）。
    #[tokio::test]
    async fn a_hit_api_key_passes_and_the_identity_carries_its_key_id() {
        let mut cfg = test_cfg();
        cfg.gateway.local_key = None;
        let keys = FakeApiKeys::new(true, vec![("sk-good", "key-1")]);
        let state = state_with_keys(cfg.clone(), keys.clone());

        let (status, _, value) = send(state.clone(), bearer("sk-good")).await;
        assert_eq!(status, StatusCode::OK, "{value}");

        // `x-api-key` 那条腿也一样（同一个 token 的另一种送达方式）
        let request = Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("x-api-key", "sk-good")
            .body(Body::from(b"{\"model\":\"m\"}".to_vec()))
            .unwrap();
        let (status, _, value) = send(state, request).await;
        assert_eq!(status, StatusCode::OK, "{value}");

        let identity = check_local_key(
            &cfg,
            &bearer("sk-good").into_parts().0.headers,
            Arc::new(crate::pipeline::StaticSecrets::default()).as_ref(),
            keys.as_ref(),
        )
        .await
        .expect("命中了就该通过");
        assert_eq!(identity, KeyIdentity::ApiKey("key-1".into()));
        assert_eq!(identity.key_id(), "key-1");
    }

    /// 本地 key 命中 → `Local`；本地 key 配了但没命中时，**仍然可以**用一把 api key 进来
    /// （两条腿是"或"的关系，不是"配了 local_key 就只能用它"）。
    #[tokio::test]
    async fn the_local_key_wins_and_api_keys_still_work_alongside_it() {
        let mut cfg = test_cfg();
        cfg.gateway.local_key = Some("local-secret".into());
        let keys = FakeApiKeys::new(true, vec![("sk-good", "key-1")]);
        let state = state_with_keys(cfg.clone(), keys.clone());

        let (status, _, value) = send(state.clone(), bearer("local-secret")).await;
        assert_eq!(status, StatusCode::OK, "{value}");
        let (status, _, value) = send(state, bearer("sk-good")).await;
        assert_eq!(status, StatusCode::OK, "{value}");

        let identity = check_local_key(
            &cfg,
            &bearer("local-secret").into_parts().0.headers,
            Arc::new(crate::pipeline::StaticSecrets::default()).as_ref(),
            keys.as_ref(),
        )
        .await
        .expect("本地密钥命中");
        assert_eq!(identity, KeyIdentity::Local);

        let identity = check_local_key(
            &cfg,
            &bearer("sk-good").into_parts().0.headers,
            Arc::new(crate::pipeline::StaticSecrets::default()).as_ref(),
            keys.as_ref(),
        )
        .await
        .expect("api key 是第二条腿");
        assert_eq!(identity, KeyIdentity::ApiKey("key-1".into()));
    }

    /// 本地密钥已经能定身份时**不该去打库**：库故障不该连带把本地 key 一起打死，
    /// 也不该给每个请求多一次查询。
    #[tokio::test]
    async fn the_local_key_path_does_not_touch_the_key_store() {
        let mut cfg = test_cfg();
        cfg.gateway.local_key = Some("local-secret".into());
        let keys = FakeApiKeys::broken();
        let state = state_with_keys(cfg, keys.clone());

        let (status, _, value) = send(state, bearer("local-secret")).await;
        assert_eq!(status, StatusCode::OK, "本地 key 不依赖 key 库：{value}");
        assert_eq!(keys.any_enabled_calls(), 0, "本地 key 那条腿不该问库");
    }

    /// **fail closed**：库查不动 → 请求失败，绝不退回"放行"。
    #[tokio::test]
    async fn a_broken_key_store_fails_closed() {
        let mut cfg = test_cfg();
        cfg.gateway.local_key = None;
        let keys = FakeApiKeys::broken();
        let state = state_with_keys(cfg.clone(), keys.clone());

        let (status, _, value) = send(state, bearer("sk-any")).await;
        assert_ne!(status, StatusCode::OK, "库挂了还放行 = 谁都能进：{value}");
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{value}");
        assert_ne!(value["error"]["code"], "TA-1001", "这不是'你没带凭据'，是'我们查不动'");

        let err = check_local_key(
            &cfg,
            &bearer("sk-any").into_parts().0.headers,
            Arc::new(crate::pipeline::StaticSecrets::default()).as_ref(),
            keys.as_ref(),
        )
        .await
        .expect_err("查不动必须报错");
        assert_eq!(err.code(), "TA-9001");
    }

    // ---------------------------------------------------------- body 上限

    #[tokio::test]
    async fn oversized_content_length_is_rejected_with_413() {
        let mut cfg = test_cfg();
        cfg.gateway.max_body_mb = 1;
        let state = state_with(cfg, IngressOptions::default(), KernelState::Ready);
        let request = Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header(header::CONTENT_LENGTH, (2 * 1024 * 1024).to_string())
            .body(Body::from(vec![b'x'; 2 * 1024 * 1024]))
            .unwrap();
        let (status, _, value) = send(state, request).await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(value["error"]["code"], "TA-1003");
        assert_eq!(value["error"]["details"]["limit"], 1024 * 1024);
    }

    #[tokio::test]
    async fn oversized_chunked_body_is_rejected_with_413() {
        let mut cfg = test_cfg();
        cfg.gateway.max_body_mb = 1;
        let state = state_with(cfg, IngressOptions::default(), KernelState::Ready);
        // 没有 content-length（分块）→ 必须靠累加检测
        let request = Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .body(Body::from(vec![b'x'; 1024 * 1024 + 16]))
            .unwrap();
        let (status, _, value) = send(state, request).await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(value["error"]["code"], "TA-1003");
    }

    #[tokio::test]
    async fn body_within_limit_passes() {
        let mut cfg = test_cfg();
        cfg.gateway.max_body_mb = 1;
        let state = state_with(cfg, IngressOptions::default(), KernelState::Ready);
        let (status, _, _) =
            send(state, json_request("POST", "/v1/chat/completions", b"{\"model\":\"m\"}")).await;
        assert_eq!(status, StatusCode::OK);
    }

    // ---------------------------------------------------------- RPM

    #[test]
    fn rpm_limiter_window_and_recovery() {
        let limiter = RpmLimiter::new();
        assert!(limiter.check(0, 2, 1000).is_ok());
        assert!(limiter.check(10, 2, 1000).is_ok());
        let retry_after = limiter.check(20, 2, 1000).unwrap_err();
        assert_eq!(retry_after, 1, "剩余 980ms 应当上取整成 1s");
        // 窗口滚动后复活
        assert!(limiter.check(1000, 2, 1000).is_ok());
        // limit = 0 / 未配置 → 不限流
        for _ in 0..100 {
            assert!(limiter.check(2000, 0, 1000).is_ok());
        }
    }

    #[tokio::test]
    async fn rpm_limit_returns_429_with_retry_after() {
        let options = IngressOptions { rpm_limit: Some(1), rpm_window_ms: 60_000 };
        let state = state_with(test_cfg(), options, KernelState::Ready);
        let body = b"{\"model\":\"claude-sonnet-4-5\"}";
        let (first, _, _) =
            send(state.clone(), json_request("POST", "/v1/chat/completions", body)).await;
        assert_eq!(first, StatusCode::OK);
        let (second, headers, value) =
            send(state, json_request("POST", "/v1/chat/completions", body)).await;
        assert_eq!(second, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(value["error"]["code"], "TA-1002");
        assert_eq!(value["error"]["type"], "tokenagency_rate_limited");
        assert_eq!(value["error"]["retryable"], Value::Bool(true));
        assert_eq!(headers.get(header::RETRY_AFTER).unwrap(), "60");
    }

    // ---------------------------------------------------------- 监听地址

    #[test]
    fn bind_address_must_be_loopback() {
        assert_eq!(parse_loopback_addr("127.0.0.1:8787").unwrap().port(), 8787);
        assert!(parse_loopback_addr("[::1]:8787").unwrap().ip().is_loopback());
        assert!(parse_loopback_addr("0.0.0.0:8787").is_err(), "永不监听 0.0.0.0");
        assert!(parse_loopback_addr("192.168.1.10:8787").is_err());
        assert!(parse_loopback_addr("not-an-addr").is_err());
    }

    #[test]
    fn localhost_is_normalized_to_loopback_not_trusted() {
        // 人就是会写 localhost：归一化成 127.0.0.1，而不是报"地址不合法"
        assert_eq!(parse_loopback_addr("localhost:8787").unwrap().to_string(), "127.0.0.1:8787");
        assert_eq!(parse_loopback_addr("LOCALHOST:8787").unwrap().to_string(), "127.0.0.1:8787");
        assert_eq!(parse_loopback_addr("  localhost:0  ").unwrap().port(), 0);
        // 归一化 ≠ 放行：其它主机名一律拒绝
        assert!(parse_loopback_addr("example.com:8787").is_err());
        assert!(parse_loopback_addr("localhost").is_err(), "缺端口必须失败");
        assert!(parse_loopback_addr("localhostevil:80").is_err(), "前缀匹配不能变成放行漏洞");
    }

    #[tokio::test]
    async fn bind_loopback_actually_binds_only_loopback() {
        let listener = bind_loopback("127.0.0.1:0").await.expect("应当能绑定");
        assert!(listener.local_addr().unwrap().ip().is_loopback());
        assert!(bind_loopback("0.0.0.0:0").await.is_err());
    }

    // ---------------------------------------------------------- 错误信封

    #[test]
    fn error_envelope_matches_the_contract() {
        let response = error_response(&TaError::Unauthorized, "req_abc");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(response.headers().get("x-tokenagency-request-id").unwrap(), "req_abc");
        assert_eq!(response.headers().get(header::CONTENT_TYPE).unwrap(), "application/json");
    }

    // ---------------------------------------------------------- hints

    #[test]
    fn hints_are_read_only_and_never_reject() {
        let body = serde_json::to_vec(&json!({
            "model": "gpt-4o",
            "stream": true,
            "messages": [{"content": [{"type": "image_url", "image_url": {"url": "x"}}]}],
        }))
        .unwrap();
        let hints = body_hints(&body);
        assert_eq!(hints.model.as_deref(), Some("gpt-4o"));
        assert!(hints.stream);
        assert!(hints.has_image);

        // 非 JSON / 空 body：读不到就是读不到，不是错误
        assert_eq!(body_hints(b"not json"), BodyHints::default());
        assert_eq!(body_hints(b""), BodyHints::default());
        assert!(!body_hints(b"{\"messages\":[{\"content\":\"just text\"}]}").has_image);
    }

    #[test]
    fn model_hint_precedence() {
        let snapshot = |headers: HeaderMap, body: &[u8], path: &str| RequestSnapshot {
            request_id: "req_x".into(),
            method: Method::POST,
            path: path.into(),
            query: None,
            endpoint: Endpoint::Wildcard,
            headers,
            body: Bytes::from(body.to_vec()),
            received_at_ms: 0,
        };
        // 1) 专用头优先
        let mut headers = HeaderMap::new();
        headers.insert("x-tokenagency-model", "from-header".parse().unwrap());
        assert_eq!(
            snapshot(headers.clone(), b"{\"model\":\"from-body\"}", "/v1/unknown")
                .model_hint()
                .as_deref(),
            Some("from-header")
        );
        // 2) body 的 model
        assert_eq!(
            snapshot(HeaderMap::new(), b"{\"model\":\"from-body\"}", "/v1/unknown")
                .model_hint()
                .as_deref(),
            Some("from-body")
        );
        // 3) 路径里的模型名（通配）
        assert_eq!(
            snapshot(HeaderMap::new(), b"", "/v1beta/models/gemini-2.5-pro:generateContent")
                .model_hint()
                .as_deref(),
            Some("gemini-2.5-pro")
        );
        // 4) 都没有 → None（由 G2 决定 NoRoute / fallback_provider）
        assert!(snapshot(HeaderMap::new(), b"", "/v1/unknown").model_hint().is_none());
    }

    /// 客户端发**别名**时，`PreparedRequest.model` 必须归一到规范模型 id。
    ///
    /// 为什么必须归一、且必须只在一处做：下游三处都要用**同一个**名字 ——
    /// ① 定价（`PriceMatch.model` 匹配对外模型名）、② 用量表的 `model` 列、
    /// ③ 成本中心按模型的分组。不归一的话，同一个模型会因为客户端发了别名而
    /// **算不出价**（显示"未定价"），并在成本中心被**拆成两行**、各算一半钱。
    #[tokio::test]
    async fn alias_is_canonicalized_to_the_model_id() {
        let fixture = golden_fixture();
        let mut doc = test_cfg();
        // golden 通道服务 `claude-sonnet-4-5`；建一个带别名的模型并把它指到那条线路，
        // 客户端发的是**别名**。
        doc.models.push(
            ta_core::domain::Model::new("m-canon")
                .with_aliases(["claude-sonnet-4-5-alias"])
                .with_lines([fixture.channel.id.clone()]),
        );
        let snapshot = RequestSnapshot {
            request_id: "req_alias".into(),
            method: Method::POST,
            path: "/v1/messages".into(),
            query: None,
            endpoint: Endpoint::Messages,
            headers: HeaderMap::new(),
            body: Bytes::from(
                serde_json::to_vec(&json!({"model": "claude-sonnet-4-5-alias", "messages": []}))
                    .unwrap(),
            ),
            received_at_ms: 0,
        };
        let prepared = build_prepared_request(PrepareInput {
            cfg: &doc,
            policy: &FullPassthrough,
            snapshot: &snapshot,
            channel: &fixture.channel,
            provider: Some(&fixture.provider),
            secret: SECRET,
            lane_id: None,
        })
        .expect("应当能构建");
        assert_eq!(
            prepared.model.as_deref(),
            Some("m-canon"),
            "客户端发的是别名 → 下游三处（定价/用量/成本分组）必须都看到规范 id"
        );
        // 归一的是**我们记下来的那个名字**，发往上游的 body 不受影响（上游认自己的名字）
        assert_eq!(prepared.upstream_model, fixture.channel.upstream_model);
    }

    /// 客户端发了一个**配置里没有的**模型名 → 如实保留它，绝不编一个规范 id 出来。
    #[tokio::test]
    async fn unknown_model_name_is_kept_verbatim() {
        let doc = test_cfg();
        let fixture = golden_fixture();
        let snapshot = RequestSnapshot {
            request_id: "req_unknown".into(),
            method: Method::POST,
            path: "/v1/messages".into(),
            query: None,
            endpoint: Endpoint::Messages,
            headers: HeaderMap::new(),
            body: Bytes::from(
                serde_json::to_vec(&json!({"model": "nope-not-configured"})).unwrap(),
            ),
            received_at_ms: 0,
        };
        let prepared = build_prepared_request(PrepareInput {
            cfg: &doc,
            policy: &FullPassthrough,
            snapshot: &snapshot,
            channel: &fixture.channel,
            provider: Some(&fixture.provider),
            secret: SECRET,
            lane_id: None,
        })
        .expect("应当能构建");
        assert_eq!(
            prepared.model.as_deref(),
            Some("nope-not-configured"),
            "查不到就照原样记下客户端发的名字（网关要如实记下它收到了什么）"
        );
    }

    #[tokio::test]
    async fn wildcard_without_model_still_reaches_the_pipeline() {
        // G1 不决定 no_route（那是 G2 的策略），但必须把请求原样送到管线
        let (status, _, value) =
            send(state(), json_request("POST", "/v1/totally-unknown", b"{\"foo\":1}")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(value["endpoint"], "wildcard");
        assert_eq!(value["body"], json!({"foo": 1}));
    }

    // ---------------------------------------------------------- 调试头开关

    /// `gateway.debug_headers: false` 时，**全部** `x-tokenagency-*` 都不出现；
    /// 但**错误信封 body 里的 `request_id` 保留**（那是内容，不是头）。
    #[tokio::test]
    async fn debug_headers_can_be_switched_off_while_the_envelope_keeps_request_id() {
        let mut cfg = test_cfg();
        cfg.gateway.debug_headers = false;
        cfg.gateway.local_key = Some("local-secret".into());
        let state = state_with(cfg, IngressOptions::default(), KernelState::Ready);

        // 成功路径：没有任何 x-tokenagency-*
        let request = Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header(header::AUTHORIZATION, "Bearer local-secret")
            .body(Body::from(b"{\"model\":\"claude-sonnet-4-5\"}".to_vec()))
            .unwrap();
        let (status, headers, _) = send(state.clone(), request).await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            !headers.keys().any(|name| name.as_str().starts_with("x-tokenagency-")),
            "关掉调试头后成功响应里也不该有：{headers:?}"
        );

        // 错误路径：同样没有头，但信封 body 里 request_id 仍在
        let (status, headers, value) =
            send(state, json_request("POST", "/v1/chat/completions", b"{\"model\":\"m\"}")).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(!headers.keys().any(|name| name.as_str().starts_with("x-tokenagency-")));
        assert_eq!(value["error"]["code"], "TA-1001");
        assert!(
            value["error"]["request_id"].as_str().is_some_and(|id| !id.is_empty()),
            "信封内部的 request_id 不受调试头开关影响：{value}"
        );
    }

    // ---------------------------------------------------------- 暂停闸门

    /// 暂停期间：**上游收到的新请求数必须是 0**，且拒绝理由要让人看出"是我自己按的暂停"。
    #[tokio::test]
    async fn paused_gateway_rejects_new_requests_without_calling_upstream() {
        use crate::runtime::{build_runtime, tests::*};

        let dialer = Arc::new(RecordingDialer::default());
        let deps = test_deps(cfg_with_channels(0), dialer.clone());
        let runtime = build_runtime(&deps, test_parts());

        // 暂停前先确认它本来是通的（否则下面"0 个请求"可能只是链路本来就坏）
        let body = br#"{"model":"claude-sonnet-4-5"}"#;
        let (status, _, _) =
            send(runtime.state.clone(), json_request("POST", "/v1/messages", body)).await;
        assert_eq!(status, StatusCode::OK);
        let calls_before = dialer.urls.lock().map(|g| g.len()).unwrap_or(0);
        assert_eq!(calls_before, 1);

        runtime.gate.pause();
        assert_eq!(runtime.gate.state(), GatewayRunState::Paused);

        let (status, _, value) =
            send(runtime.state.clone(), json_request("POST", "/v1/messages", body)).await;
        // **先断上游请求数**：这是本功能的产品要害（"暂停"若拦不住请求，其余都是装饰）。
        // 放在状态码之前，红的时候才能证明**这条**断言本身有牙。
        let calls_after = dialer.urls.lock().map(|g| g.len()).unwrap_or(0);
        assert_eq!(calls_after, calls_before, "暂停期间上游必须收到 0 个新请求");
        // 再断拒绝的形状（三条正好钉住"客户端会不会重试"这件担忧）：
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{value}");
        assert_eq!(value["error"]["code"], "TA-1005");
        assert_eq!(value["error"]["type"], "tokenagency_gateway_paused");
        assert_eq!(value["error"]["retryable"], false, "暂停不是可重试的故障：{value}");
        let message = value["error"]["message"].as_str().unwrap_or_default();
        assert!(message.contains("暂停"), "拒绝理由必须让人看出是用户按的暂停：{value}");
        println!(
            "=== 暂停闸门证据 ===\npaused_state={:?}\n暂停前上游请求数={calls_before}  暂停后={calls_after}\n响应={value}",
            runtime.gate.state()
        );
    }

    /// 恢复后请求重新流通；`/healthz` 在暂停期间**仍然可用**（托盘要显示"已暂停"，不是"失联"）。
    #[tokio::test]
    async fn resume_lets_requests_flow_again_and_healthz_survives_the_pause() {
        use crate::runtime::{build_runtime, tests::*};

        let dialer = Arc::new(RecordingDialer::default());
        let deps = test_deps(cfg_with_channels(0), dialer.clone());
        let runtime = build_runtime(&deps, test_parts());
        runtime.gate.pause();

        // healthz 不受暂停影响
        let (status, _, value) =
            send(runtime.state.clone(), json_request("GET", "/healthz", b"")).await;
        assert_eq!(status, StatusCode::OK, "{value}");
        assert_eq!(value["status"], "ok");

        // 恢复 → 请求重新流通
        runtime.gate.resume();
        assert_eq!(runtime.gate.state(), GatewayRunState::Running);
        let body = br#"{"model":"claude-sonnet-4-5"}"#;
        let (status, _, _) =
            send(runtime.state.clone(), json_request("POST", "/v1/messages", body)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(dialer.urls.lock().map(|g| g.len()).unwrap_or(0), 1);
    }

    /// **在途请求不被暂停取消**（与 G2③ 的取消语义正交）。
    ///
    /// 用一个"卡住直到被放行"的拨号器：请求已经进入管线之后才 `pause()`，
    /// 放行后它必须仍然正常返回 200。
    #[tokio::test]
    async fn an_in_flight_request_is_not_cancelled_by_pausing() {
        use crate::dispatch::{HealthReport, OutboundDialer, UpstreamRequest, UpstreamResponse};
        use crate::runtime::{build_runtime, tests::*};
        use ta_core::types::LaneId;

        struct GatedDialer {
            entered: Arc<tokio::sync::Notify>,
            release: Arc<tokio::sync::Notify>,
        }

        #[async_trait]
        impl OutboundDialer for GatedDialer {
            async fn execute(
                &self,
                _lane: &LaneId,
                _req: UpstreamRequest,
            ) -> ta_core::error::Result<UpstreamResponse> {
                self.entered.notify_one();
                self.release.notified().await;
                Ok(UpstreamResponse {
                    status: StatusCode::OK,
                    headers: HeaderMap::new(),
                    body: crate::dispatch::byte_stream_from_bytes(Bytes::from_static(b"{}")),
                })
            }
            async fn health(&self, lane: &LaneId) -> HealthReport {
                HealthReport { lane_id: lane.clone(), ok: true, latency_ms: None, error: None }
            }
        }

        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let deps = test_deps(
            cfg_with_channels(0),
            Arc::new(GatedDialer { entered: entered.clone(), release: release.clone() }),
        );
        let runtime = build_runtime(&deps, test_parts());

        let request = json_request(
            "POST",
            "/v1/messages",
            br#"{"model":"claude-sonnet-4-5","stream":false}"#,
        );
        let app = router(runtime.state.clone());
        let handle = tokio::spawn(async move {
            tower::ServiceExt::oneshot(app, request).await.expect("路由必须能处理")
        });

        // 等它**真的进了管线**（拨号器已收到请求），再暂停
        tokio::time::timeout(std::time::Duration::from_secs(2), entered.notified())
            .await
            .expect("请求应当到达拨号器");
        runtime.gate.pause();
        assert!(runtime.gate.is_paused());
        release.notify_one();

        let response = handle.await.expect("join");
        assert_eq!(response.status(), StatusCode::OK, "在途请求不该因为随后的暂停被取消");
    }

    #[test]
    fn new_request_id_is_unique_and_prefixed() {
        let a = new_request_id();
        let b = new_request_id();
        assert!(a.starts_with("req_"));
        assert_ne!(a, b);
    }

    // ---------------------------------------------------------- request_id 的 span

    ///：**拿一个 `request_id` 能筛出它经过的全部日志行**。
    ///
    /// 这条证明的不是"我们写了 `request_id` 字段"，而是**span 真的把它带出来了**：
    /// 断言的那行日志来自**拨号器**（另一个模块），而它自己**一个字段都没写**。
    /// 靠"每个日志点手写 `request_id = %rid`"是守不住的 —— 新加一行忘了写就静默失效。
    ///
    /// 用 `current_thread` 运行时 + 线程局部 subscriber：请求在测试自己的线程上跑，
    /// 捕获到的就只有这次请求的输出，不会与并行测试互相污染。
    #[tokio::test(flavor = "current_thread")]
    async fn request_id_rides_the_span_into_other_modules_log_lines() {
        use std::io::Write;
        use std::sync::Mutex as StdMutex;

        use crate::dispatch::{HealthReport, OutboundDialer, UpstreamRequest, UpstreamResponse};
        use crate::runtime::{build_runtime, tests::*};
        use ta_core::types::LaneId;

        #[derive(Clone, Default)]
        struct Buf(Arc<StdMutex<Vec<u8>>>);
        impl Write for Buf {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                match self.0.lock() {
                    Ok(mut guard) => guard.extend_from_slice(bytes),
                    Err(poisoned) => poisoned.into_inner().extend_from_slice(bytes),
                }
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        struct ChattyDialer;

        #[async_trait]
        impl OutboundDialer for ChattyDialer {
            async fn execute(
                &self,
                _lane: &LaneId,
                _req: UpstreamRequest,
            ) -> ta_core::error::Result<UpstreamResponse> {
                // 这一行**故意不写任何字段**：它必须靠 span 才带得上 request_id
                tracing::info!("拨号器被调用");
                Ok(UpstreamResponse {
                    status: StatusCode::OK,
                    headers: HeaderMap::new(),
                    body: crate::dispatch::byte_stream_from_bytes(Bytes::from_static(b"{}")),
                })
            }
            async fn health(&self, lane: &LaneId) -> HealthReport {
                HealthReport { lane_id: lane.clone(), ok: true, latency_ms: None, error: None }
            }
        }

        let buf = Buf::default();
        let writer = buf.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_max_level(tracing::Level::INFO)
            .with_writer(move || writer.clone())
            .finish();
        // **必须用全局 subscriber + 重建兴趣缓存**，否则这条测试会随测试顺序时绿时红：
        // `tracing` 把"这个 span/event 值不值得记录"缓存在**进程全局**的 callsite 上，
        // 缓存是**第一次调用**该 callsite 时按当时的 dispatcher 决定的。同进程里别的测试
        // （例如 `every_registered_route_reaches_the_pipeline`）也会走 `handle_request`，
        // 那时没有 subscriber → 缓存成"从不记录"；之后装**线程局部** subscriber 不会让
        // 那份缓存失效，而 `rebuild_interest_cache()` 是按**全局** dispatcher 重建的 ——
        // 两者叠加才是"装了就能看见"。生产路径没有这个问题：subscriber 在 `main` 里、
        // **任何请求之前**就装好了。
        // 这里第一次跑可能已被别的测试装过（返回 Err），忽略即可：我们的目的是"有 subscriber"。
        let _ = tracing::subscriber::set_global_default(subscriber);
        tracing::callsite::rebuild_interest_cache();

        let deps = test_deps(cfg_with_channels(0), Arc::new(ChattyDialer));
        let runtime = build_runtime(&deps, test_parts());
        let body = br#"{"model":"claude-sonnet-4-5","messages":[{"role":"user","content":"hi"}]}"#;
        let request = Request::builder()
            .method("POST")
            .uri("/v1/messages")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_vec()))
            .expect("构造请求");
        let response = tower::ServiceExt::oneshot(router(runtime.state.clone()), request)
            .await
            .expect("路由必须能处理请求");
        assert_eq!(response.status(), StatusCode::OK);

        let logged = match buf.0.lock() {
            Ok(guard) => String::from_utf8_lossy(&guard).into_owned(),
            Err(poisoned) => String::from_utf8_lossy(&poisoned.into_inner()).into_owned(),
        };
        let line = logged
            .lines()
            .find(|line| line.contains("拨号器被调用"))
            .unwrap_or_else(|| panic!("拨号器的日志没被捕获到：\n{logged}"));
        assert!(line.contains("request_id=req_"), "span 没有把 request_id 带进别的模块：{line}");
        // 这条是拿来贴进 commit 的真实日志行
        println!("=== 真实日志行（request_id 来自 span）===\n{line}");
    }
}
