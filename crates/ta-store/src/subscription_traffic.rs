//! 订阅流量与更新时间（`subscription` 表的 5 个可空列）—— 契约 。
//!
//! 为什么需要它：`subscription-userinfo` 响应头只有**拉订阅那一瞬间**可得，而
//! `/network` 的订阅行要一直显示「更新时间 / 已用 / 总量」。不落库就只能每开一次
//! 页面重拉一次机场 —— 既慢又白烧流量。所以每次**成功刷新**写一份"上次已知"。
//!
//! ## 两条口径（与 `ta_subscription::traffic` 一字不差）
//!
//! * **未知 = NULL**，绝不用 0 冒充。`total_bytes` / `expire_at_s` 的 0 在解析层就已经
//!   归一成 NULL（不限量 / 不适用），我们只负责原样存；
//! * `upload_bytes` / `download_bytes` 的 0 是**真实观测值**（真没用过），存 0 不存 NULL。
//!
//! `traffic_at_ms` 既是「更新时间」，也是**这一行有没有流量数据的唯一判据**：
//! 它 NULL = 从未成功刷新过 → 命令返回 `traffic: null`（而不是"有对象但字段全 null"）。
//! 所以 `record` 永远写它，`map_traffic` 见它 NULL 就整体判无。
//!
//! ⚠️ 已知边界： 的 store trait 表里**没有**这一族
//! （那份契约是在"流量可见"这条需求之前冻结的），所以这里给的是具体实现而不是 trait。
//! 不自己发明契约名 —— 补契约归主管。

use serde::{Deserialize, Serialize};
use sqlx::{Row, SqlitePool};

use ta_core::error::Result;

use crate::util::StoreResultExt;

/// 订阅服务商给的流量信息。**未知一律 `None`。**
///
/// 这是 IPC 的线上形状（`src/shared/bindings.ts` 的 `SubscriptionTraffic`，
/// `RefreshResult.traffic` 与 `subscription_traffic` 都用它）。
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubscriptionTraffic {
    pub upload_bytes: Option<u64>,
    pub download_bytes: Option<u64>,
    /// 总量；不限量 / 未声明 → `None`（解析层已把 0 归一成 None）。
    pub total_bytes: Option<u64>,
    /// 到期时刻（Unix **秒**）；不适用 → `None`。
    pub expire_at_s: Option<i64>,
    /// 这一次拿到的时刻（Unix 毫秒）—— UI 的「更新时间」。**必填**。
    pub fetched_at_ms: i64,
}

/// `subscription_traffic` 命令的一行。
///
/// `traffic: None` 的语义是**这份订阅从未成功刷新过**（"尚未获取"）。
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubscriptionTrafficRow {
    pub id: String,
    pub traffic: Option<SubscriptionTraffic>,
}

/// `subscription` 表的流量快照读写。
pub struct SqliteSubscriptionTrafficStore {
    pool: SqlitePool,
}

impl SqliteSubscriptionTrafficStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// 写入"上次已知"（每次**成功刷新**调用；同 id 覆盖，不新增行）。
    ///
    /// 这个表**没有**外键到配置：`subscription` 表是运行期观测值的落点，配置真源是
    /// `config` 表（一行一实体的 JSON）。所以这里只写 id + 流量列，其余列走表默认值。
    pub async fn record(&self, id: &str, traffic: &SubscriptionTraffic) -> Result<()> {
        sqlx::query(
            "INSERT INTO subscription
                 (id, upload_bytes, download_bytes, total_bytes, expire_at_s, traffic_at_ms)
             VALUES (?, ?, ?, ?, ?, ?)
             ON CONFLICT(id) DO UPDATE SET
                 upload_bytes   = excluded.upload_bytes,
                 download_bytes = excluded.download_bytes,
                 total_bytes    = excluded.total_bytes,
                 expire_at_s    = excluded.expire_at_s,
                 traffic_at_ms  = excluded.traffic_at_ms",
        )
        .bind(id)
        .bind(traffic.upload_bytes.map(|v| v as i64))
        .bind(traffic.download_bytes.map(|v| v as i64))
        .bind(traffic.total_bytes.map(|v| v as i64))
        .bind(traffic.expire_at_s)
        .bind(traffic.fetched_at_ms)
        .execute(&self.pool)
        .await
        .store_err()?;
        Ok(())
    }

    /// 一份订阅的"上次已知"；**从未成功刷新过 → `None`**。
    pub async fn get(&self, id: &str) -> Result<Option<SubscriptionTraffic>> {
        let row = sqlx::query(
            "SELECT upload_bytes, download_bytes, total_bytes, expire_at_s, traffic_at_ms
             FROM subscription WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .store_err()?;
        Ok(row.and_then(map_traffic))
    }

    /// 全部"上次已知"（按 id 升序）。从未刷新过的**不在结果里** —— 调用方（命令层）
    /// 按配置里的订阅清单去查表，查不到就是 `traffic: null`。
    pub async fn list(&self) -> Result<Vec<SubscriptionTrafficRow>> {
        let rows = sqlx::query(
            "SELECT id, upload_bytes, download_bytes, total_bytes, expire_at_s, traffic_at_ms
             FROM subscription ORDER BY id",
        )
        .fetch_all(&self.pool)
        .await
        .store_err()?;
        Ok(rows
            .into_iter()
            .filter_map(|r| {
                let id: String = r.get("id");
                // 有行但 `traffic_at_ms` 为 NULL（旧库 / 别的写入方）→ 不进列表：
                // 它和"没这行"一样都是"从未成功刷新过"。
                let traffic = Some(map_traffic(r)?);
                Some(SubscriptionTrafficRow { id, traffic })
            })
            .collect())
    }
}

