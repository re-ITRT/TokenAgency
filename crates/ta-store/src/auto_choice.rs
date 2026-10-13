//! 「自动」出口的**粘性状态**：一家服务商上次选中哪个出口**地址**。
//!
//! 为什么需要它：用户第 88 轮那句"自动选择的地址具有粘性 —— 订阅更新之后如果存在一个代理的
//! IP(地址)和上次选的一样，就选上次选的"。要"和上次比"，就得先**记住上次**。
//!
//! 三条设计决定（都不是随手选的）：
//!
//! * **按服务商存**，不是按线路：A 期已经定死"一个服务商一个出口"（同一服务商下的模型打同一个
//!   URL，换出口 IP 会被上游当成同账号多地并发）。粘性跟着服务商，正好是同一条纪律的延伸；
//! * **存地址（IP/主机名），不存节点名**：机场改名很常见，按名字比会把"同一个出口"误判成"换了节点"，
//!   于是每次订阅更新都换 IP —— 那正是要避免的；
//! * **复用既有的 `store_setting` KV 表**（`key` / `value` / `updated_at`），**不新建表、不加迁移**：
//!   这张表本来就是这个用途，加一张只有两列的表只会多一份要维护的 schema。
//!
//! 与 [`crate::sticky_store`] 的区别（别混）：那个是**请求级**粘性（请求键 → 通道，带 TTL 与命中数，
//! 给路由器用）；这里是**配置级**的"上次选了谁"，没有 TTL、也不参与路由决策 ——
//! 混在一起会让"清粘性"这种操作顺手把自动出口也清了。

use sqlx::SqlitePool;

use crate::util::StoreResultExt;
use ta_core::error::Result;

/// `store_setting` 里自动出口记录的键前缀：`auto_export:<provider_id>`。
const KEY_PREFIX: &str = "auto_export:";

fn key_of(provider_id: &str) -> String {
    format!("{KEY_PREFIX}{provider_id}")
}

/// 「自动」出口的粘性状态（SQLite 实现）。
#[derive(Debug, Clone)]
pub struct SqliteAutoChoiceStore {
    pool: SqlitePool,
}

impl SqliteAutoChoiceStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// 这家服务商上次选中的出口地址。
    ///
    /// `None` = **从来没有选过**（不是"选了空"）—— 调用方据此走"首次挑最低延迟"那一支。
    pub async fn get(&self, provider_id: &str) -> Result<Option<String>> {
        let key = key_of(provider_id);
        let value: Option<String> =
            sqlx::query_scalar("SELECT value FROM store_setting WHERE key = ?")
                .bind(&key)
                .fetch_optional(&self.pool)
                .await
                .store_err()?;
        // 空字符串按"没记过"处理：写不进去一个空地址，也不该被读成一个地址
        Ok(value.filter(|v| !v.is_empty()))
    }

    /// 记下这次选的地址（存在就覆盖）。
    pub async fn set(&self, provider_id: &str, address: &str, now_ms: i64) -> Result<()> {
        sqlx::query(
            "INSERT INTO store_setting (key, value, updated_at) VALUES (?, ?, ?)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        )
        .bind(key_of(provider_id))
        .bind(address)
        .bind(now_ms)
        .execute(&self.pool)
        .await
        .store_err()?;
        Ok(())
    }

    /// 忘掉这家服务商的记录（服务商被删 / 用户手动要求重挑时用）。
    pub async fn clear(&self, provider_id: &str) -> Result<()> {
        sqlx::query("DELETE FROM store_setting WHERE key = ?")
            .bind(key_of(provider_id))
            .execute(&self.pool)
            .await
            .store_err()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn store() -> SqliteAutoChoiceStore {
        let pool = crate::open_memory().await.unwrap();
        SqliteAutoChoiceStore::new(pool)
    }

    #[tokio::test]
    async fn never_chosen_is_none_not_an_empty_string() {
        let store = store().await;
        assert_eq!(store.get("p-a").await.unwrap(), None);
    }

    #[tokio::test]
    async fn set_then_get_round_trips_the_address() {
        let store = store().await;
        store.set("p-a", "203.0.113.7", 1_000).await.unwrap();
        assert_eq!(store.get("p-a").await.unwrap(), Some("203.0.113.7".to_string()));
    }

    #[tokio::test]
    async fn the_choice_is_per_provider_so_one_provider_never_leaks_into_another() {
        let store = store().await;
        store.set("p-a", "203.0.113.7", 1_000).await.unwrap();
        store.set("p-b", "198.51.100.9", 2_000).await.unwrap();
        assert_eq!(store.get("p-a").await.unwrap(), Some("203.0.113.7".to_string()));
        assert_eq!(store.get("p-b").await.unwrap(), Some("198.51.100.9".to_string()));
        // 前缀不会把 `p-a` 和 `p-a2` 搞混（键是拼出来的，不是 like 匹配）
        assert_eq!(store.get("p-a2").await.unwrap(), None);
        assert_eq!(store.get("p-").await.unwrap(), None);
    }

    #[tokio::test]
    async fn set_overwrites_the_previous_choice() {
        let store = store().await;
        store.set("p-a", "203.0.113.7", 1_000).await.unwrap();
        store.set("p-a", "203.0.113.8", 2_000).await.unwrap();
        assert_eq!(store.get("p-a").await.unwrap(), Some("203.0.113.8".to_string()));
    }

    #[tokio::test]
    async fn clear_forgets_only_that_provider() {
        let store = store().await;
        store.set("p-a", "203.0.113.7", 1_000).await.unwrap();
        store.set("p-b", "198.51.100.9", 2_000).await.unwrap();
        store.clear("p-a").await.unwrap();
        assert_eq!(store.get("p-a").await.unwrap(), None);
        assert_eq!(store.get("p-b").await.unwrap(), Some("198.51.100.9".to_string()));
    }

    #[tokio::test]
    async fn an_explicitly_blank_value_reads_back_as_never_chosen() {
        // 就算库里被手改成空串，也不许把它当成"上次选的是一个空地址"
        let store = store().await;
        store.set("p-a", "", 1_000).await.unwrap();
        assert_eq!(store.get("p-a").await.unwrap(), None);
    }
}
