//! 粘性存储：`sticky` 表（会话 key → 通道）。
//!
//! 契约：`docs/11-internal-contracts.md` §1.3（`StickyStore` get/put/release/gc）、
//! `docs/02-config-and-data-model.md` §3。
//!
//! 语义（对应 `ChannelGroup.sticky` 与 `ReleaseReason`）：
//! - 一个 key 同时只粘一个组（与 §3 草案的 `key TEXT PRIMARY KEY` 一致）；
//! - `get` 只返回**未过期**的绑定，过期即视为未命中并顺手删掉；
//! - 命中会累加 `hits`（用于看"粘性到底有没有在省事"）；
//! - `release` 是"主动解绑"（额度耗尽 / 硬错误 / 用户改 pin）。

use async_trait::async_trait;
use sqlx::{Row, SqlitePool};

use ta_core::error::Result;
use ta_core::types::{ChannelGroupId, ChannelId, StickyKey};

use crate::util::StoreResultExt;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StickyEntry {
    pub key: String,
    pub group_id: String,
    pub channel_id: String,
    pub created_at_ms: i64,
    pub expires_at_ms: i64,
    pub hits: i64,
}

#[async_trait]
pub trait StickyStore: Send + Sync {
    /// 查绑定；已过期 → `None`（并清理该行）。
    async fn get(
        &self,
        key: &StickyKey,
        group: &ChannelGroupId,
        now_ms: i64,
    ) -> Result<Option<ChannelId>>;
    /// 写入/刷新绑定（`ttl_ms` 从 `now_ms` 起算）。
    async fn put(
        &self,
        key: &StickyKey,
        group: &ChannelGroupId,
        channel: &ChannelId,
        ttl_ms: i64,
        now_ms: i64,
    ) -> Result<()>;
    /// 主动解绑（额度耗尽 / 硬错误 / pin 改变）。
    async fn release(&self, key: &StickyKey, group: &ChannelGroupId) -> Result<u64>;
    /// 清掉某个组的全部粘性（UI 的"清粘性"）。
    async fn release_group(&self, group: &ChannelGroupId) -> Result<u64>;
    /// 过期清理，返回删除行数。
    async fn gc(&self, now_ms: i64) -> Result<u64>;
    /// 当前有效绑定（调试 / UI 展示）。
    async fn list(&self, now_ms: i64, limit: u32) -> Result<Vec<StickyEntry>>;
}

pub struct SqliteStickyStore {
    pool: SqlitePool,
}

impl SqliteStickyStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }
}

#[async_trait]
impl StickyStore for SqliteStickyStore {
    async fn get(
        &self,
        key: &StickyKey,
        group: &ChannelGroupId,
        now_ms: i64,
    ) -> Result<Option<ChannelId>> {
        let row = sqlx::query("SELECT channel_id, expires_at FROM sticky WHERE key = ?")
            .bind(key.as_str())
            .fetch_optional(&self.pool)
            .await
            .store_err()?;
        let Some(row) = row else { return Ok(None) };

        let expires_at: i64 = row.get("expires_at");
        if expires_at <= now_ms {
            // 过期即解绑：避免"粘在一个已经不可用的通道上"
            sqlx::query("DELETE FROM sticky WHERE key = ?")
                .bind(key.as_str())
                .execute(&self.pool)
                .await
                .store_err()?;
            return Ok(None);
        }

        // 组不匹配 = 未命中（同一个 key 换了组）
        let stored_group: String = sqlx::query_scalar("SELECT group_id FROM sticky WHERE key = ?")
            .bind(key.as_str())
            .fetch_one(&self.pool)
            .await
            .store_err()?;
        if stored_group != group.as_str() {
            return Ok(None);
        }

        sqlx::query("UPDATE sticky SET hits = hits + 1 WHERE key = ?")
            .bind(key.as_str())
            .execute(&self.pool)
            .await
            .store_err()?;
        Ok(Some(ChannelId::new(row.get::<String, _>("channel_id"))))
    }

