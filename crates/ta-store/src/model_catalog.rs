//! 服务商**模型目录**（`provider_model_catalog`）—— 扫描记录下来的上游模型（[docs/25]）。
//!
//! # 为什么它是一张**观察**表，而不是配置的一部分
//!
//! "这家服务商提供过哪些模型"是**上游告诉我们的**（一次真实扫描的结果），会随上游漂移；
//! 而"我要用哪些模型"是**用户意图**（配置里的 `Model` + 通道）。
//! 把观察塞进配置真源就是第二份真相源：扫描一次要整份配置重写 + 两份快照，
//! 而且 `config_export_yaml` 会把几百个上游模型名一起导出（[docs/25 §0]）。
//! 所以目录落 SQLite，与 `api_key` / `health_sample` 同类（[docs/02 §4]）。
//!
//! # 三条关键语义
//!
//! 1. **upsert 不覆盖 `first_seen_ms`**：`first_seen_ms` 是"第一次见到"，
//!    `last_seen_ms` 才是每次观测都刷新的。用户要能看出"这个模型我什么时候第一次扫到"。
//! 2. **重扫以本次为准**：`mark_all_unseen` → 逐条 `upsert`（置 `seen_in_last_scan = 1`）
//!    → `prune_unseen(keep)`。`keep` 由**调用方**给（= 这家还接着通道的上游名）：
//!    用户已经接过的东西不因为一次扫描消失，否则界面会出现"有通道、但目录里看不到这个模型"。
//! 3. **没有"手动添加"**（用户明确要求）：目录里的每一行都必须来自一次真实扫描，
//!    否则"目录 = 上游真实提供了什么"这条不变量就失效了，而自动化全建在它上面。
//! 4. **原文逐字留**：`raw_json` 存的是上游 `/models` 里那一条（`data[i]`）的**完整对象**
//!    （[docs/28 §3]"扫描时把上游那一条模型对象原样记下来，能读到的字段逐字存"），
//!    不是我们挑出来的字段、更不是重新拼的。上游没给的字段一个都不编：非对象/缺失落 `{}`。
//!    这样用户能看到上游真的声明了什么，给模型填能力时有可核对的来源，
//!    而 `/v1/models` 返回**我们的**模型时也不必拿目录去"二次创作"。
//!
//! 迁移：`0006_provider_model_catalog.sql` + `0007_provider_model_catalog_raw.sql`
//! （只增不改，[docs/11 §4.1]）。

use async_trait::async_trait;
use sqlx::{Row, SqlitePool};

use ta_core::error::Result;

use crate::util::StoreResultExt;

/// 目录里的一行（= 一对 `(provider_id, upstream_model)`）。
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ModelCatalogRow {
    pub provider_id: String,
    /// 上游真实模型名（扫描给的）。与本行所属的 `provider_id` 一起构成主键。
    pub upstream_model: String,
    /// 扫描派生出来的逻辑模型名（接入时以它建 `Model`）—— **唯一真源**，界面不能自己编。
    pub suggested_id: String,
    /// 上游 `/models` 里那一条的**完整对象**（`data[i]`），原样存、原样读。
    ///
    /// 为什么是整份对象而不是几个字段：上游给什么我们不知道（`owned_by` / `created` /
    /// `permission` / `context_length` / 中转自造字段……），只挑认识的存，
    /// 剩下的一辈子都找不回来。用户要看的是"上游真的声明了什么"（[docs/27 §1.4]）。
    ///
    /// **不是对象、或压根没有 → `{}`**：我们**不补字段**（补一个 `object: "model"`
    /// 就把"上游声明过什么"变成了我们的猜测）。读出来也永远是一个 JSON 对象。
    ///
    /// `#[specta(type = Any)]`：`serde_json::Value` 是递归类型，specta 展开它会产出无限 TS 类型
    /// （与 `IpcError.details` 同一个坑）。这里它真的是任意 JSON。
    #[cfg_attr(feature = "specta", specta(type = specta_typescript::Any))]
    pub raw_json: serde_json::Value,
    /// 上一次扫描返回过它吗（false = 行还在，但这次没见到）。
    pub seen_in_last_scan: bool,
    /// 第一次见到（**upsert 不覆盖**）。
    pub first_seen_ms: i64,
    /// 最后一次见到（每次观测都刷新）。
    pub last_seen_ms: i64,
}

