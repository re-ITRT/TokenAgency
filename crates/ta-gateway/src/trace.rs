//! trace 落库与耗时瀑布。**工作包 G4 负责实现**。
//!
//! 两个 sink：
//! - [`LoggingSink`]：把每一次上游尝试写进 `tracing`（默认件，让尝试链在应用日志里立刻可见）；
//! - [`TraceStoreSink`]：把**请求级** trace（尝试链 + 耗时 + `header_diff` + 最终状态）
//!   写进 `ta_store::TraceStore`（`trace` 表）—— 这是 **C4**"拿一个 `request_id` 筛出
//!   完整轨迹"真正可端到端演示的那一环。
//! - [`TeeAttemptSink`]：生产默认把两者串起来（日志 + 表）。
//!
//! 为什么 `PipelineDeps.sink` 是**必需**依赖（不给 `Option`）："接了但没人收"比没接更糟
//! （[docs/13 §4.5 L10]）—— 尝试链会从指缝里静默消失，日志里一行都不剩。

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use ta_store::{TraceAttempt, TraceRecord, TraceStore};

use crate::pipeline::{AttemptRecord, AttemptSink, RequestTraceSummary};

/// 把尝试记录写进应用日志的 sink（G4 落库之前的默认件）。
#[derive(Debug, Default)]
pub struct LoggingSink;

impl LoggingSink {
    pub fn new() -> Self {
        Self
    }
}

impl Clone for LoggingSink {
    fn clone(&self) -> Self {
        Self
    }
}

#[async_trait]
impl AttemptSink for LoggingSink {
    async fn on_attempt(&self, record: AttemptRecord) {
        // 字段名一律取 [docs/14 §5] 白名单里的名字：request_id / channel_id /
        // credential_id / lane_id / attempt / status / code / elapsed_ms。
        // **写错名字不会报错**，只会让"按 request_id 筛出一次请求的全部轨迹"静默失效
        // —— 所以这里宁可少写几个字段，也不自造名字。
        tracing::info!(
            request_id = %record.request_id,
            channel_id = %record.channel_id,
            credential_id = %record.credential_id,
            lane_id = record.lane_id.as_ref().map_or("-", |lane| lane.as_str()),
            attempt = record.attempt,
            status = record.status,
            code = record.error_code.as_deref(),
            elapsed_ms = record.latency_ms,
            "上游尝试结束"
        );
    }
}

/// 把请求级 trace 写进 `ta_store::TraceStore`（[docs/12 §4 G4] ①）。
///
/// 分工：`on_attempt` **攒**这条请求的尝试链（按 `request_id` 归拢），
/// `on_request_complete` **落一条** `TraceRecord` 并清掉缓冲。
///
/// 为什么不让 sink 自己判断"哪次尝试是最后一次"：只有管线知道请求什么时候结束
/// （失败链可能跨多个通道、流式响应还会在返回之后才结束）。猜"最后一次"会在慢请求上出错。
pub struct TraceStoreSink {
    store: Arc<dyn TraceStore>,
    /// request_id → 这条请求的尝试链
    pending: Mutex<BTreeMap<String, Vec<TraceAttempt>>>,
}

/// 单个请求在缓冲里最多留多少条尝试：**只在异常情况下生效**
/// （正常路径 `on_request_complete` 一定会来清）。设上限是为了"进程被杀/请求异常"
/// 时不让这张内存表无限长。
const MAX_PENDING_ATTEMPTS: usize = 64;

impl TraceStoreSink {
    pub fn new(store: Arc<dyn TraceStore>) -> Self {
        Self { store, pending: Mutex::new(BTreeMap::new()) }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, Vec<TraceAttempt>>> {
        // 锁中毒不该让网关瘫痪：取回内部值继续用
        match self.pending.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

#[async_trait]
impl AttemptSink for TraceStoreSink {
    async fn on_attempt(&self, record: AttemptRecord) {
        let attempt = TraceAttempt {
            channel: Some(record.channel_id.as_str().to_string()),
            lane: record.lane_id.as_ref().map(|lane| lane.as_str().to_string()),
            node: None,
            status: record.status,
            ms: record.latency_ms,
            // `reason` 只写**网关自身**判定的失败码（TA-*）：上游说的话由 `status` 表达，
            // 把两者混在一个字段里，UI 就分不清"上游说 429"和"我们判断线路不可用"。
            reason: record.error_code.clone(),
        };
        let mut guard = self.lock();
        let chain = guard.entry(record.request_id).or_default();
        chain.push(attempt);
        if chain.len() > MAX_PENDING_ATTEMPTS {
            chain.remove(0);
        }
    }

    async fn on_request_complete(&self, summary: RequestTraceSummary) {
        // 取走缓冲（锁在这个语句结束时就释放，**绝不跨 await 持锁**）
        let attempts = self.lock().remove(&summary.request_id).unwrap_or_default();

        let model = summary.model.clone().unwrap_or_else(|| summary.upstream_model.clone());
        let mut record = TraceRecord::new(
            ta_core::types::RequestId::new(summary.request_id.clone()),
            summary.ts_ms,
            model,
        );
        record.protocol_in = summary.protocol_in;
        record.protocol_out = summary.protocol_out;
        record.stream = summary.stream;
        record.chosen_channel = summary.chosen_channel.clone();
        record.attempts = attempts;
        record.header_diff = summary.header_diff.clone();
        record.status = summary.status;
        record.total_ms = summary.total_ms;
        record.ttfb_ms = summary.ttfb_ms;
        record.error = summary.error.clone();

        if let Err(err) = self.store.write(record).await {
            // 写 trace 失败**不能**影响已经答复给客户端的响应；但要按契约成对记 code
            // （[docs/14 §6.2]：报 TaError 必带 `code = e.code()`）。
            tracing::warn!(
                request_id = %summary.request_id,
                code = err.code(),
                error = %err,
                "写 trace 失败（响应已发出，不影响本次请求）"
            );
        }
    }
}

/// 把每次尝试 / 请求结束转发给多个 sink（生产默认 = 日志 + trace 表）。
pub struct TeeAttemptSink {
    sinks: Vec<Arc<dyn AttemptSink>>,
}

impl TeeAttemptSink {
    pub fn new(sinks: Vec<Arc<dyn AttemptSink>>) -> Self {
        Self { sinks }
    }
}

#[async_trait]
impl AttemptSink for TeeAttemptSink {
    async fn on_attempt(&self, record: AttemptRecord) {
        for sink in &self.sinks {
            sink.on_attempt(record.clone()).await;
        }
    }

    async fn on_request_complete(&self, summary: RequestTraceSummary) {
        for sink in &self.sinks {
            sink.on_request_complete(summary.clone()).await;
        }
    }
}
