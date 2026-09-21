//! 管理 API（8788，仅 CLI）。**工作包 G1 负责实现**。
//!
//! 只暴露自动化需要的子集，见 `docs/10-interface-spec.md` §3。
//! **不做** Provider/Credential/Model/Channel/Policy 的写操作 —— 那些是"设计决策"，
//! 走 GUI（`docs/10 §3` 末尾）；CLI 只做只读 + 少数运维动作。
//!
//! ## 两处刻意的设计
//!
//! 1. **依赖 ta-store 的部分（usage/logs/lanes 健康）走 [`AdminBackend`] trait**，
//!    不是直接调 `ta-store`。原因：① 管理 API 只需要 JSON 进 JSON 出，
//!    接缝越窄越好；② store 尚未就绪时可以用 mock 跑测试。
//! 2. **`admin_token` 未配置 = 拒绝一切**（fail closed）。文档说 8788 必须带
//!    `Authorization: Bearer <admin_token>`，所以"没配 token"绝不等于"免鉴权"。

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use axum::extract::{Path, Request, State};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use http::{header, HeaderMap, HeaderValue, StatusCode};
use serde_json::{json, Value};

use ta_core::clock::Clock;
use ta_core::domain::ConfigDoc;
use ta_core::error::{Result, TaError};

use crate::ingress::{
    bearer_token, bind_loopback, ct_eq, error_response, new_request_id, read_body_limited,
    with_request_id, KernelProbe,
};
use crate::passthrough::percent_decode;
use crate::GatewayDeps;

// ================================================================ 查询串

/// 宽松的查询串解析（管理 API 的 curl 友好层）。
///
/// - 重复键保留顺序：`group_by=a&group_by=b` → `{"group_by": ["a","b"]}`
/// - `group_by[]=a` 归一化成 `group_by`
/// - `%XX` 与 `+` 都解码
///
/// 之所以手写而不 `#[derive(Deserialize)]`：这是**给脚本用的宽松入口**，
/// 键名与类型由 [`AdminBackend`] 的实现（store 适配层）决定，G1 不做 schema 假设。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QueryParams(BTreeMap<String, Vec<String>>);

impl QueryParams {
    pub fn parse(raw: Option<&str>) -> Self {
        let mut map: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let Some(raw) = raw else { return Self(map) };
        for pair in raw.split('&') {
            if pair.is_empty() {
                continue;
            }
            let (key, value) = match pair.split_once('=') {
                Some((key, value)) => (key, value),
                None => (pair, ""),
            };
            let key = percent_decode(key);
            let key = key.strip_suffix("[]").unwrap_or(&key).to_string();
            map.entry(key).or_default().push(percent_decode(value));
        }
        Self(map)
    }

    pub fn first(&self, key: &str) -> Option<&str> {
        self.0.get(key)?.first().map(String::as_str)
    }

    pub fn all(&self, key: &str) -> &[String] {
        self.0.get(key).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// 只保留首值的扁平视图（给只需要单值的后端用）。
    pub fn flattened(&self) -> BTreeMap<String, String> {
        self.0
            .iter()
            .filter_map(|(key, values)| values.first().map(|value| (key.clone(), value.clone())))
            .collect()
    }

    pub fn to_json(&self) -> Value {
        serde_json::to_value(&self.0).unwrap_or(Value::Null)
    }
}

// ================================================================ 后端接缝

/// 管理 API 需要"数据"的那部分（用量 / 日志 / 线路健康 / 内核运维）。
///
/// 全部是 JSON 进 JSON 出：G1 不假设 `ta-store` 的查询结构，
/// 由适配层（E2/E3 的 store 或 T2 的 service 层）实现。
#[async_trait]
pub trait AdminBackend: Send + Sync {
    /// 全量替换配置（等价 `config_apply`），返回 `ApplyResult` 形状的 JSON。
    async fn put_config(&self, doc: &ConfigDoc) -> Result<Value>;

    async fn usage_query(&self, query: &QueryParams) -> Result<Value>;

    async fn usage_dashboard(&self, query: &QueryParams) -> Result<Value>;

    async fn logs(&self, query: &QueryParams) -> Result<Value>;

    /// 单条 trace。`Ok(None)` = 没有这条记录（HTTP 仍 200，body 为 `null`）。
    async fn log_detail(&self, request_id: &str) -> Result<Option<Value>>;

    /// 线路健康：`{"<lane_id>": {...}}`。没有数据时返回 `null`。
    async fn lane_health(&self) -> Result<Value>;

    async fn kernel_restart(&self) -> Result<Value>;

