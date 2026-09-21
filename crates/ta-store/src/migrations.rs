//! 迁移。用 `sqlx::migrate!` 加载 `migrations/*.sql`。
//!
//! 契约：`docs/11-internal-contracts.md` §4.1。迁移只增不改。
//! 额外做两件文档要求的事：
//! - `PRAGMA user_version` 与最新迁移版本号保持一致（11 §4.1）；
//! - 建表前把 `auto_vacuum` 设为 `INCREMENTAL`，这样 `gc()` 才能用
//!   `PRAGMA incremental_vacuum` 回收空间而**不做全量 VACUUM**（07 §2.3）。

use sqlx::migrate::Migrator;
use sqlx::SqlitePool;

use ta_core::error::{Result, TaError};

static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

/// 跑完全部迁移。任何失败都返回 `TaError::Store`（调用方拒绝启动并提示备份路径）。
pub async fn run(pool: &SqlitePool) -> Result<()> {
    prepare_auto_vacuum(pool).await?;

    MIGRATOR.run(pool).await.map_err(|e| TaError::Store(format!("run migrations: {e}")))?;

    sync_user_version(pool).await
}

/// 期望的 schema 版本 = 最新迁移的版本号。
pub fn expected_version() -> i64 {
    MIGRATOR.iter().map(|m| m.version).max().unwrap_or(0)
}

async fn prepare_auto_vacuum(pool: &SqlitePool) -> Result<()> {
    let current: i64 = sqlx::query_scalar("PRAGMA auto_vacuum")
        .fetch_one(pool)
        .await
        .map_err(|e| TaError::Store(format!("read auto_vacuum: {e}")))?;
    // 2 = INCREMENTAL
    if current != 2 {
        sqlx::query("PRAGMA auto_vacuum = INCREMENTAL")
            .execute(pool)
            .await
            .map_err(|e| TaError::Store(format!("set auto_vacuum: {e}")))?;
        // 非空库上该 pragma 要等一次 VACUUM 才生效；我们**不做**全量 VACUUM（07 §2.3），
        // 只记录状态：此时 gc() 仍会删行，只是不回收文件空间。
        let after: i64 = sqlx::query_scalar("PRAGMA auto_vacuum")
            .fetch_one(pool)
            .await
            .map_err(|e| TaError::Store(format!("re-read auto_vacuum: {e}")))?;
        if after != 2 {
            tracing::debug!(
                "auto_vacuum 未能即时切换为 INCREMENTAL（已有表的老库需要一次 VACUUM），incremental_vacuum 将为空操作"
            );
        }
    }
    Ok(())
}

async fn sync_user_version(pool: &SqlitePool) -> Result<()> {
    let expected = expected_version();
    let current: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(pool)
        .await
        .map_err(|e| TaError::Store(format!("read user_version: {e}")))?;
    if current == expected {
        return Ok(());
    }
    if current != 0 {
        tracing::warn!(current, expected, "user_version 与迁移版本号不一致，已按迁移版本号修正");
    }
    // pragma 不接受绑定参数；expected 来自编译期嵌入的迁移文件，非用户输入。
    sqlx::query(&format!("PRAGMA user_version = {expected}"))
        .execute(pool)
        .await
        .map_err(|e| TaError::Store(format!("set user_version: {e}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn migrations_apply_and_user_version_matches() {
        let pool = crate::open_memory().await.unwrap();
        let v: i64 = sqlx::query_scalar("PRAGMA user_version").fetch_one(&pool).await.unwrap();
        assert_eq!(v, expected_version());
        assert!(v >= 1);
        // 12 张契约表都在
        for t in [
            "config",
            "config_snapshot",
            "usage_request",
            "usage_daily",
            "api_key_usage",
            "quota_state",
            "sticky",
            "trace",
            "health_sample",
            "secret_meta",
            "subscription",
            "subscription_version",
        ] {
            let n: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name = ?",
            )
            .bind(t)
            .fetch_one(&pool)
            .await
            .unwrap();
            assert_eq!(n, 1, "缺少表 {t}");
        }
        // 0002 加的列必须在（QuotaRecord 的两个状态机位，跨包缺口）
        let quota_cols: Vec<String> =
            sqlx::query_scalar("SELECT name FROM pragma_table_info('quota_state')")
                .fetch_all(&pool)
                .await
                .unwrap();
        for c in ["hard_exhausted", "invalid", "currency", "used_ratio"] {
            assert!(quota_cols.contains(&c.to_string()), "quota_state 缺少列 {c}");
        }
        assert_eq!(v, 2, "迁移只增不改：最新版本号应为 2");
    }
}
