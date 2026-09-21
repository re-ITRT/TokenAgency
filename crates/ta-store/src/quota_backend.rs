//! SQLite 版额度账本后端：实现 `ta_core::quota::backend::QuotaBackend`。
//!
//! 契约：`crates/ta-core/src/quota/backend.rs` —— 那个 trait 是**同步**的
//! （`ta-core` 不做 IO），所以这里的桥接方式是：
//!
//! ```text
//! 同步读：get/all/quota_config  →  内存快照（Arc<Mutex<..>>），永不阻塞
//! 同步写：put/remove/...        →  先改快照，再把命令丢进 mpsc（sync send，任何线程都能调）
//!                                     │
//!                    open() 时 spawn 的后台任务（与 pool 同一个 runtime）落库
//! ```
//!
//! 为什么**不**用"专用线程 + 自己的 runtime"或 `block_on`/`block_in_place`：
//! - `Runtime::block_on` 在异步上下文里直接 panic；`block_in_place` 在 current-thread
//!   运行时上同样 panic（单测常见）；
//! - 更关键的是：sqlx 的每条连接都有一个**绑定在创建它的 runtime 上**的后台任务，
//!   把 `SqlitePool` 拿到另一个 runtime 上查询会挂死（这正是第一版实现的坑，
//!   被 `works_inside_a_tokio_context` 测试抓到）。
//!
//! 一致性取舍（写清楚，避免踩坑）：
//! - 写入是**异步落库 + 立刻可见**：`put` 后马上 `get` 一定能读到（走快照）；
//!   崩溃可能丢掉最后几条排队中的写入 —— 关机/测试请调 [`SqliteQuotaBackend::flush`]。
//! - `get` 在 `open()` 返回后一定是"从库里载入过的"快照，不会把已耗尽的凭据误判成
//!   从未观测（那会导致重新启用失效 key）。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use sqlx::{Row, SqlitePool};
use tokio::sync::{mpsc, oneshot};

use ta_core::domain::QuotaConfig;
use ta_core::error::{Result, TaError};
use ta_core::quota::{QuotaBackend, QuotaRecord, QuotaState, QuotaStateKind};
use ta_core::types::CredentialId;

use crate::usage_store::{QuotaStateRow, SqliteUsageStore};
use crate::util::{from_json, to_i64, to_json, StoreResultExt};

enum Cmd {
    Put(Box<QuotaRecord>),
    Remove(CredentialId),
    SetConfig(CredentialId, Box<QuotaConfig>),
    /// 屏障：处理到这里说明前面的命令都已落库
    Barrier(oneshot::Sender<()>),
}

pub struct SqliteQuotaBackend {
    records: Arc<Mutex<HashMap<CredentialId, QuotaRecord>>>,
    configs: Arc<Mutex<HashMap<CredentialId, QuotaConfig>>>,
    tx: mpsc::UnboundedSender<Cmd>,
}

impl SqliteQuotaBackend {
    /// 打开后端：用**调用方的 runtime** 载入快照，并 spawn 一个同 runtime 的后台写入任务。
    ///
    /// 必须在 tokio 运行时里调用（`open()` 在运行时外会 panic —— 这是 sqlx 的约束，
    /// 不是我们的选择）。
    pub async fn open(pool: SqlitePool) -> Result<Self> {
        let (records, configs) = load_snapshot(&pool).await?;
        let (tx, mut rx) = mpsc::unbounded_channel::<Cmd>();
        tokio::spawn(async move {
            while let Some(cmd) = rx.recv().await {
                match cmd {
                    Cmd::Put(rec) => {
                        if let Err(e) = persist_record(&pool, &rec).await {
                            tracing::error!(code = e.code(), error = %e, "额度账本落库失败（内存快照仍然有效）");
                        }
                    }
                    Cmd::Remove(cred) => {
                        if let Err(e) = persist_remove(&pool, &cred).await {
                            tracing::error!(code = e.code(), error = %e, "额度账本删除失败");
                        }
                    }
                    Cmd::SetConfig(cred, cfg) => {
                        if let Err(e) = persist_quota_config(&pool, &cred, &cfg).await {
                            tracing::error!(code = e.code(), error = %e, "手工额度落库失败");
                        }
                    }
                    Cmd::Barrier(ack) => {
                        let _ = ack.send(());
                    }
                }
            }
        });
        Ok(Self {
            records: Arc::new(Mutex::new(records)),
            configs: Arc::new(Mutex::new(configs)),
            tx,
        })
    }

