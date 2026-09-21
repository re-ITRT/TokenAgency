//! 网关用到的存储聚合 trait。**工作包 G4/G2 共同使用；实现由 `ta-store` 提供适配。**

use async_trait::async_trait;

use ta_core::error::Result;
use ta_core::types::{RequestSummary, UsageRecord};

#[async_trait]
pub trait GatewayStore: Send + Sync {
    async fn record_usage(&self, rec: UsageRecord) -> Result<()>;
    async fn recent_summaries(&self, limit: usize) -> Result<Vec<RequestSummary>>;
}
