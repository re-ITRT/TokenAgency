//! `ta-subscription` — 订阅下载、格式嗅探、版本与回滚。
//!
//! 契约：。**实现由工作包 P6 完成。**
//!
//! ★ 必须使用**独立 Client（不走 lane）** —— 鸡生蛋：不先拿到订阅就不知道有哪些节点。
//!
//! 本包只做**业务层**（五件事）：下载 / userinfo / 版本与回滚 / 坏更新拦截 /
//! 订阅级过滤 + 预览用的节点与分组清单。**节点协议与 Clash 配置解析归内核（mihomo）**，
//! 这里的解析只是为了"预览、计数、防坏更新"，不追求覆盖全部可选字段。

pub mod auto;
pub mod detect;
pub mod fetch;
pub mod parse;
pub mod schedule;
pub mod sync;
pub mod traffic;
pub mod validate;
pub mod version;

#[cfg(test)]
pub(crate) mod testutil;

use serde::{Deserialize, Serialize};

use ta_core::domain::Subscription;
use ta_core::error::Result;

pub use auto::{
    choose_auto_export, AutoCandidate, AutoDecision, KeepReason, NoneReason, PickReason,
};
pub use schedule::{due_subscriptions, SubscriptionSchedule};
pub use traffic::SubscriptionTraffic;

/// 默认 UA：很多机场按 `User-Agent` 返回不同格式（clash / surge / v2ray），默认伪装 Clash 客户端。
pub const DEFAULT_USER_AGENT: &str = "clash-verge/v2.0.0";

/// 默认保留版本数（版本表）。
pub const DEFAULT_KEEP_VERSIONS: u32 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DetectedFormat {
    ClashYaml,
    Base64Links,
    PlainLinks,
    Html,
    Unknown,
}

impl DetectedFormat {
    pub fn as_str(&self) -> &'static str {
        match self {
            DetectedFormat::ClashYaml => "clash_yaml",
            DetectedFormat::Base64Links => "base64_links",
            DetectedFormat::PlainLinks => "plain_links",
            DetectedFormat::Html => "html",
            DetectedFormat::Unknown => "unknown",
        }
    }

    /// 链接清单形态（没有 `proxy-groups`，分组由业务层兜底生成）。
    pub fn is_link_list(&self) -> bool {
        matches!(self, DetectedFormat::Base64Links | DetectedFormat::PlainLinks)
    }

    /// 能不能拿来用（Html / Unknown 一律视为坏更新）。
    pub fn is_usable(&self) -> bool {
        !matches!(self, DetectedFormat::Html | DetectedFormat::Unknown)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubUserinfo {
    pub upload: u64,
    pub download: u64,
    pub total: u64,
    pub expire_ms: Option<i64>,
}

impl SubUserinfo {
    /// 已用流量（上传 + 下载）。
    pub fn used(&self) -> u64 {
        self.upload.saturating_add(self.download)
    }

    /// 剩余可用比例 `0.0..=1.0`；`total == 0`（不限量/未声明）返回 `None`。
    pub fn remaining_ratio(&self) -> Option<f64> {
        if self.total == 0 {
            return None;
        }
        let used = self.used().min(self.total);
        Some((self.total - used) as f64 / self.total as f64)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeMeta {
    pub name: String,
    pub kind: String,
    pub server: String,
    pub port: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    /// `subscription-userinfo` → 流量 / 到期（[`traffic`]）。
    ///
    /// ★ **头不存在 = `None`**，与"头在但字段一个都认不出"（`Some` + 全 `None`）是两件事：
    /// 前者是"这次没给"，后者是"给了但我们读不懂" —— UI 对这两种情况的说法可以不同。
    /// 字段级语义见 [`SubscriptionTraffic`]：**未知一律 `None`，绝不用 0 冒充**。
    pub traffic: Option<SubscriptionTraffic>,
    pub suggested_interval_s: Option<u64>,
    pub nodes: Vec<NodeMeta>,
    pub groups: Vec<GroupMeta>,
}

impl FetchOutcome {
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    pub fn group_count(&self) -> usize {
        self.groups.len()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcceptDecision {
    Accept,
    /// 坏更新：保留上版（HTML / 空列表 / 节点数骤降）
    Reject(String),
}

impl AcceptDecision {
    pub fn is_accept(&self) -> bool {
        matches!(self, AcceptDecision::Accept)
    }

    /// 给 UI / 日志看的原因；采纳时为 `None`。
    pub fn reason(&self) -> Option<&str> {
        match self {
            AcceptDecision::Accept => None,
            AcceptDecision::Reject(why) => Some(why.as_str()),
        }
    }
}

#[async_trait::async_trait]
pub trait SubscriptionFetcher: Send + Sync {
    async fn fetch(&self, s: &Subscription) -> Result<FetchOutcome>;
}

pub trait SubscriptionValidator: Send + Sync {
    fn accept(&self, prev: Option<&FetchOutcome>, next: &FetchOutcome) -> AcceptDecision;
}
