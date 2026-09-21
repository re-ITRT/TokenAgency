//! 入站：axum 路由、本地密钥、RPM 限流。**工作包 G1 负责实现**。
//!
//! 路由表见 `docs/10-interface-spec.md` §1.2。**只绑 127.0.0.1**。
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
//! `docs/09 §5.1` 把"记录原始头/体快照"放在第 1 步、鉴权放在第 2 步。实现上是
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
use ta_core::domain::{ChannelConfig, ConfigDoc, Provider};
use ta_core::error::{Result, TaError};
use ta_core::types::{ChannelId, LaneId, Protocol};

use crate::passthrough::{self, HeaderDiff, PassthroughPolicy};
use crate::GatewayDeps;

// ================================================================ 端点

/// 路由表（`docs/10 §1.2`）里的端点类别。
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
        ("GET", rest) if rest.starts_with("/v1/models/") && rest.len() > "/v1/models/".len() => {
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
    /// 策略级的全局线路覆盖（`PolicyAction::use_lane`）。G1 恒填 `None`；
    /// G2 从 `RouteDecision` 抄过来，之后每个候选的 lane 解析都带上它。
    pub lane_override: Option<LaneId>,
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

/// **G1 的核心纯函数**：把"客户端原始请求 + 选定通道"变成"发往上游的请求"。
///
/// 只做契约允许的事：`model` 定点 patch、鉴权头替换、`host`/`content-length` 重算、
/// hop-by-hop 剥离、以及用户显式配置的 `ChannelConfig::transform`。
pub fn build_prepared_request(input: PrepareInput<'_>) -> Result<PreparedRequest> {
    let channel = input.channel;
    let snapshot = input.snapshot;

    let Some(provider) = input.provider else {
        return Err(TaError::Internal(format!(
            "通道 {} 引用的服务商 {} 不存在（配置有洞）",
            channel.id, channel.provider
        )));
    };

    // 路径：`transform.path` 覆盖客户端路径（少数上游路径不一致的场景）
    let path = channel
        .transform
        .path
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .unwrap_or(snapshot.path.as_str());
    let mut upstream_url =
        passthrough::join_upstream_url(&provider.base_url, path, snapshot.query.as_deref());
    upstream_url = passthrough::apply_query_auth(&upstream_url, &provider.auth, input.secret);

    let patched = input.policy.patch_body(&snapshot.body, channel)?;
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
        model: hints.model,
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
        lane_override: None,
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

/// 内核状态（`docs/10 §2.2` 的 `KernelStatus.state`）。
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
/// 比编译失败更危险（`docs/13 §4.5` L10）。
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

/// 入站运行时依赖。
pub struct IngressState {
    pub cfg: Arc<arc_swap::ArcSwap<ConfigDoc>>,
    pub clock: Arc<dyn Clock>,
    pub passthrough: Arc<dyn PassthroughPolicy>,
    pub pipeline: Arc<dyn RequestPipeline>,
    pub kernel: Arc<dyn KernelProbe>,
    pub rpm: RpmLimiter,
    pub options: IngressOptions,
}

impl IngressState {
    pub fn new(
        cfg: Arc<arc_swap::ArcSwap<ConfigDoc>>,
        clock: Arc<dyn Clock>,
        passthrough: Arc<dyn PassthroughPolicy>,
        pipeline: Arc<dyn RequestPipeline>,
        kernel: Arc<dyn KernelProbe>,
        options: IngressOptions,
    ) -> Self {
        Self { cfg, clock, passthrough, pipeline, kernel, rpm: RpmLimiter::new(), options }
    }

    /// 从 `lib.rs` 的依赖包装配（T2 用）。
    pub fn from_deps(
        deps: &Arc<GatewayDeps>,
        passthrough: Arc<dyn PassthroughPolicy>,
        pipeline: Arc<dyn RequestPipeline>,
        kernel: Arc<dyn KernelProbe>,
        options: IngressOptions,
    ) -> Self {
        Self::new(deps.config.clone(), deps.clock.clone(), passthrough, pipeline, kernel, options)
    }
}

/// 构建 axum 路由（`docs/10 §1.2` 的路由表）。
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
        .route("/healthz", get(handle))
        .fallback(any(handle))
        .with_state(state)
}

/// 绑定监听端口。**只接受 loopback**（`docs/09 §1.2` I1：永不监听 `0.0.0.0`）。
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
    let path = parts.uri.path().to_string();
    let query = parts.uri.query().map(str::to_string);
    let cfg = state.cfg.load_full();
    let now_ms = state.clock.now_ms();

    // `/healthz`：不走上游、不校验密钥（本机健康检查，见 docs/10 §1.2）
    if endpoint == Endpoint::Healthz {
        return healthz_response(&state, &request_id);
    }

    let limit = cfg.gateway.max_body_mb.saturating_mul(1024 * 1024);
    // 先看声明的长度：超限的大 body 根本不必读进内存
    if let Some(size) = declared_length(&parts.headers) {
        if size > limit {
            return error_response(&TaError::BodyTooLarge { size, limit }, &request_id);
        }
    }
    if let Err(err) = check_local_key(&cfg, &parts.headers) {
        return error_response(&err, &request_id);
    }
    if let Some(limit) = state.options.rpm_limit {
        if let Err(retry_after_s) = state.rpm.check(now_ms, limit, state.options.rpm_window_ms) {
            let mut response = error_response(&TaError::RateLimited, &request_id);
            if let Ok(value) = HeaderValue::from_str(&retry_after_s.to_string()) {
                response.headers_mut().insert(header::RETRY_AFTER, value);
            }
            return response;
        }
    }
    let body = match read_body_limited(body, limit).await {
        Ok(body) => body,
        Err(err) => return error_response(&err, &request_id),
    };

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
        Err(err) => return error_response(&err, &request_id),
    };
    match state.pipeline.execute(prepared).await {
        Ok(response) => with_request_id(response, &request_id),
        Err(err) => error_response(&err, &request_id),
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

/// 本地密钥校验（`gateway.local_key` 为空则跳过）。
pub fn check_local_key(cfg: &ConfigDoc, headers: &HeaderMap) -> Result<()> {
    let expected = cfg.gateway.local_key.as_deref().map(str::trim).filter(|key| !key.is_empty());
    let Some(expected) = expected else { return Ok(()) };
    let presented = bearer_token(headers).or_else(|| header_str(headers, "x-api-key"));
    match presented {
        Some(key) if ct_eq(key.trim().as_bytes(), expected.as_bytes()) => Ok(()),
        _ => Err(TaError::Unauthorized),
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

/// `docs/10 §1.5` 的错误信封。**只有网关自身判断失败才用它**，上游错误一律原样透传。
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
        struct FixedKernel(KernelState);
        impl KernelProbe for FixedKernel {
            fn state(&self) -> KernelState {
                self.0
            }
        }
        let clock = Arc::new(ManualClock::new(1_700_000_000_000));
        Arc::new(IngressState::new(
            Arc::new(arc_swap::ArcSwap::from_pointee(cfg.clone())),
            clock,
            FullPassthrough::shared(),
            Arc::new(EchoPipeline { cfg, policy: FullPassthrough::shared() }),
            Arc::new(FixedKernel(kernel)),
            options,
        ))
    }

    fn state() -> Arc<IngressState> {
        state_with(test_cfg(), IngressOptions::default(), KernelState::Ready)
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

    #[tokio::test]
    async fn every_registered_route_reaches_the_pipeline() {
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
            ("GET", "/v1/models", "models_list"),
            ("GET", "/v1/models/gpt-4o", "model_get"),
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
        let mut builder = Request::builder().method("POST").uri("/v1/messages");
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
        assert_eq!(echoed["endpoint"], "messages");
        // 客户端说 anthropic 协议，而这个通道的上游服务商是 openai 协议
        // → G2/G3 必须走 IR 路径（两者相等时才是 RAW 零改动透传）
        assert_eq!(echoed["client_protocol"], "anthropic");
        assert_eq!(echoed["protocol"], "openai");
        assert_eq!(echoed["has_image"], Value::Bool(true), "带图请求必须被识别（只读 hint）");
        assert_eq!(echoed["upstream_url"], "https://relay.example.com/v1/messages");
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

    #[tokio::test]
    async fn wildcard_without_model_still_reaches_the_pipeline() {
        // G1 不决定 no_route（那是 G2 的策略），但必须把请求原样送到管线
        let (status, _, value) =
            send(state(), json_request("POST", "/v1/totally-unknown", b"{\"foo\":1}")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(value["endpoint"], "wildcard");
        assert_eq!(value["body"], json!({"foo": 1}));
    }

    #[test]
    fn new_request_id_is_unique_and_prefixed() {
        let a = new_request_id();
        let b = new_request_id();
        assert!(a.starts_with("req_"));
        assert_ne!(a, b);
    }
}
