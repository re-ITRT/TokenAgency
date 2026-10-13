//! 计量接线（G4）：响应 usage → `UsageRecord` → 额度账本 / 价格快照 / 用量表。
//!
//! # 这一层**只搬运，不判定**
//!
//! `metered_usage_for` 是"上游到底给没给 usage"的**唯一决策点**（铁律 4），
//! 所以这里**绝不**自己判 `null` / `is_empty` / 手拼 [`ta_core::metering::MeteredUsage`]
//! （`metered_usage_guard.rs` 会直接红）。这里只做三件事：
//!
//! 1. **把证据原样交给 ta-core**：非流式就是整包 JSON；流式是"事件碎片"，
//!    先把同一份 `usage_paths` 口径下的容器合并成最终形态，再按协议主路径包一层
//!    （为什么要包：[`evidence_envelope`] 的说明）。
//! 2. **一次请求只调一次**：流式必须在流结束（或客户端断连）时拿最终形态调，
//!    绝不在每个分片上各调一次 —— 分片上没有 usage 证据，逐片调会得到一串估算再相加。
//!    同步完成（配额判定不能等落库），只有写 `usage` 表是 spawn 出去的。
//!
//! # 流式为什么能"看"而不"攒"
//!
//! [`ResponseMeter::observe_stream`] 返回的仍是**逐 chunk 直通**的流：每个 chunk 立刻
//! 原样交给下游（[`MeteredStream::poll_next`] 只旁路解析），计量只保留**未闭合的最后一行**
//! （上限 [`MAX_PENDING_LINE_BYTES`]）。G2 的零缓冲承诺因此不被破坏。

use std::sync::Arc;

use bytes::Bytes;
use serde_json::Value;
use ta_core::clock::Clock;
use ta_core::metering::{metered_usage_for, usage_paths};
use ta_core::pricing::PricingEngine;
use ta_core::types::{Protocol, RequestId, UsageRecord};

use crate::dispatch::{BoxError, ByteStream};
use crate::pipeline::{MeterContext, ResponseMeter};
use crate::traits_store::GatewayStore;
use crate::GatewayDeps;

/// 未闭合行的上限：SSE 的一行（`data: {...}`）正常是几百字节；到这里还没遇到换行，
/// 说明对端发了个畸形/超大帧 —— 丢掉缓冲（响应本身照传）。
///
/// 为什么必须有上限：计量是**旁路**，它不许把网关变成无界缓冲
/// （宁可少解析一行 usage，也不能因为对端的畸形流把内存吃了）。
const MAX_PENDING_LINE_BYTES: usize = 256 * 1024;

/// 生产计量器。[`crate::RuntimeParts::with_usage_meter`] 是装配入口。
pub struct UsageMeter {
    core: Arc<MeterCore>,
}

/// 真正干活的四件套（也在流包装器里被 `Arc` 共享）。
struct MeterCore {
    store: Arc<dyn GatewayStore>,
    clock: Arc<dyn Clock>,
    pricing: Arc<dyn PricingEngine>,
}

impl UsageMeter {
    pub fn new(
        store: Arc<dyn GatewayStore>,
        clock: Arc<dyn Clock>,
        pricing: Arc<dyn PricingEngine>,
    ) -> Self {
        Self { core: Arc::new(MeterCore { store, clock, pricing }) }
    }

    /// 从 [`GatewayDeps`] 装配（生产路径：四件依赖都在里面）。
    pub fn from_deps(deps: &Arc<GatewayDeps>) -> Self {
        Self::new(deps.store.clone(), deps.clock.clone(), deps.pricing.clone())
    }
}

impl ResponseMeter for UsageMeter {
    fn observe_full(&self, ctx: MeterContext, body: &Bytes, _status: u16) {
        // 证据 = **整包 JSON 原样**。ta-core 自己会在协议路径上判有没有 usage
        // （连"整包就是容器"的兜底识别也在它那儿）—— 这里提前判一次就是第二套口径。
        // 不是 JSON / 是 `null` → `None`：让 ta-core 走估算（诚实），而不是"上游说 0"。
        let evidence = parse_json(body);
        let raw = evidence
            .as_ref()
            .and_then(|value| container_of(ctx.protocol, value))
            .unwrap_or(Value::Null);
        self.core.finish(&ctx, evidence, raw);
    }

