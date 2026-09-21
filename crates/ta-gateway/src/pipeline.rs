//! 12 步请求管线编排 + 失败转移。**工作包 G2 负责实现**。
//!
//! 步骤见 `docs/09-architecture-design.md` §5.1。G1 做完了 1–2 步与"定点改写"，
//! 这里做 3–12 步：hints → 路由决策 → 选择器 → 选通道 → 取凭据/lane → 建请求 →
//! 分发 → 计量/追踪 → 失败转移 → trace。
//!
//! ## 三条不能违反的规则
//!
//! 1. **只用 `ingress::build_prepared_request` 造上游请求**。它是唯一一个
//!    "整体转发 + 4 处定点改"的实现，也负责按实际 body 重算 `content-length`；
//!    在这里自己拼头/拼 body = 把全透传的铁律交给第二个人去守（必然漂移）。
//! 2. **已向客户端写出第一个字节后绝不重试**（`docs/03 §5.2`）。结构上保证：
//!    流式响应一旦返回，"再试一次"的代码路径就不存在了。
//! 3. **lane 失败 ≠ 通道失败**。lane 连不上 → 标记该 lane 不可用（只影响用这条 lane
//!    的通道）；429/402/5xx → 冷却**通道**。混成一个循环就会出现
//!    "同一个已耗尽的套餐被重试 5 次"（`docs/03 §5.1`、`docs/12 §4 G2`）。

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use bytes::Bytes;
use futures_util::StreamExt;

use ta_core::clock::Clock;
use ta_core::domain::behavior::resolve_lane;
use ta_core::domain::{ChannelConfig, ConfigDoc, NoCandidateAction};
use ta_core::error::{Result, TaError};
use ta_core::quota::{QuotaLedger, QuotaObservation};
use ta_core::routing::{
    ChannelRuntimeState, ChannelSelector, PolicyEngine, RouteContext, RouteHints,
};
use ta_core::types::{ChannelId, CredentialId, LaneId, Protocol};

use crate::dispatch::{self, ByteStream, OutboundDialer, UpstreamRequest, UpstreamResponse};
use crate::ingress::{
    build_prepared_request, PrepareInput, PreparedRequest, RequestPipeline, RequestSnapshot,
};
use crate::passthrough::PassthroughPolicy;

// ================================================================ 接缝（别的包实现）

/// 凭据解析：`"${secret:name}"` / `"${env:VAR}"` → 明文。
///
/// **解析失败必须是错误**：用空 key 去打上游会得到 401，用户看到的是"我的 key 坏了"，
/// 比直接说"钥匙串里没有这个 key"难查得多。
#[async_trait]
pub trait SecretResolver: Send + Sync {
    async fn resolve(&self, spec: &str) -> Result<String>;
}

/// 固定的 spec → 明文表（测试 / 便携模式）。
#[derive(Debug, Clone, Default)]
pub struct StaticSecrets(pub BTreeMap<String, String>);

impl StaticSecrets {
    pub fn new<I, K, V>(pairs: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        Self(pairs.into_iter().map(|(k, v)| (k.into(), v.into())).collect())
    }
}

#[async_trait]
impl SecretResolver for StaticSecrets {
    async fn resolve(&self, spec: &str) -> Result<String> {
        self.0.get(spec).cloned().ok_or_else(|| TaError::Secret(format!("没有名为 {spec} 的密钥")))
    }
}

/// 一次上游尝试的结局（trace / 事件 / 计量的输入）。
#[derive(Debug, Clone)]
pub struct AttemptRecord {
    pub request_id: String,
    pub channel_id: ChannelId,
    pub credential_id: CredentialId,
    pub lane_id: Option<LaneId>,
    /// 第几次尝试（从 1 开始；`0` = 流中断这类"不是一次尝试"的记录）
    pub attempt: u32,
    pub status: Option<u16>,
    /// 网关**自身**判断失败的错误码；上游错误保留状态码、不编错误码
    pub error_code: Option<String>,
    pub latency_ms: u32,
    /// 是否已经向客户端写出过字节（写出后的一切只记录，不重试）
    pub bytes_sent: bool,
}

/// trace / 事件接缝（G4 实现）。
#[async_trait]
pub trait AttemptSink: Send + Sync {
    async fn on_attempt(&self, record: AttemptRecord);
}

/// 计量接缝（G4 实现）：拿响应 body 去解析 usage。
///
/// `observe_stream` **必须是直通包装**（可以看，不能攒）：G2 保证零缓冲，
/// 计量不许把它变成"等整条流结束"。
pub trait ResponseMeter: Send + Sync {
    fn observe_full(&self, ctx: MeterContext, body: &Bytes, status: u16);
    fn observe_stream(&self, ctx: MeterContext, stream: ByteStream) -> ByteStream;
}

/// 计量上下文（够 G4 定位到通道/凭据/model）。
#[derive(Debug, Clone)]
pub struct MeterContext {
    pub request_id: String,
    pub channel_id: ChannelId,
    pub credential_id: CredentialId,
    pub model: Option<String>,
    pub upstream_model: String,
    pub lane_id: Option<LaneId>,
    pub protocol: Protocol,
    pub stream: bool,
}

// ================================================================ 依赖与状态

pub struct PipelineDeps {
    pub cfg: Arc<arc_swap::ArcSwap<ConfigDoc>>,
    pub clock: Arc<dyn Clock>,
    pub policy: Arc<dyn PolicyEngine>,
    pub selector: Arc<dyn ChannelSelector>,
    pub quota: Arc<dyn QuotaLedger>,
    pub secrets: Arc<dyn SecretResolver>,
    pub dialer: Arc<dyn OutboundDialer>,
    pub passthrough: Arc<dyn PassthroughPolicy>,
    pub sink: Arc<dyn AttemptSink>,
    /// `None` = 计量还没接上（G4 在途）。**不提供空实现**：
    /// 静默地"接了但没数据"比没接更糟（`docs/13 §4.5` L10）。
    pub meter: Option<Arc<dyn ResponseMeter>>,
}