    /// 等待所有已入队的写入落库（测试 / 关机前调用）。
    pub async fn flush(&self) -> Result<()> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.tx
            .send(Cmd::Barrier(ack_tx))
            .map_err(|_| TaError::Store("额度账本后台任务已退出".into()))?;
        ack_rx.await.map_err(|_| TaError::Store("额度账本后台任务已退出".into()))
    }

    /// 重新灌入 C 源（手工额度）的快照。Tauri 层在 `config:changed` 时调用。
    pub async fn refresh_quota_configs(&self, pool: &SqlitePool) -> Result<()> {
        let cfgs = load_quota_configs(pool).await?;
        let mut guard = self.config_snapshot();
        guard.clear();
        guard.extend(cfgs);
        Ok(())
    }

    pub fn snapshot_len(&self) -> usize {
        self.snapshot().len()
    }

    fn snapshot(&self) -> std::sync::MutexGuard<'_, HashMap<CredentialId, QuotaRecord>> {
        // 账本绝不因为别的线程 panic 而死锁
        self.records.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn config_snapshot(&self) -> std::sync::MutexGuard<'_, HashMap<CredentialId, QuotaConfig>> {
        self.configs.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn send(&self, cmd: Cmd, what: &str) {
        if self.tx.send(cmd).is_err() {
            tracing::error!(op = what, "额度账本后台任务已退出，本次写入只保留在内存快照里");
        }
    }
}

impl QuotaBackend for SqliteQuotaBackend {
    fn get(&self, cred: &CredentialId) -> Option<QuotaRecord> {
        self.snapshot().get(cred).cloned()
    }

    fn put(&self, rec: &QuotaRecord) {
        self.snapshot().insert(rec.state.credential_id.clone(), rec.clone());
        self.send(Cmd::Put(Box::new(rec.clone())), "put");
    }

    fn all(&self) -> Vec<QuotaRecord> {
        self.snapshot().values().cloned().collect()
    }

    fn remove(&self, cred: &CredentialId) {
        self.snapshot().remove(cred);
        self.send(Cmd::Remove(cred.clone()), "remove");
    }

    fn quota_config(&self, cred: &CredentialId) -> Option<QuotaConfig> {
        self.config_snapshot().get(cred).cloned()
    }

    fn set_quota_config(&self, cred: &CredentialId, cfg: QuotaConfig) {
        self.config_snapshot().insert(cred.clone(), cfg.clone());
        self.send(Cmd::SetConfig(cred.clone(), Box::new(cfg)), "set_quota_config");
    }
}

// ================================================================ 行 ↔ QuotaRecord

/// `QuotaStateKind` ↔ `quota_state.state` 文本。
pub fn kind_str(kind: QuotaStateKind) -> &'static str {
    match kind {
        QuotaStateKind::Unknown => "unknown",
        QuotaStateKind::Ok => "ok",
        QuotaStateKind::Soft => "soft",
        QuotaStateKind::Exhausted => "exhausted",
    }
}

pub fn kind_from_str(s: &str) -> QuotaStateKind {
    match s {
        "ok" => QuotaStateKind::Ok,
        "soft" => QuotaStateKind::Soft,
        "exhausted" => QuotaStateKind::Exhausted,
        _ => QuotaStateKind::Unknown,
    }
}