    fn observe_stream(&self, ctx: MeterContext, stream: ByteStream) -> ByteStream {
        Box::pin(MeteredStream {
            inner: stream,
            core: self.core.clone(),
            ctx,
            acc: UsageAccumulator::default(),
            line: Vec::new(),
            finished: false,
        })
    }
}

impl MeterCore {
    /// 结算的**同步部分**：定价格 → 造记录。返回 `None` = 这条响应不记账。
    ///
    /// 价格必须在这里取：`price_snapshot` 的语义是"**请求发生时**生效的费率"，
    /// 落库慢一点不能改成"落库时的费率"。
    fn settle(
        &self,
        ctx: &MeterContext,
        evidence: Option<Value>,
        raw: Value,
    ) -> Option<UsageRecord> {
        // 只记上游**成功**的响应：4xx 是"请求本身的问题"，上游没为它消耗额度，
        // 记一条 0/估算的用量只会让成本中心虚增。尝试本身由 attempt sink 记（含状态码）。
        if !(200..300).contains(&ctx.status) {
            return None;
        }
        let estimate_src = parse_json(&ctx.request_body).unwrap_or(Value::Null);
        // ★ 一次请求**恰好一次**：拿到的是最终形态（流式已合并完成）。
        let metered = metered_usage_for(ctx.protocol, evidence.as_ref(), &estimate_src);
        let model = ctx.model.clone().unwrap_or_else(|| ctx.upstream_model.clone());
        let price = self.pricing.resolve(&model, &ctx.provider_id, &ctx.credential_id);
        let cost = price.cost_of(&metered.usage);

        Some(UsageRecord {
            request_id: RequestId::new(ctx.request_id.clone()),
            ts_ms: self.clock.now_ms(),
            credential_id: ctx.credential_id.clone(),
            provider_id: ctx.provider_id.clone(),
            channel_id: ctx.channel_id.clone(),
            model,
            upstream_model: ctx.upstream_model.clone(),
            // 客户端识别（UA → ClientId）还没接线：**不知道就写不知道**，不猜一个。
            client: None,
            lane_id: ctx.lane_id.clone(),
            // 内核节点名（mihomo 的 proxy 名）不在网关这一层，留空由内核侧补。
            node: None,
            status: ctx.status,
            error_type: None,
            latency_ms: elapsed_ms(ctx),
            ttfb_ms: Some(ctx.upstream_ms),
            usage: metered.usage,
            usage_raw: raw,
            usage_source: metered.source,
            cost,
            price_snapshot: price.to_snapshot(),
        })
    }

    /// 异步落库：**失败只记日志**（响应早就交给客户端了，不能反过来影响它）。
    fn persist(&self, record: UsageRecord) {
        let request_id = record.request_id.as_str().to_string();
        let store = self.store.clone();
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn(async move {
                    if let Err(err) = store.record_usage(record).await {
                        //：报 `TaError` 必带 `code`
                        tracing::warn!(
                            request_id = %request_id,
                            code = err.code(),
                            error = %err,
                            "写用量记录失败（响应已发出，不影响本次请求）"
                        );
                    }
                });
            }
            Err(_) => {
                // 没有运行时 = 装配错了（`RuntimeParts::with_usage_meter` 必须在 tokio 里装配）。
                // 静默丢掉会让"成本中心偶发少一笔"变成无法查证的谜案，所以喊出来。
                tracing::warn!(
                    request_id = %request_id,
                    "当前不在 tokio 运行时里，用量记录未落库"
                );
            }
        }
    }

    fn finish(&self, ctx: &MeterContext, evidence: Option<Value>, raw: Value) {
        if let Some(record) = self.settle(ctx, evidence, raw) {
            self.persist(record);
        }
    }
}

/// 请求耗时（流式在流结束时才算得出总耗时）。
fn elapsed_ms(ctx: &MeterContext) -> u32 {
    ctx.request_started_at.elapsed().as_millis() as u32
}

// ================================================================ 流式：直通 + 旁路解析

/// 只旁路解析、**绝不攒**的流包装器。
struct MeteredStream {
    inner: ByteStream,
    core: Arc<MeterCore>,
    ctx: MeterContext,
    acc: UsageAccumulator,
    /// 尚未遇到换行的尾巴（跨 chunk 的一行靠它拼回来）
    line: Vec<u8>,
    finished: bool,
}