    /// 预览生成的 mihomo YAML（只读）。
    async fn kernel_generated_config(&self) -> Result<String>;
}

// ================================================================ 状态与装配

/// 管理 API 运行时依赖。
pub struct AdminState {
    pub cfg: Arc<arc_swap::ArcSwap<ConfigDoc>>,
    pub clock: Arc<dyn Clock>,
    pub backend: Arc<dyn AdminBackend>,
    pub kernel: Arc<dyn KernelProbe>,
    /// `None` / 空白 = 管理 API 对一切请求返回 401（fail closed）
    pub admin_token: Option<String>,
    started: Instant,
}

impl AdminState {
    pub fn new(
        cfg: Arc<arc_swap::ArcSwap<ConfigDoc>>,
        clock: Arc<dyn Clock>,
        backend: Arc<dyn AdminBackend>,
        kernel: Arc<dyn KernelProbe>,
        admin_token: Option<String>,
    ) -> Self {
        Self { cfg, clock, backend, kernel, admin_token, started: Instant::now() }
    }

    pub fn from_deps(
        deps: &Arc<GatewayDeps>,
        backend: Arc<dyn AdminBackend>,
        kernel: Arc<dyn KernelProbe>,
        admin_token: Option<String>,
    ) -> Self {
        Self::new(deps.config.clone(), deps.clock.clone(), backend, kernel, admin_token)
    }

