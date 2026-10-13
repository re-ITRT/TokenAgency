//! 健康样本（`health_sample`，保留 24h）与密钥元数据（`secret_meta`）。
//!
//! 契约：（`HealthStore` append/series/latest、
//! `SecretMetaStore` upsert/list/touch）。
//!
//! **硬约束**：`secret_meta` 只存元数据 —— 表里没有 value 列，
//! key 的真值永远在系统钥匙串 / 加密文件。

use async_trait::async_trait;
use sqlx::{Row, SqlitePool};

use ta_core::error::Result;
use ta_core::types::LaneId;

use crate::timebucket;
use crate::util::StoreResultExt;

/// 健康样本保留 24h（02 §3 的保留策略）。
pub const HEALTH_RETENTION_MS: i64 = 24 * timebucket::MS_HOUR;

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct HealthSample {
    pub ts_ms: i64,
    pub lane_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u32>,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
}

impl HealthSample {
    pub fn new(ts_ms: i64, lane_id: impl Into<String>, ok: bool) -> Self {
        Self { ts_ms, lane_id: lane_id.into(), node: None, latency_ms: None, ok, status: None }
    }
}

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SecretMeta {
    pub name: String,
    pub created_at: i64,
    pub last_used: Option<i64>,
    /// 给 UI 看的提示（如 `sk-…9f2c` 的尾部 4 位、或来源标签），**不含明文**
    pub hint: String,
}

#[async_trait]
pub trait HealthStore: Send + Sync {
    /// 追加样本（单条）；批量请用 `append_many`。
    async fn append(&self, sample: &HealthSample) -> Result<()>;
    async fn append_many(&self, samples: &[HealthSample]) -> Result<()>;
    /// 时间区间内的样本（按时间升序，最多 `limit` 条）。
    async fn series(
        &self,
        lane: &LaneId,
        from_ms: i64,
        to_ms: i64,
        limit: u32,
    ) -> Result<Vec<HealthSample>>;
    /// 某条线路最近一条样本。
    async fn latest(&self, lane: &LaneId) -> Result<Option<HealthSample>>;
    /// 清理超过保留期的样本（默认 24h），返回删除行数。
    async fn gc(&self, now_ms: i64, retention_ms: i64) -> Result<u64>;
}

#[async_trait]
pub trait SecretMetaStore: Send + Sync {
    /// 登记/更新元数据（key 写入钥匙串之后调用）。
    async fn upsert(&self, name: &str, hint: &str, now_ms: i64) -> Result<()>;
    async fn get(&self, name: &str) -> Result<Option<SecretMeta>>;
    async fn list(&self) -> Result<Vec<SecretMeta>>;
    /// 记录"最近一次使用时间"。
    async fn touch(&self, name: &str, now_ms: i64) -> Result<()>;
    /// 删除元数据（`ta-secrets` 删掉钥匙串条目之后调用）。
    async fn delete(&self, name: &str) -> Result<u64>;
}

// ================================================================ HealthStore

pub struct SqliteHealthStore {
    pool: SqlitePool,
}

impl SqliteHealthStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }
}

#[async_trait]
impl HealthStore for SqliteHealthStore {
    async fn append(&self, sample: &HealthSample) -> Result<()> {
        sqlx::query(
            "INSERT INTO health_sample (ts, lane_id, node, latency_ms, ok, status)
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(sample.ts_ms)
        .bind(&sample.lane_id)
        .bind(sample.node.clone())
        .bind(sample.latency_ms.map(|v| v as i64))
        .bind(if sample.ok { 1_i64 } else { 0_i64 })
        .bind(sample.status.map(|v| v as i64))
        .execute(&self.pool)
        .await
        .store_err()?;
        Ok(())
    }

    async fn append_many(&self, samples: &[HealthSample]) -> Result<()> {
        if samples.is_empty() {
            return Ok(());
        }
        let mut tx = self.pool.begin().await.store_err()?;
        for s in samples {
            if let Err(e) = sqlx::query(
                "INSERT INTO health_sample (ts, lane_id, node, latency_ms, ok, status)
                 VALUES (?, ?, ?, ?, ?, ?)",
            )
            .bind(s.ts_ms)
            .bind(&s.lane_id)
            .bind(s.node.clone())
            .bind(s.latency_ms.map(|v| v as i64))
            .bind(if s.ok { 1_i64 } else { 0_i64 })
            .bind(s.status.map(|v| v as i64))
            .execute(&mut *tx)
            .await
            {
                let _ = tx.rollback().await;
                return Err(crate::util::store_err(e));
            }
        }
        tx.commit().await.store_err()
    }

    async fn series(
        &self,
        lane: &LaneId,
        from_ms: i64,
        to_ms: i64,
        limit: u32,
    ) -> Result<Vec<HealthSample>> {
        let rows = sqlx::query(
            "SELECT ts, lane_id, node, latency_ms, ok, status FROM health_sample
             WHERE lane_id = ? AND ts >= ? AND ts < ?
             ORDER BY ts ASC LIMIT ?",
        )
        .bind(lane.as_str())
        .bind(from_ms)
        .bind(to_ms)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await
        .store_err()?;
        Ok(rows.into_iter().map(map_health).collect())
    }

    async fn latest(&self, lane: &LaneId) -> Result<Option<HealthSample>> {
        let row = sqlx::query(
            "SELECT ts, lane_id, node, latency_ms, ok, status FROM health_sample
             WHERE lane_id = ? ORDER BY ts DESC, id DESC LIMIT 1",
        )
        .bind(lane.as_str())
        .fetch_optional(&self.pool)
        .await
        .store_err()?;
        Ok(row.map(map_health))
    }

    async fn gc(&self, now_ms: i64, retention_ms: i64) -> Result<u64> {
        let cutoff = now_ms.saturating_sub(retention_ms.max(0));
        let res = sqlx::query("DELETE FROM health_sample WHERE ts < ?")
            .bind(cutoff)
            .execute(&self.pool)
            .await
            .store_err()?;
        Ok(res.rows_affected())
    }
}

fn map_health(r: sqlx::sqlite::SqliteRow) -> HealthSample {
    HealthSample {
        ts_ms: r.get("ts"),
        lane_id: r.get("lane_id"),
        node: r.get("node"),
        latency_ms: r.get::<Option<i64>, _>("latency_ms").map(|v| v as u32),
        ok: r.get::<i64, _>("ok") != 0,
        status: r.get::<Option<i64>, _>("status").map(|v| v as u16),
    }
}

// ================================================================ SecretMetaStore

pub struct SqliteSecretMetaStore {
    pool: SqlitePool,
}

impl SqliteSecretMetaStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }
}