/// 运行态：冷却与 lane 健康。**只在本进程内**（重启即忘，不需要落库）。
#[derive(Debug, Default)]
struct Runtime {
    /// 通道 → 冷却结束时间（ms）
    cooling_until_ms: BTreeMap<ChannelId, i64>,
    /// lane → 不可用**截止时间**（ms）。不是永久拉黑，见 [`LANE_RECOVERY_MS`]。
    lane_down_until_ms: BTreeMap<LaneId, i64>,
}

/// lane 被判不可用后的恢复窗口。
///
/// **为什么必须"短暂"而不是永久**：M0 真机实测（`docs/06 §1.6`）—— 内核热重载
/// （`PUT /configs?force=true`）时 listener 会经历一瞬间的关闭/重建，此时会出现
/// `connection refused`。若把它当成"这条 lane 坏了"永久拉黑，用户每改一次线路
/// 就会永久失去那条线（直到重启网关）。
const LANE_RECOVERY_MS: i64 = 5_000;

/// 一次尝试的"是谁"部分；结果由 [`AttemptBuilder::record`] 补上。
struct AttemptBuilder<'a> {
    prepared: &'a PreparedRequest,
    channel: &'a ChannelConfig,
    lane: &'a LaneId,
    attempt: u32,
}

impl AttemptBuilder<'_> {
    fn record(
        &self,
        status: Option<u16>,
        error_code: Option<&'static str>,
        latency_ms: u32,
        bytes_sent: bool,
    ) -> AttemptRecord {
        AttemptRecord {
            request_id: self.prepared.request_id.clone(),
            channel_id: self.channel.id.clone(),
            credential_id: self.channel.credential.clone(),
            lane_id: Some(self.lane.clone()),
            attempt: self.attempt,
            status,
            error_code: error_code.map(str::to_string),
            latency_ms,
            bytes_sent,
        }
    }
}

pub struct Pipeline {
    deps: Arc<PipelineDeps>,
    runtime: Mutex<Runtime>,
}

impl Pipeline {
    pub fn new(deps: Arc<PipelineDeps>) -> Self {
        Self { deps, runtime: Mutex::new(Runtime::default()) }
    }

    /// 某个候选现在能不能用（通道冷却 / lane 已判定 down）。
    pub fn is_available(&self, channel_id: &ChannelId, lane: Option<&LaneId>, now_ms: i64) -> bool {
        let guard = self.lock_runtime();
        if guard.cooling_until_ms.get(channel_id).is_some_and(|until| *until > now_ms) {
            return false;
        }
        !lane
            .map(|lane| guard.lane_down_until_ms.get(lane).is_some_and(|until| *until > now_ms))
            .unwrap_or(false)
    }

    /// 通道级失败（429/402/5xx）：冷却该通道。
    fn cool_channel(&self, channel_id: &ChannelId, cooldown_s: u32, now_ms: i64) {
        self.lock_runtime()
            .cooling_until_ms
            .insert(channel_id.clone(), now_ms + (cooldown_s as i64) * 1000);
    }

    /// lane 级失败：把这条 lane 标记为**短暂**不可用（只影响用这条 lane 的通道）。
    fn mark_lane_down(&self, lane: &LaneId, now_ms: i64) {
        self.lock_runtime().lane_down_until_ms.insert(lane.clone(), now_ms + LANE_RECOVERY_MS);
    }

    /// 锁中毒不该让网关瘫痪：取回内部值继续用。
    fn lock_runtime(&self) -> std::sync::MutexGuard<'_, Runtime> {
        match self.runtime.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// 路由上下文（只读快照：账本 / 冷却 / lane 健康）。
    fn route_context(&self, cfg: &ConfigDoc, now_ms: i64) -> RouteContext {
        let guard = self.lock_runtime();
        let mut channel_state = BTreeMap::new();
        let mut quota_remaining: BTreeMap<CredentialId, Option<f64>> = BTreeMap::new();
        for channel in &cfg.channels {
            if !quota_remaining.contains_key(&channel.credential) {
                quota_remaining.insert(
                    channel.credential.clone(),
                    self.deps.quota.remaining_ratio(&channel.credential),
                );
            }
            let quota = self.deps.quota.state(&channel.credential);
            let lane = channel.effective_lane(
                cfg.credential(channel.credential.as_str()),
                cfg.provider(channel.provider.as_str()),
                &cfg.net,
            );
            let lane_down = lane
                .and_then(|lane| guard.lane_down_until_ms.get(lane))
                .is_some_and(|until| *until > now_ms);
            channel_state.insert(
                channel.id.clone(),
                ChannelRuntimeState {
                    enabled: channel.enabled && !lane_down,
                    cooling_until_ms: guard.cooling_until_ms.get(&channel.id).copied(),
                    exhausted: quota.is_exhausted(),
                    invalid_credential: false,
                },
            );
        }
        RouteContext {
            now_ms,
            channel_state,
            quota_remaining,
            latency_ms: BTreeMap::new(),
            concurrency: BTreeMap::new(),
            lane_health: guard
                .lane_down_until_ms
                .iter()
                .map(|(lane, until)| (lane.clone(), *until <= now_ms))
                .collect(),
        }
    }

    /// 五级线路解析（策略覆盖 → channel → credential → provider → net → direct）。
    fn lane_for(
        &self,
        cfg: &ConfigDoc,
        channel: &ChannelConfig,
        lane_override: Option<&LaneId>,
    ) -> LaneId {
        resolve_lane(
            lane_override,
            channel,
            cfg.credential(channel.credential.as_str()),
            cfg.provider(channel.provider.as_str()),
            &cfg.net,
        )
        .lane_id()
        .cloned()
        .unwrap_or_else(|| LaneId::new("direct"))
    }

