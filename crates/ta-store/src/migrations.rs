//! 迁移。用 `sqlx::migrate!` 加载 `migrations/*.sql`。
//!
//! 契约：。迁移只增不改。
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
        // 契约表都在（额度账本的三张表已由 0008 删除，不在此列）
        for t in [
            "config",
            "config_snapshot",
            "usage_request",
            "usage_daily",
            "api_key_usage",
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
        // 0008：额度账本的三张表**必须已经不在**了（功能下线，留着会被误当成还能用）
        for t in ["quota_state", "quota_window_state", "quota_window_bucket"] {
            let n: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name = ?",
            )
            .bind(t)
            .fetch_one(&pool)
            .await
            .unwrap();
            assert_eq!(n, 0, "0008 之后不该还有表 {t}");
        }
        // 0003 加的列必须在。
        // 全部**可空**：从未成功刷新过的订阅这几列就是 NULL —— UI 要能区分
        // "没数据"和"数据是 0"，所以这里连"列必须允许 NULL"一起钉死。
        let sub_cols: Vec<(String, i64)> =
            sqlx::query_as("SELECT name, \"notnull\" FROM pragma_table_info('subscription')")
                .fetch_all(&pool)
                .await
                .unwrap();
        for c in ["upload_bytes", "download_bytes", "total_bytes", "expire_at_s", "traffic_at_ms"] {
            let found = sub_cols.iter().find(|(n, _)| n == c);
            let (_, notnull) = found.unwrap_or_else(|| panic!("subscription 缺少列 {c}"));
            assert_eq!(*notnull, 0, "{c} 必须可空（未知 = NULL，不用 0 冒充）");
        }
        assert_eq!(v, 8, "迁移只增不改：最新版本号应为 8（0001…0007 + 0008 删额度表）");

        // 0004 加的**新表**必须在。
        // 连同可空性一起钉住 —— 这两条各有理由，不是抄一遍 DDL：
        //   * `last_used_at` **必须可空**：NULL = 从未用过（不是 0；0 会显示成"1970 用过"）；
        //   * `note` 可空：备注本来就可以不填；
        //   * 其余列不许为空（`id` 例外，见下）。
        let key_cols: Vec<(String, i64)> =
            sqlx::query_as("SELECT name, \"notnull\" FROM pragma_table_info('api_key')")
                .fetch_all(&pool)
                .await
                .unwrap();
        let col = |name: &str| {
            key_cols
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, notnull)| *notnull)
                .unwrap_or_else(|| panic!("api_key 缺少列 {name}"))
        };
        for c in ["name", "key_hash", "key_prefix", "enabled", "created_at"] {
            assert_eq!(col(c), 1, "{c} 不许为空（0004 的定义）");
        }
        assert_eq!(col("last_used_at"), 0, "last_used_at 必须可空：NULL = 从未用过（不是 0）");
        assert_eq!(col("note"), 0, "note 可选");
        // SQLite 在 rowid 表上不把 `TEXT PRIMARY KEY` 标成 NOT NULL（历史行为），
        // 所以 `id` 的 notnull 是 0 —— 这里如实钉住它，免得下一个人以为写错了。
        assert_eq!(col("id"), 0, "SQLite 的 TEXT PRIMARY KEY 不标 notnull（rowid 表的历史行为）");

        // `key_hash` 上的 UNIQUE 不是性能优化，是**安全语义**：同一明文不可能有两个 id。
        //
        // 注意 `id TEXT PRIMARY KEY` 也会产生一个唯一索引（`origin = 'pk'`），
        // 所以这里只认**约束建出来的**那一个（`origin = 'u'`），并核对它落在 `key_hash` 上。
        let unique_from_constraint: Vec<String> = sqlx::query_scalar(
            "SELECT name FROM pragma_index_list('api_key') WHERE \"unique\" = 1 AND origin = 'u'",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            unique_from_constraint.len(),
            1,
            "key_hash 上必须有一个由 UNIQUE 约束建的唯一索引（实际：{unique_from_constraint:?}）"
        );
        let indexed_columns: Vec<String> =
            sqlx::query_scalar("SELECT name FROM pragma_index_info(?)")
                .bind(&unique_from_constraint[0])
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(
            indexed_columns,
            vec!["key_hash".to_string()],
            "UNIQUE 必须落在 key_hash 上：同一个明文不可能有两个 id"
        );

        // 0006 加的**新表**必须在。
        //
        // 三条一起钉住，每条都有"为什么不能改"：
        //   * 主键必须是 `(provider_id, upstream_model)` 且**顺序不变**：它同时是 upsert 的冲突键、
        //     "这家有哪些模型"的查询路径；
        //   * `first_seen_ms` 不许为空（它是"第一次见到"，upsert 刻意不覆盖它）；
        //   * **不许有 `origin` 之类的"来源"列**：用户明确否决了手动添加（"这样会坏自动化逻辑"），
        //     目录里每一行都必须来自真实扫描 —— 谁哪天加回一列"manual"都得先过这条断言。
        let n: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name = 'provider_model_catalog'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(n, 1, "0006 缺少表 provider_model_catalog");

        let cat_cols: Vec<(String, i64)> = sqlx::query_as(
            "SELECT name, \"notnull\" FROM pragma_table_info('provider_model_catalog')",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        for c in [
            "provider_id",
            "upstream_model",
            "suggested_id",
            "seen_in_last_scan",
            "first_seen_ms",
            "last_seen_ms",
        ] {
            let found = cat_cols.iter().find(|(name, _)| name == c);
            let (_, notnull) = found.unwrap_or_else(|| panic!("provider_model_catalog 缺少列 {c}"));
            assert_eq!(*notnull, 1, "{c} 不许为空（0006 的定义）");
        }
        assert!(
            !cat_cols.iter().any(|(name, _)| name == "origin"),
            "目录里**不该**有来源列：手动添加已被否决（docs/25 §1），每一行都必须来自真实扫描"
        );

        let cat_pk: Vec<String> = sqlx::query_scalar(
            "SELECT name FROM pragma_table_info('provider_model_catalog') WHERE pk > 0 ORDER BY pk",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            cat_pk,
            vec!["provider_id".to_string(), "upstream_model".to_string()],
            "目录表主键必须是 (provider_id, upstream_model)，且顺序不能变（upsert 的冲突键）"
        );

        // 0007 加的列必须在。
        //
        // 为什么不能省：0006 只留了**我们派生**出来的 `suggested_id`，上游原文里的
        // `object` / `owned_by` / `created` / `permission` / `context_length` 一个都没记。
        // 于是（a）用户看不到上游到底声明了什么，（b）给模型填能力字段时没有可核对的来源
        // —— 只能靠猜，而"猜"就是编字段。
        //
        // 三条一起钉住：
        //   * 列在，类型 `TEXT`（人工排查与 SQLite 的 JSON 函数都吃文本）；
        //   * `NOT NULL` 且默认值就是 `'{}'`：**没有第三态** —— "没记到原文"就是 `{}`，
        //     不用 NULL 冒充"上游给了个空对象"，也不用空字符串（那不是合法 JSON）；
        //   * 主键列序**没被这列动过**（上面那条断言仍然成立）。
        let raw_col: Vec<(String, String, i64, Option<String>)> = sqlx::query_as(
            "SELECT name, type, \"notnull\", dflt_value FROM pragma_table_info('provider_model_catalog')
             WHERE name = 'raw_json'",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(raw_col.len(), 1, "0007 缺少列 raw_json（上游模型对象原文）");
        let (_, ty, notnull, dflt) = &raw_col[0];
        assert_eq!(ty, "TEXT", "raw_json 必须是 TEXT：原文是上游给的 JSON 文本");
        assert_eq!(*notnull, 1, "raw_json 不许为空：没记到原文就是 '{{}}'，不用 NULL 冒充");
        assert_eq!(
            dflt.as_deref(),
            Some("'{}'"),
            "raw_json 的默认值必须是 '{{}}'（老库升级上来的行靠它）"
        );
    }

    /// **老库（schema v6）必须能升到 v7**：只加一列，老行自动得到 `'{}'`，原有数据一字节不动。
    ///
    /// 造"老库"的办法是**真的把它退回去**（`DROP COLUMN` + 删掉迁移记录 + 退 `user_version`），
    /// 而不是手写一份 0006 的 DDL —— 那样测的是我抄的那份 DDL，不是真实的升级路径。
    #[tokio::test]
    async fn old_db_at_v6_upgrades_and_legacy_catalog_rows_get_an_empty_raw_object() {
        let pool = crate::open_memory().await.unwrap();

        sqlx::query("ALTER TABLE provider_model_catalog DROP COLUMN raw_json")
            .execute(&pool)
            .await
            .expect("SQLite 支持 DROP COLUMN（3.35+）");
        sqlx::query("DELETE FROM _sqlx_migrations WHERE version = 7")
            .execute(&pool)
            .await
            .expect("把 0007 的迁移记录也退回去");
        sqlx::query("PRAGMA user_version = 6").execute(&pool).await.unwrap();

        // 老库里的一行（写它的时候还没有 raw_json 这一列）
        sqlx::query(
            "INSERT INTO provider_model_catalog
               (provider_id, upstream_model, suggested_id, seen_in_last_scan, first_seen_ms, last_seen_ms)
             VALUES ('p1', 'legacy', 'legacy', 1, 10, 20)",
        )
        .execute(&pool)
        .await
        .unwrap();

        run(&pool).await.expect("老库必须能开（0007 只是 ALTER TABLE ADD COLUMN）");

        let v: i64 = sqlx::query_scalar("PRAGMA user_version").fetch_one(&pool).await.unwrap();
        assert_eq!(v, 8, "升级后版本号必须是 8");

        let (suggested, raw, first_seen): (String, String, i64) = sqlx::query_as(
            "SELECT suggested_id, raw_json, first_seen_ms FROM provider_model_catalog
             WHERE provider_id = 'p1' AND upstream_model = 'legacy'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(suggested, "legacy", "老行本身一个字节都不许动");
        assert_eq!(first_seen, 10, "老行的首见时间也不许动");
        assert_eq!(raw, "{}", "老行没有原文 → '{{}}'：不编字段，也不用 NULL 冒充");
    }
}