/// 行 → 流量对象。**判据是 `traffic_at_ms`**：它为 NULL 时整行没有流量数据。
///
/// `map_traffic` 要吃掉行（sqlx 的 `Row` 取值按列名借用），所以不接受 `&Row`。
fn map_traffic(r: sqlx::sqlite::SqliteRow) -> Option<SubscriptionTraffic> {
    // 负数在库里是脏数据（我们只写非负）：判成未知，而不是 `as u64` 环绕成天文数字。
    let bytes = |col: &str| r.get::<Option<i64>, _>(col).and_then(|v| u64::try_from(v).ok());
    Some(SubscriptionTraffic {
        upload_bytes: bytes("upload_bytes"),
        download_bytes: bytes("download_bytes"),
        total_bytes: bytes("total_bytes"),
        expire_at_s: r.get("expire_at_s"),
        fetched_at_ms: r.get::<Option<i64>, _>("traffic_at_ms")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn traffic(
        upload: Option<u64>,
        download: Option<u64>,
        total: Option<u64>,
        expire: Option<i64>,
        at_ms: i64,
    ) -> SubscriptionTraffic {
        SubscriptionTraffic {
            upload_bytes: upload,
            download_bytes: download,
            total_bytes: total,
            expire_at_s: expire,
            fetched_at_ms: at_ms,
        }
    }

    async fn store() -> (SqlitePool, SqliteSubscriptionTrafficStore) {
        let pool = crate::open_memory().await.unwrap();
        let store = SqliteSubscriptionTrafficStore::new(pool.clone());
        (pool, store)
    }

    /// 五个字段**无损往返**：写进去 —— 从库里读回来 —— 逐字段对。
    #[tokio::test]
    async fn record_then_read_back_round_trips_every_field() {
        let (pool, store) = store().await;
        let t = traffic(
            Some(123_456_789),
            Some(98_765_432_100),
            Some(139_586_437_120),
            Some(1_893_456_000),
            1_700_000_000_000,
        );
        store.record("sub-a", &t).await.unwrap();

        // ★ 不看返回值：直接从库里读回来
        let back = store.get("sub-a").await.unwrap().expect("写过就该读得到");
        assert_eq!(back, t);
        // 列里的原始值也要对（不是只对了我们自己拼出来的对象）
        let raw = sqlx::query(
            "SELECT upload_bytes, download_bytes, total_bytes, expire_at_s, traffic_at_ms
             FROM subscription WHERE id = 'sub-a'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let col = |c: &str| raw.get::<Option<i64>, _>(c);
        assert_eq!(col("upload_bytes"), Some(123_456_789));
        assert_eq!(col("download_bytes"), Some(98_765_432_100));
        assert_eq!(col("total_bytes"), Some(139_586_437_120));
        assert_eq!(col("expire_at_s"), Some(1_893_456_000));
        assert_eq!(col("traffic_at_ms"), Some(1_700_000_000_000));
    }

    /// 未知字段在库里必须留 NULL、读回来还是 `None`；而 `0` 是**真实观测值**，原样存回。
    #[tokio::test]
    async fn unknown_fields_stay_null_but_zero_stays_zero() {
        let (pool, store) = store().await;
        let t = traffic(Some(0), None, None, None, 1_700_000_000_000);
        store.record("sub-b", &t).await.unwrap();

        let back = store.get("sub-b").await.unwrap().unwrap();
        assert_eq!(back.upload_bytes, Some(0), "0 是观测值，不许被当成缺失");
        assert_eq!(back.download_bytes, None);
        assert_eq!(back.total_bytes, None, "不限量/未声明 → NULL");
        assert_eq!(back.expire_at_s, None, "不适用 → NULL");
        assert_eq!(back.fetched_at_ms, 1_700_000_000_000);

        let nulls: (Option<i64>, Option<i64>, Option<i64>) = sqlx::query_as(
            "SELECT download_bytes, total_bytes, expire_at_s FROM subscription WHERE id = 'sub-b'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(nulls, (None, None, None), "库里也得是 NULL，不是 0");
    }

    /// **从未刷新过 → `None`**（不是"全是 null 的对象"）。含"表里有行但没有流量列"这种。
    #[tokio::test]
    async fn never_refreshed_is_absent_not_all_null() {
        let (pool, store) = store().await;
        assert!(store.get("sub-none").await.unwrap().is_none());
        assert!(store.list().await.unwrap().is_empty());

        // 别的路径（旧 schema 时代 / 其它写入方）留下的行：有订阅、没流量
        sqlx::query("INSERT INTO subscription (id, name) VALUES ('sub-legacy', '旧行')")
            .execute(&pool)
            .await
            .unwrap();
        assert!(
            store.get("sub-legacy").await.unwrap().is_none(),
            "traffic_at_ms 为 NULL == 没有流量数据"
        );
        assert!(store.list().await.unwrap().is_empty(), "没有流量的行不该出现在列表里");
    }

    /// 再刷一次覆盖同一行（"上次已知"是**快照**，不是流水账）。
    #[tokio::test]
    async fn record_overwrites_last_known_in_place() {
        let (pool, store) = store().await;
        store.record("sub-c", &traffic(Some(1), Some(2), Some(100), None, 1_000)).await.unwrap();
        let newer = traffic(Some(10), Some(20), Some(200), Some(1_893_456_000), 2_000);
        store.record("sub-c", &newer).await.unwrap();

        assert_eq!(store.get("sub-c").await.unwrap().unwrap(), newer);
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM subscription WHERE id = 'sub-c'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(rows, 1, "同 id 覆盖，不该攒成两行");
    }

    /// `list()` 按 id 升序给出所有**有流量**的订阅（命令层据此填 `{id, traffic}[]`）。
    #[tokio::test]
    async fn list_returns_only_rows_with_traffic_in_id_order() {
        let (_pool, store) = store().await;
        store.record("sub-z", &traffic(None, None, None, None, 5)).await.unwrap();
        store.record("sub-a", &traffic(Some(7), None, None, None, 9)).await.unwrap();

        let rows = store.list().await.unwrap();
        assert_eq!(rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), vec!["sub-a", "sub-z"]);
        assert_eq!(rows[0].traffic.as_ref().unwrap().upload_bytes, Some(7));
        // 全未知但**拿到过**（头在、读不懂）：仍然是 Some，更新时间要留
        let z = rows[1].traffic.as_ref().unwrap();
        assert_eq!(z.fetched_at_ms, 5);
        assert!(z.upload_bytes.is_none() && z.total_bytes.is_none());
    }
}