    fn resolved_channel(&self, cfg: &ConfigDoc, id: &ChannelId) -> Result<ChannelConfig> {
        cfg.channel(id.as_str())
            .cloned()
            .ok_or_else(|| TaError::Internal(format!("通道 {id} 不在配置里（路由与配置不一致）")))
    }

    /// 用 G1 的定点改写造一次尝试请求（**唯一**的上游请求构造入口）。
    async fn build_attempt(
        &self,
        cfg: &ConfigDoc,
        snapshot: &RequestSnapshot,
        channel: &ChannelConfig,
        lane_override: Option<&LaneId>,
    ) -> Result<PreparedRequest> {
        let credential = cfg
            .credential(channel.credential.as_str())
            .ok_or_else(|| TaError::Internal(format!("凭据 {} 不存在", channel.credential)))?;
        let provider = cfg
            .provider(channel.provider.as_str())
            .ok_or_else(|| TaError::Internal(format!("服务商 {} 不存在", channel.provider)))?;
        let secret = self.deps.secrets.resolve(&credential.secret).await?;
        let lane = self.lane_for(cfg, channel, lane_override);
        build_prepared_request(PrepareInput {
            cfg,
            policy: self.deps.passthrough.as_ref(),
            snapshot,
            channel,
            provider: Some(provider),
            secret: &secret,
            lane_id: Some(lane),
        })
    }

    /// 429/402/5xx 的处置：402 直接判额度耗尽，其余进冷却。
    fn classify_cooling(
        &self,
        channel: &ChannelConfig,
        status: http::StatusCode,
        cooldown_s: u32,
        now_ms: i64,
    ) {
        if status == http::StatusCode::PAYMENT_REQUIRED {
            self.deps.quota.observe(
                &channel.credential,
                QuotaObservation::StatusCode {
                    status: status.as_u16(),
                    error_code: None,
                    message: Some("上游返回 402".into()),
                },
            );
            self.deps.quota.mark_exhausted(&channel.credential, None);
        }
        self.cool_channel(&channel.id, cooldown_s, now_ms);
    }

    async fn emit(&self, record: AttemptRecord) {
        self.deps.sink.on_attempt(record).await;
    }

    /// 一次尝试的记录器（把"谁/第几次"与"结果"分开，避免一个函数八个参数）。
    fn attempt<'a>(
        &self,
        prepared: &'a PreparedRequest,
        channel: &'a ChannelConfig,
        lane: &'a LaneId,
        attempt: u32,
    ) -> AttemptBuilder<'a> {
        AttemptBuilder { prepared, channel, lane, attempt }
    }

    /// 只观察、不改写：流异常结束时记一次 `TA-3003`（**不重试**）。
    fn watch_stream(&self, template: AttemptRecord, stream: ByteStream) -> ByteStream {
        let sink = self.deps.sink.clone();
        Box::pin(stream.inspect(move |item| {
            if item.is_err() {
                let sink = sink.clone();
                let mut record = template.clone();
                record.error_code = Some(TaError::StreamBroken.code().to_string());
                record.bytes_sent = true;
                // fire-and-forget：流已经断了，不能等 trace 写完再结束响应
                tokio::spawn(async move {
                    sink.on_attempt(record).await;
                });
            }
        }))
    }
}

// ================================================================ hints（步骤 3）

/// 从快照里读路由需要的那几个字段。**只读，不改请求**（`docs/03 §1`）。
fn route_hints(snapshot: &RequestSnapshot) -> RouteHints {
    let hints = snapshot.hints();
    let headers: BTreeMap<String, String> = snapshot
        .headers
        .iter()
        .filter_map(|(name, value)| {
            value.to_str().ok().map(|value| (name.as_str().to_string(), value.to_string()))
        })
        .collect();
    RouteHints {
        model: snapshot.model_hint(),
        protocol: Some(snapshot.endpoint.protocol()),
        has_image: hints.has_image,
        has_tools: body_has_tools(&snapshot.body),
        stream: hints.stream,
        // 估算归 G4 的 TokenMeter；这里不猜（策略里的 input_tokens_lt 因此不命中）
        est_input_tokens: 0,
        client: None,
        ua: headers.get("user-agent").cloned(),
        headers,
        body_hash: None,
        user_id: metadata_user_id(&snapshot.body),
        session_id: None,
        conversation_id: None,
    }
}

fn body_has_tools(body: &[u8]) -> bool {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(body) else {
        return false;
    };
    ["tools", "functions"]
        .iter()
        .any(|key| value.get(*key).and_then(|v| v.as_array()).is_some_and(|a| !a.is_empty()))
}