    pub fn uptime_s(&self) -> u64 {
        self.started.elapsed().as_secs()
    }
}

/// 构建管理 API 路由（`docs/10 §3` 的子集）。
pub fn router(state: Arc<AdminState>) -> Router {
    Router::new()
        .route("/status", get(h_status))
        .route("/config", get(h_config_get).put(h_config_put))
        .route("/config/validate", post(h_config_validate))
        .route("/usage/query", get(h_usage_query))
        .route("/usage/dashboard", get(h_usage_dashboard))
        .route("/logs", get(h_logs))
        .route("/logs/{request_id}", get(h_log_detail))
        .route("/pricing", get(h_pricing))
        .route("/lanes", get(h_lanes))
        .route("/kernel/restart", post(h_kernel_restart))
        .route("/kernel/generated-config", get(h_kernel_generated_config))
        .with_state(state)
}

/// 起管理 API（监听地址取 `gateway.admin_listen`）。**同样只绑 loopback**。
pub async fn serve(state: Arc<AdminState>) -> Result<()> {
    let listen = state.cfg.load().gateway.admin_listen.clone();
    let listener = bind_loopback(&listen).await?;
    serve_on(listener, state).await
}

pub async fn serve_on(listener: tokio::net::TcpListener, state: Arc<AdminState>) -> Result<()> {
    axum::serve(listener, router(state))
        .await
        .map_err(|e| TaError::Internal(format!("管理 API 退出：{e}")))
}

// ================================================================ 鉴权

/// 独立 token 鉴权。未配置 token 时**拒绝**（不静默放行）。
pub fn check_admin_token(state: &AdminState, headers: &HeaderMap) -> Result<()> {
    let expected = state
        .admin_token
        .as_deref()
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .ok_or(TaError::Unauthorized)?;
    match bearer_token(headers) {
        Some(presented) if ct_eq(presented.trim().as_bytes(), expected.as_bytes()) => Ok(()),
        _ => Err(TaError::Unauthorized),
    }
}

fn authorized(state: &AdminState, req: &Request, request_id: &str) -> Option<Response> {
    match check_admin_token(state, req.headers()) {
        Ok(()) => None,
        Err(err) => Some(error_response(&err, request_id)),
    }
}

fn params(req: &Request) -> QueryParams {
    QueryParams::parse(req.uri().query())
}

/// 文本响应（YAML 导出 / 内核配置预览），不套 JSON。
fn text_response(body: String, content_type: &str, request_id: &str) -> Response {
    let mut response = Response::new(axum::body::Body::from(body));
    if let Ok(value) = HeaderValue::from_str(content_type) {
        response.headers_mut().insert(header::CONTENT_TYPE, value);
    }
    with_request_id(response, request_id)
}

// ================================================================ handlers

async fn h_status(State(state): State<Arc<AdminState>>, req: Request) -> Response {
    let request_id = new_request_id();
    if let Some(response) = authorized(&state, &req, &request_id) {
        return response;
    }
    let cfg = state.cfg.load_full();
    // 生效策略 = 第一个 enabled 的（`docs/10 §2.2 app_status`）
    let active_policy_id =
        cfg.enabled_policies_in_order().first().map(|p| p.id.as_str().to_string());
    let body = json!({
        "gateway": "running",
        "kernel": state.kernel.state().as_str(),
        "kernel_version": state.kernel.version(),
        "version": env!("CARGO_PKG_VERSION"),
        "uptime_s": state.uptime_s(),
        "active_policy_id": active_policy_id,
        "default_lane_id": cfg.net.default_lane.as_ref().map(|lane| lane.as_str()),
        "current_node": state.kernel.current_node(),
    });
    with_request_id((StatusCode::OK, axum::Json(body)).into_response(), &request_id)
}

async fn h_config_get(State(state): State<Arc<AdminState>>, req: Request) -> Response {
    let request_id = new_request_id();
    if let Some(response) = authorized(&state, &req, &request_id) {
        return response;
    }
    let cfg = state.cfg.load_full();
    if params(&req).first("format") == Some("yaml") {
        return match ta_core::config::to_yaml(&cfg) {
            Ok(yaml) => text_response(yaml, "application/yaml; charset=utf-8", &request_id),
            Err(err) => error_response(&err, &request_id),
        };
    }
    match serde_json::to_value(cfg.as_ref()) {
        Ok(value) => {
            with_request_id((StatusCode::OK, axum::Json(value)).into_response(), &request_id)
        }
        Err(err) => {
            error_response(&TaError::Internal(format!("配置序列化失败：{err}")), &request_id)
        }
    }
}

async fn h_config_put(State(state): State<Arc<AdminState>>, req: Request) -> Response {
    let request_id = new_request_id();
    if let Some(response) = authorized(&state, &req, &request_id) {
        return response;
    }
    let Some(doc) = (match read_config_body(&state, req).await {
        Ok(doc) => doc,
        Err(err) => return error_response(&err, &request_id),
    }) else {
        return error_response(
            &TaError::MalformedRequest("请求体不是合法 JSON".into()),
            &request_id,
        );
    };

    let report = ta_core::config::validate(&doc);
    if !report.is_ok() {
        // 配置非法 → 只返回首条错误附近的完整列表，**不动内存里的旧配置**
        return error_response(&TaError::ConfigInvalid { errors: report.errors }, &request_id);
    }
    let applied = match state.backend.put_config(&doc).await {
        Ok(value) => value,
        Err(err) => return error_response(&err, &request_id),
    };
    // 落库成功后才切内存：失败时旧配置原样可用（干净失败 > 半成品）
    state.cfg.store(Arc::new(doc));
    let body = if applied.is_null() {
        json!({ "applied": true, "restart_required": false, "kernel_reload": false })
    } else {
        applied
    };
    with_request_id((StatusCode::OK, axum::Json(body)).into_response(), &request_id)
}

async fn h_config_validate(State(state): State<Arc<AdminState>>, req: Request) -> Response {
    let request_id = new_request_id();
    if let Some(response) = authorized(&state, &req, &request_id) {
        return response;
    }
    let Some(doc) = (match read_config_body(&state, req).await {
        Ok(doc) => doc,
        Err(err) => return error_response(&err, &request_id),
    }) else {
        return error_response(
            &TaError::MalformedRequest("请求体不是合法 JSON".into()),
            &request_id,
        );
    };
    let report = ta_core::config::validate(&doc);
    // 校验结果是**数据**不是错误：200 + ok/errors/warnings（前端"跳到出错字段"用）
    match serde_json::to_value(&report) {
        Ok(value) => {
            with_request_id((StatusCode::OK, axum::Json(value)).into_response(), &request_id)
        }
        Err(err) => {
            error_response(&TaError::Internal(format!("校验报告序列化失败：{err}")), &request_id)
        }
    }
}

async fn h_usage_query(State(state): State<Arc<AdminState>>, req: Request) -> Response {
    let request_id = new_request_id();
    if let Some(response) = authorized(&state, &req, &request_id) {
        return response;
    }
    let query = params(&req);
    match state.backend.usage_query(&query).await {
        Ok(value) => {
            with_request_id((StatusCode::OK, axum::Json(value)).into_response(), &request_id)
        }
        Err(err) => error_response(&err, &request_id),
    }
}

async fn h_usage_dashboard(State(state): State<Arc<AdminState>>, req: Request) -> Response {
    let request_id = new_request_id();
    if let Some(response) = authorized(&state, &req, &request_id) {
        return response;
    }
    let query = params(&req);
    match state.backend.usage_dashboard(&query).await {
        Ok(value) => {
            with_request_id((StatusCode::OK, axum::Json(value)).into_response(), &request_id)
        }
        Err(err) => error_response(&err, &request_id),
    }
}

async fn h_logs(State(state): State<Arc<AdminState>>, req: Request) -> Response {
    let request_id = new_request_id();
    if let Some(response) = authorized(&state, &req, &request_id) {
        return response;
    }
    let query = params(&req);
    match state.backend.logs(&query).await {
        Ok(value) => {
            with_request_id((StatusCode::OK, axum::Json(value)).into_response(), &request_id)
        }
        Err(err) => error_response(&err, &request_id),
    }
}

async fn h_log_detail(
    State(state): State<Arc<AdminState>>,
    Path(id): Path<String>,
    req: Request,
) -> Response {
    let request_id = new_request_id();
    if let Some(response) = authorized(&state, &req, &request_id) {
        return response;
    }
    match state.backend.log_detail(&id).await {
        // 契约没定义 404 语义，而错误码表里没有"记录不存在"——
        // 与其发明一个错误码，不如 200 + null（见交付清单 E 块）
        Ok(None) => {
            with_request_id((StatusCode::OK, axum::Json(Value::Null)).into_response(), &request_id)
        }
        Ok(Some(value)) => {
            with_request_id((StatusCode::OK, axum::Json(value)).into_response(), &request_id)
        }
        Err(err) => error_response(&err, &request_id),
    }
}

async fn h_pricing(State(state): State<Arc<AdminState>>, req: Request) -> Response {
    let request_id = new_request_id();
    if let Some(response) = authorized(&state, &req, &request_id) {
        return response;
    }
    // 定价的真源就是配置本身（`pricing.entries`），不需要额外的 store 接缝
    let cfg = state.cfg.load_full();
    match serde_json::to_value(&cfg.pricing) {
        Ok(value) => {
            with_request_id((StatusCode::OK, axum::Json(value)).into_response(), &request_id)
        }
        Err(err) => {
            error_response(&TaError::Internal(format!("定价表序列化失败：{err}")), &request_id)
        }
    }
}

async fn h_lanes(State(state): State<Arc<AdminState>>, req: Request) -> Response {
    let request_id = new_request_id();
    if let Some(response) = authorized(&state, &req, &request_id) {
        return response;
    }
    let health = match state.backend.lane_health().await {
        Ok(Value::Null) => Value::Object(serde_json::Map::new()),
        Ok(value) => value,
        Err(err) => return error_response(&err, &request_id),
    };
    let cfg = state.cfg.load_full();
    let lanes: Vec<Value> = cfg
        .lanes
        .iter()
        .map(|lane| {
            json!({
                "id": lane.id.as_str(),
                "name": lane.name,
                "kind": lane.kind,
                "enabled": lane.enabled,
                "subscription": lane.subscription.as_ref().map(|s| s.as_str()),
                "node": lane.node.as_deref(),
                "health": health.get(lane.id.as_str()).cloned().unwrap_or(Value::Null),
            })
        })
        .collect();
    let body = json!({ "lanes": lanes });
    with_request_id((StatusCode::OK, axum::Json(body)).into_response(), &request_id)
}

async fn h_kernel_restart(State(state): State<Arc<AdminState>>, req: Request) -> Response {
    let request_id = new_request_id();
    if let Some(response) = authorized(&state, &req, &request_id) {
        return response;
    }
    match state.backend.kernel_restart().await {
        Ok(Value::Null) => with_request_id(
            (StatusCode::OK, axum::Json(json!({ "kernel": state.kernel.state().as_str() })))
                .into_response(),
            &request_id,
        ),
        Ok(value) => {
            with_request_id((StatusCode::OK, axum::Json(value)).into_response(), &request_id)
        }
        Err(err) => error_response(&err, &request_id),
    }
}

async fn h_kernel_generated_config(State(state): State<Arc<AdminState>>, req: Request) -> Response {
    let request_id = new_request_id();
    if let Some(response) = authorized(&state, &req, &request_id) {
        return response;
    }
    match state.backend.kernel_generated_config().await {
        Ok(yaml) => text_response(yaml, "application/yaml; charset=utf-8", &request_id),
        Err(err) => error_response(&err, &request_id),
    }
}

/// 读并解析配置体。`Ok(None)` = 空体（调用方转成 400）。
async fn read_config_body(state: &AdminState, req: Request) -> Result<Option<ConfigDoc>> {
    let limit = state.cfg.load().gateway.max_body_mb.saturating_mul(1024 * 1024).max(1024 * 1024);
    let body = read_body_limited(req.into_body(), limit).await?;
    if body.is_empty() {
        return Ok(None);
    }
    serde_json::from_slice::<ConfigDoc>(&body)
        .map(Some)
        .map_err(|e| TaError::MalformedRequest(format!("配置不是合法 JSON：{e}")))
}

// ================================================================ 测试

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use std::sync::Mutex;
    use ta_core::clock::ManualClock;
    use tower::ServiceExt;

