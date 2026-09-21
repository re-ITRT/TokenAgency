//! `ta-store` — SQLite 持久化：配置、账本、trace、粘性、额度状态。
//!
//! 契约：`docs/11-internal-contracts.md` §4。
//! **实现由工作包 P4 完成**；此处仅定义对外接口与模块边界。

pub mod config_store;
pub mod health_store;
pub mod migrations;
pub mod sticky_store;
pub mod trace_store;
pub mod usage_store;

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
    let opts = SqliteConnectOptions::new()
        .in_memory(true)
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts)
        .await
        .map_err(|e| TaError::Store(format!("open memory db: {e}")))?;
    migrations::run(&pool).await?;
    Ok(pool)
}
