//! `ta-subscription` — 订阅下载、格式嗅探、版本与回滚。
//!
//! 契约：`docs/11-internal-contracts.md` §3。**实现由工作包 P6 完成。**
//!
//! ★ 必须使用**独立 Client（不走 lane）** —— 鸡生蛋：不先拿到订阅就不知道有哪些节点。

pub mod detect;
pub mod fetch;
pub mod validate;

use serde::{Deserialize, Serialize};

use ta_core::domain::Subscription;
use ta_core::error::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DetectedFormat {
    ClashYaml,
    Base64Links,
    PlainLinks,
    Html,
    Unknown,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SubUserinfo {
    pub upload: u64,
    pub download: u64,
    pub total: u64,
    pub expire_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeMeta {
    pub name: String,
    pub kind: String,
    pub server: String,
    pub port: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupMeta {
    pub name: String,
    pub kind: String,
    pub node_count: usize,
}

#[derive(Debug, Clone)]
pub struct FetchOutcome {
    pub raw: Vec<u8>,
    pub format: DetectedFormat,
    pub userinfo: Option<SubUserinfo>,
    pub suggested_interval_s: Option<u64>,
    pub nodes: Vec<NodeMeta>,
    pub groups: Vec<GroupMeta>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcceptDecision {
    Accept,
    /// 坏更新：保留上版（HTML / 空列表 / 节点数骤降）
    Reject(String),
}

#[async_trait::async_trait]
pub trait SubscriptionFetcher: Send + Sync {
    async fn fetch(&self, s: &Subscription) -> Result<FetchOutcome>;
}

pub trait SubscriptionValidator: Send + Sync {
    fn accept(&self, prev: Option<&FetchOutcome>, next: &FetchOutcome) -> AcceptDecision;
}