    use crate::ingress::KernelState;

    const TOKEN: &str = "admin-token-xyz";

    #[derive(Default)]
    struct TestBackend {
        applied: Value,
        restart: Value,
        generated: String,
        detail: Option<Value>,
        lane_health: Value,
        put_calls: Mutex<Vec<ConfigDoc>>,
    }

    #[async_trait]
    impl AdminBackend for TestBackend {
        async fn put_config(&self, doc: &ConfigDoc) -> Result<Value> {
            match self.put_calls.lock() {
                Ok(mut calls) => calls.push(doc.clone()),
                Err(poisoned) => poisoned.into_inner().push(doc.clone()),
            }
            Ok(self.applied.clone())
        }

        async fn usage_query(&self, query: &QueryParams) -> Result<Value> {
            Ok(json!({ "kind": "usage", "params": query.to_json() }))
        }

        async fn usage_dashboard(&self, query: &QueryParams) -> Result<Value> {
            Ok(json!({ "kind": "dashboard", "params": query.to_json() }))
        }

        async fn logs(&self, query: &QueryParams) -> Result<Value> {
            Ok(json!({ "kind": "logs", "params": query.to_json() }))
        }

        async fn log_detail(&self, request_id: &str) -> Result<Option<Value>> {
            Ok(self.detail.clone().map(|value| json!({ "request_id": request_id, "trace": value })))
        }

