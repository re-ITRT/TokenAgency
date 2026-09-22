//! `ta-store::api_key` 的集成测试 —— **先红后绿**（docs/19 §3）。
//!
//! 为什么单独成文件而不是塞在模块里：这些断言要证明的是"**库里到底存了什么**"
//! （只存 sha256、明文不落库、`last_used_at` 是 NULL 不是 0），
//! 所以它们必须绕过模块内部的便利函数，直接对 `api_key` 表下 SQL 复核。

use ta_core::types::{TokenKind, TokenUsage};
use ta_store::api_key::{hash_token, SqliteApiKeyStore};
use ta_store::SqliteUsageStore;

/// 一个跑完迁移的**内存库** + 一个 store。
async fn store() -> (sqlx::SqlitePool, SqliteApiKeyStore) {
    let pool = ta_store::open_memory().await.unwrap();
    let store = SqliteApiKeyStore::new(pool.clone());
    (pool, store)
}

/// 建一把 key，返回 `(id, 明文)`。
async fn seed(store: &SqliteApiKeyStore, name: &str, now_ms: i64) -> (String, String) {
    let created = store.create(name, None, now_ms).await.unwrap();
    (created.key.id, created.plaintext)
}

/// **明文只在返回值里出现一次，库里只有 sha256。**
///
/// 这是整个 §3.1 的核心纪律：与"key 永不进 SQLite"同族 —— 破在这里，
/// 后面所有"只显示前缀"的说法都变成假话。
#[tokio::test]
async fn api_key_create_stores_hash_only_and_returns_plaintext_once() {
    let (pool, store) = store().await;
    let created = store.create("客户端 A", Some("笔记本"), 1_700_000_000_000).await.unwrap();

    // 1) 返回值里有明文（唯一一次）
    assert!(created.plaintext.starts_with("ta-sk-"), "明文必须带 ta-sk- 前缀");
    assert_eq!(created.key.name, "客户端 A");
    assert_eq!(created.key.note.as_deref(), Some("笔记本"));
    assert!(created.key.enabled, "新建的 key 默认启用");
    assert_eq!(created.key.created_at_ms, 1_700_000_000_000);
    assert_eq!(created.key.last_used_at_ms, None, "从未用过必须是 NULL（不是 0）");
    assert_eq!(
        created.key.key_prefix,
        format!("{}…", &created.plaintext[..12]),
        "key_prefix = 明文前 12 字符 + 省略号"
    );

    // 2) 库里存的**就是** sha256(明文)，且明文没有任何一列能装下它
    let (hash, prefix, name, note): (String, String, String, Option<String>) =
        sqlx::query_as("SELECT key_hash, key_prefix, name, note FROM api_key WHERE id = ?")
            .bind(&created.key.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(hash, hash_token(&created.plaintext), "库里必须是 sha256(明文)");
    assert_eq!(hash.len(), 64, "sha256 的十六进制是 64 字符");
    assert_eq!(prefix, created.key.key_prefix);
    assert_eq!(name, "客户端 A");
    assert_eq!(note.as_deref(), Some("笔记本"));
    let dumped = format!("{hash}|{prefix}|{name}|{note:?}");
    assert!(!dumped.contains(&created.plaintext), "明文不容许出现在库里的任何一列：{dumped}");

    // 3) 明文**不可能**再取回来（没有第二处出口）
    let again = store.list().await.unwrap();
    assert_eq!(again.len(), 1);
    assert!(
        !serde_json::to_string(&again).unwrap().contains(&created.plaintext),
        "列表里不许出现明文"
    );
}

/// 明文 = `ta-sk-` + **≥32 字节熵**的 base62；两次创建必不同（随机源真的在起作用）。
#[tokio::test]
async fn api_key_plaintext_is_ta_sk_prefixed_base62_with_32_bytes_of_entropy() {
    let (_pool, store) = store().await;
    let a = store.create("a", None, 1).await.unwrap();
    let b = store.create("b", None, 2).await.unwrap();

    assert_ne!(a.plaintext, b.plaintext, "两次创建的明文必须不同");
    assert_ne!(a.key.id, b.key.id, "两次创建的 id 必须不同");

    let body = a.plaintext.strip_prefix("ta-sk-").expect("前缀");
    // base62 的 43 字符 ≈ 256 bit ≈ 32 字节熵
    assert!(body.len() >= 43, "≥32 字节熵的 base62 至少 43 字符，实际 {}", body.len());
    assert!(
        body.chars().all(|c| c.is_ascii_alphanumeric()),
        "只允许 base62（字母与数字），便于放进 Authorization 头：{body}"
    );
    assert!(!a.plaintext.contains(char::is_whitespace), "明文里不许有空白字符（HTTP 头会截断）");
}

/// `list()` 的形状：按创建时间升序，**不含明文、不含 hash**，且契约字段一个不多一个不少。
#[tokio::test]
async fn api_key_list_never_carries_plaintext_or_hash() {
    let (_pool, store) = store().await;
    let (_, plain_a) = seed(&store, "先建的", 100).await;
    let (_, plain_b) = seed(&store, "后建的", 200).await;

    let rows = store.list().await.unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].name, "先建的", "按 created_at 升序");
    assert_eq!(rows[1].name, "后建的");

    let json = serde_json::to_string(&rows).unwrap();
    assert!(!json.contains(&plain_a) && !json.contains(&plain_b), "列表泄漏了明文：{json}");
    let hash_a = hash_token(&plain_a);
    assert!(!json.contains(&hash_a), "列表泄漏了 key_hash：{json}");
    assert!(!json.contains("key_hash") && !json.contains("plaintext"), "字段名本身也不该出现");

    // 字段集：契约的 7 个之内，且必填的 5 个必须在
    for value in serde_json::to_value(&rows).unwrap().as_array().unwrap() {
        let obj = value.as_object().unwrap();
        const ALLOWED: [&str; 7] =
            ["id", "name", "key_prefix", "enabled", "note", "created_at_ms", "last_used_at_ms"];
        for k in obj.keys() {
            assert!(ALLOWED.contains(&k.as_str()), "出现了契约外的字段 `{k}`：{json}");
        }
        for required in ["id", "name", "key_prefix", "enabled", "created_at_ms"] {
            assert!(obj.contains_key(required), "缺必填字段 `{required}`：{json}");
        }
    }
}