impl MeteredStream {
    /// 结束（正常读完 / 上游断流 / 被丢弃）→ **恰好一次**结算。
    fn settle(&mut self, why: &'static str) {
        if self.finished {
            return;
        }
        self.finished = true;
        let evidence =
            self.acc.merged.as_ref().map(|c| evidence_envelope(self.ctx.protocol, c.clone()));
        let raw = self.acc.merged.clone().unwrap_or(Value::Null);
        tracing::debug!(request_id = %self.ctx.request_id, reason = why, "计量：流式响应结束");
        self.core.finish(&self.ctx, evidence, raw);
    }

    fn feed_bytes(&mut self, chunk: &[u8]) {
        self.line.extend_from_slice(chunk);
        while let Some(pos) = self.line.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.line.drain(..=pos).collect();
            self.feed_line(&line);
        }
        if self.line.len() > MAX_PENDING_LINE_BYTES {
            self.line.clear();
            tracing::warn!(
                request_id = %self.ctx.request_id,
                "流式响应出现超长未闭合行，计量放弃解析该行（响应本身不受影响）"
            );
        }
    }

    fn feed_line(&mut self, line: &[u8]) {
        let line = trim_ascii(line);
        let payload = match line.strip_prefix(b"data:") {
            Some(rest) => trim_ascii(rest),
            // 非 SSE 的 NDJSON（有些兼容端点这么发）：整行就是一个 JSON 对象
            None => {
                if !line.starts_with(b"{") {
                    return;
                }
                line
            }
        };
        if payload.is_empty() || payload == b"[DONE]" {
            return;
        }
        if let Ok(value) = serde_json::from_slice::<Value>(payload) {
            self.acc.feed(self.ctx.protocol, &value);
        }
    }
}

impl futures_util::Stream for MeteredStream {
    type Item = Result<Bytes, BoxError>;

    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        let this = self.get_mut();
        let polled = this.inner.as_mut().poll_next(cx);
        match &polled {
            // 先看后传：这一个 chunk **原样**返回，不做任何改写/重编码
            std::task::Poll::Ready(Some(Ok(chunk))) => this.feed_bytes(chunk),
            // 上游断流：流以 Err 结束，之后必然被 drop（`finished` 保证不重复结算）
            std::task::Poll::Ready(Some(Err(_))) => this.settle("上游流中断"),
            std::task::Poll::Ready(None) => this.settle("正常读完"),
            std::task::Poll::Pending => {}
        }
        polled
    }
}

impl Drop for MeteredStream {
    fn drop(&mut self) {
        // 客户端中途断连 → axum 丢掉 body → 走到这里。有始有终：
        // 已经发生的上游消耗必须留下记录（拿到的部分照实记，拿不到的部分走 Estimated）。
        self.settle("流被丢弃");
    }
}

// ================================================================ usage 容器合并

/// 一条流里所有事件携带的 usage 容器合并成的"最终形态"。
#[derive(Debug, Default)]
struct UsageAccumulator {
    merged: Option<Value>,
}

impl UsageAccumulator {
    fn feed(&mut self, protocol: Protocol, payload: &Value) {
        let Some(container) = container_of(protocol, payload) else {
            return;
        };
        match &mut self.merged {
            Some(acc) => merge_into(acc, &container),
            None => self.merged = Some(container),
        }
    }
}

/// 一个事件里的 usage 容器：**只在 `ta_core::metering::usage_paths` 这张表上找**，
/// 首个命中的对象胜出（与 ta-core 的 `usage_container` 同序）。
///
/// **路径口径只有一处**。这里曾经还有一层"信封回退"（去每个顶层对象里再试一次同样的路径），
/// 理由是 Anthropic 流式的 input 在 `message_start.message.usage` 而不在 `/usage`。
/// 第 12 轮 E2 把 `/message/usage` 补进了那张表（并把 `normalize_anthropic` 也改成用表），
/// 于是 `message_start` 在**顶层**就能命中 —— 回退成了死分支，已删（主管裁决）。
/// 留着它的唯一作用是让下一个人以为"路径口径有两处"。
fn container_of(protocol: Protocol, payload: &Value) -> Option<Value> {
    for path in usage_paths(protocol) {
        if let Some(v) = payload.pointer(path) {
            if v.is_object() {
                return Some(v.clone());
            }
        }
    }
    None
}

