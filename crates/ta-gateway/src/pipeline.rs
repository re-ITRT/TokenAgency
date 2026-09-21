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

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use bytes::Bytes;
use futures_util::StreamExt;

use ta_core::clock::Clock;
use ta_core::domain::behavior::resolve_lane;
use ta_core::domain::{ChannelConfig, ConfigDoc, NoCandidateAction};
use ta_core::error::{Result, TaError};
use ta_core::quota::{QuotaLedger, QuotaObservation};
use ta_core::routing::{ChannelRuntimeState, EliminateReason, RouteContext, RouteHints};
use ta_core::types::{ChannelId, CredentialId, LaneId, Protocol};

use crate::dispatch::{self, ByteStream, OutboundDialer, UpstreamRequest, UpstreamResponse};
use crate::ingress::{
    build_prepared_request, PrepareInput, PreparedRequest, RequestPipeline, RequestSnapshot,
};
use crate::passthrough::{HeaderDiff, PassthroughPolicy};
use crate::runtime::RoutingPlanner;

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

    /// **请求结束（每次请求恰好一次）**。默认空实现：只想看尝试的 sink 不必实现它。
    ///
    /// 为什么需要它：一条 trace 是**请求级**的（`TraceRecord` 里有尝试链、总耗时、
    /// `header_diff`），而 `on_attempt` 只知道单次尝试。没有这个钩子，sink 就只能靠
    /// "攒着等超时"或"猜哪次是最后一次"，两种都会在慢请求上出错。
    async fn on_request_complete(&self, _summary: RequestTraceSummary) {}
}

/// 请求级 trace 的输入（由管线组装，sink 负责落库）。
///
/// 字段按 `ta_store::TraceRecord` 的需要挑 —— 这里**不引入 ta-store 的类型**，
/// 是为了让 sink 接口保持窄（测试里可以用假 sink；换存储也不用改管线）。
#[derive(Debug, Clone)]
pub struct RequestTraceSummary {
    pub request_id: String,
    pub ts_ms: i64,
    /// 客户端发来的逻辑模型名
    pub model: Option<String>,
    pub upstream_model: String,
    pub protocol_in: Protocol,
    pub protocol_out: Protocol,
    pub stream: bool,
    /// 最终答复客户端的那个通道（失败链走完时是"最后一个说话的上游"）
    pub chosen_channel: Option<ChannelId>,
    pub lane: Option<LaneId>,
    /// 自检：**正常恒为空**，非空说明网关自己吞/加了头（[docs/03 §7.4]）
    pub header_diff: Option<HeaderDiff>,
    /// 客户端最终看到的 HTTP 状态码（上游透传的就是上游的码）
    pub status: u16,
    pub total_ms: u32,
    /// 首字节耗时（流式就是拿到上游响应头的时间）
    pub ttfb_ms: Option<u32>,
    /// 网关自身错误的码 + 人话（上游透传的错误**不进这里** —— 那是上游的话）
    pub error: Option<String>,
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
    /// 路由计划器：**一次绑定、一次求值**。
    ///
    /// 这里刻意**不是** `policy` + `selector` 两个 trait 对象：分开调用的两次之间，
    /// 配置可能换代（[`crate::runtime::LiveRouter`] 每次调用都会跟代），
    /// 于是"候选集来自第 N 代、选择在第 N+1 代求值" —— 换配置时偶发
    /// 「无可用通道」。计划器把 decide/select 关进一次调用，并回传那一版配置。
    pub planner: Arc<dyn RoutingPlanner>,
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
    /// 通道 → **连续**失败次数（退避阶梯用）。成功一次清零（[docs/03 §5.3]）。
    channel_failures: BTreeMap<ChannelId, u32>,
    /// lane → 不可用**截止时间**（ms）。不是永久拉黑，见 [`LANE_RECOVERY_MS`]。
    lane_down_until_ms: BTreeMap<LaneId, i64>,
    /// lane → **连续**连接失败次数。成功一次清零（[docs/03 §5.3]）。
    lane_failures: BTreeMap<LaneId, u32>,
}

/// 冷却退避的上限（[docs/03 §5.3]：1 小时）。
const COOLDOWN_CAP_S: u32 = 3_600;

/// 第 `failures` 次连续失败后该冷却多久（[docs/03 §5.3] 的指数退避）。
///
/// 文档给的阶梯是 **`120s → 300s → 900s`，上限 1h**。这里把它写成"相对配置的
/// `cooldown_s`"：base=120 时逐级得到 120 / 300 / 900（正是文档那串），再往上每级 ×3
/// （900 → 2700 → 8100），撞到 1h 上限为止。
///
/// **为什么不写成 `base * 2^n`**：那样 base=120 时第二级是 240 而不是文档的 300；
/// 文档给的是三个具体值，照抄这三个值比"看起来更数学"更重要 —— 用户与 UI 是照着
/// 那几个秒数建立预期的。
fn cooldown_seconds(base_s: u32, failures: u32) -> u32 {
    let base = base_s.max(1) as f64;
    let raw = match failures.max(1) {
        1 => base,
        2 => base * 2.5,
        n => base * 7.5 * 3f64.powi((n - 3) as i32),
    };
    (raw.min(COOLDOWN_CAP_S as f64).max(1.0)) as u32
}

/// 连续失败多少次才把一条 lane 判为降级（[docs/03 §5.3]：连续 3 次）。
///
/// **不能一次失败就降级**：内核热重载（K1）时 listener 有一瞬的关闭/重建，
/// 一次 `connection refused` 就拉黑 5 秒，等于用户每改一次线路就多一次失败 ——
/// 而且下一批请求会因为在窗口内被淘汰而拿到「无可用通道」，连一次尝试都没有。
const LANE_DEGRADE_AFTER: u32 = 3;

/// lane 被判降级后的恢复窗口。
///
/// **为什么必须"短暂"而不是永久**：M0 真机实测（`docs/06 §1.6`）—— 内核热重载
/// （`PUT /configs?force=true`）时 listener 会经历一瞬间的关闭/重建，此时会出现
/// `connection refused`。若把它当成"这条 lane 坏了"永久拉黑，用户每改一次线路
/// 就会永久失去那条线（直到重启网关）。
///
/// 窗口从**本次请求开始时刻**起算（`execute` 只取一次 `now_ms`）：一次请求内不会因为
/// 自己的重试把窗口推后。跨请求看是精确的。
const LANE_RECOVERY_MS: i64 = 5_000;

