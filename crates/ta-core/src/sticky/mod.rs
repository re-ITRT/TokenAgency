//! 会话粘性：同一会话固定走同一凭据。
//!
//! 为什么重要：换 key 会让 prompt cache 全 miss（成本和延迟都涨），
//! 也会让多个账号都被小额消耗、每个都"快用完但没用完"。

use serde::{Deserialize, Serialize};

use crate::domain::ReleaseReason;
use crate::types::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StickyRecord {
    pub key: StickyKey,
    pub group_id: ChannelGroupId,
    pub channel_id: ChannelId,
    pub created_at_ms: i64,
    pub expires_at_ms: i64,
    pub hits: u64,
}

impl StickyRecord {
    pub fn is_alive(&self, now_ms: i64) -> bool {
        self.expires_at_ms > now_ms
    }
}

pub trait StickyResolver: Send + Sync {
    /// 按 group.sticky.key 的顺序派生 key；全部失败返回 None
    fn key_for(&self, group: &crate::domain::ChannelGroup, hints: &crate::routing::RouteHints) -> Option<StickyKey>;

    fn lookup(&self, key: &StickyKey, group: &ChannelGroupId) -> Option<StickyRecord>;

    fn store(&self, key: StickyKey, group: ChannelGroupId, channel: ChannelId, ttl_ms: i64);

    /// 释放（额度耗尽 / 连续硬失败 / 冷却 / 用户手动）
    fn release(&self, key: &StickyKey, why: ReleaseReason);

    /// 用户手动 pin 到别的通道时，全局重写
    fn release_group(&self, group: &ChannelGroupId, why: ReleaseReason);

    fn gc(&self, now_ms: i64) -> u64;
}

/// `body_hash` 的派生规则：**首条 system 消息 + 首条 user 消息前 512 字符**。
/// 这样同一会话的多轮请求会稳定命中同一个 key。
pub fn derive_body_hash(system_first: Option<&str>, user_first: Option<&str>) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    if let Some(s) = system_first {
        h.update(s.as_bytes());
    }
    h.update(b"\x1f");
    if let Some(u) = user_first {
        let take = u.char_indices().take_while(|(i, _)| *i < 512).last();
        let cut = take.map(|(i, c)| i + c.len_utf8()).unwrap_or(0);
        h.update(u[..cut].as_bytes());
    }
    let out = h.finalize();
    // 只取前 8 字节，够用且短
    out[..8].iter().map(|b| format!("{b:02x}")).collect()
}