        async fn lane_health(&self) -> Result<Value> {
            Ok(self.lane_health.clone())
        }

        async fn kernel_restart(&self) -> Result<Value> {
            Ok(self.restart.clone())
        }

        async fn kernel_generated_config(&self) -> Result<String> {
            Ok(self.generated.clone())
        }
    }

    struct FixedKernel(KernelState);

    impl KernelProbe for FixedKernel {
        fn state(&self) -> KernelState {
            self.0
        }
        fn version(&self) -> Option<String> {
            Some("1.19.0".into())
        }
        fn current_node(&self) -> Option<String> {
            Some("东京-01".into())
        }
    }

    fn test_cfg() -> ConfigDoc {
        let mut doc = ta_core::config::default_doc();
        doc.net.default_lane = Some(ta_core::types::LaneId::new("lane-direct"));
        doc
    }

    /// 引用完整性坏掉的配置：通道指向不存在的 provider / credential。
    fn broken_cfg_value() -> Value {
        let mut doc = serde_json::to_value(test_cfg()).unwrap();
        doc["channels"] = json!([{
            "id": "ch-broken",
            "model": "gpt-4o",
            "provider": "p-none",
            "credential": "c-none",
            "upstream_model": "gpt-4o"
        }]);
        doc
    }

    fn state_with(cfg: ConfigDoc, backend: TestBackend, token: Option<&str>) -> Arc<AdminState> {
        let clock = Arc::new(ManualClock::new(1_700_000_000_000));
        Arc::new(AdminState::new(
            Arc::new(arc_swap::ArcSwap::from_pointee(cfg)),
            clock,
            Arc::new(backend),
            Arc::new(FixedKernel(KernelState::Ready)),
            token.map(str::to_string),
        ))
    }

    fn state(backend: TestBackend) -> Arc<AdminState> {
        state_with(test_cfg(), backend, Some(TOKEN))
    }