impl QuotaStateRow {
    /// `QuotaRecord` → 行（**无损**：state + `hard_exhausted` + `invalid` 全部落库）。
    pub fn from_record(rec: &QuotaRecord) -> Self {
        let s = &rec.state;
        Self {
            credential_id: s.credential_id.as_str().to_string(),
            window_start: s.window_start_ms,
            window_end: s.window_end_ms.unwrap_or(0),
            used_tokens: to_i64(s.used_tokens),
            used_requests: to_i64(s.used_requests),
            used_cost: s.used_cost,
            currency: s.currency.clone(),
            reported_remaining: s.reported_remaining,
            reported_reset_at: s.reported_reset_at_ms,
            used_ratio: s.used_ratio,
            state: kind_str(s.kind).to_string(),
            hard_exhausted: rec.hard_exhausted,
            invalid: rec.invalid,
            updated_at: s.updated_at_ms,
        }
    }

    /// 行 → `QuotaRecord`。
    pub fn to_record(&self) -> QuotaRecord {
        QuotaRecord {
            state: QuotaState {
                credential_id: CredentialId::new(self.credential_id.clone()),
                kind: kind_from_str(&self.state),
                window_start_ms: self.window_start,
                window_end_ms: if self.window_end == 0 { None } else { Some(self.window_end) },
                used_tokens: self.used_tokens.max(0) as u64,
                used_requests: self.used_requests.max(0) as u64,
                used_cost: self.used_cost,
                currency: self.currency.clone(),
                reported_remaining: self.reported_remaining,
                reported_reset_at_ms: self.reported_reset_at,
                used_ratio: self.used_ratio,
                updated_at_ms: self.updated_at,
            },
            hard_exhausted: self.hard_exhausted,
            invalid: self.invalid,
        }
    }
}

// ================================================================ SQL

async fn load_snapshot(
    pool: &SqlitePool,
) -> Result<(HashMap<CredentialId, QuotaRecord>, HashMap<CredentialId, QuotaConfig>)> {
    let store = SqliteUsageStore::new(pool.clone());
    let rows = store.list_quota_states().await?;
    let mut records = HashMap::with_capacity(rows.len());
    for row in rows {
        let rec = row.to_record();
        records.insert(rec.state.credential_id.clone(), rec);
    }
    let configs = load_quota_configs(pool).await?.into_iter().collect();
    Ok((records, configs))
}

/// C 源（手工额度）的真源是配置表里的 `Credential.quota`。
async fn load_quota_configs(pool: &SqlitePool) -> Result<Vec<(CredentialId, QuotaConfig)>> {
    let rows = sqlx::query("SELECT json FROM config WHERE kind = 'credential' ORDER BY ord")
        .fetch_all(pool)
        .await
        .store_err()?;
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let cred: ta_core::domain::Credential = from_json(r.get::<String, _>("json").as_str())?;
        out.push((cred.id.clone(), cred.quota.clone()));
    }
    Ok(out)
}

async fn persist_record(pool: &SqlitePool, rec: &QuotaRecord) -> Result<()> {
    let store = SqliteUsageStore::new(pool.clone());
    store.upsert_quota_state(&QuotaStateRow::from_record(rec)).await
}

async fn persist_remove(pool: &SqlitePool, cred: &CredentialId) -> Result<()> {
    sqlx::query("DELETE FROM quota_state WHERE credential_id = ?")
        .bind(cred.as_str())
        .execute(pool)
        .await
        .store_err()?;
    Ok(())
}

