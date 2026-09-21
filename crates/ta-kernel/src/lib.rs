//! `ta-kernel` — mihomo sidecar 的进程管理、配置生成、REST 控制、lane 端口分配。
//!
//! 契约：`docs/11-internal-contracts.md` §2。**实现由工作包 P5 完成。**
//!
//! 这里**绝不**做任何系统级网络设置：不写系统代理、不改路由表、不装 TUN。

pub mod config_gen;
pub mod ports;
pub mod process;
pub mod rest;

#[cfg(test)]
pub(crate) mod testutil;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use ta_core::error::Result;
use ta_core::types::LaneId;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum KernelState {
    Stopped,
    Starting,
    Ready,
    Restarting,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KernelStatus {
    pub state: KernelState,
    pub version: Option<String>,
    pub pid: Option<u32>,
    pub restarts: u32,
    pub last_error: Option<String>,
}

impl Default for KernelStatus {
    fn default() -> Self {
        Self {
            state: KernelState::Stopped,
            version: None,
            pid: None,
            restarts: 0,
            last_error: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvidersSnapshot {
    pub proxies: Vec<ProxyEntry>,
    pub providers: Vec<ProviderEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyEntry {
    pub name: String,
    pub kind: String,
    pub now: Option<String>,
    pub all: Vec<String>,
    pub history: Vec<DelaySample>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DelaySample {
    pub time: String,
    pub delay: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderEntry {
    pub name: String,
    pub vehicle_type: String,
    pub updated_at: Option<String>,
    pub node_count: usize,
}

#[derive(Debug, Clone, Default)]
pub struct PortMap {
    pub map: std::collections::BTreeMap<LaneId, u16>,
}

#[async_trait]
pub trait KernelController: Send + Sync {
    async fn status(&self) -> KernelStatus;
    async fn start(&self) -> Result<()>;
    async fn stop(&self) -> Result<()>;
    async fn restart(&self) -> Result<()>;
    /// 生成 + 落盘 + reload（需要重启时自动重启）
    async fn apply(&self) -> Result<KernelStatus>;
    /// `PUT /proxies/{group}` —— 组内切节点
    async fn set_node(&self, group: &str, node: &str) -> Result<()>;
    /// 把某条 lane 对应的组切到指定节点（`select` 组）—— 上层只认 lane，
    /// 「lane → 内核组名」的映射是内核层的细节（见 `docs/04` §8.2 的线路下拉/托盘切换）。
    async fn set_select(&self, lane: &LaneId, node: &str) -> Result<()>;
    /// `GET /proxies/{name}/delay`
    async fn delay_of(&self, name: &str, timeout_ms: u64) -> Result<u32>;
    /// `PUT /providers/proxies/{key}` —— 刷新订阅
    async fn refresh_provider(&self, key: &str) -> Result<()>;
    async fn snapshot(&self) -> Result<ProvidersSnapshot>;
    async fn log_tail(&self, lines: u32) -> Result<String>;
}

pub trait KernelConfigGen: Send + Sync {
    /// 领域模型 → 生成的 YAML。**必须校验 `listeners[].proxy` 引用的组名存在**，
    /// 否则内核会启动失败。
    fn generate(&self, doc: &ta_core::domain::ConfigDoc, ports: &PortMap) -> Result<String>;
}