/// 候选**全部试过之后**再重试一次连接失败候选的间隔。
///
/// 只为跨过内核 listener 重建那一瞬（K1），**不是通用退避**：
/// 通用退避属于通道级冷却（`cooldown_s`），两者混在一起会让"重试"变成"重打同一个
/// 已耗尽的套餐"。
///
/// ⚠️ **这个 50ms 是估的，没有实测**（[docs/13 L19] 的同一条纪律：别把估计写成事实）。
/// K1 实测的是"reload 是进程内重载、PID 不变、可逆"，**没有量过 listener 关闭→重建
/// 持续多久**。若真实时长更大，单候选配置在这一瞬仍可能打不通 ——
/// 那会表现为"改线路时偶发一次失败"，正是 K1 要消灭的形态。
/// 要收紧它，得在真内核上量：循环发请求 + `PUT /configs?force=true`，
/// 记录被拒的持续时间，再把这里换成实测值（并写进 commit 证据）。
const LANE_RELOAD_RETRY_DELAY_MS: u64 = 50;

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

    /// 通道级失败（429/402/5xx/传输失败）：冷却该通道，**按连续失败次数退避**
    /// （[docs/03 §5.3] 的 120s → 300s → 900s → … → 上限 1h）。
    fn cool_channel(&self, channel_id: &ChannelId, cooldown_s: u32, now_ms: i64) {
        let mut guard = self.lock_runtime();
        let failures = {
            let count = guard.channel_failures.entry(channel_id.clone()).or_insert(0);
            *count = count.saturating_add(1);
            *count
        };
        let seconds = cooldown_seconds(cooldown_s, failures);
        guard.cooling_until_ms.insert(channel_id.clone(), now_ms + (seconds as i64) * 1000);
    }

    /// 通道级成功：清掉连续失败计数并立即解除冷却
    /// （[docs/03 §5.3]「成功一次 → 冷却计时器清零」）。
    ///
    /// 不这么做的话，"偶发 429 的通道"会被自己的历史一次次推得更远 ——
    /// 那正是 [docs/03 §6.2] 说的"自动学习降级比不降级更糟"。
    fn record_channel_success(&self, channel_id: &ChannelId) {
        let mut guard = self.lock_runtime();
        guard.channel_failures.remove(channel_id);
        guard.cooling_until_ms.remove(channel_id);
    }

    /// lane 级失败：记一次**连续**失败；到阈值才降级（[docs/03 §5.3]）。
    ///
    /// 阈值判断与降级写在同一个锁里：分两次加锁会让并发的成功/失败互相覆盖计数。
    /// 注意 `entry(..).or_insert(..)` 返回的 `&mut` 必须在动 `guard` 的其它字段**之前**
    /// 结束借用（NLL），所以计数先落到局部变量 `count`。
    fn record_lane_failure(&self, lane: &LaneId, now_ms: i64) {
        let mut guard = self.lock_runtime();
        let count = {
            let failures = guard.lane_failures.entry(lane.clone()).or_insert(0);
            *failures = failures.saturating_add(1);
            *failures
        };
        if count >= LANE_DEGRADE_AFTER {
            // 降级后重新计数：窗口过去再连续失败 3 次才再次降级
            guard.lane_failures.insert(lane.clone(), 0);
            guard.lane_down_until_ms.insert(lane.clone(), now_ms + LANE_RECOVERY_MS);
        }
    }

    /// lane 级成功：成功一次就把连续失败计数清零，并立即解除降级
    /// （[docs/03 §5.3]「成功一次 → 冷却计时器清零」）。
    fn record_lane_success(&self, lane: &LaneId) {
        let mut guard = self.lock_runtime();
        guard.lane_failures.remove(lane);
        guard.lane_down_until_ms.remove(lane);
    }

    /// 这条 lane 现在是否处于降级窗口内。
    fn lane_is_degraded(&self, lane: &LaneId, now_ms: i64) -> bool {
        self.lock_runtime().lane_down_until_ms.get(lane).is_some_and(|until| *until > now_ms)
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

    /// 请求级 trace 的**唯一出口**：所有 return 之前都经这里，保证"每个请求恰好一条"。
    #[allow(clippy::too_many_arguments)]
    async fn emit_request_trace(
        &self,
        prepared: &PreparedRequest,
        status: u16,
        error: Option<String>,
        chosen_channel: Option<&ChannelId>,
        lane: Option<&LaneId>,
        ttfb_ms: Option<u32>,
        total_ms: u32,
        now_ms: i64,
    ) {
        self.deps
            .sink
            .on_request_complete(RequestTraceSummary {
                request_id: prepared.request_id.clone(),
                ts_ms: now_ms,
                model: prepared.model.clone(),
                upstream_model: prepared.upstream_model.clone(),
                protocol_in: prepared.client_protocol,
                protocol_out: prepared.protocol,
                stream: prepared.stream,
                chosen_channel: chosen_channel.cloned(),
                lane: lane.cloned(),
                // 自检信息一定要落库：非空就说明网关自己动了头（[docs/03 §7.4]）
                header_diff: Some(prepared.header_diff.clone()),
                status,
                total_ms,
                ttfb_ms,
                error,
            })
            .await;
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
        let now_ms = self.deps.clock.now_ms();
        let hints = route_hints(snapshot);
        // 运行态快照用的配置（冷却/lane 健康/额度）先取一份；**它只影响 overlay**，
        // 缺项等于"没有运行态限制"，所以即使它与下面路由用的那版不同代也无害。
        let ctx = self.route_context(&self.deps.planner.current_doc(), now_ms);

        // 步骤 4–7：**一次绑定**求值 —— `plan.cfg` 就是 decision/selection 依据的那一版，
        // 后面解析通道、算 lane、造请求全部用它，不再自己 load（否则就回到"一次请求混代"）。
        let plan = self.deps.planner.plan(&hints, &ctx);
        let cfg = plan.cfg;
        let mut decision = plan.decision;
        if decision.candidates.is_empty() && decision.on_no_candidate == NoCandidateAction::UseAny {
            // 策略说"没人可用也照发" → 退回该模型的全部启用通道（这是策略语义，不是静默降级）
            decision.candidates = self.all_channels_for(&cfg, &hints);
        }
        if decision.candidates.is_empty() {
            // [docs/11 §5] 的两分法在这里也适用：**本地账本**判定"候选全都耗尽了"时，
            // 我们**根本没有问过上游** → 没有"上游说的话"可以透传 → 由我们自己开口，
            // 而且要说清是**额度**问题（402 / TA-2004），不是含糊的"无可用通道"（415 / TA-2003）。
            // 判据是**全部淘汰原因都是 `QuotaExhausted`**：只要还夹着别的理由（标签排除、
            // 冷却、lane 降级…），那就不是"额度问题"，说成 402 会误导用户去充值。
            let all_quota_exhausted = !decision.eliminated.is_empty()
                && decision
                    .eliminated
                    .iter()
                    .all(|(_, reason)| *reason == EliminateReason::QuotaExhausted);
            if all_quota_exhausted {
                return Err(TaError::AllChannelsExhausted);
            }
            return Err(TaError::NoViableChannel {
                message: format!(
                    "模型 {} 没有可用通道（淘汰 {} 个）",
                    hints.model.as_deref().unwrap_or("(未指定)"),
                    decision.eliminated.len()
                ),
            });
        }

        // 步骤 6–7：选择（粘性优先，由计划器内的选择器实现）
        let selection = plan.selection.ok_or_else(|| TaError::NoViableChannel {
            message: format!("{} 个候选在求值后都不可用", decision.candidates.len()),
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
        // 请求级 trace 的总耗时从这里起算（含失败链上的所有尝试）
        let started_at = std::time::Instant::now();
        let cfg = self.deps.cfg.load_full();
        let now_ms = self.deps.clock.now_ms();
        let max_attempts = cfg.gateway.retry.attempts_for(prepared.stream).max(1);
        let cooldown_s = cfg.gateway.retry.cooldown_s;

        // 候选顺序：首选 + 失败链（去重，保序）。用队列而不是 `for`：
        // K1 需要在"候选全部试过之后"再给连接失败的候选**一次**有界重试。
        let mut queue: VecDeque<ChannelId> = VecDeque::new();
        for id in std::iter::once(&prepared.channel_id).chain(prepared.failover_chain.iter()) {
            if !queue.contains(id) {
                queue.push_back(id.clone());
            }
        }

        let mut last_error: Option<TaError> = None;
        let mut saw_exhausted = false;
        let mut saw_other = false;
        let mut attempt: u32 = 0;
        // 上游**真正回了话**的最后一个响应（429/402/5xx），连同它的通道/线路/第几次尝试。
        // 失败链走完若没有候选成功，就把它原样交给客户端 —— 见函数末尾与 [docs/11 §5]。
        let mut last_upstream: Option<(UpstreamResponse, ChannelId, LaneId, u32)> = None;
        // 连接失败的候选：等队列空了再重试一次（K1）。
        //
        // ⚠️ 它**只记住最后一个** lane 级失败的候选：若 A、B 两个候选都是 lane 连接失败，
        // 只有 B 能拿到这次重试（有界是对的，但这份不对称是刻意的取舍，不是"每个候选各重试一次"）。
        // 想改成"每个候选都重试"就得让预算按候选分配，会把 attempts 语义变复杂。
        let mut lane_retry: Option<(ChannelId, LaneId)> = None;

        loop {
            let Some(channel_id) = queue.pop_front() else {
                // 队列空了。若最后是"lane 连不上"，且还有预算、这条 lane 也没被降级，
                // 就给同一个候选一次重试 —— 这是 K1 的全部意义：内核 listener 重建那一瞬的
                // `connection refused` 必须能挺过去。否则单通道/单 lane 配置下，
                // 用户每改一次线路就会随机看到一次失败（而"改线路"正是触发重载的时刻）。
                match lane_retry.take() {
                    Some((channel_id, lane))
                        if attempt < max_attempts && !self.lane_is_degraded(&lane, now_ms) =>
                    {
                        tokio::time::sleep(std::time::Duration::from_millis(
                            LANE_RELOAD_RETRY_DELAY_MS,
                        ))
                        .await;
                        queue.push_back(channel_id);
                        continue;
                    }
                    _ => break,
                }
            };
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
                    // 这条 lane 通了：连续失败计数清零，降级立即解除（[docs/03 §5.3]）
                    self.record_lane_success(&lane);
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
                        // 上游**说了话**（429/402/5xx）→ 记住它的原话**以及它的来源**，
                        // 失败链走完没人成功时原样交给客户端，并让 `x-tokenagency-*` 能指出
                        // "这句话是哪个通道/哪条线路/第几次尝试说的"。
                        // 边界就在"上游说不说话"：没听到（传输失败）才由我们包装。
                        last_upstream = Some((upstream, channel_id.clone(), lane.clone(), attempt));
                        continue;
                    }

                    // 上游给了**非换通道**状态（2xx/其它 4xx）→ 这个通道是通的：
                    // 连续失败计数清零、冷却立即解除（[docs/03 §5.3]「成功一次 → 清零」）。
                    // 放在这里而不是"读完 body"之后：状态码已经证明它服务正常，
                    // 后面 body 读坏了属于传输层问题（那条路径有自己的处理）。
                    self.record_channel_success(&channel_id);

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
                        // ④ 下游断连 → 取消上游（[docs/12 §4 G2]）：
                        // body 被 axum 丢掉且**没读完**时置位；正常读完不置位
                        // （否则每一次成功的流式响应都会记一条假的"客户端断连"）。
                        let (stream, downstream) =
                            dispatch::cancel_on_drop(self.watch_stream(record, stream));
                        let request_id = prepared.request_id.clone();
                        tokio::spawn(async move {
                            downstream.cancelled().await;
                            tracing::info!(
                                request_id = %request_id,
                                "客户端在流中途断连：放弃上游连接（不重试、不再写）"
                            );
                        });
                        self.emit_request_trace(
                            &prepared,
                            status.as_u16(),
                            None,
                            Some(&channel_id),
                            Some(&lane),
                            Some(latency_ms),
                            started_at.elapsed().as_millis() as u32,
                            now_ms,
                        )
                        .await;
                        return Ok(with_debug_headers(
                            dispatch::client_response_from_parts(status, headers, stream),
                            &cfg,
                            &channel_id,
                            &lane,
                            attempt,
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
                            self.emit_request_trace(
                                &prepared,
                                status.as_u16(),
                                None,
                                Some(&channel_id),
                                Some(&lane),
                                Some(latency_ms),
                                started_at.elapsed().as_millis() as u32,
                                now_ms,
                            )
                            .await;
                            return Ok(with_debug_headers(
                                dispatch::client_response_from_bytes(
                                    status,
                                    upstream.headers,
                                    bytes,
                                ),
                                &cfg,
                                &channel_id,
                                &lane,
                                attempt,
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
                        // **lane 级失败**：只记这条 lane 的连续失败（其它 lane 不受牵连）。
                        // 到阈值才降级；单次失败只留下"队列空后重试一次"的依据 ——
                        // 一次失败就拉黑，正是 K1 要避免的。
                        TaError::LaneUnavailable { .. } => {
                            self.record_lane_failure(&lane, now_ms);
                            lane_retry = Some((channel_id.clone(), lane.clone()));
                        }
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

        // ---- 失败链走完了。对外语义按 [docs/11 §5] 的两分法：
        //
        // 1) **上游说了话**（我们手里有它的 HTTP 响应）→ 原样透传。
        //    状态码 / body / 头一个都不改（`x-tokenagency-*` 由 ingress 加）。
        //    客户端必须看到上游原话：429 的 `retry-after`、401 的 key 无效说明、
        //    404 的模型不存在……**不许改写成我们的信封** —— 那会让用户以为"是 TokenAgency 坏了"。
        // 2) **我们没听到上游说话**（连接被拒/TLS/超时/读到一半断）→ 用我们自己的码 +
        //    `retryable` + 人话。裸 socket 错误没有可透传的上游语义，直接吐出去只会被
        //    误判成"中转站的服务没了"。
        if let Some((upstream, channel_id, lane, attempts)) = last_upstream {
            self.emit_request_trace(
                &prepared,
                upstream.status.as_u16(),
                // 上游说的错**不进** `error`（那是上游的话，不是我们的判断）
                None,
                Some(&channel_id),
                Some(&lane),
                None,
                started_at.elapsed().as_millis() as u32,
                now_ms,
            )
            .await;
            return Ok(with_debug_headers(
                dispatch::into_client_response(upstream),
                &cfg,
                &channel_id,
                &lane,
                attempts,
            ));
        }

        // 没有任何上游响应：额度全耗尽与"都挂了"是两种不同的对外语义
        if !saw_other && saw_exhausted {
            self.emit_request_trace(
                &prepared,
                TaError::AllChannelsExhausted.http_status(),
                Some(TaError::AllChannelsExhausted.to_string()),
                None,
                None,
                None,
                started_at.elapsed().as_millis() as u32,
                now_ms,
            )
            .await;
            return Err(TaError::AllChannelsExhausted);
        }
        let err = last_error.unwrap_or(TaError::NoViableChannel {
            message: "所有候选通道都不可用".into(),
        });
        self.emit_request_trace(
            &prepared,
            err.http_status(),
            Some(err.to_string()),
            None,
            None,
            None,
            started_at.elapsed().as_millis() as u32,
            now_ms,
        )
        .await;
        Err(err)
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

/// [docs/10 §1.4] 的 `x-tokenagency-*` 调试头。
///
/// 三个原则：
/// 1. **只追加，绝不改上游的头** —— 这是加在响应上的"我们自己的补充信息"，
///    不是对上游响应的改写（[docs/10 §1.4] 末句）。
/// 2. `gateway.debug_headers: false` 时**一个都不加**（有些客户端对未知头敏感）。
/// 3. **不写猜的值**：`x-tokenagency-usage-source` 只有在计量**真的**给出来源时才加
///    （[docs/11 §6] 铁律"不猜不估算"）；没有数据源就留空 —— 编一个 `upstream` 出来，
///    比没有这个头更坏。
fn with_debug_headers(
    mut response: axum::response::Response,
    cfg: &ConfigDoc,
    channel_id: &ChannelId,
    lane: &LaneId,
    attempts: u32,
) -> axum::response::Response {
    if !cfg.gateway.debug_headers {
        return response;
    }
    let headers = response.headers_mut();
    for (name, value) in
        [("x-tokenagency-channel", channel_id.as_str()), ("x-tokenagency-lane", lane.as_str())]
    {
        if let Ok(value) = http::HeaderValue::from_str(value) {
            headers.insert(name, value);
        }
    }
    if let Ok(value) = http::HeaderValue::from_str(&attempts.to_string()) {
        headers.insert("x-tokenagency-attempts", value);
    }
    response
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
    use ta_core::domain::{Credential, Lane, LaneKind, Model, Provider, Selector};
    use ta_core::quota::{QuotaState, QuotaStateKind};
    use ta_core::routing::{ChannelSelector, PolicyEngine, RouteDecision, Selection};

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

    /// 一个"候选全被淘汰"的策略：用来测 [docs/11 §5] 里"谁开口"的分界
    /// （本地账本耗尽 → 我们开口 TA-2004；其它理由 → TA-2003）。
    struct EliminatingPolicy {
        eliminated: Vec<(ChannelId, EliminateReason)>,
    }

    impl PolicyEngine for EliminatingPolicy {
        fn decide(&self, _hints: &RouteHints, _ctx: &RouteContext) -> RouteDecision {
            RouteDecision {
                candidates: vec![],
                eliminated: self.eliminated.clone(),
                on_no_candidate: NoCandidateAction::Error,
                selector: Selector::Priority,
                ..Default::default()
            }
        }
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
        let cfg = Arc::new(arc_swap::ArcSwap::from_pointee(cfg));
        Pipeline::new(Arc::new(PipelineDeps {
            cfg: cfg.clone(),
            clock: Arc::new(ManualClock::new(1_700_000_000_000)),
            planner: Arc::new(crate::runtime::PolicySelectorPlanner::new(
                cfg,
                policy,
                Arc::new(ChainSelector),
            )),
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

    /// 与 [`pipeline`] 同义，但注入**可控时钟**并固定单候选 ch-a ——
    /// 冷却/退避类测试需要推进时间才看得到"下一级"。
    fn pipeline_with_clock(
        cfg: ConfigDoc,
        dialer: Arc<dyn OutboundDialer>,
        clock: Arc<ManualClock>,
    ) -> Pipeline {
        let cfg = Arc::new(arc_swap::ArcSwap::from_pointee(cfg));
        Pipeline::new(Arc::new(PipelineDeps {
            cfg: cfg.clone(),
            clock,
            planner: Arc::new(crate::runtime::PolicySelectorPlanner::new(
                cfg,
                Arc::new(FixedPolicy {
                    candidates: vec!["ch-a"],
                    on_no_candidate: NoCandidateAction::Error,
                }),
                Arc::new(ChainSelector),
            )),
            quota: Arc::new(NoQuota),
            secrets: Arc::new(StaticSecrets::new([("${secret:a}", SECRET)])),
            dialer,
            passthrough: FullPassthrough::shared(),
            sink: Arc::new(RecordingSink::default()),
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

    /// [docs/11 §5]：**本地账本**判定"候选全都耗尽"时 —— 我们**没问过上游**，
    /// 没有"上游说的话"可以透传 → 由我们自己开口，且必须说清是**额度**（402 / TA-2004），
    /// 不是含糊的"无可用通道"（415 / TA-2003）。
    ///
    /// 顺带钉住"没问过上游"这件事：`dialer.calls() == 0`。
    #[tokio::test]
    async fn locally_exhausted_candidates_report_2004_without_calling_upstream() {
        let dialer = Arc::new(ScriptedDialer::default());
        let pipeline = pipeline(
            test_cfg("https://a.example/v1", "https://b.example/v1"),
            Arc::new(EliminatingPolicy {
                eliminated: vec![
                    (ChannelId::new("ch-a"), EliminateReason::QuotaExhausted),
                    (ChannelId::new("ch-b"), EliminateReason::QuotaExhausted),
                ],
            }),
            dialer.clone(),
            Arc::new(RecordingSink::default()),
        );
        let err = pipeline
            .prepare(&snapshot(br#"{"model":"claude-sonnet-4-5"}"#))
            .await
            .expect_err("本地都说耗尽了");
        assert_eq!(err.code(), "TA-2004", "本地额度耗尽 = 我们开口的 402");
        assert_eq!(err.http_status(), 402);
        assert_eq!(dialer.calls(), 0, "本地判定耗尽时不该有任何上游调用");
    }

    /// 反向控制：只要淘汰理由**不全是**额度（标签排除 / 冷却 / lane 降级…），
    /// 就不能说成 402 —— 否则会把用户骗去充值。
    #[tokio::test]
    async fn non_quota_eliminations_stay_2003_even_if_one_of_them_is_quota() {
        for eliminated in [
            vec![(ChannelId::new("ch-a"), EliminateReason::TaggedOut)],
            vec![
                (ChannelId::new("ch-a"), EliminateReason::QuotaExhausted),
                (ChannelId::new("ch-b"), EliminateReason::TaggedOut),
            ],
        ] {
            let pipeline = pipeline(
                test_cfg("https://a.example/v1", "https://b.example/v1"),
                Arc::new(EliminatingPolicy { eliminated }),
                Arc::new(ScriptedDialer::default()),
                Arc::new(RecordingSink::default()),
            );
            let err = pipeline
                .prepare(&snapshot(br#"{"model":"claude-sonnet-4-5"}"#))
                .await
                .expect_err("没有候选必须报错");
            assert_eq!(err.code(), "TA-2003", "不全是额度问题就不该报 402");
            assert_eq!(err.http_status(), 415);
        }
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
        // lane 级失败只影响那条 lane，而且**一次失败不降级**（[docs/03 §5.3] 要求连续 3 次）：
        // 内核热重载会让 listener 一瞬间关闭重建（M0 实测 K1），一次 refused 就把整条线
        // 拉黑 5 秒，会让"改线路"这个动作本身变成一次失败。
        let now = 1_700_000_000_000;
        assert!(
            pipeline.is_available(&ChannelId::new("ch-a"), Some(&LaneId::new("lane-jp")), now),
            "一次连接失败不该把 lane 拉黑（否则改线路时那一瞬会连带把整条线关掉 5 秒）"
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

    // ---------------------------------------------------------- A2：上游真的收到了这些字节

    /// A2（`docs/06 §2.1`）的 **wire 级**证据：把请求真的打到回环假上游，
    /// 然后拿**上游收到的原始报文**逐项核对。
    ///
    /// 与 `passthrough.rs` 的 insta 快照互补：快照证明"我们构造出的请求是对的"，
    /// 这条证明"**上游真的收到了它**"。覆盖 A2 的三条：
    /// ① 图片块（含 `detail`）真的到达；② `x-stainless-*` / `anthropic-beta` /
    /// 自定义 UA 逐字节到达；③ 未知 part 类型与未知顶层字段不丢。
    ///
    /// 用 `--nocapture` 跑可以看到打印出来的原始报文。
    #[tokio::test]
    async fn a2_upstream_receives_the_golden_request_byte_for_byte() {
        use crate::dispatch::tests::spawn_recording_upstream;
        use crate::dispatch::{DialerOptions, KernelDialer, StaticLanePorts};
        use crate::passthrough::tests::golden_fixture;

        let upstream = spawn_recording_upstream().await;
        let fixture = golden_fixture();

        // 同一个 golden 夹具，只是把上游换成我们的回环假上游、lane 换成 direct
        let mut provider = fixture.provider.clone();
        provider.base_url = format!("http://{}", upstream.addr);
        let mut channel = fixture.channel.clone();
        channel.lane = Some(LaneId::new("direct"));

        let mut doc = ConfigDoc::default();
        doc.models.push(Model::new(MODEL));
        doc.lanes.push(Lane::new("direct", "直连（本机网络）", LaneKind::Direct));
        doc.providers.push(provider);
        // 用 `${secret:a}` 以便复用测试夹具的 StaticSecrets
        doc.credentials.push(Credential::new("c-relay-a", "p-relay", "中转A", "${secret:a}"));
        doc.channels.push(channel);

        let pipeline = pipeline(
            doc,
            Arc::new(FixedPolicy {
                candidates: vec!["ch-relay-a"],
                on_no_candidate: NoCandidateAction::Error,
            }),
            Arc::new(KernelDialer::new(
                Arc::new(StaticLanePorts::default()),
                DialerOptions::default(),
            )),
            Arc::new(RecordingSink::default()),
        );

        let body = serde_json::to_vec(&fixture.body).expect("golden body");
        let snapshot = RequestSnapshot {
            request_id: "req_a2_golden".into(),
            method: http::Method::POST,
            path: "/v1/messages".into(),
            query: None,
            endpoint: crate::ingress::Endpoint::Messages,
            // fixture 的 content-length 是**故意写错的 999999**，正好用来证明
            // 上游看到的是按实际 body 重算后的值
            headers: fixture.headers.clone(),
            body: Bytes::from(body),
            received_at_ms: 1_700_000_000_000,
        };
        let prepared = pipeline.prepare(&snapshot).await.expect("prepare");
        let response = pipeline.execute(prepared).await.expect("execute");
        assert_eq!(response.status(), http::StatusCode::OK);

        let raw = upstream.raw(0);
        assert!(!raw.is_empty(), "假上游必须真的收到请求");
        let text = String::from_utf8_lossy(&raw).into_owned();
        println!("=== A2 证据：上游收到的原始报文 ===\n{text}\n=== 原文结束 ===");

        let head_end = raw.windows(4).position(|w| w == b"\r\n\r\n").expect("头体分界") + 4;
        let head = &text[..head_end];
        let head_lower = head.to_ascii_lowercase();
        let body_raw = String::from_utf8_lossy(&raw[head_end..]).into_owned();

        // ---- ① body：图片块（含 detail）、未知 part、未知顶层字段逐字节在
        for needle in [
            r#""type":"image_url""#,
            r#""detail":"high""#,
            "data:image/png;base64,iVBORw0KGgo=",
            r#""type":"brand_new_part_type""#,
            r#""unknown":[1,2,3]"#,
            "future_top_level_field",
        ] {
            assert!(body_raw.contains(needle), "body 里缺了 {needle}：{body_raw}");
        }
        // 除了 model（逻辑名 → 上游名），一个字段都不许变
        let mut expected = fixture.body.clone();
        expected["model"] = serde_json::json!(fixture.channel.upstream_model);
        let received: serde_json::Value =
            serde_json::from_str(&body_raw).expect("上游收到的是合法 JSON");
        assert_eq!(received, expected, "除 model 外任何字段/结构变化都是全透传失败");

        // ---- ② 代理标识 / 自定义头逐字节到达（值大小写原样，所以按原文断言）
        for keep in [
            "user-agent: claude-cli/1.0.60 (external, cli)",
            "x-stainless-lang: js",
            "x-stainless-package-version: 0.60.0",
            "x-stainless-os: Windows",
            "x-stainless-runtime: node",
            "anthropic-beta: prompt-caching-2024-07-31,computer-use-2025-01-24",
            "x-session-id: sess-8f3a2b",
            "x-weirdly-named-custom: keep-me",
        ] {
            assert!(head.contains(keep), "上游收到的头里缺了 {keep}：\n{head}");
        }

        // ---- 4 处例外：host 重写且只有一个、content-length 按实际 body 重算、
        //      鉴权头换成通道凭据、hop-by-hop 全剥
        let host_count =
            head_lower.matches("\r\nhost:").count() + usize::from(head_lower.starts_with("host:"));
        assert_eq!(host_count, 1, "host 头只能有一个：\n{head}");
        assert!(head_lower.contains(&format!("host: {}", upstream.addr)), "{head}");
        assert!(head.contains("authorization: Bearer sk-relay-secret"), "{head}");
        let declared = head_lower
            .lines()
            .find_map(|line| line.strip_prefix("content-length:"))
            .and_then(|value| value.trim().parse::<usize>().ok())
            .expect("必须有 content-length");
        assert_eq!(declared, raw.len() - head_end, "content-length 必须等于真实发出的字节数");
        for gone in ["connection:", "keep-alive:", "proxy-connection:", "transfer-encoding:"] {
            assert!(!head_lower.contains(gone), "{gone} 必须被剥离：\n{head}");
        }

        // ---- 5) 头集合的两个方向都要钉死：客户端发的**一个都不能丢**，
        //      而"上游多看到的"必须**恰好**是那条已知偏离。
        //
        // 已知偏离（不是我们写的代码，是 reqwest 的默认头）：
        // `reqwest-0.12.28/src/async_impl/client.rs:286` —— `ClientBuilder::new()` 就把
        // `accept: */*` 塞进了 `config.headers`，而 `default_headers()` 只做合并（:1163），
        // **清不掉**。于是客户端没发 `accept` 时，上游会多看到这一条。
        // 这条断言的作用：把偏离**变成摆明的、被钉住的事实**（[docs/03 §7.2.1] 那种做法），
        // reqwest 哪天再多塞一个默认头，这条就会红。
        let header_name = |line: &str| line.split(':').next().map(str::trim).map(str::to_string);
        let sent: std::collections::BTreeSet<String> = head_lower
            .lines()
            .skip(1)
            .filter_map(header_name)
            .filter(|line| !line.is_empty())
            .collect();
        let client: std::collections::BTreeSet<String> = fixture
            .headers
            .keys()
            .map(|name| name.as_str().to_string())
            .filter(|name| !crate::passthrough::is_hop_by_hop(name))
            .collect();
        let dropped: Vec<&String> = client.difference(&sent).collect();
        assert!(dropped.is_empty(), "客户端发的头不许丢：{dropped:?}\n{head}");
        let injected: std::collections::BTreeSet<String> =
            sent.difference(&client).cloned().collect();
        assert_eq!(
            injected,
            std::collections::BTreeSet::from(["accept".to_string()]),
            "除已知的 accept 偏离外，不许有第 5 类凭空多出来的头：\n{head}"
        );
        assert!(head_lower.contains("accept: */*"), "{head}");
    }

    // ---------------------------------------------------------- G2 ④：下游断连取消上游

    /// [docs/12 §4 G2] ④：**客户端中断时上游连接必须被及时取消**。
    ///
    /// 用真 TCP。假上游发头 + 第 1 个 chunk 后继续慢速发；测试只读 1 个 chunk 就把响应
    /// 丢掉（=客户端走人），然后断言**假上游的读半边看到连接被关**（`read` 返回 0）。
    /// 用读半边而不是"写失败"判：小分片会被内核缓冲，写失败可能来得很晚甚至不来。
    ///
    /// 这条测试证明的是 **DoD 的属性**（此前没有任何测试覆盖它）；包装器自身的语义
    /// （"读完不置位、被丢才置位"）由 `dispatch::tests` 的 4 个用例钉住。
    #[tokio::test]
    async fn client_disconnect_tears_down_the_upstream_stream() {
        use crate::dispatch::tests::spawn_watching_upstream;
        use crate::dispatch::{DialerOptions, KernelDialer, StaticLanePorts};
        use http_body_util::BodyExt;
        use tokio::io::AsyncWriteExt;

        let upstream = spawn_watching_upstream(|_index, _head, mut write| async move {
            let _ = write
                .write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\
                      transfer-encoding: chunked\r\n\r\n",
                )
                .await;
            for i in 1..=60 {
                let chunk = format!("data: {{\"i\":{i}}}\n\n");
                let frame = format!("{:x}\r\n{}\r\n", chunk.len(), chunk);
                if write.write_all(frame.as_bytes()).await.is_err() {
                    return;
                }
                let _ = write.flush().await;
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            let _ = write.write_all(b"0\r\n\r\n").await;
        })
        .await;

        let mut cfg =
            test_cfg(&format!("http://{}/v1/messages", upstream.addr), "https://b.example/v1");
        cfg.channels[0].lane = Some(LaneId::new("direct"));
        let pipeline = pipeline(
            cfg,
            Arc::new(FixedPolicy {
                candidates: vec!["ch-a"],
                on_no_candidate: NoCandidateAction::Error,
            }),
            Arc::new(KernelDialer::new(
                Arc::new(StaticLanePorts::default()),
                DialerOptions::default(),
            )),
            Arc::new(RecordingSink::default()),
        );

        let prepared = pipeline
            .prepare(&snapshot(br#"{"model":"claude-sonnet-4-5","stream":true}"#))
            .await
            .expect("prepare");
        assert!(prepared.stream);
        let mut response = pipeline.execute(prepared).await.expect("首字节应当到达");

        // 读第 1 个 chunk —— 证明流真的在流
        let first =
            tokio::time::timeout(std::time::Duration::from_secs(2), response.body_mut().frame())
                .await
                .expect("首字节必须到达")
                .expect("应当有帧")
                .expect("帧应当可读");
        let first_text =
            String::from_utf8_lossy(&first.into_data().unwrap_or_default()).to_string();
        assert!(first_text.contains("data: {\"i\":1}"), "{first_text}");

        // **正向控制**：此刻连接**还活着**（我们还握着响应体），所以标志必须是 false ——
        // 否则下面那条断言可能只是"它一直是 true"（假守卫）。
        tokio::time::sleep(std::time::Duration::from_millis(120)).await;
        assert!(
            !upstream.client_gone.load(Ordering::SeqCst),
            "还握着响应体时上游连接不该被关（否则这条测试证明不了任何事）"
        );

        // 客户端走人：丢掉响应体（真实的 axum 在客户端断连时就是这么做的）
        drop(response);

        let mut gone = false;
        for _ in 0..150 {
            if upstream.client_gone.load(Ordering::SeqCst) {
                gone = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(
            gone,
            "客户端断连必须传导到上游：上游读半边应在 ~3s 内看到连接被关（而不是继续写下去）"
        );
    }

    // ---------------------------------------------------------- K1：listener 重建那一瞬

    /// K1（`docs/06 §1.6`）：内核热重载 `PUT /configs?force=true` 是**进程内**重载、PID 不变，
    /// 但 listener 有一瞬间的关闭/重建 → 此时会出现 `connection refused`。
    ///
    /// **只有一个候选**时（"一个人的中转站"很常见，而"改线路"正是触发重载的动作），
    /// 这一瞬必须靠一次有界重试挺过去；打不通就直接报错，等于用户每改一次线路
    /// 就随机看到一次失败。
    ///
    /// 这条测试在旧实现上**红**：旧实现是 `mark_lane_down` + 换下一个候选，而这里没有
    /// 下一个候选 —— 请求直接失败，`dialer.calls() == 1`。
    #[tokio::test]
    async fn single_candidate_survives_the_kernel_listener_blip() {
        let sink = Arc::new(RecordingSink::default());
        let dialer = Arc::new(ScriptedDialer::with(vec![
            Scripted::LaneDown,
            Scripted::Ok {
                status: 200,
                headers: vec![("content-type", "application/json")],
                body: r#"{"ok":true}"#,
            },
        ]));
        let pipeline = pipeline(
            test_cfg("https://a.example/v1", "https://b.example/v1"),
            Arc::new(FixedPolicy {
                candidates: vec!["ch-a"],
                on_no_candidate: NoCandidateAction::Error,
            }),
            dialer.clone(),
            sink,
        );
        let prepared =
            pipeline.prepare(&snapshot(br#"{"model":"claude-sonnet-4-5"}"#)).await.unwrap();
        let response =
            pipeline.execute(prepared).await.expect("listener 重建那一瞬必须被有界重试挺过去");
        assert_eq!(response.status(), http::StatusCode::OK);
        assert_eq!(dialer.calls(), 2, "一次连接失败后必须真的再试同一个候选，而不是直接放弃");
    }

    /// 重试必须是**有界**的：用的是同一个 `attempts` 预算，不额外开一条无限循环。
    /// 全部失败时对外语义是 TA-3002（503 可重试），**不是** TA-2003（415 无可用通道）
    /// —— 后者会让客户端以为"这个模型没配置"，而真实原因是线路暂时连不上。
    #[tokio::test]
    async fn lane_retry_is_bounded_by_the_attempt_budget() {
        let dialer = Arc::new(ScriptedDialer::with(vec![
            Scripted::LaneDown,
            Scripted::LaneDown,
            Scripted::LaneDown,
        ]));
        let pipeline = pipeline(
            test_cfg("https://a.example/v1", "https://b.example/v1"),
            Arc::new(FixedPolicy {
                candidates: vec!["ch-a"],
                on_no_candidate: NoCandidateAction::Error,
            }),
            dialer.clone(),
            Arc::new(RecordingSink::default()),
        );
        let prepared =
            pipeline.prepare(&snapshot(br#"{"model":"claude-sonnet-4-5"}"#)).await.unwrap();
        let err = pipeline.execute(prepared).await.expect_err("一直连不上就必须报错");
        // 非流式 `max_attempts` 默认 3 → 总共 3 次尝试，一次都不多
        assert_eq!(dialer.calls(), 3, "重试不许超过 attempts 预算");
        assert_eq!(err.code(), "TA-3002", "线路问题是 503 可重试，不是 415 无可用通道");
        assert_eq!(err.http_status(), 503);
        // 连续 3 次才降级（[docs/03 §5.3]）
        assert!(
            !pipeline.is_available(
                &ChannelId::new("ch-a"),
                Some(&LaneId::new("lane-jp")),
                1_700_000_000_000
            ),
            "连续 3 次失败后必须降级，否则下一条请求还会撞同一堵墙"
        );
    }

    /// 连续失败计数**成功一次就清零**（[docs/03 §5.3]「成功一次 → 冷却计时器清零」）。
    ///
    /// 两个请求各失败 2 次后成功：如果计数不清零，第二个请求的第一次失败就是第 3 次连续
    /// 失败 → 立刻降级 → 那个请求会拿 503 而不是成功。所以这条测试能区分"清零"与"不清零"。
    #[tokio::test]
    async fn lane_failure_counter_is_cleared_by_a_success() {
        let ok = Scripted::Ok { status: 200, headers: vec![], body: r#"{"ok":true}"# };
        let dialer = Arc::new(ScriptedDialer::with(vec![
            Scripted::LaneDown,
            Scripted::LaneDown,
            Scripted::Ok { status: 200, headers: vec![], body: r#"{"ok":true}"# },
            Scripted::LaneDown,
            Scripted::LaneDown,
            ok,
        ]));
        let pipeline = pipeline(
            test_cfg("https://a.example/v1", "https://b.example/v1"),
            Arc::new(FixedPolicy {
                candidates: vec!["ch-a"],
                on_no_candidate: NoCandidateAction::Error,
            }),
            dialer.clone(),
            Arc::new(RecordingSink::default()),
        );
        for round in 1..=2 {
            let prepared =
                pipeline.prepare(&snapshot(br#"{"model":"claude-sonnet-4-5"}"#)).await.unwrap();
            let response = pipeline.execute(prepared).await.unwrap_or_else(|e| {
                panic!("第 {round} 轮必须成功（失败 2 次 + 成功 1 次）：{e:?}")
            });
            assert_eq!(response.status(), http::StatusCode::OK);
        }
        assert_eq!(dialer.calls(), 6, "每轮 3 次（失败、失败、成功）");
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

    /// 全部候选都回 402 时，客户端看到的是**上游的 402**。
    ///
    /// 这条原先断言"返回我们的 TA-2004 信封"，按 [docs/11 §5] 第 1 行已改：
    /// **上游返回了 HTTP 响应（哪怕 4xx/5xx）就原样透传** —— 401/404/429/402 都是
    /// "上游说的话"，改写成我们的信封会让用户以为"是 TokenAgency 坏了"。
    /// 账本侧的副作用不变（402 仍把该凭据标 `exhausted`）。
    #[tokio::test]
    async fn upstream_402_is_passed_through_when_every_candidate_is_exhausted() {
        let upstream_body = r#"{"error":{"message":"insufficient_quota"}}"#;
        let dialer = Arc::new(ScriptedDialer::with(vec![
            Scripted::Ok {
                status: 402,
                headers: vec![("content-type", "application/json")],
                body: upstream_body,
            },
            Scripted::Ok {
                status: 402,
                headers: vec![("content-type", "application/json")],
                body: upstream_body,
            },
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
        let response = pipeline.execute(prepared).await.expect("上游说了话就必须透传它");
        assert_eq!(response.status(), http::StatusCode::PAYMENT_REQUIRED);
        assert_eq!(response.headers().get("content-type").unwrap(), "application/json");
        assert_eq!(body_string(response).await, upstream_body);
    }

    /// [docs/11 §5] 第 1 行：**上游返回了 HTTP 响应（哪怕 4xx/5xx）就原样透传**。
    ///
    /// 这是用户提出的那条边界 —— 客户端必须看到上游原话（`retry-after`、错误 JSON），
    /// 而不是我们的信封。单候选、上游回 429：旧实现会把它冷却掉再拿我们自己的
    /// `TA-2005`（502）去答复，客户端**永远看不到那个 `retry-after`**。
    #[tokio::test]
    async fn upstream_error_response_reaches_the_client_verbatim() {
        let upstream_body =
            r#"{"type":"error","error":{"type":"rate_limit_error","message":"slow down"}}"#;
        let dialer = Arc::new(ScriptedDialer::with(vec![Scripted::Ok {
            status: 429,
            headers: vec![
                ("content-type", "application/json"),
                ("retry-after", "9"),
                ("anthropic-ratelimit-requests-remaining", "0"),
            ],
            body: upstream_body,
        }]));
        let pipeline = pipeline(
            test_cfg("https://a.example/v1", "https://b.example/v1"),
            Arc::new(FixedPolicy {
                candidates: vec!["ch-a"],
                on_no_candidate: NoCandidateAction::Error,
            }),
            dialer,
            Arc::new(RecordingSink::default()),
        );
        let prepared =
            pipeline.prepare(&snapshot(br#"{"model":"claude-sonnet-4-5"}"#)).await.unwrap();
        let response =
            pipeline.execute(prepared).await.expect("必须透传上游响应，而不是报我们的错");
        assert_eq!(response.status(), http::StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(response.headers().get("retry-after").unwrap(), "9");
        assert_eq!(response.headers().get("anthropic-ratelimit-requests-remaining").unwrap(), "0");
        assert_eq!(response.headers().get("content-type").unwrap(), "application/json");
        let body = body_string(response).await;
        assert_eq!(body, upstream_body, "上游的 body 必须逐字节到达");
        assert!(!body.contains("tokenagency"), "不许把上游的话改写成我们的信封：{body}");
    }

    /// [docs/11 §5] 第 2 行：**没拿到响应**（连接被拒/TLS/超时）时**不假装那是上游的响应**。
    ///
    /// 客户端收到的是我们自己的信封：`TA-3002` + `retryable: true` + `tokenagency_*` 类型 +
    /// 人话，而不是裸 socket 错误文本（那会被误判成"中转站的服务没了"）。
    #[tokio::test]
    async fn unreachable_upstream_becomes_our_envelope_not_a_fake_response() {
        use crate::dispatch::{DialerOptions, KernelDialer, StaticLanePorts};

        // ch-a 走 lane-jp；把 lane-jp 指到一个没人监听的端口 → 连接必被拒
        let lane = LaneId::new("lane-jp");
        let dialer = Arc::new(KernelDialer::new(
            Arc::new(StaticLanePorts::new([(lane, 1)])),
            DialerOptions { connect_timeout_s: 1, ..Default::default() },
        ));
        let pipeline = pipeline(
            test_cfg("https://a.example/v1", "https://b.example/v1"),
            Arc::new(FixedPolicy {
                candidates: vec!["ch-a"],
                on_no_candidate: NoCandidateAction::Error,
            }),
            dialer,
            Arc::new(RecordingSink::default()),
        );
        let prepared =
            pipeline.prepare(&snapshot(br#"{"model":"claude-sonnet-4-5"}"#)).await.unwrap();
        let err = pipeline.execute(prepared).await.expect_err("连不上就不能假装有上游响应");
        assert_eq!(err.code(), "TA-3002", "线路连不上是我们的判断，不是上游的话");
        assert_eq!(err.http_status(), 503);
        assert!(err.retryable(), "503 必须可重试");
        assert!(err.type_str().starts_with("tokenagency_"), "{:?}", err.type_str());

        // 渲染成客户端真正收到的 body：必须是我们的 JSON 信封
        let response = crate::ingress::error_response(&err, "req_test");
        assert_eq!(response.status(), http::StatusCode::SERVICE_UNAVAILABLE);
        let body = body_string(response).await;
        let envelope: serde_json::Value =
            serde_json::from_str(&body).unwrap_or_else(|e| panic!("信封必须是 JSON：{e}\n{body}"));
        assert_eq!(envelope["error"]["code"], "TA-3002");
        assert_eq!(envelope["error"]["retryable"], true);
        assert_eq!(envelope["error"]["type"], "tokenagency_lane_unavailable");
        assert_eq!(envelope["error"]["request_id"], "req_test");
    }

    // ---------------------------------------------------------- trace 落库（C4）

    /// **C4 的端到端证据**：一次真实请求（ch-a 500 → ch-b 200）写进**真 SQLite trace 表**，
    /// 再按 `request_id` 查回来，必须能看到完整的尝试链与最终结果。
    ///
    /// 用 `ta_store::open_memory()`（跑过迁移的真库）+ `SqliteTraceStore`，
    /// 而不是假 store —— 假 store 只能证明"我们调用了一个方法"，证明不了"表里真有这条轨迹"。
    #[tokio::test]
    async fn one_request_writes_one_queryable_trace_with_its_attempt_chain() {
        use crate::trace::TraceStoreSink;
        use ta_store::TraceStore;

        let pool = ta_store::open_memory().await.expect("内存库");
        let clock = Arc::new(ManualClock::new(1_700_000_000_000));
        let store: Arc<dyn TraceStore> =
            Arc::new(ta_store::SqliteTraceStore::with_clock(pool, clock.clone()));
        let sink: Arc<dyn AttemptSink> = Arc::new(TraceStoreSink::new(store.clone()));

        let cfg = Arc::new(arc_swap::ArcSwap::from_pointee(test_cfg(
            "https://a.example/v1",
            "https://b.example/v1",
        )));
        let pipeline = Pipeline::new(Arc::new(PipelineDeps {
            cfg: cfg.clone(),
            clock,
            planner: Arc::new(crate::runtime::PolicySelectorPlanner::new(
                cfg,
                Arc::new(FixedPolicy {
                    candidates: vec!["ch-a", "ch-b"],
                    on_no_candidate: NoCandidateAction::Error,
                }),
                Arc::new(ChainSelector),
            )),
            quota: Arc::new(NoQuota),
            secrets: Arc::new(StaticSecrets::new([
                ("${secret:a}", SECRET),
                ("${secret:b}", SECRET),
            ])),
            dialer: Arc::new(ScriptedDialer::with(vec![
                Scripted::Ok { status: 500, headers: vec![], body: "{}" },
                Scripted::Ok { status: 200, headers: vec![], body: r#"{"ok":true}"# },
            ])),
            passthrough: FullPassthrough::shared(),
            sink,
            meter: None,
        }));

        let prepared =
            pipeline.prepare(&snapshot(br#"{"model":"claude-sonnet-4-5"}"#)).await.unwrap();
        let request_id = prepared.request_id.clone();
        let response = pipeline.execute(prepared).await.expect("第二个通道应当成功");
        assert_eq!(response.status(), http::StatusCode::OK);
        // 流/非流式的 body 都要被丢弃，否则 trace 写不写得到不影响断言
        drop(response);

        let detail = store
            .get(&request_id)
            .await
            .expect("查询 trace 不该报错")
            .unwrap_or_else(|| panic!("trace 表里必须有 {request_id} 这一条"));
        assert_eq!(detail.row.request_id, request_id);
        assert_eq!(detail.row.model, "claude-sonnet-4-5");
        assert_eq!(detail.row.status, 200);
        assert_eq!(detail.attempts.len(), 2, "两次尝试都要在链里：{:?}", detail.attempts);
        assert_eq!(detail.attempts[0].channel.as_deref(), Some("ch-a"));
        assert_eq!(detail.attempts[0].status, Some(500));
        assert_eq!(detail.attempts[1].channel.as_deref(), Some("ch-b"));
        assert_eq!(detail.attempts[1].status, Some(200));
        assert!(
            detail.header_diff.as_ref().is_none_or(|diff| diff.is_empty()),
            "自检 header_diff 必须为空（非空=网关自己动过头）：{:?}",
            detail.header_diff
        );
        // 拿来贴进 PR 的真实轨迹
        println!(
            "=== 真轨迹（trace 表读回）===\nrequest_id={}\nmodel={} status={} outcome={} total_ms={}\nattempts={:?}",
            detail.row.request_id,
            detail.row.model,
            detail.row.status,
            detail.row.outcome,
            detail.row.total_ms,
            detail.attempts
        );
    }

    /// 三个头必须指出**真实**的来源：通道、线路、**第几次尝试**。
    /// 这里让 ch-a 先 429 再换 ch-b 成功 → `attempts` 必须是 2（不是常数、不是 1）。
    #[tokio::test]
    async fn debug_headers_carry_the_real_channel_lane_and_attempt_count() {
        let dialer = Arc::new(ScriptedDialer::with(vec![
            Scripted::Ok { status: 429, headers: vec![], body: "{}" },
            Scripted::Ok { status: 200, headers: vec![], body: r#"{"ok":true}"# },
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
        let response = pipeline.execute(prepared).await.expect("第二个通道应当成功");
        assert_eq!(response.status(), http::StatusCode::OK);
        assert_eq!(response.headers().get("x-tokenagency-channel").unwrap(), "ch-b");
        assert_eq!(response.headers().get("x-tokenagency-lane").unwrap(), "lane-tokyo");
        assert_eq!(response.headers().get("x-tokenagency-attempts").unwrap(), "2");
    }

    /// 上游错误被透传时，`x-tokenagency-*` 也要指出"这句话是谁说的"，
    /// 且**不许动上游自己的头**（我们只追加）。
    #[tokio::test]
    async fn debug_headers_ride_along_the_passthrough_without_touching_upstream_headers() {
        let dialer = Arc::new(ScriptedDialer::with(vec![Scripted::Ok {
            status: 429,
            headers: vec![("x-upstream-note", "keep-me"), ("retry-after", "9")],
            body: "{}",
        }]));
        let pipeline = pipeline(
            test_cfg("https://a.example/v1", "https://b.example/v1"),
            Arc::new(FixedPolicy {
                candidates: vec!["ch-a"],
                on_no_candidate: NoCandidateAction::Error,
            }),
            dialer,
            Arc::new(RecordingSink::default()),
        );
        let prepared =
            pipeline.prepare(&snapshot(br#"{"model":"claude-sonnet-4-5"}"#)).await.unwrap();
        let response = pipeline.execute(prepared).await.expect("429 透传");
        assert_eq!(response.status(), http::StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(response.headers().get("x-upstream-note").unwrap(), "keep-me");
        assert_eq!(response.headers().get("retry-after").unwrap(), "9");
        assert_eq!(response.headers().get("x-tokenagency-channel").unwrap(), "ch-a");
        assert_eq!(response.headers().get("x-tokenagency-attempts").unwrap(), "1");
    }

    /// `gateway.debug_headers: false` → **一个都不加**（有些客户端对未知头敏感）。
    #[tokio::test]
    async fn debug_headers_are_absent_when_switched_off() {
        let mut cfg = test_cfg("https://a.example/v1", "https://b.example/v1");
        cfg.gateway.debug_headers = false;
        let pipeline = pipeline(
            cfg,
            Arc::new(FixedPolicy {
                candidates: vec!["ch-a"],
                on_no_candidate: NoCandidateAction::Error,
            }),
            Arc::new(ScriptedDialer::with(vec![Scripted::Ok {
                status: 200,
                headers: vec![],
                body: r#"{"ok":true}"#,
            }])),
            Arc::new(RecordingSink::default()),
        );
        let prepared =
            pipeline.prepare(&snapshot(br#"{"model":"claude-sonnet-4-5"}"#)).await.unwrap();
        let response = pipeline.execute(prepared).await.expect("应当成功");
        assert!(
            !response.headers().keys().any(|name| name.as_str().starts_with("x-tokenagency-")),
            "关掉调试头之后一个都不该出现：{:?}",
            response.headers()
        );
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

    // ---------------------------------------------------------- 冷却退避（[docs/03 §5.3]）

    /// 同一通道连续失败时，冷却必须按文档的阶梯退避：`120s → 300s → 900s → … → 上限 1h`。
    ///
    /// 断言写成"冷却结束前 1ms 仍不可用、结束那一刻可用"，把每一级的**秒数**钉死；
    /// 不能只断言"有冷却"（那对固定 `cooldown_s` 的旧实现也成立）。
    #[tokio::test]
    async fn cooldown_backs_off_per_the_documented_ladder() {
        let clock = Arc::new(ManualClock::new(1_700_000_000_000));
        let ladder: [i64; 5] = [120, 300, 900, 2_700, 3_600];
        let dialer = Arc::new(ScriptedDialer::with(
            (0..ladder.len())
                .map(|_| Scripted::Ok { status: 429, headers: vec![], body: "{}" })
                .collect(),
        ));
        let pipeline = pipeline_with_clock(
            test_cfg("https://a.example/v1", "https://b.example/v1"),
            dialer.clone(),
            clock.clone(),
        );

        let channel = ChannelId::new("ch-a");
        let lane = LaneId::new("lane-jp");
        let mut now = 1_700_000_000_000;
        for (round, expected_s) in ladder.iter().enumerate() {
            let prepared =
                pipeline.prepare(&snapshot(br#"{"model":"claude-sonnet-4-5"}"#)).await.unwrap();
            // 429 是"上游说的话" → 按 [docs/11 §5] 原样交给客户端（不是我们的错误信封）。
            // 冷却仍然照记（本测试要测的是冷却阶梯）。
            let response = pipeline.execute(prepared).await.expect("429 应当被透传");
            assert_eq!(response.status(), http::StatusCode::TOO_MANY_REQUESTS);
            drop(response);
            let until = now + expected_s * 1000;
            assert!(
                !pipeline.is_available(&channel, Some(&lane), until - 1),
                "第 {} 轮：冷却必须覆盖到 {}s",
                round + 1,
                expected_s
            );
            assert!(
                pipeline.is_available(&channel, Some(&lane), until),
                "第 {} 轮：冷却必须恰好是 {}s（[docs/03 §5.3] 的阶梯）",
                round + 1,
                expected_s
            );
            now = until;
            clock.advance(expected_s * 1000);
        }
        assert_eq!(dialer.calls(), ladder.len() as u32, "每轮都要真的打到同一个通道");
    }

    /// 成功一次必须把"连续失败计数"与冷却一起清零：否则偶发 429 的通道会被自己的历史
    /// 越推越远（[docs/03 §6.2]「自动学习降级比不降级更糟」）。
    ///
    /// 可观测点：成功后的下一次失败应当**从阶梯第一级（120s）重新开始**，
    /// 而不是接着上一级（300s）。
    #[tokio::test]
    async fn a_success_clears_the_channel_backoff() {
        let clock = Arc::new(ManualClock::new(1_700_000_000_000));
        let dialer = Arc::new(ScriptedDialer::with(vec![
            Scripted::Ok { status: 429, headers: vec![], body: "{}" },
            Scripted::Ok { status: 200, headers: vec![], body: r#"{"ok":true}"# },
            Scripted::Ok { status: 429, headers: vec![], body: "{}" },
        ]));
        let pipeline = pipeline_with_clock(
            test_cfg("https://a.example/v1", "https://b.example/v1"),
            dialer,
            clock.clone(),
        );
        let channel = ChannelId::new("ch-a");
        let lane = LaneId::new("lane-jp");
        let start = 1_700_000_000_000;

        // 第 1 轮：429 → 120s
        let prepared =
            pipeline.prepare(&snapshot(br#"{"model":"claude-sonnet-4-5"}"#)).await.unwrap();
        let response = pipeline.execute(prepared).await.expect("429 应当被透传");
        assert_eq!(response.status(), http::StatusCode::TOO_MANY_REQUESTS);
        drop(response);
        assert!(!pipeline.is_available(&channel, Some(&lane), start + 119_000));
        assert!(pipeline.is_available(&channel, Some(&lane), start + 120_000));

        // 第 2 轮：冷却过期后成功 → 计数清零
        clock.advance(120_000);
        let prepared =
            pipeline.prepare(&snapshot(br#"{"model":"claude-sonnet-4-5"}"#)).await.unwrap();
        let response = pipeline.execute(prepared).await.expect("应当成功");
        assert_eq!(response.status(), http::StatusCode::OK);
        assert!(
            pipeline.is_available(&channel, Some(&lane), start + 120_000),
            "成功后不该还留着冷却"
        );

        // 第 3 轮：再 429 → 必须回到第一级 120s（若计数没清零，这里会是 300s）
        let after_success = start + 120_000;
        let prepared =
            pipeline.prepare(&snapshot(br#"{"model":"claude-sonnet-4-5"}"#)).await.unwrap();
        let response = pipeline.execute(prepared).await.expect("429 应当被透传");
        assert_eq!(response.status(), http::StatusCode::TOO_MANY_REQUESTS);
        drop(response);
        assert!(
            !pipeline.is_available(&channel, Some(&lane), after_success + 119_000),
            "第 3 轮：冷却内必须不可用"
        );
        assert!(
            pipeline.is_available(&channel, Some(&lane), after_success + 120_000),
            "成功必须把退避计数清零：第 3 轮应回到 120s，而不是 300s"
        );
    }

    // ---------------------------------------------------------- 降级窗口的正向控制

    /// 降级窗口必须**真的会过去**。
    ///
    /// 缺了这条正向控制，"把窗口写成 `i64::MAX`（永久拉黑）"不会有任何测试变红 ——
    /// 而永久拉黑正是 M0 实测（`docs/06 §1.6`）要求避免的那条：用户改一次线路
    /// 就永久失去那条线（直到重启网关）。
    #[tokio::test]
    async fn a_degraded_lane_is_available_again_after_the_recovery_window() {
        let dialer = Arc::new(ScriptedDialer::with(vec![
            Scripted::LaneDown,
            Scripted::LaneDown,
            Scripted::LaneDown,
        ]));
        let pipeline = pipeline(
            test_cfg("https://a.example/v1", "https://b.example/v1"),
            Arc::new(FixedPolicy {
                candidates: vec!["ch-a"],
                on_no_candidate: NoCandidateAction::Error,
            }),
            dialer,
            Arc::new(RecordingSink::default()),
        );
        let prepared =
            pipeline.prepare(&snapshot(br#"{"model":"claude-sonnet-4-5"}"#)).await.unwrap();
        let _ = pipeline.execute(prepared).await.expect_err("三次都连不上");

        let channel = ChannelId::new("ch-a");
        let lane = LaneId::new("lane-jp");
        let now = 1_700_000_000_000;
        assert!(
            !pipeline.is_available(&channel, Some(&lane), now),
            "连续 3 次失败后必须处于降级窗口内"
        );
        // 窗口长度**故意独立写死**，不从被测量的常量取：否则断言会退化成同义反复 ——
        // 把 `LANE_RECOVERY_MS` 改成 `i64::MAX`（永久拉黑）也照样通过，
        // 而那正是 [docs/06 §1.6] 要求避免的形态。
        const EXPECTED_WINDOW_MS: i64 = 5_000;
        assert!(
            !pipeline.is_available(&channel, Some(&lane), now + EXPECTED_WINDOW_MS - 1),
            "窗口内必须仍然不可用（降级要真的生效）"
        );
        assert!(
            pipeline.is_available(&channel, Some(&lane), now + EXPECTED_WINDOW_MS),
            "恢复窗口过后必须重新可用（[docs/06 §1.6]：reload 那一瞬不是永久故障）"
        );
    }

    // ---------------------------------------------------------- 同代求值

    /// 一次 `prepare` 只能有**一个权威代次**：通道解析必须用计划回传的那一版配置。
    ///
    /// 造一个"计划绑定在 v1、`deps.config` 已经是 v2"的场景（真实里是配置换代恰好落在
    /// 路由求值前后）。旧实现自己 `load_full()` → 会拿 v2 解析通道
    /// （`upstream_model = "upstream-v2"`），于是"路由按 v1 选、请求按 v2 造"。
    /// 现在必须一律用计划回传的 v1。
    #[tokio::test]
    async fn prepare_uses_the_plan_binding_and_never_mixes_generations() {
        struct BoundPlanner {
            doc: Arc<ConfigDoc>,
        }
        impl RoutingPlanner for BoundPlanner {
            fn current_doc(&self) -> Arc<ConfigDoc> {
                self.doc.clone()
            }
            fn plan(&self, _hints: &RouteHints, _ctx: &RouteContext) -> crate::runtime::RoutePlan {
                crate::runtime::RoutePlan {
                    cfg: self.doc.clone(),
                    decision: RouteDecision {
                        candidates: vec![ChannelId::new("ch-a")],
                        selector: Selector::Priority,
                        on_no_candidate: NoCandidateAction::Error,
                        ..Default::default()
                    },
                    selection: Some(Selection {
                        channel: ChannelId::new("ch-a"),
                        sticky_hit: false,
                        sticky_key: None,
                        chain: vec![ChannelId::new("ch-a")],
                    }),
                }
            }
        }

        let mut v1 = test_cfg("https://a.example/v1", "https://b.example/v1");
        v1.channels[0].upstream_model = "upstream-v1".into();
        let v1 = Arc::new(v1);
        let mut v2 = test_cfg("https://a.example/v1", "https://b.example/v1");
        v2.channels[0].upstream_model = "upstream-v2".into();

        let pipeline = Pipeline::new(Arc::new(PipelineDeps {
            // deps.config 是**后一代**（v2）—— 模拟"换代发生在路由求值之后"
            cfg: Arc::new(arc_swap::ArcSwap::from_pointee(v2)),
            clock: Arc::new(ManualClock::new(1_700_000_000_000)),
            planner: Arc::new(BoundPlanner { doc: v1 }),
            quota: Arc::new(NoQuota),
            secrets: Arc::new(StaticSecrets::new([("${secret:a}", SECRET)])),
            dialer: Arc::new(ScriptedDialer::default()),
            passthrough: FullPassthrough::shared(),
            sink: Arc::new(RecordingSink::default()),
            meter: None,
        }));

        let prepared = pipeline
            .prepare(&snapshot(br#"{"model":"claude-sonnet-4-5"}"#))
            .await
            .expect("应当能准备");
        assert_eq!(
            prepared.upstream_model, "upstream-v1",
            "通道解析必须用计划回传的那一版配置，而不是自己再 load 一次"
        );
        let patched: serde_json::Value =
            serde_json::from_slice(&prepared.body).expect("body 是 JSON");
        assert_eq!(patched["model"], "upstream-v1", "body 也必须按同一版配置改写");
    }
}
