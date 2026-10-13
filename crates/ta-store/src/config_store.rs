//! 配置存储：`config` 表（一行一实体的 JSON）+ `config_snapshot`（快照 / 回滚）。
//!
//! 契约：、/§4。
//!
//! 设计要点：
//! - **真源是 SQLite**（02 §4）：YAML 只做导入导出，所以配置以 JSON 落库；
//! - 一行一实体 → 只改一个 provider 也只写一行，事务里"整体替换"保证一致性；
//! - `kind='meta'` 单独存 `version` 与未知顶层字段（`ConfigDoc::extra`，前向兼容）；
//! - `ord` 保留数组顺序（policies 的拖拽顺序、模型的展示顺序都靠它）。

use async_trait::async_trait;
use sqlx::{Row, SqlitePool};
use std::sync::Arc;

use ta_core::clock::{Clock, SystemClock};
use ta_core::domain::ConfigDoc;
use ta_core::error::{Result, TaError};

use crate::util::{from_json, to_json, StoreResultExt};

/// 快照 ID（`snap_<时间戳ms>_<序号>`，不依赖 uuid 依赖）。
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(
    Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(transparent)]
pub struct SnapshotId(pub String);

impl SnapshotId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SnapshotId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// 快照元数据（`config_history_list` 返回）。
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SnapshotMeta {
    pub snapshot_id: SnapshotId,
    pub label: String,
    pub created_at_ms: i64,
    pub version: u32,
    /// 文档 JSON 字节数
    pub bytes: u64,
}

#[async_trait]
pub trait ConfigStore: Send + Sync {
    /// 读完整配置树（空库 → `ConfigDoc::default()`）。
    async fn load(&self) -> Result<ConfigDoc>;
    /// 整体替换写入（**单事务**）。
    async fn save(&self, doc: &ConfigDoc) -> Result<()>;
    /// 留一份完整文档快照。
    async fn snapshot(&self, doc: &ConfigDoc, label: &str) -> Result<SnapshotId>;
    /// 最近 N 份快照，按时间倒序。
    async fn list_snapshots(&self, limit: u32) -> Result<Vec<SnapshotMeta>>;
    /// 回滚：把快照内容写回真源并返回该文档（**单事务**）。
    async fn rollback(&self, id: &SnapshotId) -> Result<ConfigDoc>;
}

pub struct SqliteConfigStore {
    pool: SqlitePool,
    clock: Arc<dyn Clock>,
}

impl SqliteConfigStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool, clock: Arc::new(SystemClock) }
    }

    /// 注入时钟（测试用；快照时间戳与排序依赖它）。
    pub fn with_clock(pool: SqlitePool, clock: Arc<dyn Clock>) -> Self {
        Self { pool, clock }
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// 只读某类实体（UI 的列表页、dashboard 的额度标签都走这里，避免整树解析）。
    pub async fn entities<T: serde::de::DeserializeOwned>(&self, kind: &str) -> Result<Vec<T>> {
        let rows = sqlx::query("SELECT json FROM config WHERE kind = ? ORDER BY ord")
            .bind(kind)
            .fetch_all(&self.pool)
            .await
            .store_err()?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            out.push(from_json::<T>(r.get::<String, _>("json").as_str())?);
        }
        Ok(out)
    }

    /// 只读单例子树（`gateway` / `request_log` / `pricing` / `net` / `system_integration`）。
    pub async fn section<T: serde::de::DeserializeOwned>(&self, name: &str) -> Result<Option<T>> {
        let row = sqlx::query("SELECT json FROM config WHERE kind = 'section' AND entity_id = ?")
            .bind(name)
            .fetch_optional(&self.pool)
            .await
            .store_err()?;
        match row {
            Some(r) => Ok(Some(from_json::<T>(r.get::<String, _>("json").as_str())?)),
            None => Ok(None),
        }
    }
}