/// 手工额度写回配置表里的 `Credential.quota`（定点 patch 那一个字段，其余字段原样保留）。
async fn persist_quota_config(
    pool: &SqlitePool,
    cred: &CredentialId,
    cfg: &QuotaConfig,
) -> Result<()> {
    let row = sqlx::query("SELECT json FROM config WHERE kind = 'credential' AND entity_id = ?")
        .bind(cred.as_str())
        .fetch_optional(pool)
        .await
        .store_err()?;
    let Some(row) = row else {
        tracing::warn!(credential_id = cred.as_str(), "手工额度对应的凭据不在配置里，跳过落库");
        return Ok(());
    };
    let json: String = row.get("json");
    let mut value: serde_json::Value = from_json(&json)?;
    let quota_value: serde_json::Value = from_json(&to_json(cfg)?)?;
    let Some(obj) = value.as_object_mut() else {
        return Err(TaError::Store(format!("凭据 {} 的配置不是 JSON 对象", cred.as_str())));
    };
    obj.insert("quota".to_string(), quota_value);
    sqlx::query("UPDATE config SET json = ? WHERE kind = 'credential' AND entity_id = ?")
        .bind(to_json(&value)?)
        .bind(cred.as_str())
        .execute(pool)
        .await
        .store_err()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config_store::{ConfigStore, SqliteConfigStore};
    use ta_core::domain::{ConfigDoc, Credential};
    use ta_core::types::ProviderId;

    fn cred(id: &str) -> CredentialId {
        CredentialId::new(id)
    }

    fn sample_record(id: &str) -> QuotaRecord {
        let mut rec = QuotaRecord::new(&cred(id));
        rec.invalid = true;
        rec.hard_exhausted = true;
        rec.state.kind = QuotaStateKind::Exhausted;
        rec.state.used_tokens = 12_345;
        rec.state.used_requests = 7;
        rec.state.used_cost = 1.25;
        rec.state.currency = "CNY".into();
        rec.state.used_ratio = Some(0.83);
        rec.state.reported_remaining = Some(1234.5);
        rec.state.reported_reset_at_ms = Some(1_770_000_000_000);
        rec.state.window_start_ms = 100;
        rec.state.window_end_ms = Some(200);
        rec.state.updated_at_ms = 300;
        rec
    }

    #[tokio::test]
    async fn put_get_roundtrip_keeps_flags() {
        let pool = crate::open_memory().await.unwrap();
        let backend = SqliteQuotaBackend::open(pool.clone()).await.unwrap();

        // 从未观测 → None（而不是"默认可用"）
        assert!(backend.get(&cred("c-1")).is_none());

        let rec = sample_record("c-1");
        backend.put(&rec);
        // 立刻可见（读内存快照，不依赖后台任务跑完）
        let back = backend.get(&cred("c-1")).expect("put 之后必须能读到");
        assert!(back.invalid, "invalid 必须无损往返");
        assert!(back.hard_exhausted, "hard_exhausted 必须无损往返");
        assert_eq!(back.state.kind, QuotaStateKind::Exhausted);
        assert_eq!(back.state.used_tokens, 12_345);
        assert_eq!(back.state.used_requests, 7);
        assert!((back.state.used_cost - 1.25).abs() < 1e-12);
        assert_eq!(back.state.currency, "CNY");
        assert_eq!(back.state.used_ratio, Some(0.83));
        assert_eq!(back.state.reported_remaining, Some(1234.5));
        assert_eq!(back.state.reported_reset_at_ms, Some(1_770_000_000_000));
        assert_eq!(back.state.window_end_ms, Some(200));
        assert!(!back.never_observed());

        // 落库确认 → 重开一个后端（模拟进程重启）→ 两个位必须从库里读回来
        backend.flush().await.unwrap();
        drop(backend);
        let reopened = SqliteQuotaBackend::open(pool.clone()).await.unwrap();
        let after_restart = reopened.get(&cred("c-1")).expect("重启后必须从库里读回来");
        assert!(after_restart.invalid, "invalid 必须真的落库，不是只在内存里");
        assert!(after_restart.hard_exhausted);
        assert_eq!(reopened.all().len(), 1);

        // 窗口滚动：清用量、解除 hard_exhausted，但 **invalid 必须保留**
        let mut rolling = QuotaRecord::new(&cred("c-2"));
        rolling.invalid = true;
        rolling.hard_exhausted = true;
        rolling.state.used_tokens = 999;
        reopened.put(&rolling);
        reopened.flush().await.unwrap();
        let store = SqliteUsageStore::new(pool.clone());
        store.begin_quota_window("c-2", 0, 1000, "ok", 10).await.unwrap();
        let rolled = store.quota_state("c-2").await.unwrap().unwrap();
        assert_eq!(rolled.used_tokens, 0, "窗口滚动清用量");
        assert!(!rolled.hard_exhausted, "窗口滚动解除 hard_exhausted");
        assert!(rolled.invalid, "窗口滚动**不**解除 invalid（换周期不换 key）");
        assert_eq!(rolled.state, "ok");
        assert_eq!(rolled.used_ratio, None);

        // remove 落库
        reopened.remove(&cred("c-1"));
        reopened.flush().await.unwrap();
        drop(reopened);
        let again = SqliteQuotaBackend::open(pool.clone()).await.unwrap();
        assert!(again.get(&cred("c-1")).is_none(), "删除必须落库");
        assert_eq!(again.snapshot_len(), 1);
    }

    #[tokio::test]
    async fn quota_config_comes_from_config_table() {
        let pool = crate::open_memory().await.unwrap();
        let cfg_store = SqliteConfigStore::new(pool.clone());
        let mut doc = ConfigDoc::default();
        doc.credentials.push(Credential {
            id: cred("c-1"),
            provider: ProviderId::new("p-1"),
            label: "中转A".into(),
            secret: "${secret:a}".into(),
            lane: None,
            quota: QuotaConfig { limit: Some(5_000_000.0), ..Default::default() },
            guard: Default::default(),
            tags: vec![],
            enabled: true,
            extra: Default::default(),
        });
        cfg_store.save(&doc).await.unwrap();

        let backend = SqliteQuotaBackend::open(pool.clone()).await.unwrap();
        let q = backend.quota_config(&cred("c-1")).expect("C 源来自配置里的 Credential.quota");
        assert_eq!(q.limit, Some(5_000_000.0));
        assert!(backend.quota_config(&cred("nope")).is_none());

        // set_quota_config 要写回配置表（真源是 SQLite，不是内存）
        let mut updated = q.clone();
        updated.limit = Some(1.0);
        backend.set_quota_config(&cred("c-1"), updated);
        backend.flush().await.unwrap();
        drop(backend);

        let reloaded_configs = SqliteQuotaBackend::open(pool.clone()).await.unwrap();
        assert_eq!(reloaded_configs.quota_config(&cred("c-1")).unwrap().limit, Some(1.0));
        let doc = cfg_store.load().await.unwrap();
        assert_eq!(doc.credentials[0].quota.limit, Some(1.0), "定点 patch 只改 quota 字段");
        assert_eq!(doc.credentials[0].label, "中转A", "其它字段原样保留");
    }

    /// 关键回归：同步 trait 必须在 tokio 上下文里也能安全调用（不能嵌套 block_on）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sync_trait_is_safe_inside_tokio_and_from_plain_threads() {
        let pool = crate::open_memory().await.unwrap();
        let backend = Arc::new(SqliteQuotaBackend::open(pool.clone()).await.unwrap());

        let mut rec = QuotaRecord::new(&cred("c-9"));
        rec.invalid = true;
        backend.put(&rec);
        assert!(backend.get(&cred("c-9")).unwrap().invalid);

        // 从普通 OS 线程调用同步 trait 也要能工作（后台任务在别的 runtime 上跑）
        let b2 = Arc::clone(&backend);
        let handle = std::thread::spawn(move || {
            let mut rec = QuotaRecord::new(&cred("c-10"));
            rec.state.used_tokens = 5;
            b2.put(&rec);
            assert_eq!(b2.get(&cred("c-10")).unwrap().state.used_tokens, 5);
        });
        handle.join().unwrap();

        backend.flush().await.unwrap();
        let store = SqliteUsageStore::new(pool.clone());
        let rows = store.list_quota_states().await.unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().any(|r| r.credential_id == "c-9" && r.invalid));
        assert!(rows.iter().any(|r| r.credential_id == "c-10" && r.used_tokens == 5));
    }
}