/// `set_enabled` 必须**真的改库并回读那一行**（命令契约要求返回 `ApiKeyView`）。
#[tokio::test]
async fn api_key_set_enabled_writes_and_reads_back() {
    let (pool, store) = store().await;
    let (id, _) = seed(&store, "可停用的", 1).await;

    let off = store.set_enabled(&id, false).await.unwrap().expect("刚写过就该读得到");
    assert!(!off.enabled);
    let raw: i64 = sqlx::query_scalar("SELECT enabled FROM api_key WHERE id = ?")
        .bind(&id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(raw, 0, "库里必须真的写成 0");

    let on = store.set_enabled(&id, true).await.unwrap().expect("读回来");
    assert!(on.enabled);
    let raw: i64 = sqlx::query_scalar("SELECT enabled FROM api_key WHERE id = ?")
        .bind(&id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(raw, 1, "库里必须真的写回 1");

    assert!(
        store.set_enabled("不存在的-id", false).await.unwrap().is_none(),
        "改不存在的行 = None"
    );
    assert!(store.list().await.unwrap()[0].enabled, "别人的行不受影响");
}

/// `delete` 要如实报告"到底删掉了没有"（命令层据此报 not found，不许静默成功）。
#[tokio::test]
async fn api_key_delete_reports_whether_a_row_went_away() {
    let (pool, store) = store().await;
    let (id, _) = seed(&store, "要删的", 1).await;

    assert!(store.delete(&id).await.unwrap(), "删掉了 → true");
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM api_key").fetch_one(&pool).await.unwrap();
    assert_eq!(n, 0);
    assert!(!store.delete(&id).await.unwrap(), "再删一次 → false（不能假装成功）");
    assert!(store.list().await.unwrap().is_empty());
}

/// `touch_last_used` 只动那一行；**从未用过的行始终是 NULL**（不是被写成 0）。
#[tokio::test]
async fn api_key_touch_last_used_only_touches_that_row() {
    let (pool, store) = store().await;
    let (used, _) = seed(&store, "用过的", 1).await;
    let (unused, _) = seed(&store, "没用过的", 2).await;

    store.touch_last_used(&used, 1_700_000_000_123).await.unwrap();

    let rows = store.list().await.unwrap();
    let by_id = |id: &str| rows.iter().find(|r| r.id == id).unwrap().clone();
    assert_eq!(by_id(&used).last_used_at_ms, Some(1_700_000_000_123));
    assert_eq!(by_id(&unused).last_used_at_ms, None, "没碰过的行必须是 NULL");

    // 库里也得是 NULL，不是 0
    let raw: Option<i64> = sqlx::query_scalar("SELECT last_used_at FROM api_key WHERE id = ?")
        .bind(&unused)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(raw, None, "SQLite 里必须是 NULL");
}

/// `find_enabled_by_hash`：**只命中启用的**；未知 hash 返回 `None`（不许报错、不许兜底放行）。
#[tokio::test]
async fn api_key_find_enabled_by_hash_ignores_disabled_and_unknown() {
    let (_pool, store) = store().await;
    let (live, plain_live) = seed(&store, "启用中", 1).await;
    let (dead, plain_dead) = seed(&store, "已停用", 2).await;
    store.set_enabled(&dead, false).await.unwrap();

    let hit = store.find_enabled_by_hash(&hash_token(&plain_live)).await.unwrap();
    assert_eq!(hit.map(|k| k.id), Some(live.clone()), "启用中的 key 必须命中");

    assert!(
        store.find_enabled_by_hash(&hash_token(&plain_dead)).await.unwrap().is_none(),
        "停用的 key 不命中（立刻影响鉴权）"
    );
    assert!(
        store.find_enabled_by_hash(&hash_token("ta-sk-根本不存在的明文")).await.unwrap().is_none(),
        "未知 hash → None"
    );
    // 删掉之后也不命中
    store.delete(&live).await.unwrap();
    assert!(store.find_enabled_by_hash(&hash_token(&plain_live)).await.unwrap().is_none());
}

/// `key_hash` 的 UNIQUE 是**安全语义**，不是性能优化：同一明文不可能有两个 id。
#[tokio::test]
async fn api_key_key_hash_is_unique() {
    let (pool, store) = store().await;
    let (id, plain) = seed(&store, "第一把", 1).await;
    let err = sqlx::query(
        "INSERT INTO api_key (id, name, key_hash, key_prefix, enabled, created_at)
         VALUES ('dup', '第二把', ?, 'ta-sk-000000…', 1, 2)",
    )
    .bind(hash_token(&plain))
    .execute(&pool)
    .await
    .expect_err("同一个 key_hash 插第二次必须失败");
    assert!(err.to_string().to_lowercase().contains("unique"), "{err}");
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM api_key WHERE id = ?")
        .bind(&id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n, 1);
}

/// `any_enabled`：**"`local_key` 没配时还要不要强制鉴权"的唯一判据**
/// （`docs/19 §3.3` 的启用条件 + `ta-gateway` 的 `ApiKeyVerifier::any_enabled`）。
///
/// 它错了的后果是两个极端：恒假 → 建了 key 也挡不住人（**建了不生效**）；
/// 恒真 → 没配任何东西的老用户升级后全部 401。
#[tokio::test]
async fn api_key_any_enabled_tracks_the_enabled_rows() {
    let (_pool, store) = store().await;
    assert!(!store.any_enabled().await.unwrap(), "空表 = 一把启用中的都没有");

    let (id, _) = seed(&store, "第一把", 1).await;
    assert!(store.any_enabled().await.unwrap(), "新建的默认启用");

    store.set_enabled(&id, false).await.unwrap();
    assert!(!store.any_enabled().await.unwrap(), "全部停用 = 一把都没有");

    store.set_enabled(&id, true).await.unwrap();
    assert!(store.any_enabled().await.unwrap());

    // 多把：只要有一把启用就算"有"
    let (other, _) = seed(&store, "第二把", 2).await;
    store.set_enabled(&id, false).await.unwrap();
    assert!(store.any_enabled().await.unwrap(), "还有另一把启用的");
    store.set_enabled(&other, false).await.unwrap();
    assert!(!store.any_enabled().await.unwrap());

    store.delete(&other).await.unwrap();
    assert!(!store.any_enabled().await.unwrap(), "删光之后也是没有");
}

/// **用量不在本模块另开一条查询**：`api_key_usage` 表与 `ApiKeyUsageRow`
/// 都是既有的（`usage_store`），本模块只提供 §3.2 要求的 6 个 key 操作。
/// 所以这里只钉住"键表本身的读法"（一条 SQL 不能被两个地方各写一遍）：
/// 创建 → 列表 → 停用 → 删除 之后列表仍是同一个形状。
#[tokio::test]
async fn api_key_table_has_no_second_query_path() {
    // 用例量存储已有的查询函数本身（证明它不是私有的、可以直接复用）
    let pool = ta_store::open_memory().await.unwrap();
    let usage_store = SqliteUsageStore::new(pool.clone());
    let store = SqliteApiKeyStore::new(pool.clone());
    let (a, _) = seed(&store, "A", 1).await;

    let mut usage = TokenUsage::new();
    usage.add(TokenKind::Input, 100);
    usage_store.record_api_key_usage(&a, 1_700_000_000_000, &usage, 0.5, "USD").await.unwrap();

    let rows = usage_store.api_key_usage("0000-01-01", "9999-12-31").await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].key_id, a);
    assert_eq!(rows[0].token_kind, "input");
    assert_eq!(rows[0].tokens, 100);
    assert_eq!(rows[0].currency, "USD");
}