#[async_trait]
pub trait ModelCatalogStore: Send + Sync {
    /// 这家服务商的目录行（按 `upstream_model` 升序 —— 顺序稳定，界面不会自己跳）。
    async fn list(&self, provider_id: &str) -> Result<Vec<ModelCatalogRow>>;
    /// 全库的目录行（按 provider、再按 upstream 升序）。
    async fn list_all(&self) -> Result<Vec<ModelCatalogRow>>;
    /// 读一行（接入时用它取 `suggested_id`：逻辑名只从目录里来）。
    async fn get(&self, provider_id: &str, upstream_model: &str)
        -> Result<Option<ModelCatalogRow>>;
    /// 写入一行：冲突时更新 `suggested_id` / `raw_json` / `seen_in_last_scan` / `last_seen_ms`，
    /// **保留 `first_seen_ms`**。
    ///
    /// 原文**每次观测都刷新**：上游改了字段（例如新加了 `context_length`），
    /// 目录要跟着它走；`first_seen_ms` 才是那个"不许被重扫抹掉"的事实。
    async fn upsert(&self, row: &ModelCatalogRow) -> Result<()>;
    /// 把这家的全部行标成"本次没见过"（重扫的第一步），返回影响行数。
    async fn mark_all_unseen(&self, provider_id: &str) -> Result<u64>;
    /// 清掉本次没返回的行（[docs/25 §2.1]），返回被删的 `upstream_model` 清单。
    ///
    /// `keep` = **不能删**的那些（调用方传"这家还接着通道的上游名"）：
    /// 用户接过的行留着并标成"本次未返回"，让用户自己决定。
    async fn prune_unseen(&self, provider_id: &str, keep: &[String]) -> Result<Vec<String>>;
    /// 服务商被删时把它的目录行一起清掉，返回删除行数。
    async fn remove_provider(&self, provider_id: &str) -> Result<u64>;
}

pub struct SqliteModelCatalogStore {
    pool: SqlitePool,
}

impl SqliteModelCatalogStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }
}

fn map_row(row: &sqlx::sqlite::SqliteRow) -> ModelCatalogRow {
    ModelCatalogRow {
        provider_id: row.get("provider_id"),
        upstream_model: row.get("upstream_model"),
        suggested_id: row.get("suggested_id"),
        raw_json: parse_raw_object(&row.get::<String, _>("raw_json")),
        seen_in_last_scan: row.get::<i64, _>("seen_in_last_scan") != 0,
        first_seen_ms: row.get("first_seen_ms"),
        last_seen_ms: row.get("last_seen_ms"),
    }
}

/// 一个**空对象** `{}` —— "我们没记到原文"的**唯一**表示（不用 NULL，也不用空串）。
fn empty_object() -> serde_json::Value {
    serde_json::Value::Object(serde_json::Map::new())
}

/// 库里那一段文本 → 上游原文对象。
///
/// **只接受 JSON 对象**：不是对象（数组/字符串/数字/null）、或者文本根本不是 JSON，
/// 一律得到 `{}` —— 宁可"我们什么都没记到"，也**绝不补字段**（[docs/28 §3]）。
/// 读路径不报错：一行坏数据不该让整份目录读不出来（那会让界面空得莫名其妙）。
fn parse_raw_object(text: &str) -> serde_json::Value {
    match serde_json::from_str::<serde_json::Value>(text) {
        Ok(value @ serde_json::Value::Object(_)) => value,
        _ => empty_object(),
    }
}

/// 上游原文对象 → 库里那一段文本（[`parse_raw_object`] 的写侧，规则完全一样）。
///
/// 上游那一条不是对象、或调用方没拿到原文（`Value::Null`）时写的就是 `'{}'`：
/// 这是"没记到"的如实表示，不是我们造的对象。
fn raw_object_text(raw: &serde_json::Value) -> String {
    match raw {
        serde_json::Value::Object(_) => {
            // `preserve_order` 开着，键序与上游给的一致；序列化一个已解析的 `Value`
            // 不会失败，兜底成 `'{}'` 只是为了不给这里留一个 `unwrap`。
            serde_json::to_string(raw).unwrap_or_else(|_| "{}".to_string())
        }
        _ => "{}".to_string(),
    }
}

/// 目录列（`SELECT` 的固定顺序，避免各处各写一份）。
const COLUMNS: &str = "provider_id, upstream_model, suggested_id, raw_json, seen_in_last_scan, \
                       first_seen_ms, last_seen_ms";