    async fn put(
        &self,
        key: &StickyKey,
        group: &ChannelGroupId,
        channel: &ChannelId,
        ttl_ms: i64,
        now_ms: i64,
    ) -> Result<()> {
        let expires = now_ms.saturating_add(ttl_ms.max(0));
        sqlx::query(
            "INSERT INTO sticky (key, group_id, channel_id, created_at, expires_at, hits)
             VALUES (?, ?, ?, ?, ?, 0)
             ON CONFLICT(key) DO UPDATE SET
                group_id = excluded.group_id,
                channel_id = excluded.channel_id,
                created_at = excluded.created_at,
                expires_at = excluded.expires_at",
        )
        .bind(key.as_str())
        .bind(group.as_str())
        .bind(channel.as_str())
        .bind(now_ms)
        .bind(expires)
        .execute(&self.pool)
        .await
        .store_err()?;
        Ok(())
    }

    async fn release(&self, key: &StickyKey, group: &ChannelGroupId) -> Result<u64> {
        let res = sqlx::query("DELETE FROM sticky WHERE key = ? AND group_id = ?")
            .bind(key.as_str())
            .bind(group.as_str())
            .execute(&self.pool)
            .await
            .store_err()?;
        Ok(res.rows_affected())
    }

    async fn release_group(&self, group: &ChannelGroupId) -> Result<u64> {
        let res = sqlx::query("DELETE FROM sticky WHERE group_id = ?")
            .bind(group.as_str())
            .execute(&self.pool)
            .await
            .store_err()?;
        Ok(res.rows_affected())
    }

    async fn gc(&self, now_ms: i64) -> Result<u64> {
        let res = sqlx::query("DELETE FROM sticky WHERE expires_at <= ?")
            .bind(now_ms)
            .execute(&self.pool)
            .await
            .store_err()?;
        Ok(res.rows_affected())
    }

    async fn list(&self, now_ms: i64, limit: u32) -> Result<Vec<StickyEntry>> {
        let rows = sqlx::query(
            "SELECT key, group_id, channel_id, created_at, expires_at, hits
             FROM sticky WHERE expires_at > ? ORDER BY expires_at DESC LIMIT ?",
        )
        .bind(now_ms)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await
        .store_err()?;
        Ok(rows
            .into_iter()
            .map(|r| StickyEntry {
                key: r.get("key"),
                group_id: r.get("group_id"),
                channel_id: r.get("channel_id"),
                created_at_ms: r.get("created_at"),
                expires_at_ms: r.get("expires_at"),
                hits: r.get("hits"),
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn put_get_expire_release() {
        let pool = crate::open_memory().await.unwrap();
        let store = SqliteStickyStore::new(pool);
        let key = StickyKey::new("k1");
        let group = ChannelGroupId::new("grp-1");
        let ch = ChannelId::new("ch-a");

        assert!(store.get(&key, &group, 1_000).await.unwrap().is_none());
        store.put(&key, &group, &ch, 5_000, 1_000).await.unwrap();
        assert_eq!(store.get(&key, &group, 1_500).await.unwrap(), Some(ChannelId::new("ch-a")));
        // 命中次数累加
        let entry = &store.list(2_000, 10).await.unwrap()[0];
        assert_eq!(entry.hits, 1);
        // 过期 → 未命中且被清理
        assert!(store.get(&key, &group, 9_999).await.unwrap().is_none());
        assert!(store.list(9_999, 10).await.unwrap().is_empty());

        // 主动释放
        store.put(&key, &group, &ch, 5_000, 1_000).await.unwrap();
        assert_eq!(store.release(&key, &group).await.unwrap(), 1);
        assert!(store.get(&key, &group, 1_500).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn group_mismatch_is_a_miss_and_gc_cleans() {
        let pool = crate::open_memory().await.unwrap();
        let store = SqliteStickyStore::new(pool);
        let key = StickyKey::new("k1");
        store.put(&key, &ChannelGroupId::new("g1"), &ChannelId::new("c1"), 1_000, 0).await.unwrap();
        assert!(store.get(&key, &ChannelGroupId::new("g2"), 10).await.unwrap().is_none());
        assert_eq!(store.gc(2_000).await.unwrap(), 1);
    }
}