#[async_trait]
impl SecretMetaStore for SqliteSecretMetaStore {
    async fn upsert(&self, name: &str, hint: &str, now_ms: i64) -> Result<()> {
        sqlx::query(
            "INSERT INTO secret_meta (name, created_at, hint) VALUES (?, ?, ?)
             ON CONFLICT(name) DO UPDATE SET hint = excluded.hint",
        )
        .bind(name)
        .bind(now_ms)
        .bind(hint)
        .execute(&self.pool)
        .await
        .store_err()?;
        Ok(())
    }

    async fn get(&self, name: &str) -> Result<Option<SecretMeta>> {
        let row =
            sqlx::query("SELECT name, created_at, last_used, hint FROM secret_meta WHERE name = ?")
                .bind(name)
                .fetch_optional(&self.pool)
                .await
                .store_err()?;
        Ok(row.map(map_secret))
    }

    async fn list(&self) -> Result<Vec<SecretMeta>> {
        let rows =
            sqlx::query("SELECT name, created_at, last_used, hint FROM secret_meta ORDER BY name")
                .fetch_all(&self.pool)
                .await
                .store_err()?;
        Ok(rows.into_iter().map(map_secret).collect())
    }

    async fn touch(&self, name: &str, now_ms: i64) -> Result<()> {
        sqlx::query(
            "INSERT INTO secret_meta (name, created_at, last_used, hint) VALUES (?, ?, ?, '')
             ON CONFLICT(name) DO UPDATE SET last_used = excluded.last_used",
        )
        .bind(name)
        .bind(now_ms)
        .bind(now_ms)
        .execute(&self.pool)
        .await
        .store_err()?;
        Ok(())
    }

    async fn delete(&self, name: &str) -> Result<u64> {
        let res = sqlx::query("DELETE FROM secret_meta WHERE name = ?")
            .bind(name)
            .execute(&self.pool)
            .await
            .store_err()?;
        Ok(res.rows_affected())
    }
}

fn map_secret(r: sqlx::sqlite::SqliteRow) -> SecretMeta {
    SecretMeta {
        name: r.get("name"),
        created_at: r.get("created_at"),
        last_used: r.get("last_used"),
        hint: r.get("hint"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn health_series_latest_and_gc() {
        let pool = crate::open_memory().await.unwrap();
        let store = SqliteHealthStore::new(pool);
        let lane = LaneId::new("lane-jp");
        for (i, lat) in [80u32, 120, 60].iter().enumerate() {
            let mut s = HealthSample::new(1_000 + i as i64 * 10, "lane-jp", true);
            s.latency_ms = Some(*lat);
            s.node = Some("东京-03".into());
            store.append(&s).await.unwrap();
        }
        let series = store.series(&lane, 0, 10_000, 10).await.unwrap();
        assert_eq!(series.len(), 3);
        assert_eq!(series[2].latency_ms, Some(60));
        let latest = store.latest(&lane).await.unwrap().unwrap();
        assert_eq!(latest.latency_ms, Some(60));
        assert!(latest.ok);

        // 保留 24h：只删老的
        assert_eq!(store.gc(1_030 + HEALTH_RETENTION_MS, HEALTH_RETENTION_MS).await.unwrap(), 3);
        assert!(store.latest(&lane).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn secret_meta_never_stores_value() {
        let pool = crate::open_memory().await.unwrap();
        let store = SqliteSecretMetaStore::new(pool.clone());
        store.upsert("openai-personal", "sk-…9f2c", 100).await.unwrap();
        store.touch("openai-personal", 200).await.unwrap();
        let m = store.get("openai-personal").await.unwrap().unwrap();
        assert_eq!(m.created_at, 100);
        assert_eq!(m.last_used, Some(200));

        // 表里只有 4 列，不存在任何可以放明文的地方
        let cols: Vec<String> =
            sqlx::query_scalar("SELECT name FROM pragma_table_info('secret_meta')")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(cols, vec!["name", "created_at", "last_used", "hint"]);
        assert_eq!(store.list().await.unwrap().len(), 1);
        assert_eq!(store.delete("openai-personal").await.unwrap(), 1);
    }
}