    async fn send(state: Arc<AdminState>, request: Request) -> (StatusCode, HeaderMap, Value) {
        let response = router(state).oneshot(request).await.expect("管理 API 必须能处理请求");
        let status = response.status();
        let headers = response.headers().clone();
        let bytes =
            axum::body::to_bytes(response.into_body(), 8 * 1024 * 1024).await.expect("响应体可读");
        let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, headers, value)
    }

    fn get_(uri: &str) -> Request {
        Request::builder()
            .method("GET")
            .uri(uri)
            .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
            .body(Body::empty())
            .expect("构造请求")
    }

    fn post_json(uri: &str, body: Value) -> Request {
        Request::builder()
            .method("POST")
            .uri(uri)
            .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .expect("构造请求")
    }

    fn put_json(uri: &str, body: Value) -> Request {
        Request::builder()
            .method("PUT")
            .uri(uri)
            .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .expect("构造请求")
    }

    // ---------------------------------------------------------- 查询串

    #[test]
    fn query_params_parse_repeats_and_escapes() {
        let q = QueryParams::parse(Some(
            "limit=20&group_by=model&group_by=credential&model=gpt-4o%20mini&empty",
        ));
        assert_eq!(q.first("limit"), Some("20"));
        assert_eq!(q.all("group_by"), ["model".to_string(), "credential".to_string()]);
        assert_eq!(q.first("model"), Some("gpt-4o mini"));
        assert_eq!(q.first("empty"), Some(""));
        assert!(q.first("nope").is_none());
        assert!(QueryParams::parse(None).is_empty());

        // `group_by[]=a` 归一化，`+` 解码成空格
        let q = QueryParams::parse(Some("group_by%5B%5D=time&from=a+b"));
        assert_eq!(q.all("group_by"), ["time".to_string()]);
        assert_eq!(q.first("from"), Some("a b"));

        let flat = q.flattened();
        assert_eq!(flat.get("from").map(String::as_str), Some("a b"));
        assert_eq!(q.to_json()["group_by"], json!(["time"]));
    }

    // ---------------------------------------------------------- 鉴权

    #[tokio::test]
    async fn admin_token_is_required_and_checked() {
        let state = state(TestBackend::default());

        // 正确
        let (status, _, _) = send(state.clone(), get_("/status")).await;
        assert_eq!(status, StatusCode::OK);

        // 缺失
        let (status, _, value) = send(
            state.clone(),
            Request::builder().method("GET").uri("/status").body(Body::empty()).unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(value["error"]["code"], "TA-1001");

        // 错误
        let request = Request::builder()
            .method("GET")
            .uri("/status")
            .header(header::AUTHORIZATION, "Bearer wrong-token")
            .body(Body::empty())
            .unwrap();
        let (status, _, _) = send(state, request).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn unconfigured_admin_token_denies_everything() {
        let state = state_with(test_cfg(), TestBackend::default(), None);
        let (status, _, _) = send(
            state,
            Request::builder()
                .method("GET")
                .uri("/status")
                .header(header::AUTHORIZATION, "Bearer anything")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "没配 token 绝不等于免鉴权");

        // 空白 token 同样视为未配置
        let blank = state_with(test_cfg(), TestBackend::default(), Some("  "));
        let (status, _, _) = send(blank, get_("/status")).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    // ---------------------------------------------------------- 只读端点

    #[tokio::test]
    async fn status_reflects_config_and_kernel() {
        let (status, headers, value) = send(state(TestBackend::default()), get_("/status")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(value["gateway"], "running");
        assert_eq!(value["kernel"], "ready");
        assert_eq!(value["kernel_version"], "1.19.0");
        assert_eq!(value["default_lane_id"], "lane-direct");
        assert_eq!(value["current_node"], "东京-01");
        assert_eq!(value["active_policy_id"], "pol-default");
        assert_eq!(value["version"], env!("CARGO_PKG_VERSION"));
        assert!(value["uptime_s"].is_u64());
        assert!(headers["x-tokenagency-request-id"].to_str().unwrap().starts_with("req_"));
    }

    #[tokio::test]
    async fn config_export_serves_json_and_yaml() {
        let state = state(TestBackend::default());
        let (status, headers, value) = send(state.clone(), get_("/config")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers[header::CONTENT_TYPE], "application/json");
        assert_eq!(value["gateway"]["listen"], "127.0.0.1:8787");
        assert!(value["channels"].is_array());

        let (status, headers, _) = send(state, get_("/config?format=yaml")).await;
        assert_eq!(status, StatusCode::OK);
        assert!(headers[header::CONTENT_TYPE].to_str().unwrap().starts_with("application/yaml"));
    }

    #[tokio::test]
    async fn pricing_comes_from_config_truth() {
        let (status, _, value) = send(state(TestBackend::default()), get_("/pricing")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(value["currency"], "USD");
        assert_eq!(value["unit"], "per_1m");
        assert!(value["entries"].is_array());
    }

    #[tokio::test]
    async fn lanes_merge_config_with_health() {
        let backend = TestBackend {
            lane_health: json!({ "lane-direct": { "ok": false, "latency_ms": 120 } }),
            ..Default::default()
        };
        let (status, _, value) = send(state(backend), get_("/lanes")).await;
        assert_eq!(status, StatusCode::OK);
        let lanes = value["lanes"].as_array().unwrap();
        assert!(!lanes.is_empty());
        let direct = lanes.iter().find(|lane| lane["id"] == "lane-direct").unwrap();
        assert_eq!(direct["health"]["latency_ms"], 120);
        assert_eq!(direct["kind"], "direct");
        // 没有健康数据的 lane → null（不编造）
        let other = lanes.iter().find(|lane| lane["id"] != "lane-direct").unwrap();
        assert!(other["health"].is_null());
    }

    #[tokio::test]
    async fn usage_and_logs_forward_query_params_to_backend() {
        let base = state(TestBackend::default());
        let (status, _, value) =
            send(base.clone(), get_("/usage/query?group_by=model&range=7d")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(value["kind"], "usage");
        assert_eq!(value["params"]["group_by"], json!(["model"]));

        let (status, _, value) = send(base.clone(), get_("/usage/dashboard?range=today")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(value["kind"], "dashboard");

        let (status, _, value) = send(base.clone(), get_("/logs?limit=5")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(value["kind"], "logs");

        // 单条不存在 → 200 + null（不发明错误码）
        let (status, _, value) = send(base, get_("/logs/req_missing")).await;
        assert_eq!(status, StatusCode::OK);
        assert!(value.is_null());

        let with_detail =
            state(TestBackend { detail: Some(json!({"attempts": 2})), ..Default::default() });
        let (status, _, value) = send(with_detail, get_("/logs/req_abc")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(value["request_id"], "req_abc");
        assert_eq!(value["trace"]["attempts"], 2);
    }

    #[tokio::test]
    async fn unknown_admin_path_is_404() {
        let (status, _, _) = send(state(TestBackend::default()), get_("/nope")).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    // ---------------------------------------------------------- 运维端点

    #[tokio::test]
    async fn kernel_endpoints_delegate_to_backend() {
        let backend = TestBackend {
            restart: json!({ "state": "restarting" }),
            generated: "mode: rule\n".into(),
            ..Default::default()
        };
        let state = state(backend);
        let (status, _, value) = send(state.clone(), post_json("/kernel/restart", json!({}))).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(value["state"], "restarting");

        let response = router(state).oneshot(get_("/kernel/generated-config")).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.headers()["content-type"]
            .to_str()
            .unwrap()
            .starts_with("application/yaml"));
        let bytes = axum::body::to_bytes(response.into_body(), 1024).await.unwrap();
        assert_eq!(String::from_utf8_lossy(&bytes), "mode: rule\n");
    }

    // ---------------------------------------------------------- 配置校验 / 应用

    #[tokio::test]
    async fn config_validate_reports_errors_as_data() {
        let state = state(TestBackend::default());
        let doc = serde_json::to_value(test_cfg()).unwrap();
        let (status, _, value) = send(state.clone(), post_json("/config/validate", doc)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(value["ok"], true);
        assert_eq!(value["errors"], json!([]));

        // 引用完整性坏掉 → ok=false 且带字段路径（仍然是 200：校验结果是数据）
        let (status, _, value) =
            send(state, post_json("/config/validate", broken_cfg_value())).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(value["ok"], false);
        let errors = value["errors"].as_array().unwrap();
        assert!(!errors.is_empty());
        assert!(
            errors.iter().any(|e| e["path"].as_str().unwrap_or("").contains("channels[0]")),
            "错误必须定位到具体字段：{errors:?}"
        );
    }

    #[tokio::test]
    async fn config_put_applies_only_valid_config() {
        let state = state_with(test_cfg(), TestBackend::default(), Some(TOKEN));

        // 非法配置：400 + TA-5001，且内存里的配置不变
        let (status, _, value) = send(state.clone(), put_json("/config", broken_cfg_value())).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "非法配置必须被拒：{value}");
        assert_eq!(value["error"]["code"], "TA-5001");
        assert_eq!(value["error"]["type"], "tokenagency_config_invalid");
        assert!(value["error"]["details"].is_array(), "校验细节必须回传，便于定位");
        assert_eq!(state.cfg.load().gateway.listen, "127.0.0.1:8787", "失败时不能改内存配置");

        // 合法配置：应用 + 切内存 + 交给 backend 落库
        let mut updated = serde_json::to_value(test_cfg()).unwrap();
        updated["gateway"]["listen"] = json!("127.0.0.1:9999");
        let (status, _, value) = send(state.clone(), put_json("/config", updated)).await;
        assert_eq!(status, StatusCode::OK, "{value}");
        assert_eq!(value["applied"], true);
        assert_eq!(state.cfg.load().gateway.listen, "127.0.0.1:9999");
    }

    #[tokio::test]
    async fn config_put_rejects_malformed_json() {
        let state = state(TestBackend::default());
        let request = Request::builder()
            .method("PUT")
            .uri("/config")
            .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
            .body(Body::from("{not json"))
            .unwrap();
        let response = router(state).oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    // ---------------------------------------------------------- 装配

    #[tokio::test]
    async fn admin_serve_binds_loopback_only() {
        let listener = bind_loopback("127.0.0.1:0").await.unwrap();
        assert!(listener.local_addr().unwrap().ip().is_loopback());
        // 管理 API 与网关共用同一条"只绑 loopback"的规则
        assert!(bind_loopback("0.0.0.0:8788").await.is_err());
    }

    #[tokio::test]
    async fn from_deps_shares_the_config_arc() {
        // 只验证"共享配置/时钟的接线"这一件事：`from_deps` 必须与调用方看到**同一份**配置
        // （否则 GUI 改配置后管理 API 还读旧的）。其余 GatewayDeps 依赖用最小 mock。
        struct NoStore;
        #[async_trait]
        impl crate::traits::GatewayStore for NoStore {
            async fn record_usage(&self, _rec: ta_core::types::UsageRecord) -> Result<()> {
                Ok(())
            }
            async fn recent_summaries(
                &self,
                _limit: usize,
            ) -> Result<Vec<ta_core::types::RequestSummary>> {
                Ok(vec![])
            }
        }
        struct NoDialer;
        #[async_trait]
        impl crate::traits::OutboundDialer for NoDialer {
            async fn execute(
                &self,
                _lane: &ta_core::types::LaneId,
                _req: crate::dispatch::UpstreamRequest,
            ) -> Result<crate::dispatch::UpstreamResponse> {
                Err(TaError::Internal("mock：本测试不该真的出网".into()))
            }
            async fn health(&self, lane: &ta_core::types::LaneId) -> crate::dispatch::HealthReport {
                crate::dispatch::HealthReport {
                    lane_id: lane.clone(),
                    ok: false,
                    latency_ms: None,
                    error: Some("mock".into()),
                }
            }
        }
        struct NoEvents;
        impl crate::traits::EventBus for NoEvents {
            fn publish(&self, _e: crate::events::TaEvent) {}
            fn subscribe(&self) -> tokio::sync::broadcast::Receiver<crate::events::TaEvent> {
                tokio::sync::broadcast::channel(1).1
            }
        }
        struct NoPricing;
        impl ta_core::pricing::PricingEngine for NoPricing {
            fn resolve(
                &self,
                _model: &str,
                _provider: &ta_core::types::ProviderId,
                _cred: &ta_core::types::CredentialId,
            ) -> ta_core::pricing::ResolvedPrice {
                ta_core::pricing::ResolvedPrice::unpriced("USD", 1.0)
            }
        }
        struct NoQuota;
        impl ta_core::quota::QuotaLedger for NoQuota {
            fn state(&self, _cred: &ta_core::types::CredentialId) -> ta_core::quota::QuotaState {
                ta_core::quota::QuotaState::default()
            }
            fn observe(
                &self,
                _cred: &ta_core::types::CredentialId,
                _obs: ta_core::quota::QuotaObservation,
            ) {
            }
            fn mark_exhausted(&self, _cred: &ta_core::types::CredentialId, _hint: Option<i64>) {}
            fn mark_invalid(&self, _cred: &ta_core::types::CredentialId) {}
            fn roll_window(&self, _now_ms: i64) -> Vec<ta_core::types::CredentialId> {
                vec![]
            }
            fn remaining_ratio(&self, _cred: &ta_core::types::CredentialId) -> Option<f64> {
                None
            }
        }

        let deps = Arc::new(GatewayDeps {
            config: Arc::new(arc_swap::ArcSwap::from_pointee(test_cfg())),
            store: Arc::new(NoStore),
            dialer: Arc::new(NoDialer),
            events: Arc::new(NoEvents),
            clock: Arc::new(ManualClock::new(0)),
            pricing: Arc::new(NoPricing),
            quota: Arc::new(NoQuota),
        });
        let state = AdminState::from_deps(
            &deps,
            Arc::new(TestBackend::default()),
            Arc::new(FixedKernel(KernelState::Ready)),
            Some(TOKEN.into()),
        );
        // 通过 deps 改配置 → 管理 API 必须立刻看到（证明是同一份 Arc 而不是拷贝）
        let mut changed = test_cfg();
        changed.net.default_lane = Some(ta_core::types::LaneId::new("lane-changed"));
        deps.config.store(Arc::new(changed));

        let (status, _, value) = send(Arc::new(state), get_("/status")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(value["default_lane_id"], "lane-changed");
    }

    #[test]
    fn query_params_default_is_empty() {
        assert_eq!(QueryParams::default(), QueryParams::parse(None));
        assert!(QueryParams::default().all("x").is_empty());
    }
}
