//! 分发：取 lane 的 client、发送、SSE 零缓冲转发、失败转移。**工作包 G2 负责实现**。
//!
//! 铁律：**已向客户端写出第一个字节后绝不重试**（只记 `stream_broken`）。

use async_trait::async_trait;
use http::HeaderMap;

use ta_core::error::Result;
use ta_core::types::LaneId;

/// 发往上游的一次请求（已经过 `passthrough::patch_body` 处理）。
#[derive(Debug, Clone)]
pub struct UpstreamRequest {
    pub method: String,
    pub url: String,
    pub headers: HeaderMap,
    /// 原始 body 字节（透传路径下是"原样 + 定点 patch"的结果）
    pub body: bytes::Bytes,
    pub stream: bool,
}

pub struct UpstreamResponse {
    pub status: http::StatusCode,
    pub headers: HeaderMap,
    /// 流式响应体；非流式时也可整体读取
    pub body: reqwest::Response,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct HealthReport {
    pub lane_id: LaneId,
    pub ok: bool,
    pub latency_ms: Option<u32>,
    pub error: Option<String>,
}

#[async_trait]
pub trait OutboundDialer: Send + Sync {
    async fn execute(&self, lane: &LaneId, req: UpstreamRequest) -> Result<UpstreamResponse>;
    async fn health(&self, lane: &LaneId) -> HealthReport;
}

// TODO(G2): KernelDialer（每 lane 一个 reqwest Client，绑定 127.0.0.1:3910x）
