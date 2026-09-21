//! 迁移。**工作包 P4 负责实现**：用 `sqlx::migrate!` 加载 `migrations/*.sql`。
//!
//! 契约：`docs/11-internal-contracts.md` §4.1。迁移只增不改。

use sqlx::SqlitePool;

use ta_core::error::Result;

pub async fn run(_pool: &SqlitePool) -> Result<()> {
    // TODO(P4): sqlx::migrate!("./migrations").run(pool).await
    Ok(())
}