#[async_trait]
impl ModelCatalogStore for SqliteModelCatalogStore {
    async fn list(&self, provider_id: &str) -> Result<Vec<ModelCatalogRow>> {
        let rows = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM provider_model_catalog WHERE provider_id = ?
             ORDER BY upstream_model ASC"
        ))
        .bind(provider_id)
        .fetch_all(&self.pool)
        .await
        .store_err()?;
        Ok(rows.iter().map(map_row).collect())
    }

    async fn list_all(&self) -> Result<Vec<ModelCatalogRow>> {
        let rows = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM provider_model_catalog
             ORDER BY provider_id ASC, upstream_model ASC"
        ))
        .fetch_all(&self.pool)
        .await
        .store_err()?;
        Ok(rows.iter().map(map_row).collect())
    }

    async fn get(
        &self,
        provider_id: &str,
        upstream_model: &str,
    ) -> Result<Option<ModelCatalogRow>> {
        let row = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM provider_model_catalog
             WHERE provider_id = ? AND upstream_model = ?"
        ))
        .bind(provider_id)
        .bind(upstream_model)
        .fetch_optional(&self.pool)
        .await
        .store_err()?;
        Ok(row.as_ref().map(map_row))
    }

    async fn upsert(&self, row: &ModelCatalogRow) -> Result<()> {
        // `first_seen_ms` **不在冲突分支里**：它记的是"第一次见到"，不是"最后一次观测"。
        sqlx::query(
            "INSERT INTO provider_model_catalog
               (provider_id, upstream_model, suggested_id, raw_json, seen_in_last_scan, first_seen_ms, last_seen_ms)
             VALUES (?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(provider_id, upstream_model) DO UPDATE SET
               suggested_id = excluded.suggested_id,
               raw_json = excluded.raw_json,
               seen_in_last_scan = excluded.seen_in_last_scan,
               last_seen_ms = excluded.last_seen_ms",
        )
        .bind(&row.provider_id)
        .bind(&row.upstream_model)
        .bind(&row.suggested_id)
        .bind(raw_object_text(&row.raw_json))
        .bind(if row.seen_in_last_scan { 1_i64 } else { 0_i64 })
        .bind(row.first_seen_ms)
        .bind(row.last_seen_ms)
        .execute(&self.pool)
        .await
        .store_err()?;
        Ok(())
    }

    async fn mark_all_unseen(&self, provider_id: &str) -> Result<u64> {
        let res = sqlx::query(
            "UPDATE provider_model_catalog SET seen_in_last_scan = 0 WHERE provider_id = ?",
        )
        .bind(provider_id)
        .execute(&self.pool)
        .await
        .store_err()?;
        Ok(res.rows_affected())
    }

    async fn prune_unseen(&self, provider_id: &str, keep: &[String]) -> Result<Vec<String>> {
        // 先选出要删的（**要回传清单**给调用方做事实汇报），再按同一条件删 —— 两个语句之间
        // 只有"扫描"这一个写者，且都在同一个连接池上顺序执行。
        let placeholders = if keep.is_empty() {
            String::new()
        } else {
            format!(" AND upstream_model NOT IN ({})", vec!["?"; keep.len()].join(", "))
        };
        let select_sql = format!(
            "SELECT upstream_model FROM provider_model_catalog
             WHERE provider_id = ? AND seen_in_last_scan = 0{placeholders}
             ORDER BY upstream_model ASC"
        );
        let mut q = sqlx::query(&select_sql).bind(provider_id);
        for k in keep {
            q = q.bind(k);
        }
        let doomed: Vec<String> = q
            .fetch_all(&self.pool)
            .await
            .store_err()?
            .iter()
            .map(|r| r.get::<String, _>("upstream_model"))
            .collect();
        if doomed.is_empty() {
            return Ok(doomed);
        }

        let delete_sql = format!(
            "DELETE FROM provider_model_catalog
             WHERE provider_id = ? AND seen_in_last_scan = 0{placeholders}"
        );
        let mut del = sqlx::query(&delete_sql).bind(provider_id);
        for k in keep {
            del = del.bind(k);
        }
        del.execute(&self.pool).await.store_err()?;
        Ok(doomed)
    }

    async fn remove_provider(&self, provider_id: &str) -> Result<u64> {
        let res = sqlx::query("DELETE FROM provider_model_catalog WHERE provider_id = ?")
            .bind(provider_id)
            .execute(&self.pool)
            .await
            .store_err()?;
        Ok(res.rows_affected())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一个**真的文件库**（临时目录里的 SQLite）—— 观察数据必须"关掉再开还在"。
    async fn fresh(tag: &str) -> (SqliteModelCatalogStore, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("ta-cat-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("建临时目录");
        let path = dir.join("db.sqlite");
        let pool = crate::open(&path).await.expect("开库");
        (SqliteModelCatalogStore::new(pool), path)
    }

    fn row(
        provider: &str,
        upstream: &str,
        logical: &str,
        seen: bool,
        first: i64,
        last: i64,
    ) -> ModelCatalogRow {
        ModelCatalogRow {
            provider_id: provider.to_string(),
            upstream_model: upstream.to_string(),
            suggested_id: logical.to_string(),
            // 大多数用例不关心原文；**没有原文就是 `{}`**（不编字段），
            // 关心原文的用例用 `..row(...)` 覆盖这一项。
            raw_json: serde_json::json!({}),
            seen_in_last_scan: seen,
            first_seen_ms: first,
            last_seen_ms: last,
        }
    }

    #[tokio::test]
    async fn upsert_keeps_first_seen_and_updates_the_rest() {
        let (store, path) = fresh("upsert").await;
        store.upsert(&row("p-a", "gpt-4o", "gpt-4o", true, 100, 100)).await.unwrap();

        // 第二次观测：逻辑名变了、时间变了、**上游原文也变了**（它新加了字段）
        // → 除了 `first_seen_ms` 都该跟着变
        let second_raw = serde_json::json!({ "id": "gpt-4o", "owned_by": "vendor", "created": 1 });
        store
            .upsert(&ModelCatalogRow {
                raw_json: second_raw.clone(),
                ..row("p-a", "gpt-4o", "gpt4o", true, 999, 200)
            })
            .await
            .unwrap();
        let got = store.get("p-a", "gpt-4o").await.unwrap().expect("行在");
        assert_eq!(got.suggested_id, "gpt4o");
        assert_eq!(got.first_seen_ms, 100, "first_seen_ms 是'第一次见到'，不能被覆盖");
        assert_eq!(got.last_seen_ms, 200);
        assert!(got.seen_in_last_scan);
        assert_eq!(got.raw_json, second_raw, "原文是'最后一次观测'，要跟着上游走");

        // 关掉库再打开：观察数据必须还在（这正是这一轮要修的缺陷：以前只在内存里）
        drop(store);
        let pool = crate::open(&path).await.expect("重开");
        let again = SqliteModelCatalogStore::new(pool);
        let rows = again.list("p-a").await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].first_seen_ms, 100);
        assert_eq!(rows[0].raw_json, second_raw, "原文也要经得起'关库再开'");
    }

    /// **上游给了什么就存什么**：我们不认识的字段（`permission` 这种嵌套结构）也要原样留下，
    /// 读回来逐字段相同、且**文本也相同**（键序没被重排）。
    ///
    /// 为什么这条是这一轮的核心：只留 `suggested_id` 的话，用户永远看不到上游声明了什么，
    /// 给模型填能力时也没有可核对的来源 —— 那就只能编字段（[docs/28 §3] 明令禁止）。
    #[tokio::test]
    async fn upsert_round_trips_the_upstream_object_verbatim() {
        let (store, path) = fresh("raw").await;
        // 上游那一条：`object` / `owned_by` / `created` / `permission` 我们一个都没建模
        let raw: serde_json::Value = serde_json::json!({
            "id": "x",
            "object": "model",
            "owned_by": "vendor",
            "created": 123,
            "permission": [{ "x": 1 }],
        });
        store
            .upsert(&ModelCatalogRow { raw_json: raw.clone(), ..row("p-a", "x", "x", true, 1, 1) })
            .await
            .unwrap();

        let got = store.get("p-a", "x").await.unwrap().expect("行在");
        assert_eq!(got.raw_json, raw, "上游原文必须逐字段相同");
        assert_eq!(
            serde_json::to_string(&got.raw_json).unwrap(),
            r#"{"id":"x","object":"model","owned_by":"vendor","created":123,"permission":[{"x":1}]}"#,
            "文本也要逐字相同（preserve_order：键序不许被重排，字段一个都不许丢）"
        );

        // 关掉库再打开：原文也必须还在（目录是**落 SQLite** 的观察数据，不是内存缓存）
        drop(store);
        let pool = crate::open(&path).await.expect("重开");
        let again = SqliteModelCatalogStore::new(pool);
        assert_eq!(again.list("p-a").await.unwrap()[0].raw_json, raw, "重开之后原文还在");
    }

    /// 上游那一条**不是对象**（或压根没有原文）→ 落 `{}`，**绝不补字段**；
    /// 库里那段文本读不出来时同样得到 `{}`（一行坏数据不该让整份目录读不出来）。
    #[tokio::test]
    async fn raw_json_only_accepts_objects_and_never_fabricates() {
        let (store, _) = fresh("raw-nonobj").await;
        let bad: [(&str, serde_json::Value); 4] = [
            ("bare-string", serde_json::json!("x")),
            ("array", serde_json::json!([{ "id": "x" }])),
            ("number", serde_json::json!(7)),
            ("null", serde_json::Value::Null),
        ];
        for (upstream, value) in &bad {
            store
                .upsert(&ModelCatalogRow {
                    raw_json: value.clone(),
                    ..row("p-a", upstream, upstream, true, 1, 1)
                })
                .await
                .unwrap();
        }

        for (upstream, _) in &bad {
            let got = store.get("p-a", upstream).await.unwrap().expect("行在");
            assert_eq!(got.raw_json, serde_json::json!({}), "`{upstream}` 不是对象 → `{{}}`");
            assert!(
                got.raw_json.as_object().is_some_and(|o| o.is_empty()),
                "`{upstream}` 落下来必须是一个空对象：不编 `object`/不编 `id`/不编任何字段"
            );
        }

        // 库里那段文本坏了（手改/半截写入）→ 读出来还是 `{}`，而不是整份目录读取失败
        sqlx::query(
            "UPDATE provider_model_catalog SET raw_json = '不是 JSON' WHERE upstream_model = ?",
        )
        .bind("array")
        .execute(store.pool())
        .await
        .unwrap();
        let got = store.get("p-a", "array").await.unwrap().expect("行还在");
        assert_eq!(got.raw_json, serde_json::json!({}), "文本不是 JSON → `{{}}`，不报错也不编");
    }

    #[tokio::test]
    async fn list_is_ordered_and_scoped_to_one_provider() {
        let (store, _) = fresh("list").await;
        store.upsert(&row("p-a", "zzz", "zzz", true, 1, 1)).await.unwrap();
        store.upsert(&row("p-a", "aaa", "aaa", true, 1, 1)).await.unwrap();
        store.upsert(&row("p-b", "bbb", "bbb", true, 1, 1)).await.unwrap();

        let a: Vec<String> =
            store.list("p-a").await.unwrap().into_iter().map(|r| r.upstream_model).collect();
        assert_eq!(
            a,
            vec!["aaa".to_string(), "zzz".to_string()],
            "按 upstream_model 升序且只含这家"
        );
        assert_eq!(store.list_all().await.unwrap().len(), 3);
    }

    #[tokio::test]
    async fn prune_removes_unseen_but_keeps_attached_and_other_providers() {
        let (store, _) = fresh("prune").await;
        // 上轮扫到三条
        store.upsert(&row("p-a", "kept", "kept", true, 1, 1)).await.unwrap();
        store.upsert(&row("p-a", "attached", "attached", true, 1, 1)).await.unwrap();
        store.upsert(&row("p-a", "gone", "gone", true, 1, 1)).await.unwrap();
        store.upsert(&row("p-b", "other", "other", true, 1, 1)).await.unwrap();

        // 本轮扫描：只返回 kept
        store.mark_all_unseen("p-a").await.unwrap();
        store.upsert(&row("p-a", "kept", "kept", true, 1, 5000)).await.unwrap();

        // `attached` 还接着通道 → 进 keep 名单，不删
        let dropped = store.prune_unseen("p-a", &["attached".to_string()]).await.unwrap();
        assert_eq!(dropped, vec!["gone".to_string()], "只删本次没返回且没接入的那条");

        let left: Vec<String> =
            store.list("p-a").await.unwrap().into_iter().map(|r| r.upstream_model).collect();
        assert_eq!(left, vec!["attached".to_string(), "kept".to_string()]);
        let attached = store.get("p-a", "attached").await.unwrap().expect("保留的行还在");
        assert!(!attached.seen_in_last_scan, "保留的行要标成'本次未返回'，让用户自己决定");
        assert_eq!(store.list("p-b").await.unwrap().len(), 1, "别的服务商一行都不能动");
    }

    #[tokio::test]
    async fn empty_keep_list_prunes_everything_unseen() {
        let (store, _) = fresh("prune-all").await;
        store.upsert(&row("p-a", "old", "old", false, 1, 1)).await.unwrap();
        let dropped = store.prune_unseen("p-a", &[]).await.unwrap();
        assert_eq!(dropped, vec!["old".to_string()]);
        assert!(store.list("p-a").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn remove_provider_touches_only_that_provider() {
        let (store, _) = fresh("gc").await;
        store.upsert(&row("p-a", "x", "x", true, 1, 1)).await.unwrap();
        store.upsert(&row("p-a", "y", "y", true, 1, 1)).await.unwrap();
        store.upsert(&row("p-b", "z", "z", true, 1, 1)).await.unwrap();
        assert_eq!(store.remove_provider("p-a").await.unwrap(), 2);
        assert!(store.list("p-a").await.unwrap().is_empty());
        assert_eq!(store.list("p-b").await.unwrap().len(), 1);
    }
}