/// 把 `ConfigDoc` 展开成 (kind, entity_id, ord, json) 行。
fn doc_rows(doc: &ConfigDoc) -> Result<Vec<(&'static str, String, i64, String)>> {
    let mut rows: Vec<(&'static str, String, i64, String)> = Vec::new();
    let meta = serde_json::json!({ "version": doc.version, "extra": doc.extra });
    rows.push(("meta", "root".to_string(), 0, to_json(&meta)?));
    rows.push(("section", "gateway".into(), 0, to_json(&doc.gateway)?));
    rows.push(("section", "request_log".into(), 0, to_json(&doc.request_log)?));
    rows.push(("section", "pricing".into(), 0, to_json(&doc.pricing)?));
    rows.push(("section", "net".into(), 0, to_json(&doc.net)?));
    rows.push(("section", "system_integration".into(), 0, to_json(&doc.system_integration)?));

    for (i, s) in doc.proxy_subscriptions.iter().enumerate() {
        rows.push(("subscription", s.id.as_str().to_string(), i as i64, to_json(s)?));
    }
    for (i, l) in doc.lanes.iter().enumerate() {
        rows.push(("lane", l.id.as_str().to_string(), i as i64, to_json(l)?));
    }
    for (i, p) in doc.providers.iter().enumerate() {
        rows.push(("provider", p.id.as_str().to_string(), i as i64, to_json(p)?));
    }
    for (i, c) in doc.credentials.iter().enumerate() {
        rows.push(("credential", c.id.as_str().to_string(), i as i64, to_json(c)?));
    }
    for (i, m) in doc.models.iter().enumerate() {
        rows.push(("model", m.id.clone(), i as i64, to_json(m)?));
    }
    for (i, c) in doc.channels.iter().enumerate() {
        rows.push(("channel", c.id.as_str().to_string(), i as i64, to_json(c)?));
    }
    for (i, g) in doc.channel_groups.iter().enumerate() {
        rows.push(("channel_group", g.id.as_str().to_string(), i as i64, to_json(g)?));
    }
    for (i, p) in doc.policies.iter().enumerate() {
        rows.push(("policy", p.id.as_str().to_string(), i as i64, to_json(p)?));
    }
    Ok(rows)
}

async fn write_rows(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    doc: &ConfigDoc,
    now: i64,
) -> Result<()> {
    sqlx::query("DELETE FROM config").execute(&mut **tx).await.store_err()?;
    for (kind, entity_id, ord, json) in doc_rows(doc)? {
        sqlx::query(
            "INSERT INTO config (kind, entity_id, ord, json, updated_at) VALUES (?, ?, ?, ?, ?)",
        )
        .bind(kind)
        .bind(entity_id)
        .bind(ord)
        .bind(json)
        .bind(now)
        .execute(&mut **tx)
        .await
        .store_err()?;
    }
    Ok(())
}

/// 从 `config` 表组装完整文档。
async fn read_doc(pool: &SqlitePool) -> Result<ConfigDoc> {
    let rows = sqlx::query("SELECT kind, entity_id, json FROM config ORDER BY kind, ord")
        .fetch_all(pool)
        .await
        .store_err()?;

    let mut doc = ConfigDoc::default();
    let mut seen_any = false;
    for r in rows {
        seen_any = true;
        let kind: String = r.get("kind");
        let entity_id: String = r.get("entity_id");
        let json: String = r.get("json");
        match kind.as_str() {
            "meta" => {
                let v: serde_json::Value = from_json(&json)?;
                if let Some(ver) = v.get("version").and_then(|x| x.as_u64()) {
                    doc.version = ver as u32;
                }
                if let Some(extra) = v.get("extra").and_then(|x| x.as_object()) {
                    doc.extra = extra.clone();
                }
            }
            "section" => match entity_id.as_str() {
                "gateway" => doc.gateway = from_json(&json)?,
                "request_log" => doc.request_log = from_json(&json)?,
                "pricing" => doc.pricing = from_json(&json)?,
                "net" => doc.net = from_json(&json)?,
                "system_integration" => doc.system_integration = from_json(&json)?,
                _ => {}
            },
            "subscription" => doc.proxy_subscriptions.push(from_json(&json)?),
            "lane" => doc.lanes.push(from_json(&json)?),
            "provider" => doc.providers.push(from_json(&json)?),
            "credential" => doc.credentials.push(from_json(&json)?),
            "model" => doc.models.push(from_json(&json)?),
            "channel" => doc.channels.push(from_json(&json)?),
            "channel_group" => doc.channel_groups.push(from_json(&json)?),
            "policy" => doc.policies.push(from_json(&json)?),
            other => tracing::warn!(kind = other, "配置表里出现未知 kind，已忽略"),
        }
    }
    if !seen_any {
        return Ok(ConfigDoc::default());
    }
    Ok(doc)
}

#[async_trait]
impl ConfigStore for SqliteConfigStore {
    async fn load(&self) -> Result<ConfigDoc> {
        read_doc(&self.pool).await
    }

    async fn save(&self, doc: &ConfigDoc) -> Result<()> {
        let now = self.clock.now_ms();
        let mut tx = self.pool.begin().await.store_err()?;
        write_rows(&mut tx, doc, now).await?;
        tx.commit().await.store_err()?;
        Ok(())
    }

    async fn snapshot(&self, doc: &ConfigDoc, label: &str) -> Result<SnapshotId> {
        let now = self.clock.now_ms();
        let seq: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM config_snapshot")
            .fetch_one(&self.pool)
            .await
            .store_err()?;
        let id = SnapshotId(format!("snap_{now}_{}", seq + 1));
        let doc_json = to_json(doc)?;
        let yaml = ta_core::config::to_yaml(doc).ok();
        sqlx::query(
            "INSERT INTO config_snapshot (snapshot_id, label, created_at, version, doc_json, yaml)
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(id.as_str())
        .bind(label)
        .bind(now)
        .bind(doc.version as i64)
        .bind(&doc_json)
        .bind(yaml)
        .execute(&self.pool)
        .await
        .store_err()?;
        Ok(id)
    }

    async fn list_snapshots(&self, limit: u32) -> Result<Vec<SnapshotMeta>> {
        let rows = sqlx::query(
            "SELECT snapshot_id, label, created_at, version, LENGTH(doc_json) AS bytes
             FROM config_snapshot ORDER BY created_at DESC, snapshot_id DESC LIMIT ?",
        )
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await
        .store_err()?;
        Ok(rows
            .into_iter()
            .map(|r| SnapshotMeta {
                snapshot_id: SnapshotId(r.get("snapshot_id")),
                label: r.get("label"),
                created_at_ms: r.get("created_at"),
                version: r.get::<i64, _>("version") as u32,
                bytes: r.get::<Option<i64>, _>("bytes").unwrap_or(0).max(0) as u64,
            })
            .collect())
    }

    async fn rollback(&self, id: &SnapshotId) -> Result<ConfigDoc> {
        let doc_json: Option<String> =
            sqlx::query_scalar("SELECT doc_json FROM config_snapshot WHERE snapshot_id = ?")
                .bind(id.as_str())
                .fetch_optional(&self.pool)
                .await
                .store_err()?;
        let Some(doc_json) = doc_json else {
            return Err(TaError::Store(format!("快照不存在：{}", id.as_str())));
        };
        let doc: ConfigDoc = from_json(&doc_json)?;
        let now = self.clock.now_ms();
        let mut tx = self.pool.begin().await.store_err()?;
        write_rows(&mut tx, &doc, now).await?;
        tx.commit().await.store_err()?;
        Ok(doc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ta_core::clock::FixedClock;
    use ta_core::domain::Provider;
    use ta_core::types::{Protocol, ProviderId};

    fn clock() -> Arc<dyn Clock> {
        Arc::new(FixedClock::new(1_770_000_000_000))
    }

    fn provider(id: &str, name: &str) -> Provider {
        Provider {
            id: ProviderId::new(id),
            name: name.into(),
            protocol: Protocol::Openai,
            base_url: "https://api.openai.com/v1".into(),
            auth: Default::default(),
            lane: None,
            headers: Default::default(),
            on_header_conflict: Default::default(),
            passthrough: Default::default(),
            timeout: Default::default(),
            tls: Default::default(),
            model_prefix: None,
            enabled: true,
            extra: Default::default(),
        }
    }

    #[tokio::test]
    async fn save_load_roundtrip_one_row_per_entity() {
        let pool = crate::open_memory().await.unwrap();
        let store = SqliteConfigStore::with_clock(pool.clone(), clock());

        let mut doc = ConfigDoc::default();
        doc.providers.push(provider("p-a", "A"));
        doc.providers.push(provider("p-b", "B"));
        doc.gateway.listen = "127.0.0.1:9999".into();
        doc.extra.insert("future_field".into(), serde_json::json!({"x": 1}));
        store.save(&doc).await.unwrap();

        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM config WHERE kind='provider'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(rows, 2, "一行一实体");

        let back = store.load().await.unwrap();
        assert_eq!(back.providers.len(), 2);
        assert_eq!(back.providers[0].id.as_str(), "p-a");
        assert_eq!(back.providers[1].id.as_str(), "p-b");
        assert_eq!(back.gateway.listen, "127.0.0.1:9999");
        assert_eq!(back.extra.get("future_field").unwrap()["x"], 1);
    }

    #[tokio::test]
    async fn second_save_replaces_atomically() {
        let pool = crate::open_memory().await.unwrap();
        let store = SqliteConfigStore::with_clock(pool.clone(), clock());
        let mut doc = ConfigDoc::default();
        doc.providers.push(provider("p-a", "A"));
        store.save(&doc).await.unwrap();
        doc.providers.clear();
        doc.providers.push(provider("p-b", "B"));
        store.save(&doc).await.unwrap();
        let back = store.load().await.unwrap();
        assert_eq!(back.providers.len(), 1);
        assert_eq!(back.providers[0].id.as_str(), "p-b");
    }

    #[tokio::test]
    async fn snapshot_list_and_rollback() {
        let pool = crate::open_memory().await.unwrap();
        let store = SqliteConfigStore::with_clock(pool, clock());
        let mut v1 = ConfigDoc::default();
        v1.providers.push(provider("p-a", "A"));
        store.save(&v1).await.unwrap();
        let id = store.snapshot(&v1, "before-change").await.unwrap();

        let mut v2 = ConfigDoc::default();
        v2.providers.push(provider("p-b", "B"));
        store.save(&v2).await.unwrap();
        assert_eq!(store.load().await.unwrap().providers[0].id.as_str(), "p-b");

        let list = store.list_snapshots(10).await.unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].label, "before-change");
        assert!(list[0].bytes > 0);

        let restored = store.rollback(&id).await.unwrap();
        assert_eq!(restored.providers[0].id.as_str(), "p-a");
        assert_eq!(store.load().await.unwrap().providers[0].id.as_str(), "p-a");

        let missing = SnapshotId("snap_nope".into());
        assert!(store.rollback(&missing).await.is_err());
    }

    #[tokio::test]
    async fn empty_db_yields_default_doc() {
        let pool = crate::open_memory().await.unwrap();
        let store = SqliteConfigStore::new(pool);
        let doc = store.load().await.unwrap();
        assert!(doc.providers.is_empty());
        // 版本号只有一个来源（起是 v2）：别在这里写死字面量
        assert_eq!(doc.version, ta_core::config::migrate::CURRENT_CONFIG_VERSION);
    }
}