/// 把合并好的容器**包回协议主路径**（`usage_paths` 的第一条）。
///
/// 为什么必须包：空对象 `{}` **也是证据**（上游明确说 0）。直接把它当整包传给
/// `metered_usage_for`，ta-core 的 `usage_is_absent` 会认不出来（它看的是"协议路径上
/// 有没有对象"），于是把"上游说 0"错记成"我们不知道"→ 变成一条估算。
/// 包一层之后，**判定仍然由 ta-core 做**，这里只是把证据放回它认得的位置。
fn evidence_envelope(protocol: Protocol, container: Value) -> Value {
    match usage_paths(protocol).first() {
        Some(primary) => {
            let key = primary.trim_start_matches('/');
            let mut obj = serde_json::Map::new();
            obj.insert(key.to_string(), container);
            Value::Object(obj)
        }
        // `usage_paths` 各协议都非空；真为空时原样交出去（判定交给 ta-core）
        None => container,
    }
}

/// 深合并（后来的事件覆盖先前的标量）：Anthropic 的 `message_delta` 给最终
/// `output_tokens`、Gemini 的每个 chunk 给**累计** `usageMetadata` —— 都是"后者为准"。
fn merge_into(dst: &mut Value, src: &Value) {
    match (dst, src) {
        (Value::Object(d), Value::Object(s)) => {
            for (k, v) in s {
                match d.get_mut(k) {
                    Some(slot) => merge_into(slot, v),
                    None => {
                        d.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        (d, s) => *d = s.clone(),
    }
}

/// 解析 JSON；不是 JSON / 是 `null` → `None`（"没有证据"，由 ta-core 走估算）。
fn parse_json(bytes: &[u8]) -> Option<Value> {
    let value: Value = serde_json::from_slice(bytes).ok()?;
    if value.is_null() {
        None
    } else {
        Some(value)
    }
}

fn trim_ascii(bytes: &[u8]) -> &[u8] {
    let start = bytes.iter().position(|b| !b.is_ascii_whitespace()).unwrap_or(bytes.len());
    let end = bytes.iter().rposition(|b| !b.is_ascii_whitespace()).map_or(start, |i| i + 1);
    &bytes[start..end]
}

// ================================================================ 测试

/// 测试替身。**放在独立模块里**：管线侧的端到端测试也要复用它们 ——
/// "接上生产计量器之后记录到底对不对"只能用**真** [`UsageMeter`] + 可读回的 store 证明。
#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    use async_trait::async_trait;
    use std::sync::Mutex;
    use ta_core::clock::ManualClock;
    use ta_core::error::Result;
    use ta_core::pricing::{PriceSourceKind, ResolvedPrice};
    use ta_core::types::{CredentialId, ProviderId, RequestSummary, TokenKind};

    /// 固定费率：input 3 / 1M、output 15 / 1M（乘数 1）
    pub(crate) const INPUT_RATE: f64 = 3.0;
    pub(crate) const OUTPUT_RATE: f64 = 15.0;

    #[derive(Default)]
    pub(crate) struct RecordingStore {
        records: Mutex<Vec<UsageRecord>>,
    }

    impl RecordingStore {
        pub(crate) fn records(&self) -> Vec<UsageRecord> {
            self.records.lock().expect("store 锁被毒化").clone()
        }
    }

    #[async_trait]
    impl GatewayStore for RecordingStore {
        async fn record_usage(&self, rec: UsageRecord) -> Result<()> {
            self.records.lock().expect("store 锁被毒化").push(rec);
            Ok(())
        }
        async fn recent_summaries(&self, _limit: usize) -> Result<Vec<RequestSummary>> {
            Ok(vec![])
        }
    }

    struct FixedPricing;

    impl PricingEngine for FixedPricing {
        fn resolve(
            &self,
            _model: &str,
            _provider: &ProviderId,
            _cred: &CredentialId,
        ) -> ResolvedPrice {
            ResolvedPrice {
                rates: [(TokenKind::Input, INPUT_RATE), (TokenKind::Output, OUTPUT_RATE)]
                    .into_iter()
                    .collect(),
                multiplier: 1.0,
                currency: "USD".into(),
                divisor: 1_000_000.0,
                source: PriceSourceKind::Manual,
                plan_fee: None,
                entry_index: 0,
            }
        }
    }

    /// 真 [`UsageMeter`] + 可读回的 store。
    pub(crate) fn recording_meter() -> (UsageMeter, Arc<RecordingStore>) {
        let store = Arc::new(RecordingStore::default());
        let meter = UsageMeter::new(
            store.clone(),
            Arc::new(ManualClock::new(1_700_000_000_000)),
            Arc::new(FixedPricing),
        );
        (meter, store)
    }

    /// 把 spawn 出去的落库任务推到完成（`#[tokio::test]` 默认单线程运行时）。
    pub(crate) async fn drain_tasks() {
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;

    use futures_util::StreamExt;
    use pretty_assertions::assert_eq;
    use serde_json::json;
    use ta_core::types::{ChannelId, CredentialId, LaneId, ProviderId, TokenKind, UsageSource};

    const REQUEST_BODY: &str =
        r#"{"model":"claude-sonnet-4-5","messages":[{"role":"user","content":"hello there"}]}"#;

    fn ctx(protocol: Protocol, stream: bool, status: u16) -> MeterContext {
        MeterContext {
            request_id: "req-1".into(),
            provider_id: ProviderId::new("p-a"),
            channel_id: ChannelId::new("c-a"),
            credential_id: CredentialId::new("cred-a"),
            model: Some("claude-sonnet-4-5".into()),
            upstream_model: "claude-sonnet-4-5".into(),
            lane_id: Some(LaneId::new("127.0.0.1:39101")),
            protocol,
            stream,
            status,
            request_body: Bytes::from_static(REQUEST_BODY.as_bytes()),
            request_started_at: std::time::Instant::now(),
            upstream_ms: 12,
        }
    }

    /// 真计量器 + 可读回的 store（假件在 [`test_support`]，管线侧测试也复用）。
    struct Harness {
        meter: UsageMeter,
        store: Arc<RecordingStore>,
    }

    fn harness() -> Harness {
        let (meter, store) = recording_meter();
        Harness { meter, store }
    }

    fn stream_of(chunks: &[&'static [u8]]) -> ByteStream {
        let items: Vec<Result<Bytes, BoxError>> =
            chunks.iter().map(|c| Ok(Bytes::from_static(c))).collect();
        Box::pin(futures_util::stream::iter(items))
    }

    async fn collect(stream: ByteStream) -> Vec<u8> {
        let mut out = Vec::new();
        let mut stream = stream;
        while let Some(item) = stream.next().await {
            out.extend_from_slice(&item.expect("测试流不该出错"));
        }
        out
    }

    // ---------------------------------------------------------- 非流式

    #[tokio::test]
    async fn non_stream_records_usage_cost_snapshot_once() {
        let h = harness();
        let body = Bytes::from_static(
            br#"{"choices":[{"message":{"content":"hi"}}],"usage":{"prompt_tokens":1000,"completion_tokens":500,"total_tokens":1500}}"#,
        );

        h.meter.observe_full(ctx(Protocol::Openai, false, 200), &body, 200);
        drain_tasks().await;

        let records = h.store.records();
        assert_eq!(records.len(), 1, "一次请求恰好一条用量记录");
        let r = &records[0];
        assert_eq!(r.request_id.as_str(), "req-1");
        assert_eq!(r.provider_id.as_str(), "p-a");
        assert_eq!(r.channel_id.as_str(), "c-a");
        assert_eq!(r.credential_id.as_str(), "cred-a");
        assert_eq!(r.status, 200);
        assert_eq!(r.model, "claude-sonnet-4-5");
        assert_eq!(r.lane_id.as_ref().map(|l| l.as_str()), Some("127.0.0.1:39101"));
        assert_eq!(r.ttfb_ms, Some(12));
        assert_eq!(r.usage.get(&TokenKind::Input), 1000);
        assert_eq!(r.usage.get(&TokenKind::Output), 500);
        // 上游没给细分（详情字段）→ UpstreamPartial，而不是 Estimated
        assert_eq!(r.usage_source, UsageSource::UpstreamPartial);
        assert_eq!(r.usage_raw["total_tokens"], json!(1500));
        // 1000×3 / 1M + 500×15 / 1M = 0.003 + 0.0075
        assert!((r.cost.amount - 0.0105).abs() < 1e-12, "cost={}", r.cost.amount);
        assert_eq!(r.cost.currency, "USD");
        // 价格快照必须留下**当时**的费率（改价不改历史）
        assert_eq!(r.price_snapshot["rates"]["input"], json!(3.0));
        assert_eq!(r.price_snapshot["divisor"], json!(1_000_000.0));
    }

    #[tokio::test]
    async fn empty_usage_object_is_upstream_said_zero_not_our_guess() {
        let h = harness();
        let body = Bytes::from_static(br#"{"usage":{}}"#);
        h.meter.observe_full(ctx(Protocol::Openai, false, 200), &body, 200);
        drain_tasks().await;

        let records = h.store.records();
        assert_eq!(records.len(), 1);
        // ★ 关键：这是"上游明确给了一个空容器"（说 0），**不是**"我们不知道"。
        // 若这里变成 Estimated，就是拿估算冒充上游数据（铁律 4）。
        assert_eq!(records[0].usage_source, UsageSource::UpstreamPartial);
        assert_eq!(records[0].usage.total(), 0);
        assert_eq!(records[0].usage_raw, json!({}));
    }

    #[tokio::test]
    async fn missing_usage_is_estimated_and_marked_as_such() {
        let h = harness();
        let body = Bytes::from_static(br#"{"choices":[{"message":{"content":"hi"}}]}"#);
        h.meter.observe_full(ctx(Protocol::Openai, false, 200), &body, 200);
        drain_tasks().await;

        let records = h.store.records();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].usage_source, UsageSource::Estimated);
        assert!(records[0].usage.total() > 0, "估算至少要有输入 token（来自请求体）");
        assert_eq!(records[0].usage_raw, Value::Null, "没有上游证据时不许编一个出来");
    }

    #[tokio::test]
    async fn non_success_responses_are_not_metered() {
        let h = harness();
        let body = Bytes::from_static(br#"{"error":{"message":"bad request"}}"#);
        h.meter.observe_full(ctx(Protocol::Openai, false, 400), &body, 400);
        drain_tasks().await;

        assert!(h.store.records().is_empty(), "4xx 不该记用量（尝试链另有记录）");
    }

    // ---------------------------------------------------------- 流式

    #[tokio::test]
    async fn anthropic_stream_merges_message_start_and_delta_into_one_record() {
        let h = harness();
        // 真实 Anthropic 流：input 只在 `message_start.message.usage`，output 在 `message_delta.usage`
        let sse: &[u8] = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",",
            "\"usage\":{\"input_tokens\":4000,\"output_tokens\":1}}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"delta\":{\"text\":\"hi\"}}\n\n",
            "event: message_delta\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},",
            "\"usage\":{\"output_tokens\":250}}\n\n",
        )
        .as_bytes();

        // **故意在任意位置切断**：一行的 JSON 跨 chunk 也必须能拼回来
        let chunks: [&'static [u8]; 4] = [&sse[..7], &sse[7..40], &sse[40..137], &sse[137..]];
        let out = collect(
            h.meter.observe_stream(ctx(Protocol::Anthropic, true, 200), stream_of(&chunks)),
        )
        .await;

        // ① 直通：字节一个不多一个不少（计量绝不改流）
        assert_eq!(out, sse, "流式响应必须逐字节透传");
        drain_tasks().await;

        // ② 恰好一条记录，且 input **没有丢**（这正是 message_start 合并的意义）
        let records = h.store.records();
        assert_eq!(records.len(), 1, "一条流恰好一条记录");
        let r = &records[0];
        assert_eq!(r.usage.get(&TokenKind::Input), 4000, "message_start 的 input 必须留下");
        assert_eq!(r.usage.get(&TokenKind::Output), 250, "message_delta 是最终 output");
        assert_eq!(r.usage_source, UsageSource::UpstreamPartial);
        assert_eq!(r.usage_raw["input_tokens"], json!(4000));
        assert_eq!(r.usage_raw["output_tokens"], json!(250));
        assert_eq!(r.status, 200);
        assert_eq!(r.ttfb_ms, Some(12));
    }

    #[tokio::test]
    async fn stream_without_usage_is_estimated_once_and_passes_through() {
        let h = harness();
        let sse: &[u8] = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"he\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"llo\"}}]}\n\n",
            "data: [DONE]\n\n",
        )
        .as_bytes();
        let out =
            collect(h.meter.observe_stream(ctx(Protocol::Openai, true, 200), stream_of(&[sse])))
                .await;
        assert_eq!(out, sse);
        drain_tasks().await;

        let records = h.store.records();
        assert_eq!(records.len(), 1, "逐分片各调一次会得到多条 —— 必须只在流结束调一次");
        assert_eq!(records[0].usage_source, UsageSource::Estimated);
        assert!(records[0].usage.total() > 0);
        assert_eq!(records[0].usage_raw, Value::Null);
    }

    /// 流里出现**显式空** usage 容器 = 上游说 0，不是"我们不知道"。
    ///
    /// 这条守着 [`evidence_envelope`] 存在的理由：合并出来的容器必须**包回协议路径**，
    /// 否则 `{}` 会被 ta-core 的 `usage_is_absent` 认不出来 → 错记成一条估算。
    #[tokio::test]
    async fn stream_with_explicit_empty_usage_is_upstream_said_zero() {
        let h = harness();
        let sse: &[u8] = b"data: {\"choices\":[],\"usage\":{}}\n\ndata: [DONE]\n\n";
        collect(h.meter.observe_stream(ctx(Protocol::Openai, true, 200), stream_of(&[sse]))).await;
        drain_tasks().await;

        let records = h.store.records();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].usage_source, UsageSource::UpstreamPartial);
        assert_eq!(records[0].usage.total(), 0);
        assert_eq!(records[0].usage_raw, json!({}));
    }

    #[tokio::test]
    async fn gemini_stream_keeps_the_last_cumulative_usage_metadata() {
        let h = harness();
        let sse: &[u8] = concat!(
            "data: {\"usageMetadata\":{\"promptTokenCount\":100,\"candidatesTokenCount\":10,",
            "\"totalTokenCount\":110}}\n\n",
            "data: {\"usageMetadata\":{\"promptTokenCount\":100,\"candidatesTokenCount\":50,",
            "\"totalTokenCount\":150}}\n\n",
        )
        .as_bytes();
        collect(h.meter.observe_stream(ctx(Protocol::Gemini, true, 200), stream_of(&[sse]))).await;
        drain_tasks().await;

        let records = h.store.records();
        assert_eq!(records.len(), 1);
        // 累计值 → 后者为准（不是把两个 chunk 相加）
        assert_eq!(records[0].usage.get(&TokenKind::Input), 100);
        assert_eq!(records[0].usage.get(&TokenKind::Output), 50);
        assert_eq!(records[0].usage.total(), 150, "必须等于上游最后报的总数");
        assert_eq!(records[0].usage_raw["totalTokenCount"], json!(150));
    }

    #[tokio::test]
    async fn dropped_stream_still_records_exactly_once() {
        let h = harness();
        let sse: &[u8] = concat!(
            "data: {\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":5,",
            "\"total_tokens\":15}}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"never consumed\"}}]}\n\n",
        )
        .as_bytes();

        {
            let mut stream =
                h.meter.observe_stream(ctx(Protocol::Openai, true, 200), stream_of(&[sse]));
            // 读一个 chunk 就走（= 客户端中途断连 / 请求被取消）
            let first = stream.next().await.expect("第一块必须立刻可用").expect("不该出错");
            assert_eq!(first.as_ref(), sse);
            drop(stream);
        }
        drain_tasks().await;

        let records = h.store.records();
        assert_eq!(records.len(), 1, "被丢弃的流也必须留下恰好一条（有始有终）");
        assert_eq!(records[0].usage.get(&TokenKind::Input), 10);
        assert_eq!(records[0].usage.get(&TokenKind::Output), 5);
    }

    #[tokio::test]
    async fn stream_ends_with_upstream_error_still_records_once() {
        let h = harness();
        let items: Vec<Result<Bytes, BoxError>> = vec![
            Ok(Bytes::from_static(
                b"data: {\"usage\":{\"prompt_tokens\":4,\"completion_tokens\":3,\"total_tokens\":7}}\n\n",
            )),
            Err(Box::new(std::io::Error::other("上游中途断连")) as BoxError),
        ];
        let stream: ByteStream = Box::pin(futures_util::stream::iter(items));

        let mut wrapped = h.meter.observe_stream(ctx(Protocol::Openai, true, 200), stream);
        // 消费到 Err 为止
        let mut seen_err = false;
        while let Some(item) = wrapped.next().await {
            if item.is_err() {
                seen_err = true;
            }
        }
        assert!(seen_err, "测试流必须走到 Err");
        drop(wrapped);
        drain_tasks().await;

        let records = h.store.records();
        assert_eq!(records.len(), 1, "Err 结束 + drop 只能结算一次");
        assert_eq!(records[0].usage.get(&TokenKind::Input), 4);
        assert_eq!(records[0].usage.get(&TokenKind::Output), 3);
        assert_eq!(records[0].usage_source, UsageSource::UpstreamPartial);
    }

    #[tokio::test]
    async fn non_success_stream_is_not_metered_but_still_passes_through() {
        let h = harness();
        let sse: &[u8] = b"data: {\"error\":\"nope\"}\n\n";
        let out =
            collect(h.meter.observe_stream(ctx(Protocol::Openai, true, 400), stream_of(&[sse])))
                .await;
        assert_eq!(out, sse);
        drain_tasks().await;
        assert!(h.store.records().is_empty());
    }

    #[tokio::test]
    async fn error_body_is_not_mistaken_for_usage() {
        let h = harness();
        // 有些中转在 200 里塞错误体：没有 usage 证据 → 估算，且 raw 为空
        let sse: &[u8] = b"data: {\"error\":{\"message\":\"upstream boom\"}}\n\n";
        collect(h.meter.observe_stream(ctx(Protocol::Openai, true, 200), stream_of(&[sse]))).await;
        drain_tasks().await;
        let records = h.store.records();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].usage_source, UsageSource::Estimated);
        assert_eq!(records[0].usage_raw, Value::Null);
    }

    // ---------------------------------------------------------- 容器口径

    /// **路径口径全部来自 `ta_core::metering::usage_paths` 那一张表**，
    /// 首个命中的对象胜出。这条测试守的是**规则**（表里有什么路径就该命中什么），
    /// 不是实现细节 —— 所以它逐条覆盖表里的形态，而不是断言"有没有信封回退"。
    ///
    /// Anthropic 那两条尤其重要：`message_start`（input/缓存）与 `message_delta`（output）
    /// 分散在两个事件里，而合并成最终形态的正是 [`UsageAccumulator`]；
    /// 端到端的守门人是 `anthropic_stream_merges_message_start_and_delta_into_one_record`
    /// （它的 RED 实证：把这条路径从表里拿掉 → input 丢成 0）。
    #[test]
    fn container_lookup_uses_only_the_ta_core_path_table() {
        // OpenAI 的 `/usage`
        assert_eq!(
            container_of(Protocol::Openai, &json!({"usage":{"prompt_tokens":5}})),
            Some(json!({"prompt_tokens":5}))
        );
        // Responses 的 `/response/usage`
        assert_eq!(
            container_of(
                Protocol::OpenaiResponses,
                &json!({"type":"response.completed","response":{"usage":{"input_tokens":7}}})
            ),
            Some(json!({"input_tokens":7}))
        );
        // Anthropic 的 `/message/usage`（`message_start` 的真实形态，第 12 轮补进表里）
        assert_eq!(
            container_of(
                Protocol::Anthropic,
                &json!({"type":"message_start","message":{"usage":{"input_tokens":9}}})
            ),
            Some(json!({"input_tokens":9}))
        );
        // Anthropic 的 `/usage`（`message_delta` 的真实形态）
        assert_eq!(
            container_of(
                Protocol::Anthropic,
                &json!({"type":"message_delta","usage":{"output_tokens":3}})
            ),
            Some(json!({"output_tokens":3}))
        );
        // 两条同时出现 → 表里的**第一条**（`/usage`）胜出，与 ta-core 的取值顺序一致
        assert_eq!(
            container_of(
                Protocol::Anthropic,
                &json!({"usage":{"output_tokens":3},"message":{"usage":{"input_tokens":9}}})
            ),
            Some(json!({"output_tokens":3}))
        );
        // Gemini 的 `/usageMetadata`
        assert_eq!(
            container_of(Protocol::Gemini, &json!({"usageMetadata":{"totalTokenCount":11}})),
            Some(json!({"totalTokenCount":11}))
        );
        // 没有证据
        assert_eq!(container_of(Protocol::Openai, &json!({"choices":[]})), None);
        assert_eq!(container_of(Protocol::Openai, &json!({"usage":null})), None);
        assert_eq!(container_of(Protocol::Anthropic, &json!({"type":"content_block_delta"})), None);
    }

    #[test]
    fn evidence_envelope_puts_the_container_back_on_the_protocol_path() {
        assert_eq!(
            evidence_envelope(Protocol::Openai, json!({"total_tokens":1})),
            json!({"usage":{"total_tokens":1}})
        );
        assert_eq!(
            evidence_envelope(Protocol::Gemini, json!({"totalTokenCount":1})),
            json!({"usageMetadata":{"totalTokenCount":1}})
        );
    }

    #[test]
    fn trim_ascii_handles_crlf_and_blank_lines() {
        assert_eq!(trim_ascii(b"  data: {}\r\n"), b"data: {}");
        assert_eq!(trim_ascii(b"\t \r"), b"");
    }
}