fn metadata_user_id(body: &[u8]) -> Option<String> {
    let value: serde_json::Value = serde_json::from_slice(body).ok()?;
    value
        .get("metadata")
        .and_then(|m| m.get("user_id"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .or_else(|| value.get("user").and_then(|v| v.as_str()).map(str::to_string))
}

// ================================================================ 管线

#[async_trait]
impl RequestPipeline for Pipeline {
    async fn prepare(&self, snapshot: &RequestSnapshot) -> Result<PreparedRequest> {
        let cfg = self.deps.cfg.load_full();
        let now_ms = self.deps.clock.now_ms();
        let hints = route_hints(snapshot);
        let ctx = self.route_context(&cfg, now_ms);

        // 步骤 4–5：候选集（策略引擎负责标签过滤与淘汰原因）
        let mut decision = self.deps.policy.decide(&hints, &ctx);
        if decision.candidates.is_empty() && decision.on_no_candidate == NoCandidateAction::UseAny {
            // 策略说"没人可用也照发" → 退回该模型的全部启用通道（这是策略语义，不是静默降级）
            decision.candidates = self.all_channels_for(&cfg, &hints);
        }
        if decision.candidates.is_empty() {
            return Err(TaError::NoViableChannel {
                message: format!(
                    "模型 {} 没有可用通道（淘汰 {} 个）",
                    hints.model.as_deref().unwrap_or("(未指定)"),
                    decision.eliminated.len()
                ),
            });
        }

        // 步骤 6–7：选择（粘性优先，由选择器实现）
        let selection = self.deps.selector.select(&decision, &hints, &ctx).ok_or_else(|| {
            TaError::NoViableChannel {
                message: format!("{} 个候选在求值后都不可用", decision.candidates.len()),
            }
        })?;

        // 步骤 8：定点改写（**只走 G1 的实现**）
        let channel = self.resolved_channel(&cfg, &selection.channel)?;
        let mut prepared =
            self.build_attempt(&cfg, snapshot, &channel, decision.lane_override.as_ref()).await?;
        prepared.failover_chain =
            selection.chain.iter().filter(|id| **id != selection.channel).cloned().collect();
        prepared.lane_override = decision.lane_override.clone();
        Ok(prepared)
    }

    async fn execute(&self, prepared: PreparedRequest) -> Result<axum::response::Response> {
        let cfg = self.deps.cfg.load_full();
        let now_ms = self.deps.clock.now_ms();
        let max_attempts = cfg.gateway.retry.attempts_for(prepared.stream).max(1);
        let cooldown_s = cfg.gateway.retry.cooldown_s;

        // 候选顺序：首选 + 失败链（去重，保序）
        let mut chain: Vec<ChannelId> = Vec::new();
        for id in std::iter::once(&prepared.channel_id).chain(prepared.failover_chain.iter()) {
            if !chain.contains(id) {
                chain.push(id.clone());
            }
        }

        let mut last_error: Option<TaError> = None;
        let mut saw_exhausted = false;
        let mut saw_other = false;
        let mut attempt: u32 = 0;

        for channel_id in chain {
            if attempt >= max_attempts {
                break;
            }
            attempt += 1;
            let Ok(channel) = self.resolved_channel(&cfg, &channel_id) else {
                // 配置在 prepare 之后变了：跳过已经不存在的候选
                continue;
            };
            let lane = self.lane_for(&cfg, &channel, prepared.lane_override.as_ref());
            if !self.is_available(&channel_id, Some(&lane), now_ms) {
                continue;
            }
            let attempt_request = match self
                .build_attempt(&cfg, &prepared.snapshot, &channel, prepared.lane_override.as_ref())
                .await
            {
                Ok(request) => request,
                Err(err) => {
                    saw_other = true;
                    last_error = Some(err);
                    continue;
                }
            };

            let request = UpstreamRequest {
                method: prepared.snapshot.method.to_string(),
                url: attempt_request.upstream_url.clone(),
                headers: attempt_request.headers.clone(),
                body: attempt_request.body.clone(),
                stream: prepared.stream,
                total_timeout_s: cfg
                    .provider(channel.provider.as_str())
                    .and_then(|provider| provider.timeout.total_s),
            };

            let started = std::time::Instant::now();
            match self.deps.dialer.execute(&lane, request).await {
                Ok(upstream) => {
                    let latency_ms = started.elapsed().as_millis() as u32;
                    if dispatch::is_failover_status(upstream.status) {
                        // **通道级失败**：冷却这个通道，换下一个候选
                        if upstream.status == http::StatusCode::PAYMENT_REQUIRED {
                            saw_exhausted = true;
                        } else {
                            saw_other = true;
                        }
                        self.classify_cooling(&channel, upstream.status, cooldown_s, now_ms);
                        self.emit(self.attempt(&prepared, &channel, &lane, attempt).record(
                            Some(upstream.status.as_u16()),
                            None,
                            latency_ms,
                            false,
                        ))
                        .await;
                        last_error = Some(TaError::AllChannelsFailed {
                            message: format!("通道 {channel_id} 返回 {}", upstream.status.as_u16()),
                        });
                        continue;
                    }

                    if prepared.stream {
                        // **已经向客户端交出流**：从这里开始任何失败都只记录，绝不重试
                        let record = self.attempt(&prepared, &channel, &lane, attempt).record(
                            Some(upstream.status.as_u16()),
                            None,
                            latency_ms,
                            true,
                        );
                        self.emit(record.clone()).await;
                        let UpstreamResponse { status, headers, body } = upstream;
                        let ctx = meter_context(&prepared, &channel, &lane);
                        let stream = match &self.deps.meter {
                            Some(meter) => meter.observe_stream(ctx, body),
                            None => body,
                        };
                        return Ok(dispatch::client_response_from_parts(
                            status,
                            headers,
                            self.watch_stream(record, stream),
                        ));
                    }

                    // 非流式：body 还没吐给客户端，读失败仍然可以换通道
                    match collect_body(upstream.body).await {
                        Ok(bytes) => {
                            let status = upstream.status;
                            if let Some(meter) = &self.deps.meter {
                                meter.observe_full(
                                    meter_context(&prepared, &channel, &lane),
                                    &bytes,
                                    status.as_u16(),
                                );
                            }
                            self.emit(self.attempt(&prepared, &channel, &lane, attempt).record(
                                Some(status.as_u16()),
                                None,
                                latency_ms,
                                false,
                            ))
                            .await;
                            return Ok(dispatch::client_response_from_bytes(
                                status,
                                upstream.headers,
                                bytes,
                            ));
                        }
                        Err(err) => {
                            saw_other = true;
                            self.emit(self.attempt(&prepared, &channel, &lane, attempt).record(
                                None,
                                Some(err.code()),
                                latency_ms,
                                false,
                            ))
                            .await;
                            last_error = Some(err);
                            continue;
                        }
                    }
                }
                Err(err) => {
                    let latency_ms = started.elapsed().as_millis() as u32;
                    match &err {
                        // **lane 级失败**：只标记这条 lane（其它 lane 不受牵连）
                        TaError::LaneUnavailable { .. } => self.mark_lane_down(&lane, now_ms),
                        _ => {
                            saw_other = true;
                            self.cool_channel(&channel_id, cooldown_s, now_ms);
                        }
                    }
                    self.emit(self.attempt(&prepared, &channel, &lane, attempt).record(
                        None,
                        Some(err.code()),
                        latency_ms,
                        false,
                    ))
                    .await;
                    last_error = Some(err);
                    continue;
                }
            }
        }

        // 全部候选都失败：额度全耗尽与"都挂了"是两种不同的对外语义
        if !saw_other && saw_exhausted {
            return Err(TaError::AllChannelsExhausted);
        }
        Err(last_error.unwrap_or(TaError::NoViableChannel {
            message: "所有候选通道都不可用".into(),
        }))
    }
}

impl Pipeline {
    /// `UseAny` 兜底：该逻辑模型（或通配路径给的模型名）下的全部启用通道。
    fn all_channels_for(&self, cfg: &ConfigDoc, hints: &RouteHints) -> Vec<ChannelId> {
        let wanted = hints.model.as_deref().and_then(|name| cfg.find_model(name).cloned());
        cfg.channels
            .iter()
            .filter(|channel| {
                channel.enabled
                    && match &wanted {
                        Some(model) => model.matches(&channel.model),
                        None => true,
                    }
            })
            .map(|channel| channel.id.clone())
            .collect()
    }
}

fn meter_context(
    prepared: &PreparedRequest,
    channel: &ChannelConfig,
    lane: &LaneId,
) -> MeterContext {
    MeterContext {
        request_id: prepared.request_id.clone(),
        channel_id: channel.id.clone(),
        credential_id: channel.credential.clone(),
        model: prepared.model.clone(),
        upstream_model: prepared.upstream_model.clone(),
        lane_id: Some(lane.clone()),
        protocol: prepared.protocol,
        stream: prepared.stream,
    }
}

/// 非流式响应体：读完（内存在这里被一次性占用，但非流式响应本来就要完整交给客户端）。
async fn collect_body(mut stream: ByteStream) -> Result<Bytes> {
    let mut buf = Vec::new();
    while let Some(item) = stream.next().await {
        match item {
            Ok(chunk) => buf.extend_from_slice(&chunk),
            Err(err) => {
                return Err(TaError::AllChannelsFailed {
                    message: format!("读取上游响应体失败：{err}"),
                })
            }
        }
    }
    Ok(Bytes::from(buf))
}

// ================================================================ 测试

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::{byte_stream_from_bytes, HealthReport};
    use crate::passthrough::FullPassthrough;
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicU32, Ordering};
    use ta_core::clock::ManualClock;
    use ta_core::domain::{Credential, Model, Provider, Selector};
    use ta_core::quota::{QuotaState, QuotaStateKind};
    use ta_core::routing::{RouteDecision, Selection};

    const MODEL: &str = "claude-sonnet-4-5";
    const SECRET: &str = "sk-relay-secret";

    // ---------------------------------------------------------- mocks

    struct FixedPolicy {
        candidates: Vec<&'static str>,
        on_no_candidate: NoCandidateAction,
    }

    impl PolicyEngine for FixedPolicy {
        fn decide(&self, _hints: &RouteHints, _ctx: &RouteContext) -> RouteDecision {
            RouteDecision {
                candidates: self.candidates.iter().map(|id| ChannelId::new(*id)).collect(),
                selector: Selector::Priority,
                on_no_candidate: self.on_no_candidate,
                ..Default::default()
            }
        }
    }

    struct ChainSelector;

    impl ChannelSelector for ChainSelector {
        fn select(
            &self,
            decision: &RouteDecision,
            _hints: &RouteHints,
            _ctx: &RouteContext,
        ) -> Option<Selection> {
            Some(Selection {
                channel: decision.candidates.first()?.clone(),
                sticky_hit: false,
                sticky_key: None,
                chain: decision.candidates.clone(),
            })
        }
    }

    enum Scripted {
        Ok { status: u16, headers: Vec<(&'static str, &'static str)>, body: &'static str },
        LaneDown,
        Transport(String),
    }

    #[derive(Default)]
    struct ScriptedDialer {
        queue: Mutex<VecDeque<Scripted>>,
        calls: AtomicU32,
        seen_urls: Mutex<Vec<String>>,
    }

    impl ScriptedDialer {
        fn with(script: Vec<Scripted>) -> Self {
            Self { queue: Mutex::new(script.into()), ..Default::default() }
        }
        fn calls(&self) -> u32 {
            self.calls.load(Ordering::SeqCst)
        }
        fn seen_urls(&self) -> Vec<String> {
            match self.seen_urls.lock() {
                Ok(guard) => guard.clone(),
                Err(poisoned) => poisoned.into_inner().clone(),
            }
        }
    }

    #[async_trait]
    impl OutboundDialer for ScriptedDialer {
        async fn execute(&self, lane: &LaneId, req: UpstreamRequest) -> Result<UpstreamResponse> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            match self.seen_urls.lock() {
                Ok(mut guard) => guard.push(format!("{}|{}", lane, req.url)),
                Err(poisoned) => poisoned.into_inner().push(format!("{}|{}", lane, req.url)),
            }
            let next = match self.queue.lock() {
                Ok(mut guard) => guard.pop_front(),
                Err(poisoned) => poisoned.into_inner().pop_front(),
            };
            match next {
                Some(Scripted::Ok { status, headers, body }) => {
                    let mut map = http::HeaderMap::new();
                    for (name, value) in headers {
                        map.insert(name, value.parse().unwrap());
                    }
                    Ok(UpstreamResponse {
                        status: http::StatusCode::from_u16(status).unwrap(),
                        headers: map,
                        body: byte_stream_from_bytes(Bytes::from_static(body.as_bytes())),
                    })
                }
                Some(Scripted::LaneDown) => {
                    Err(TaError::LaneUnavailable { lane: lane.to_string() })
                }
                Some(Scripted::Transport(message)) => Err(TaError::AllChannelsFailed { message }),
                None => Ok(UpstreamResponse {
                    status: http::StatusCode::OK,
                    headers: http::HeaderMap::new(),
                    body: byte_stream_from_bytes(Bytes::from_static(b"{}")),
                }),
            }
        }

        async fn health(&self, lane: &LaneId) -> HealthReport {
            HealthReport { lane_id: lane.clone(), ok: true, latency_ms: Some(1), error: None }
        }
    }

    #[derive(Default)]
    struct RecordingSink {
        records: Mutex<Vec<AttemptRecord>>,
    }

    #[async_trait]
    impl AttemptSink for RecordingSink {
        async fn on_attempt(&self, record: AttemptRecord) {
            match self.records.lock() {
                Ok(mut guard) => guard.push(record),
                Err(poisoned) => poisoned.into_inner().push(record),
            }
        }
    }

    struct NoQuota;

    impl QuotaLedger for NoQuota {
        fn state(&self, cred: &CredentialId) -> QuotaState {
            QuotaState {
                credential_id: cred.clone(),
                kind: QuotaStateKind::Unknown,
                ..Default::default()
            }
        }
        fn observe(&self, _cred: &CredentialId, _obs: QuotaObservation) {}
        fn mark_exhausted(&self, _cred: &CredentialId, _hint: Option<i64>) {}
        fn mark_invalid(&self, _cred: &CredentialId) {}
        fn roll_window(&self, _now_ms: i64) -> Vec<CredentialId> {
            vec![]
        }
        fn remaining_ratio(&self, _cred: &CredentialId) -> Option<f64> {
            None
        }
    }

    // ---------------------------------------------------------- 夹具

    /// 两个通道：ch-a 走 lane-jp，ch-b 走 lane-tokyo（base_url 便于区分）。
    fn test_cfg(provider_a: &str, provider_b: &str) -> ConfigDoc {
        let mut doc = ConfigDoc::default();
        doc.providers.push(Provider::new("p-a", "A", Protocol::Openai, provider_a));
        doc.providers.push(Provider::new("p-b", "B", Protocol::Openai, provider_b));
        doc.credentials.push(Credential::new("c-a", "p-a", "A", "${secret:a}"));
        doc.credentials.push(Credential::new("c-b", "p-b", "B", "${secret:b}"));
        doc.models.push(Model::new(MODEL));
        doc.channels.push(
            ChannelConfig::new("ch-a", MODEL, "p-a", "c-a", "upstream-a").with_lane("lane-jp"),
        );
        doc.channels.push(
            ChannelConfig::new("ch-b", MODEL, "p-b", "c-b", "upstream-b").with_lane("lane-tokyo"),
        );
        doc
    }

    fn snapshot(body: &'static [u8]) -> RequestSnapshot {
        let mut headers = http::HeaderMap::new();
        headers.insert("content-type", "application/json".parse().unwrap());
        headers.insert("user-agent", "claude-cli/1.0.60".parse().unwrap());
        headers.insert("x-session-id", "sess-1".parse().unwrap());
        RequestSnapshot {
            request_id: "req_test".into(),
            method: http::Method::POST,
            path: "/v1/messages".into(),
            query: None,
            endpoint: crate::ingress::Endpoint::Messages,
            headers,
            body: Bytes::from_static(body),
            received_at_ms: 1_700_000_000_000,
        }
    }

    fn pipeline(
        cfg: ConfigDoc,
        policy: Arc<dyn PolicyEngine>,
        dialer: Arc<dyn OutboundDialer>,
        sink: Arc<RecordingSink>,
    ) -> Pipeline {
        Pipeline::new(Arc::new(PipelineDeps {
            cfg: Arc::new(arc_swap::ArcSwap::from_pointee(cfg)),
            clock: Arc::new(ManualClock::new(1_700_000_000_000)),
            policy,
            selector: Arc::new(ChainSelector),
            quota: Arc::new(NoQuota),
            secrets: Arc::new(StaticSecrets::new([
                ("${secret:a}", SECRET),
                ("${secret:b}", SECRET),
            ])),
            dialer,
            passthrough: FullPassthrough::shared(),
            sink,
            meter: None,
        }))
    }

    async fn body_string(response: axum::response::Response) -> String {
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024).await.expect("body");
        String::from_utf8_lossy(&bytes).into_owned()
    }

    /// 容忍"上游半路断连"的读法：能读到多少算多少（这正是客户端真实体验）。
    async fn body_string_lenient(response: axum::response::Response) -> String {
        use http_body_util::BodyExt;
        let mut response = response;
        let mut received = Vec::new();
        while let Some(frame) = response.body_mut().frame().await {
            let Ok(frame) = frame else { break };
            if let Ok(data) = frame.into_data() {
                received.extend_from_slice(&data);
            }
        }
        String::from_utf8_lossy(&received).into_owned()
    }

    // ---------------------------------------------------------- 步骤 3–8

    #[tokio::test]
    async fn prepare_uses_policy_selector_and_g1_rewrite() {
        let sink = Arc::new(RecordingSink::default());
        let pipeline = pipeline(
            test_cfg("https://a.example/v1", "https://b.example/v1"),
            Arc::new(FixedPolicy {
                candidates: vec!["ch-a", "ch-b"],
                on_no_candidate: NoCandidateAction::Error,
            }),
            Arc::new(ScriptedDialer::default()),
            sink,
        );
        let body = br#"{"model":"claude-sonnet-4-5","messages":[{"role":"user","content":"hi"}]}"#;
        let prepared = pipeline.prepare(&snapshot(body)).await.expect("应当能准备");

        assert_eq!(prepared.channel_id.as_str(), "ch-a");
        assert_eq!(prepared.upstream_model, "upstream-a");
        assert_eq!(prepared.upstream_url, "https://a.example/v1/messages");
        assert_eq!(prepared.lane_id.as_ref().map(|l| l.as_str()), Some("lane-jp"));
        assert_eq!(prepared.failover_chain, vec![ChannelId::new("ch-b")]);
        // 模型名被映射，其余字段原样
        let patched: serde_json::Value = serde_json::from_slice(&prepared.body).unwrap();
        assert_eq!(patched["model"], "upstream-a");
        assert_eq!(patched["messages"][0]["content"], "hi");
        // 凭据被写进鉴权头，且自检为空
        assert_eq!(
            prepared.headers.get("authorization").unwrap().to_str().unwrap(),
            format!("Bearer {SECRET}")
        );
        assert!(prepared.header_diff.is_empty(), "{:?}", prepared.header_diff);
        // **content-length 必须等于实际字节数**（主管裁决 #1 要求的断言）
        assert_eq!(
            prepared.headers.get("content-length").unwrap(),
            prepared.body.len().to_string().as_str()
        );
    }

    #[tokio::test]
    async fn no_candidate_is_reported_as_no_viable_channel() {
        let pipeline = pipeline(
            test_cfg("https://a.example/v1", "https://b.example/v1"),
            Arc::new(FixedPolicy { candidates: vec![], on_no_candidate: NoCandidateAction::Error }),
            Arc::new(ScriptedDialer::default()),
            Arc::new(RecordingSink::default()),
        );
        let err = pipeline
            .prepare(&snapshot(br#"{"model":"claude-sonnet-4-5"}"#))
            .await
            .expect_err("没有候选必须报错");
        assert_eq!(err.code(), "TA-2003");
        assert_eq!(err.http_status(), 415);
    }

    // ---------------------------------------------------------- 失败转移

    #[tokio::test]
    async fn rate_limited_channel_fails_over_to_the_next_one() {
        let sink = Arc::new(RecordingSink::default());
        let dialer = Arc::new(ScriptedDialer::with(vec![
            Scripted::Ok { status: 429, headers: vec![("retry-after", "9")], body: "{}" },
            Scripted::Ok {
                status: 200,
                headers: vec![("content-type", "application/json")],
                body: r#"{"ok":true}"#,
            },
        ]));
        let pipeline = pipeline(
            test_cfg("https://a.example/v1", "https://b.example/v1"),
            Arc::new(FixedPolicy {
                candidates: vec!["ch-a", "ch-b"],
                on_no_candidate: NoCandidateAction::Error,
            }),
            dialer.clone(),
            sink.clone(),
        );
        let prepared =
            pipeline.prepare(&snapshot(br#"{"model":"claude-sonnet-4-5"}"#)).await.unwrap();
        let response = pipeline.execute(prepared).await.expect("第二个通道应当成功");
        assert_eq!(response.status(), http::StatusCode::OK);
        assert_eq!(body_string(response).await, r#"{"ok":true}"#);

        // 两次尝试都记了（第一次 429），且第二次确实换了通道（URL 变了）
        assert_eq!(dialer.calls(), 2);
        let urls = dialer.seen_urls();
        assert!(urls[0].contains("a.example"), "{urls:?}");
        assert!(urls[1].contains("b.example"), "{urls:?}");
        let records = sink.records.lock().unwrap().clone();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].status, Some(429));
        assert_eq!(records[0].channel_id.as_str(), "ch-a");
        assert_eq!(records[1].status, Some(200));
        assert_eq!(records[1].channel_id.as_str(), "ch-b");

        // 冷却生效：再来一次请求时 ch-a 直接被跳过
        let prepared =
            pipeline.prepare(&snapshot(br#"{"model":"claude-sonnet-4-5"}"#)).await.unwrap();
        let _ = pipeline.execute(prepared).await.unwrap();
        assert_eq!(dialer.calls(), 3, "ch-a 已在冷却期，不该被再打一次");
        assert!(dialer.seen_urls()[2].contains("b.example"));
    }

    #[tokio::test]
    async fn lane_failure_marks_only_that_lane_and_tries_the_next_candidate() {
        let sink = Arc::new(RecordingSink::default());
        let dialer = Arc::new(ScriptedDialer::with(vec![
            Scripted::LaneDown,
            Scripted::Ok { status: 200, headers: vec![], body: r#"{"ok":true}"# },
        ]));
        let pipeline = pipeline(
            test_cfg("https://a.example/v1", "https://b.example/v1"),
            Arc::new(FixedPolicy {
                candidates: vec!["ch-a", "ch-b"],
                on_no_candidate: NoCandidateAction::Error,
            }),
            dialer.clone(),
            sink.clone(),
        );
        let prepared =
            pipeline.prepare(&snapshot(br#"{"model":"claude-sonnet-4-5"}"#)).await.unwrap();
        let response = pipeline.execute(prepared).await.expect("换 lane 后应当成功");
        assert_eq!(response.status(), http::StatusCode::OK);

        let urls = dialer.seen_urls();
        assert!(urls[0].starts_with("lane-jp|"), "{urls:?}");
        assert!(urls[1].starts_with("lane-tokyo|"), "{urls:?}");
        // lane 级失败只标记那条 lane，而且**是短暂的**：内核热重载会让 listener
        // 一瞬间关闭重建（M0 实测），永久拉黑会让用户"改一次线路就永久少一条线"。
        let now = 1_700_000_000_000;
        assert!(!pipeline.is_available(
            &ChannelId::new("ch-a"),
            Some(&LaneId::new("lane-jp")),
            now
        ));
        assert!(
            pipeline.is_available(
                &ChannelId::new("ch-a"),
                Some(&LaneId::new("lane-jp")),
                now + LANE_RECOVERY_MS
            ),
            "恢复窗口过后必须重新可用，否则改线路 = 永久失去这条线"
        );
        assert!(pipeline.is_available(
            &ChannelId::new("ch-a"),
            Some(&LaneId::new("lane-other")),
            now
        ));
        let records = sink.records.lock().unwrap().clone();
        assert_eq!(records[0].error_code.as_deref(), Some("TA-3002"));
        assert_eq!(records[0].lane_id.as_ref().map(|l| l.as_str()), Some("lane-jp"));
    }

    #[tokio::test]
    async fn non_failover_status_is_passed_through_without_retry() {
        let sink = Arc::new(RecordingSink::default());
        let dialer = Arc::new(ScriptedDialer::with(vec![Scripted::Ok {
            status: 400,
            headers: vec![("content-type", "application/json"), ("x-upstream-note", "keep")],
            body: r#"{"error":{"message":"bad request"}}"#,
        }]));
        let pipeline = pipeline(
            test_cfg("https://a.example/v1", "https://b.example/v1"),
            Arc::new(FixedPolicy {
                candidates: vec!["ch-a", "ch-b"],
                on_no_candidate: NoCandidateAction::Error,
            }),
            dialer.clone(),
            sink.clone(),
        );
        let prepared =
            pipeline.prepare(&snapshot(br#"{"model":"claude-sonnet-4-5"}"#)).await.unwrap();
        let response = pipeline.execute(prepared).await.unwrap();
        assert_eq!(response.status(), http::StatusCode::BAD_REQUEST);
        assert_eq!(response.headers().get("x-upstream-note").unwrap(), "keep");
        assert_eq!(body_string(response).await, r#"{"error":{"message":"bad request"}}"#);
        assert_eq!(dialer.calls(), 1, "4xx 是请求本身的问题，换通道只会重演");
        assert_eq!(sink.records.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn exhausted_channels_report_2004_not_2005() {
        let dialer = Arc::new(ScriptedDialer::with(vec![
            Scripted::Ok { status: 402, headers: vec![], body: "{}" },
            Scripted::Ok { status: 402, headers: vec![], body: "{}" },
        ]));
        let pipeline = pipeline(
            test_cfg("https://a.example/v1", "https://b.example/v1"),
            Arc::new(FixedPolicy {
                candidates: vec!["ch-a", "ch-b"],
                on_no_candidate: NoCandidateAction::Error,
            }),
            dialer,
            Arc::new(RecordingSink::default()),
        );
        let prepared =
            pipeline.prepare(&snapshot(br#"{"model":"claude-sonnet-4-5"}"#)).await.unwrap();
        let err = pipeline.execute(prepared).await.expect_err("全部 402 必须报额度耗尽");
        assert_eq!(err.code(), "TA-2004");
        assert_eq!(err.http_status(), 402);
    }

    #[tokio::test]
    async fn transport_error_cools_the_channel_and_fails_over() {
        // 非 lane 的传输层失败（超时/协议）→ **冷却通道**（而不是只标记 lane）
        let dialer = Arc::new(ScriptedDialer::with(vec![
            Scripted::Transport("read timeout".into()),
            Scripted::Ok { status: 200, headers: vec![], body: r#"{"ok":true}"# },
        ]));
        let pipeline = pipeline(
            test_cfg("https://a.example/v1", "https://b.example/v1"),
            Arc::new(FixedPolicy {
                candidates: vec!["ch-a", "ch-b"],
                on_no_candidate: NoCandidateAction::Error,
            }),
            dialer.clone(),
            Arc::new(RecordingSink::default()),
        );
        let prepared =
            pipeline.prepare(&snapshot(br#"{"model":"claude-sonnet-4-5"}"#)).await.unwrap();
        let response = pipeline.execute(prepared).await.expect("第二个通道应当成功");
        assert_eq!(response.status(), http::StatusCode::OK);
        assert_eq!(dialer.calls(), 2);

        let now = 1_700_000_000_000;
        assert!(
            !pipeline.is_available(&ChannelId::new("ch-a"), Some(&LaneId::new("lane-jp")), now),
            "传输失败必须冷却通道，否则下一次还会打到同一个坏通道"
        );
    }

    // ---------------------------------------------------------- DoD ②：断流不重试（管线级）

    #[tokio::test]
    async fn streaming_abort_after_three_chunks_makes_exactly_one_upstream_request() {
        use crate::dispatch::tests::spawn_fake_upstream;
        use crate::dispatch::{DialerOptions, KernelDialer, StaticLanePorts};

        let upstream = spawn_fake_upstream(|_index, _head, mut write| async move {
            use tokio::io::AsyncWriteExt;
            let _ = write
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n")
                .await;
            for i in 1..=3 {
                let chunk = format!("data: {{\"i\":{i}}}\n\n");
                let frame = format!("{:x}\r\n{}\r\n", chunk.len(), chunk);
                let _ = write.write_all(frame.as_bytes()).await;
                let _ = write.flush().await;
            }
            // 半路掐断（不发终止块）
        })
        .await;

        // 通道指向假上游；lane = direct（不走代理）
        let mut cfg = test_cfg(&upstream.url("/v1/messages").await, "https://b.example/v1");
        cfg.channels[0].lane = Some(LaneId::new("direct"));
        let sink = Arc::new(RecordingSink::default());
        let dialer = Arc::new(KernelDialer::new(
            Arc::new(StaticLanePorts::default()),
            DialerOptions::default(),
        ));
        let pipeline = pipeline(
            cfg,
            Arc::new(FixedPolicy {
                candidates: vec!["ch-a"],
                on_no_candidate: NoCandidateAction::Error,
            }),
            dialer,
            sink,
        );
        let prepared = pipeline
            .prepare(&snapshot(br#"{"model":"claude-sonnet-4-5","stream":true}"#))
            .await
            .unwrap();
        assert!(prepared.stream);
        let response = pipeline.execute(prepared).await.expect("首字节已经到手");
        assert_eq!(response.status(), http::StatusCode::OK);

        // 客户端仍然能读到 3 个 chunk（之后流才断）
        let text = body_string_lenient(response).await;
        for i in 1..=3 {
            assert!(text.contains(&format!("data: {{\"i\":{i}}}")), "第 {i} 个 chunk 丢了：{text}");
        }
        // **只有 1 次上游请求**（DoD 原文：输出 3 个 chunk 后断连 → 只有 1 次上游请求）
        assert_eq!(upstream.connection_count(), 1, "已输出字节后绝不允许重试");
    }
}
