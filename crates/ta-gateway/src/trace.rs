//! trace 落库与耗时瀑布。**工作包 G4 负责实现**。
//!
//! 目前这里只有 [`LoggingSink`]：把每一次上游尝试写进 `tracing`。
//!
//! 它存在的理由是条反例约束 —— `PipelineDeps.sink` 是**必需**依赖，而"接了但没人收"
//! 比没接更糟（[docs/13 §4.5 L10]）：尝试链会从指缝里静默消失，日志里一行都不剩，
//! 排查时还会得出"我们的代码没有报错"这种错误结论。
//! G4 的落库（trace 表 + 耗时瀑布 + 淘汰原因）在它之上叠加，不改管线。

use async_trait::async_trait;

use crate::pipeline::{AttemptRecord, AttemptSink};

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
