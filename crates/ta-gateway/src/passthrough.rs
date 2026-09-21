//! **全透传（本项目最重要的一个模块）。** 工作包 G1 负责实现。
//!
//! 只允许 4 处修改：`host` / `content-length` / 鉴权头 / hop-by-hop 剥离。
//! **禁止**用 `#[derive(Deserialize)]` 定义请求体结构——那会静默吞掉未知字段，
//! 也就是"把视觉干成瞎子"的根因。

use http::HeaderMap;

use ta_core::domain::{Channel, ConfigDoc, HeaderMode};
use ta_core::error::Result;

/// 客户端头 vs 上游收到的头的差异。**非空 = 网关有 bug**（自检，不是策略）。
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct HeaderDiff {
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub changed: Vec<(String, String, String)>,
}

impl HeaderDiff {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && self.changed.is_empty()
    }
}

pub trait PassthroughPolicy: Send + Sync {
    /// 整体转发 + 定点修改
    fn build_headers(
        &self,
        cfg: &ConfigDoc,
        client: &HeaderMap,
        channel: &Channel,
        secret: &str,
        upstream_url: &str,
    ) -> HeaderMap;

    /// `serde_json::Value` 定点 patch；解析失败则返回原字节（不阻断）
    fn patch_body(&self, raw: &[u8], channel: &Channel) -> Result<Vec<u8>>;

    fn diff_headers(&self, client: &HeaderMap, upstream: &HeaderMap) -> HeaderDiff;

    fn header_mode(&self, cfg: &ConfigDoc, channel: &Channel) -> HeaderMode;
}
