//! `ta-store` — SQLite 持久化：配置、账本、trace、粘性、额度状态。
//!
//! 契约：只存事实，不存策略；写路径全部走事务。
//!
//! 实现（P4）：
//! - [`config_store`] 一行一实体的 JSON 配置真源 + 快照/回滚；
//! - [`usage_store`] 账本：单事务写入 + 任意维度查询 + 看板 + CSV；
//! - [`trace_store`] 请求日志：zstd 压缩 BLOB、脱敏、双限清理（retention + max_rows/max_size）；
//! - [`sticky_store`] / [`health_store`] 会话粘性、健康样本、密钥元数据；
//! - [`subscription_traffic`] 订阅的"上次已知流量与更新时间"。

pub mod api_key;
pub mod auto_choice;
pub mod config_store;
pub mod health_store;
pub mod migrations;
pub mod model_catalog;
pub mod sticky_store;
pub mod subscription_traffic;
pub mod timebucket;
pub mod trace_store;
pub mod usage_store;
mod util;

pub use api_key::{ApiKeyCreated, ApiKeyView, SqliteApiKeyStore};
pub use auto_choice::SqliteAutoChoiceStore;
pub use config_store::{ConfigStore, SnapshotId, SnapshotMeta, SqliteConfigStore};
pub use health_store::{
    HealthSample, HealthStore, SecretMeta, SecretMetaStore, SqliteHealthStore,
    SqliteSecretMetaStore,
};
pub use model_catalog::{ModelCatalogRow, ModelCatalogStore, SqliteModelCatalogStore};
pub use sticky_store::{SqliteStickyStore, StickyEntry, StickyStore};
pub use subscription_traffic::{
    SqliteSubscriptionTrafficStore, SubscriptionTraffic, SubscriptionTrafficRow,
};
pub use timebucket::TimeBucket;
pub use trace_store::{
    ClearMode, LogPage, LogQuery, LogStats, SqliteTraceStore, StorageStats, TraceAttempt,
    TraceDetail, TraceLimits, TraceRecord, TraceRow, TraceStore,
};
pub use usage_store::{
    ApiKeyUsageRow, CacheStat, DashboardCards, DashboardData, GroupDim, LaneHealthItem, Metric,
    Num, Sparklines, SqliteUsageStore, StatusBar, TopModel, TrendPoint, UsageFilters, UsageFormat,
    UsageQuery, UsageRow, UsageSeriesPoint, UsageStatus, UsageStore, UsageTable,
};

use std::path::Path;

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;

use ta_core::error::{Result, TaError};

/// 打开（并在必要时创建）数据库，跑完迁移。
pub async fn open(db_path: &Path) -> Result<SqlitePool> {
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let opts = SqliteConnectOptions::new()
        .filename(db_path)
        .create_if_missing(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
        .synchronous(sqlx::sqlite::SqliteSynchronous::Normal)
        .foreign_keys(true)
        .busy_timeout(std::time::Duration::from_secs(10));

    let pool = SqlitePoolOptions::new()
        .max_connections(4)
        .connect_with(opts)
        .await
        .map_err(|e| TaError::Store(format!("open db: {e}")))?;

    migrations::run(&pool).await?;
    Ok(pool)
}

/// 打开一个纯内存库（测试用）。
pub async fn open_memory() -> Result<SqlitePool> {
    let opts = SqliteConnectOptions::new().in_memory(true).foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts)
        .await
        .map_err(|e| TaError::Store(format!("open memory db: {e}")))?;
    migrations::run(&pool).await?;
    Ok(pool)
}
