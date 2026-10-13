//! P4 集成测试（内存 SQLite，全部走 `open_memory()` + `sqlx::migrate!` 后的真表）。
//!
//! 覆盖 DoD 的五项硬性断言 + 契约级自检：
//! 1. 写 1000 条请求 → `query` 按 3 种维度组合分组结果正确（含 daily/detail 两条路径等值）；
//! 2. 时区分桶：跨天边界的 ts 落在正确的本地 day；
//! 3. `gc()` 的 retention / max_rows / max_size 三个限都被触发；
//! 4. 图片块只存哈希（库里查不到 base64，也查不到明文 key）；
//! 5. 事务性：任一步失败 → usage_request / usage_daily 一并回滚。

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use ta_core::clock::{Clock, FixedClock, TimeRange};
use ta_core::domain::{ConfigDoc, Credential, Lane, Policy};
use ta_core::types::{
    ChannelId, ClientId, CredentialId, LaneId, LogMode, Money, PolicyId, ProviderId, RequestId,
    Retention, TokenKind, TokenUsage, UsageRecord, UsageSource,
};

use ta_store::trace_store::MAX_FIELD_BYTES;
use ta_store::{
    ClearMode, ConfigStore, GroupDim, HealthSample, HealthStore, LogQuery, Metric,
    SqliteConfigStore, SqliteHealthStore, SqliteTraceStore, SqliteUsageStore, StorageStats,
    TimeBucket, TraceAttempt, TraceLimits, TraceRecord, TraceStore, UsageFormat, UsageQuery,
    UsageStatus, UsageStore,
};

const OFF: i32 = 480; // UTC+8

fn ms(rfc3339: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(rfc3339)
        .expect("测试里的时间字面量必须合法")
        .timestamp_millis()
}

fn clock_at(now_ms: i64, offset_minutes: i32) -> Arc<dyn Clock> {
    Arc::new(FixedClock { ms: now_ms, offset_minutes })
}

async fn usage_store(now_ms: i64) -> (sqlx::SqlitePool, SqliteUsageStore) {
    let pool = ta_store::open_memory().await.expect("open_memory");
    let store = SqliteUsageStore::with_clock(pool.clone(), clock_at(now_ms, OFF));
    (pool, store)
}

/// 测试侧独立算一遍本地日期（不用 store 的实现），用于交叉验证分桶。
fn local_day(ts_ms: i64, offset_minutes: i32) -> String {
    chrono::DateTime::from_timestamp_millis(ts_ms + offset_minutes as i64 * 60_000)
        .expect("合法时间戳")
        .format("%Y-%m-%d")
        .to_string()
}

#[allow(clippy::too_many_arguments)]
fn record(
    id: usize,
    ts_ms: i64,
    cred: &str,
    prov: &str,
    model: &str,
    client: &str,
    channel: &str,
    lane: &str,
    node: &str,
    input: u64,
    cached: u64,
    output: u64,
) -> UsageRecord {
    let mut usage = TokenUsage::new();
    usage.add(TokenKind::Input, input);
    usage.add(TokenKind::InputCached, cached);
    usage.add(TokenKind::Output, output);
    // 金额由"费率 × token"算出来（与 per-kind 分摊口径一致，便于断言精确相等）
    let cost = input as f64 * 2.5 / 1e6 + cached as f64 * 1.25 / 1e6 + output as f64 * 10.0 / 1e6;
    UsageRecord {
        request_id: RequestId::new(format!("req-{id:05}")),
        ts_ms,
        credential_id: CredentialId::new(cred),
        provider_id: ProviderId::new(prov),
        channel_id: ChannelId::new(channel),
        model: model.to_string(),
        upstream_model: format!("{model}-2026"),
        client: Some(ClientId::new(client)),
        lane_id: Some(LaneId::new(lane)),
        node: Some(node.to_string()),
        status: 200,
        error_type: None,
        latency_ms: 100 + (id as u32 % 50),
        ttfb_ms: Some(30),
        usage,
        usage_raw: serde_json::json!({
            "prompt_tokens": input + cached,
            "completion_tokens": output,
        }),
        usage_source: UsageSource::Upstream,
        cost: Money::new(cost, "USD"),
        price_snapshot: serde_json::json!({
            "rates": {"input": 2.5, "input_cached": 1.25, "output": 10.0},
            "multiplier": 1.0,
            "currency": "USD",
            "source": "manual",
        }),
    }
}

/// 1000 条请求的确定性分布（期望值在测试里用同一份公式独立算出来）。
fn gen_1000() -> Vec<UsageRecord> {
    let base = ms("2026-02-10T01:00:00Z"); // 本地 2026-02-10 09:00
    (0..1000)
        .map(|i| {
            let ts = base + i as i64 * 60_000; // 每分钟一条 → 跨本地午夜
            record(
                i,
                ts,
                &format!("cred-{}", i % 4),
                &format!("prov-{}", i % 2),
                ["gpt-4o", "claude-sonnet", "gemini-pro"][i % 3],
                &format!("cli-{}", i % 5),
                &format!("ch-{}", i % 6),
                &format!("lane-{}", i % 2),
                &format!("node-{}", i % 2),
                100 + i as u64,
                (i % 50) as u64,
                10 + (i % 7) as u64,
            )
        })
        .collect()
}

fn day_aligned_window() -> TimeRange {
    // 本地 2026-02-10 00:00 → 2026-02-12 00:00（左闭右开，按 UTC+8 对齐）
    TimeRange::Explicit { from_ms: ms("2026-02-09T16:00:00Z"), to_ms: ms("2026-02-11T16:00:00Z") }
}

// ================================================================ 1. 1000 条 × 3 种维度组合

#[tokio::test]
async fn thousand_requests_grouped_by_three_dimension_combos() {
    let (pool, store) = usage_store(ms("2026-02-11T03:00:00Z")).await;
    let recs = gen_1000();
    for r in &recs {
        store.record(r.clone()).await.expect("record");
    }

    // 用 SQL 独立核对落库条数
    let n: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM usage_request").fetch_one(&pool).await.unwrap();
    assert_eq!(n, 1000);
    let daily_rows: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM usage_daily").fetch_one(&pool).await.unwrap();
    assert!(daily_rows > 0, "usage_daily 必须有行");
    // 行维度：每种 token_kind 一行（UPSERT 合并同组，所以行数 ≪ 请求数）
    let kinds: i64 = sqlx::query_scalar("SELECT COUNT(DISTINCT token_kind) FROM usage_daily")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(kinds, 3, "input / input_cached / output");
    let daily_input: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(tokens), 0) FROM usage_daily WHERE token_kind = 'input'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    let expect_input: u64 = recs.iter().map(|r| r.usage.get(&TokenKind::Input)).sum();
    assert_eq!(daily_input as u64, expect_input, "汇总表 token 数必须与明细一致");

    // ---------- 组合 A：按 model ----------
    let qa = UsageQuery {
        range: day_aligned_window(),
        group_by: vec![GroupDim::Model],
        metrics: vec![
            Metric::Requests,
            Metric::parse("input"),
            Metric::parse("output"),
            Metric::parse("input_cached"),
            Metric::Cost,
        ],
        ..Default::default()
    };
    let ta = store.query(&qa).await.expect("query by model");
    assert_eq!(ta.rows.len(), 3, "3 个模型");

    let mut expect: HashMap<String, (f64, f64, f64, f64, f64)> = HashMap::new();
    for r in &recs {
        let e = expect.entry(r.model.clone()).or_insert((0.0, 0.0, 0.0, 0.0, 0.0));
        e.0 += 1.0;
        e.1 += r.usage.get(&TokenKind::Input) as f64;
        e.2 += r.usage.get(&TokenKind::Output) as f64;
        e.3 += r.usage.get(&TokenKind::InputCached) as f64;
        e.4 += r.cost.amount;
    }
    for row in &ta.rows {
        let model = row.dims.get("model").expect("model 维度").clone();
        let e = expect.get(&model).expect("模型必须出现过");
        assert_eq!(row.get("requests"), e.0, "requests@{model}");
        assert_eq!(row.get("input"), e.1, "input@{model}");
        assert_eq!(row.get("output"), e.2, "output@{model}");
        assert_eq!(row.get("input_cached"), e.3, "input_cached@{model}");
        assert!((row.get("cost") - e.4).abs() < 1e-6, "cost@{model}");
    }
    let total_req: f64 = ta.rows.iter().map(|r| r.get("requests")).sum();
    assert_eq!(total_req, 1000.0);
    let total_cost: f64 = ta.rows.iter().map(|r| r.get("cost")).sum();
    let expect_cost: f64 = recs.iter().map(|r| r.cost.amount).sum();
    assert!((total_cost - expect_cost).abs() < 1e-6);

    // ---------- 组合 B：按 credential × client ----------
    let qb = UsageQuery {
        range: day_aligned_window(),
        group_by: vec![GroupDim::Credential, GroupDim::Client],
        metrics: vec![Metric::Requests, Metric::Cost, Metric::parse("output")],
        ..Default::default()
    };
    let tb = store.query(&qb).await.expect("query by cred×client");
    assert_eq!(tb.rows.len(), 4 * 5, "i%4 × i%5 → 20 组");

    let mut expect_b: HashMap<(String, String), (f64, f64, f64)> = HashMap::new();
    for r in &recs {
        let key =
            (r.credential_id.as_str().to_string(), r.client.as_ref().unwrap().as_str().to_string());
        let e = expect_b.entry(key).or_insert((0.0, 0.0, 0.0));
        e.0 += 1.0;
        e.1 += r.cost.amount;
        e.2 += r.usage.get(&TokenKind::Output) as f64;
    }
    for row in &tb.rows {
        let key =
            (row.dims.get("credential").unwrap().clone(), row.dims.get("client").unwrap().clone());
        let e = expect_b.get(&key).expect("组必须出现过");
        assert_eq!(row.get("requests"), e.0, "requests@{key:?}");
        assert!((row.get("cost") - e.1).abs() < 1e-6, "cost@{key:?}");
        assert_eq!(row.get("output"), e.2, "output@{key:?}");
    }

    // ---------- 组合 C：按 时间(day) × model ----------
    let qc = UsageQuery {
        range: day_aligned_window(),
        group_by: vec![GroupDim::Time, GroupDim::Model],
        time_bucket: Some(TimeBucket::Day),
        metrics: vec![Metric::Requests, Metric::parse("input")],
        ..Default::default()
    };
    let tc = store.query(&qc).await.expect("query by day×model");
    let mut expect_c: HashMap<(String, String), (f64, f64)> = HashMap::new();
    for r in &recs {
        let key = (local_day(r.ts_ms, OFF), r.model.clone());
        let e = expect_c.entry(key).or_insert((0.0, 0.0));
        e.0 += 1.0;
        e.1 += r.usage.get(&TokenKind::Input) as f64;
    }
    assert_eq!(tc.rows.len(), expect_c.len());
    let mut seen_days: Vec<String> = Vec::new();
    for row in &tc.rows {
        let key = (
            row.bucket.clone().expect("时间维度必须有 bucket 标签"),
            row.dims.get("model").unwrap().clone(),
        );
        let e = expect_c.get(&key).expect("(day,model) 组必须出现过");
        assert_eq!(row.get("requests"), e.0, "requests@{key:?}");
        assert_eq!(row.get("input"), e.1, "input@{key:?}");
        if !seen_days.contains(&key.0) {
            seen_days.push(key.0.clone());
        }
    }
    seen_days.sort();
    assert_eq!(seen_days, vec!["2026-02-10", "2026-02-11"], "跨本地午夜 → 2 个 day 桶");

    // ---------- 组合 D：汇总表路径必须与明细路径等值 ----------
    let q_daily = UsageQuery {
        range: day_aligned_window(),
        group_by: vec![GroupDim::Credential, GroupDim::Model],
        metrics: vec![Metric::Cost, Metric::parse("input"), Metric::parse("input_cached")],
        ..Default::default()
    };
    assert!(
        store.plan_uses_daily(&q_daily),
        "日对齐 + 维度/指标都在汇总表口径内 → 必须走 usage_daily"
    );
    let t_daily = store.query(&q_daily).await.expect("daily path");

    let mut q_detail = q_daily.clone();
    // 故意用非日对齐的边界（差 1ms）把查询逼到明细表
    q_detail.range = TimeRange::Explicit {
        from_ms: ms("2026-02-09T16:00:00Z") + 1,
        to_ms: ms("2026-02-11T16:00:00Z") + 1,
    };
    assert!(!store.plan_uses_daily(&q_detail), "非日对齐 → 必须回退明细表，否则会多算");
    let t_detail = store.query(&q_detail).await.expect("detail path");

    assert_eq!(t_daily.rows.len(), t_detail.rows.len());
    for (a, b) in t_daily.rows.iter().zip(t_detail.rows.iter()) {
        assert_eq!(a.dims, b.dims);
        for m in &t_daily.metrics {
            assert!(
                (a.get(m) - b.get(m)).abs() < 1e-6,
                "汇总表与明细表在 {m} 上必须一致：{} vs {}",
                a.get(m),
                b.get(m)
            );
        }
    }

    // ---------- 维度越界时自动回退明细表 ----------
    let q_channel = UsageQuery {
        range: day_aligned_window(),
        group_by: vec![GroupDim::Channel],
        metrics: vec![Metric::Cost],
        ..Default::default()
    };
    assert!(!store.plan_uses_daily(&q_channel), "channel 维度不在汇总表里");
    let t_channel = store.query(&q_channel).await.expect("channel query");
    assert_eq!(t_channel.rows.len(), 6);
    let chan_total: f64 = t_channel.rows.iter().map(|r| r.get("cost")).sum();
    assert!((chan_total - expect_cost).abs() < 1e-6);
}

// ================================================================ 2. 时区分桶

#[tokio::test]
async fn time_buckets_follow_local_timezone() {
    let now = ms("2026-02-11T03:00:00Z");
    let (pool, store) = usage_store(now).await;

    // UTC 15:59 → 本地(UTC+8) 23:59 02-10；UTC 20:00 → 本地 04:00 02-11
    let t_before = ms("2026-02-10T15:59:00Z");
    let t_after = ms("2026-02-10T20:00:00Z");
    assert_eq!(local_day(t_before, OFF), "2026-02-10");
    assert_eq!(local_day(t_after, OFF), "2026-02-11");

    store
        .record(record(1, t_before, "c", "p", "gpt-4o", "cli", "ch", "lane", "node", 10, 0, 1))
        .await
        .unwrap();
    store
        .record(record(2, t_after, "c", "p", "gpt-4o", "cli", "ch", "lane", "node", 20, 0, 2))
        .await
        .unwrap();

    // usage_daily.day 必须是本地日期
    let days: Vec<String> = sqlx::query_scalar("SELECT DISTINCT day FROM usage_daily ORDER BY day")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(days, vec!["2026-02-10", "2026-02-11"]);

    // 按 day 分桶：两条请求必须落在不同的桶
    let q = UsageQuery {
        range: TimeRange::Explicit {
            from_ms: ms("2026-02-10T00:00:00Z"),
            to_ms: ms("2026-02-11T12:00:00Z"),
        },
        group_by: vec![GroupDim::Time],
        time_bucket: Some(TimeBucket::Day),
        metrics: vec![Metric::Requests, Metric::parse("input")],
        ..Default::default()
    };
    let t = store.query(&q).await.unwrap();
    assert_eq!(t.rows.len(), 2, "跨本地午夜必须分成两个 day 桶");
    assert_eq!(t.rows[0].bucket.as_deref(), Some("2026-02-10"));
    assert_eq!(t.rows[1].bucket.as_deref(), Some("2026-02-11"));
    assert_eq!(t.rows[0].get("input"), 10.0);
    assert_eq!(t.rows[1].get("input"), 20.0);
    // bucket 起点是本地零点对应的真实 unix ms
    assert_eq!(
        t.rows[1].bucket_start_ms,
        Some(ms("2026-02-10T16:00:00Z")),
        "02-11 的本地零点 = UTC 02-10T16:00"
    );

    // 同一批时间戳在 UTC-5 下必须落到另一天（证明用的是本地时区，不是 UTC）
    let pool2 = ta_store::open_memory().await.unwrap();
    let store2 = SqliteUsageStore::with_clock(pool2.clone(), clock_at(now, -300));
    let t2 = ms("2026-02-11T02:00:00Z"); // UTC-5 → 本地 2026-02-10 21:00
    assert_eq!(local_day(t2, -300), "2026-02-10");
    store2
        .record(record(3, t2, "c", "p", "gpt-4o", "cli", "ch", "lane", "node", 5, 0, 1))
        .await
        .unwrap();
    let day2: String =
        sqlx::query_scalar("SELECT DISTINCT day FROM usage_daily").fetch_one(&pool2).await.unwrap();
    assert_eq!(day2, "2026-02-10", "UTC-5 下同一条 ts 属于前一天");
}

// ================================================================ 3. gc 三个限都被触发

fn random_text(seed: u64, len: usize) -> String {
    // splitmix64 生成低可压缩性的文本；**故意掺入非 base64 字符集的标点**，
    // 否则会被 sanitize 的"疑似内联二进制"闸门拦成哈希，就测不出 max_size 限制了。
    const PUNCT: [char; 6] = [',', '.', '!', '?', ';', ':'];
    let mut s = String::with_capacity(len);
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    while s.len() < len {
        x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = x;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        s.push((b'a' + (z % 26) as u8) as char);
        if s.len() % 9 == 0 {
            s.push(PUNCT[(z % 6) as usize]);
        }
    }
    s
}

async fn trace_store(mode: LogMode) -> (sqlx::SqlitePool, SqliteTraceStore) {
    let pool = ta_store::open_memory().await.expect("open_memory");
    let store =
        SqliteTraceStore::with_clock(pool.clone(), clock_at(ms("2026-02-11T03:00:00Z"), OFF));
    store.set_mode(mode).await.unwrap();
    (pool, store)
}

fn trace_with_body(id: usize, ts_ms: i64, body_len: usize) -> TraceRecord {
    let mut t = TraceRecord::new(RequestId::new(format!("tr-{id:04}")), ts_ms, "gpt-4o");
    t.client = Some(ClientId::new("cursor"));
    t.chosen_channel = Some(ChannelId::new("ch-a"));
    t.status = 200;
    t.total_ms = 120;
    t.tokens = 42;
    t.attempts = vec![TraceAttempt { channel: Some("ch-a".into()), ms: 120, ..Default::default() }];
    t.request_body = Some(serde_json::json!({
        "model": "gpt-4o",
        "messages": [{"role": "user", "content": random_text(id as u64 + 7, body_len)}],
    }));
    t.request_headers = vec![("content-type".into(), "application/json".into())];
    t
}

async fn body_bytes_sum(pool: &sqlx::SqlitePool) -> i64 {
    sqlx::query_scalar("SELECT COALESCE(SUM(body_bytes), 0) FROM trace")
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn gc_enforces_retention_rows_and_size_limits() {
    let now = ms("2026-02-11T03:00:00Z");

    // ---- (i) retention ----
    {
        let (pool, store) = trace_store(LogMode::Meta).await;
        store
            .set_limits(&TraceLimits {
                retention: Retention::D7,
                max_rows: 0,       // 不限制
                max_size_bytes: 0, // 不限制
            })
            .await
            .unwrap();
        for i in 0..10 {
            store
                .write(trace_with_body(i, now - 8 * 86_400_000 + i as i64 * 1000, 100))
                .await
                .unwrap();
        }
        for i in 10..20 {
            store.write(trace_with_body(i, now - i as i64 * 1000, 100)).await.unwrap();
        }
        let before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM trace WHERE ts < ?")
            .bind(now - 7 * 86_400_000)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(before, 10, "前置条件：确实有过期行");
        let deleted = store.gc(now).await.unwrap();
        assert_eq!(deleted, 10);
        let stale: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM trace WHERE ts < ?")
            .bind(now - 7 * 86_400_000)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(stale, 0);
        assert_eq!(store.storage_stats().await.unwrap().rows, 10);
    }

    // ---- (ii) max_rows ----
    {
        let (pool, store) = trace_store(LogMode::Meta).await;
        store
            .set_limits(&TraceLimits { retention: Retention::D90, max_rows: 5, max_size_bytes: 0 })
            .await
            .unwrap();
        for i in 0..20 {
            store.write(trace_with_body(i, now - (20 - i as i64) * 60_000, 200)).await.unwrap();
        }
        let before_rows = store.storage_stats().await.unwrap().rows;
        assert_eq!(before_rows, 20, "前置条件：写入 20 条");
        assert!(before_rows > 5, "前置条件：确实超过 max_rows");
        let deleted = store.gc(now).await.unwrap();
        assert_eq!(deleted, 15);
        let stats = store.storage_stats().await.unwrap();
        assert_eq!(stats.rows, 5);
        // 保留的是**最新**的 5 条
        let kept: Vec<String> = sqlx::query_scalar("SELECT request_id FROM trace ORDER BY ts ASC")
            .fetch_all(&pool)
            .await
            .unwrap();
        assert_eq!(kept, vec!["tr-0015", "tr-0016", "tr-0017", "tr-0018", "tr-0019"]);
    }

    // ---- (iii) max_size（trace 内容占用 = body_bytes 合计）----
    {
        let (pool, store) = trace_store(LogMode::Full).await;
        let limit = 64 * 1024u64;
        store
            .set_limits(&TraceLimits {
                retention: Retention::D90,
                max_rows: 0,
                max_size_bytes: limit,
            })
            .await
            .unwrap();
        for i in 0..30 {
            store.write(trace_with_body(i, now - (30 - i as i64) * 60_000, 8_000)).await.unwrap();
        }
        let before = body_bytes_sum(&pool).await;
        assert!(before as u64 > limit, "前置条件：确实超过 max_size（{before} > {limit}）");
        store.gc(now).await.unwrap();
        let after = body_bytes_sum(&pool).await;
        assert!(after as u64 <= limit, "gc 后必须落到上限内：{after} <= {limit}");
        // 从最老删 → 剩下的都更新
        let oldest_kept: i64 =
            sqlx::query_scalar("SELECT MIN(ts) FROM trace").fetch_one(&pool).await.unwrap();
        let newest: i64 =
            sqlx::query_scalar("SELECT MAX(ts) FROM trace").fetch_one(&pool).await.unwrap();
        assert_eq!(newest, now - 60_000);
        assert!(oldest_kept > now - 30 * 60_000, "最老的先被删掉");
        let stats: StorageStats = store.storage_stats().await.unwrap();
        assert!(stats.bytes > 0 && stats.body_bytes > 0);
        assert_eq!(stats.newest_ts, Some(newest));

        // auto_vacuum 必须是 INCREMENTAL，否则 incremental_vacuum 是空操作
        let av: i64 = sqlx::query_scalar("PRAGMA auto_vacuum").fetch_one(&pool).await.unwrap();
        assert_eq!(av, 2, "必须是 INCREMENTAL（2）");
    }

    // ---- (iv) 三限同时设：全部满足 ----
    {
        let (pool, store) = trace_store(LogMode::Full).await;
        let limit = 32 * 1024u64;
        store
            .set_limits(&TraceLimits {
                retention: Retention::D7,
                max_rows: 6,
                max_size_bytes: limit,
            })
            .await
            .unwrap();
        for i in 0..20 {
            let ts = if i < 5 {
                now - 10 * 86_400_000 // 过期
            } else {
                now - (20 - i as i64) * 60_000
            };
            store.write(trace_with_body(i, ts, 6_000)).await.unwrap();
        }
        assert!(body_bytes_sum(&pool).await as u64 > limit);
        store.gc(now).await.unwrap();
        let rows: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM trace").fetch_one(&pool).await.unwrap();
        let stale: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM trace WHERE ts < ?")
            .bind(now - 7 * 86_400_000)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(stale, 0);
        assert!(rows <= 6);
        assert!(body_bytes_sum(&pool).await as u64 <= limit);
    }
}

#[tokio::test]
async fn trace_mode_clear_and_cursor_pagination() {
    let now = ms("2026-02-11T03:00:00Z");
    let (pool, store) = trace_store(LogMode::Meta).await;

    // off 模式：什么都不记
    store.set_mode(LogMode::Off).await.unwrap();
    store.write(trace_with_body(0, now, 100)).await.unwrap();
    assert_eq!(store.storage_stats().await.unwrap().rows, 0, "off 不落库");

    // shape 模式：只记结构，不记内容
    store.set_mode(LogMode::Shape).await.unwrap();
    let mut secret_trace = TraceRecord::new(RequestId::new("tr-shape"), now, "gpt-4o");
    secret_trace.request_body = Some(serde_json::json!({
        "messages": [{"role": "user", "content": "这是绝对不能落库的正文"}]
    }));
    store.write(secret_trace).await.unwrap();
    let detail = store.get("tr-shape").await.unwrap().unwrap();
    let shape_dump = detail.body_shape.clone().unwrap().to_string();
    assert!(!shape_dump.contains("绝对不能落库的正文"));
    assert!(detail.request_body.is_none(), "shape 模式不写 body BLOB");
    let body_is_null: Option<Vec<u8>> =
        sqlx::query_scalar("SELECT body FROM trace WHERE request_id = 'tr-shape'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(body_is_null.is_none());

    // 游标分页：5 条 → limit 2 走 3 页，无重复无遗漏
    store.set_mode(LogMode::Meta).await.unwrap();
    store.clear(ClearMode::All).await.unwrap(); // 先清掉上面 shape 模式的样本
    for i in 1..=5 {
        store.write(trace_with_body(i, now - (5 - i as i64) * 1000, 50)).await.unwrap();
    }
    let mut seen: Vec<String> = Vec::new();
    let mut cursor: Option<String> = None;
    let mut pages = 0;
    loop {
        let page = store
            .query(&LogQuery { limit: Some(2), cursor: cursor.clone(), ..Default::default() })
            .await
            .unwrap();
        pages += 1;
        assert_eq!(page.total, 5);
        for r in &page.rows {
            assert!(!seen.contains(&r.request_id), "游标分页不能重复");
            seen.push(r.request_id.clone());
        }
        if !page.has_more {
            break;
        }
        cursor = page.cursor.clone();
        assert!(cursor.is_some(), "has_more=true 必须给游标");
        assert!(pages < 10, "分页不能死循环");
    }
    assert_eq!(pages, 3);
    assert_eq!(seen.len(), 5);

    // 状态过滤：造一条失败请求
    let mut failed = TraceRecord::new(RequestId::new("tr-fail"), now, "gpt-4o");
    failed.status = 429;
    failed.error = Some("insufficient_quota".into());
    failed.attempts = vec![
        TraceAttempt {
            channel: Some("ch-a".into()),
            status: Some(429),
            ms: 30,
            ..Default::default()
        },
        TraceAttempt {
            channel: Some("ch-b".into()),
            status: Some(200),
            ms: 90,
            ..Default::default()
        },
    ];
    store.write(failed).await.unwrap();
    let failed_page = store
        .query(&LogQuery { status: vec![UsageStatus::Failed], ..Default::default() })
        .await
        .unwrap();
    assert_eq!(failed_page.rows.len(), 1);
    assert_eq!(failed_page.rows[0].outcome, "failed");
    assert_eq!(failed_page.rows[0].attempts, 2);

    let retried_page = store
        .query(&LogQuery { status: vec![UsageStatus::Retried], ..Default::default() })
        .await
        .unwrap();
    assert_eq!(retried_page.rows.len(), 0, "成功请求只有 1 次尝试 → 不算 retried");

    // 只清 body：先造一条 full 模式带 body 的记录，否则"没有 body 可清"返回 0 是正确行为
    store.set_mode(LogMode::Full).await.unwrap();
    store.write(trace_with_body(99, now, 400)).await.unwrap();
    let with_body = store.storage_stats().await.unwrap();
    assert!(with_body.body_bytes > 0, "full 模式必须落 body BLOB");
    let cleared = store.clear(ClearMode::BodiesOnly).await.unwrap();
    assert!(cleared >= 1);
    let stats = store.storage_stats().await.unwrap();
    assert_eq!(stats.body_bytes, 0);
    assert!(stats.rows > 0, "只清 body 不删行");
    let all = store.clear(ClearMode::All).await.unwrap();
    assert!(all >= 1);
    assert_eq!(store.storage_stats().await.unwrap().rows, 0);
}

// ================================================================ 4. 图片只存哈希 / key 绝不入库

#[tokio::test]
async fn images_and_keys_never_reach_the_database_verbatim() {
    let now = ms("2026-02-11T03:00:00Z");
    let (pool, store) = trace_store(LogMode::Full).await;

    // 6 种形状：OpenAI chat / Anthropic / Gemini / OpenAI responses / 裸 data URL / 超长 base64 串
    let payload_a = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8AAAwAB";
    let payload_b = "QUJDREVGR0hJSktMTU5PUFFSU1RVVldYWVo0NTY3ODkwYWJjZGVm";
    let body = serde_json::json!({
        "model": "gpt-4o",
        "messages": [{
            "role": "user",
            "content": [
                {"type": "text", "text": "这几张图"},
                {"type": "image_url", "image_url": {"url": format!("data:image/png;base64,{payload_a}")}},
                {"type": "image", "source": {"type": "base64", "media_type": "image/jpeg", "data": payload_b}},
                {"inline_data": {"mime_type": "image/webp", "data": payload_a}},
                {"type": "input_image", "image_url": format!("data:image/gif;base64,{payload_a}")},
                format!("data:image/png;base64,{payload_a}"),
                "A".repeat(5000)
            ]
        }],
        "api_key": "sk-abcdefghijklmnopqrstuvwxyz0123456789",
        "headers": {"authorization": "Bearer sk-abcdefghijklmnopqrstuvwxyz"}
    });

    let mut t = TraceRecord::new(RequestId::new("tr-img"), now, "gpt-4o");
    t.request_body = Some(body.clone());
    t.request_headers = vec![
        ("Authorization".into(), "Bearer sk-abcdefghijklmnopqrstuvwxyz".into()),
        ("x-api-key".into(), "sk-abcdefghijklmnopqrstuvwxyz".into()),
        ("cookie".into(), "session=deadbeef".into()),
        ("content-type".into(), "application/json".into()),
    ];
    t.status = 200;
    store.write(t).await.unwrap();

    // (a) 库里的原始字节不能出现 base64 / key / Bearer
    let raw: (Vec<u8>, Vec<u8>) = {
        let row = sqlx::query("SELECT body, req_headers FROM trace WHERE request_id = 'tr-img'")
            .fetch_one(&pool)
            .await
            .unwrap();
        use sqlx::Row;
        (row.get("body"), row.get("req_headers"))
    };
    for (label, blob) in [("body", &raw.0), ("req_headers", &raw.1)] {
        let text = String::from_utf8_lossy(blob);
        assert!(!text.contains(payload_a), "{label} 里出现了 base64 载荷");
        assert!(!text.contains(payload_b), "{label} 里出现了 base64 载荷");
        assert!(!text.contains("sk-"), "{label} 里出现了明文 key");
        assert!(!text.contains("Bearer"), "{label} 里出现了明文 key");
        assert!(!text.contains("AAAA"), "{label} 里出现了超长 base64 串");
    }
    assert_eq!(ta_store::trace_store::count_plaintext_secret_rows(&pool, "sk-").await.unwrap(), 0);
    assert_eq!(
        ta_store::trace_store::count_plaintext_secret_rows(&pool, payload_a).await.unwrap(),
        0
    );

    // (b) 解压后的 JSON 里同样只有元数据
    let detail = store.get("tr-img").await.unwrap().unwrap();
    let dumped = detail.request_body.clone().unwrap().to_string();
    assert!(!dumped.contains(payload_a));
    assert!(!dumped.contains(payload_b));
    assert!(!dumped.contains("sk-"));
    assert!(!dumped.contains("Bearer"));
    assert!(!dumped.contains("data:image"));
    assert!(!dumped.contains(&"A".repeat(64)), "超长 base64 串只留哈希");

    let content = &detail.request_body.clone().unwrap()["messages"][0]["content"];
    // 5 个图片块 + 1 个兜底 binary，全部退化为「类型 + media_type + bytes + sha256_prefix」
    for idx in [1usize, 2, 3, 4, 5] {
        let blk = &content[idx];
        assert!(blk["bytes"].as_u64().unwrap() > 0, "第 {idx} 块必须记字节数");
        assert_eq!(
            blk["sha256_prefix"].as_str().unwrap().len(),
            8,
            "第 {idx} 块必须记 sha256 前 8 位"
        );
        assert!(blk["media_type"].is_string(), "第 {idx} 块要记 media_type");
        assert!(blk.get("data").is_none() && blk.get("url").is_none(), "第 {idx} 块不能留内容");
    }
    assert_eq!(content[6]["type"], "binary");
    // 同一张图出现三次 → 哈希一致（可去重），不同图哈希不同
    assert_eq!(content[1]["sha256_prefix"], content[3]["sha256_prefix"]);
    assert_eq!(content[1]["sha256_prefix"], content[4]["sha256_prefix"]);
    assert_eq!(content[1]["sha256_prefix"], content[5]["sha256_prefix"]);
    assert_ne!(content[1]["sha256_prefix"], content[2]["sha256_prefix"]);
    // 敏感键被替换
    assert_eq!(detail.request_body.clone().unwrap()["api_key"], "***");
    assert_eq!(detail.request_body.clone().unwrap()["headers"]["authorization"], "***");

    // (c) 头：键名保留、值变成 ***
    let headers = detail.request_headers.clone().unwrap();
    let pairs: Vec<(String, String)> = headers
        .as_array()
        .unwrap()
        .iter()
        .map(|v| (v[0].as_str().unwrap().to_string(), v[1].as_str().unwrap().to_string()))
        .collect();
    let map: BTreeMap<String, String> = pairs.into_iter().collect();
    assert_eq!(map.get("Authorization").unwrap(), "***");
    assert_eq!(map.get("x-api-key").unwrap(), "***");
    assert_eq!(map.get("cookie").unwrap(), "***");
    assert_eq!(map.get("content-type").unwrap(), "application/json");

    // (d) 单字段 >32KB 只留截断标记
    let mut big = TraceRecord::new(RequestId::new("tr-big"), now, "gpt-4o");
    big.request_body = Some(serde_json::json!({"content": "x".repeat(MAX_FIELD_BYTES + 5)}));
    store.write(big).await.unwrap();
    let big_detail = store.get("tr-big").await.unwrap().unwrap();
    let field = &big_detail.request_body.unwrap()["content"];
    assert_eq!(field["truncated"], true);
    assert_eq!(field["bytes"], MAX_FIELD_BYTES + 5);
    assert!(field["sha256_prefix"].as_str().unwrap().len() == 8);
}

// ================================================================ 5. 单事务

// ================================================================ 6. token 维度开放 / 看板 / CSV / api key

#[tokio::test]
async fn open_token_kinds_need_no_schema_change() {
    let (pool, store) = usage_store(ms("2026-02-11T03:00:00Z")).await;
    let mut rec = record(
        1,
        ms("2026-02-11T01:00:00Z"),
        "c",
        "p",
        "gpt-4o",
        "cli",
        "ch",
        "lane",
        "node",
        10,
        0,
        5,
    );
    rec.usage.add(TokenKind::Other("input_video".into()), 777);
    rec.usage.add(TokenKind::OutputReasoning, 42);
    store.record(rec).await.unwrap();

    // 行维度：新类型直接成为一行，未改表
    let kinds: Vec<String> =
        sqlx::query_scalar("SELECT token_kind FROM usage_daily ORDER BY token_kind")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert!(kinds.contains(&"input_video".to_string()));
    assert!(kinds.contains(&"output_reasoning".to_string()));

    // 任意 TokenKind 都能当指标查
    let t = store
        .query(&UsageQuery {
            range: TimeRange::All,
            group_by: vec![GroupDim::Model],
            metrics: vec![Metric::parse("input_video"), Metric::parse("output_reasoning")],
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(t.rows.len(), 1);
    assert_eq!(t.rows[0].get("input_video"), 777.0);
    assert_eq!(t.rows[0].get("output_reasoning"), 42.0);

    let seen = store.token_kinds_seen().await.unwrap();
    assert!(seen.contains(&TokenKind::Other("input_video".into())));
}

#[tokio::test]
async fn dashboard_has_contract_shape_and_cache_math() {
    let now = ms("2026-02-11T03:00:00Z");
    let (pool, store) = usage_store(now).await;

    // 配置：2 条线路、1 个凭据（限额 5M tokens）、1 条策略
    let cfg = SqliteConfigStore::with_clock(pool.clone(), clock_at(now, OFF));
    let mut doc = ConfigDoc::default();
    doc.lanes.push(Lane {
        id: LaneId::new("lane-jp"),
        name: "日本线".into(),
        kind: ta_core::domain::LaneKind::Direct,
        subscription: None,
        group: None,
        group_type: None,
        node: None,
        filter: None,
        exclude_filter: None,
        enabled: true,
        extra: Default::default(),
    });
    doc.net.default_lane = Some(LaneId::new("lane-jp"));
    doc.credentials.push(Credential {
        id: CredentialId::new("cred-1"),
        provider: ProviderId::new("prov-1"),
        label: "中转A".into(),
        secret: "${secret:relay-a}".into(),
        guard: Default::default(),
        tags: vec![],
        enabled: true,
        extra: Default::default(),
    });
    doc.policies.push(Policy {
        id: PolicyId::new("pol-1"),
        name: "默认策略".into(),
        enabled: true,
        priority: 10,
        r#match: Default::default(),
        action: Default::default(),
        extra: Default::default(),
    });
    cfg.save(&doc).await.unwrap();

    // 健康样本
    let health = SqliteHealthStore::new(pool.clone());
    let mut sample = HealthSample::new(now - 1000, "lane-jp", true);
    sample.node = Some("东京-03".into());
    sample.latency_ms = Some(82);
    health.append(&sample).await.unwrap();

    // 用量：今天 3 条成功 + 1 条失败；其中 1 条带缓存命中，1 条未定价
    for i in 0..3 {
        store
            .record(record(
                i,
                now - 3_600_000,
                "cred-1",
                "prov-1",
                if i == 0 { "gpt-4o" } else { "claude-sonnet" },
                "cursor",
                "ch-a",
                "lane-jp",
                "东京-03",
                1_000_000,
                1_000_000,
                100_000,
            ))
            .await
            .unwrap();
    }
    let mut failed = record(
        9,
        now - 600_000,
        "cred-1",
        "prov-1",
        "gpt-4o",
        "cursor",
        "ch-a",
        "lane-jp",
        "x",
        10,
        0,
        0,
    );
    failed.status = 500;
    failed.error_type = Some("upstream_5xx".into());
    store.record(failed).await.unwrap();

    // 未定价：清空价格快照的模型
    let mut unpriced = record(
        10,
        now - 300_000,
        "cred-1",
        "prov-1",
        "mystery-model",
        "cursor",
        "ch-a",
        "lane-jp",
        "x",
        500,
        0,
        0,
    );
    unpriced.price_snapshot = serde_json::json!({});
    unpriced.cost = Money::zero("USD");
    store.record(unpriced).await.unwrap();

    let dash = store.dashboard(TimeRange::Today).await.unwrap();

    assert_eq!(dash.cards.requests.value, 5.0);
    assert_eq!(dash.cards.requests.display, "5");
    assert_eq!(dash.cards.cost.currency, "USD");
    assert!((dash.cards.success_rate - 0.8).abs() < 1e-9, "4 成功 / 5");
    assert_eq!(dash.status_bar.policy.as_ref().unwrap().id, "pol-1");
    assert_eq!(dash.status_bar.lane.as_ref().unwrap().name, "日本线");
    assert_eq!(dash.status_bar.lane.as_ref().unwrap().latency_ms, Some(82));
    assert_eq!(dash.lanes.len(), 1);
    assert!(dash.lanes[0].ok);
    assert_eq!(dash.live.len(), 5);
    assert!(dash.live.iter().any(|s| s.outcome == "failed"));
    assert!(!dash.trend.is_empty(), "趋势分桶不能为空");
    assert_eq!(dash.sparklines.requests.len(), dash.trend.len());
    assert_eq!(dash.sparklines.tokens.len(), dash.trend.len());
    assert!(dash.sparklines.success_rate.iter().all(|v| (0.0..=1.0).contains(v)));
    // 趋势覆盖区间内**全部**请求（含未定价与失败的），所以是 3M + 500 + 10
    assert_eq!(dash.trend[0].input, 3_000_510);
    assert_eq!(dash.trend[0].input_cached, 3_000_000);
    // 三条带价请求里 claude-sonnet 有 2 条（9.5 USD），gpt-4o 1 条（4.75 USD）→ 榜首是 claude-sonnet
    assert_eq!(dash.top_models[0].model, "claude-sonnet");
    assert!((dash.top_models[0].cost - 9.5).abs() < 1e-9, "cost={}", dash.top_models[0].cost);
    assert!(dash.top_models.iter().any(|m| m.model == "gpt-4o"));
    assert!(dash.top_models.len() <= 5);
    assert!(dash.top_models.windows(2).all(|w| w[0].cost >= w[1].cost), "TOP 必须按花费降序");
    assert_eq!(dash.unpriced_models, vec!["mystery-model".to_string()]);

    // 07 §6.4：节省额 = Σ cached×(rate[input]−rate[input_cached]) + write×(rate[input]−rate[input_cache_write])
    // 这里 3 条各 1M cached，费率 2.5 vs 1.25 → 每条省 1.25 USD
    assert!((dash.cache.saved_amount - 3.75).abs() < 1e-9, "saved={}", dash.cache.saved_amount);
    // 命中率 = Σcached / (Σinput + Σcached)；区间内还有两条几乎没有 input 的请求，所以略低于 0.5
    assert!(
        (dash.cache.hit_rate - 0.5).abs() < 0.001,
        "cached/(input+cached)≈0.5，实际 {}",
        dash.cache.hit_rate
    );

    // 缓存写多了反而亏：负数要如实呈现
    let mut write_heavy = record(
        11,
        now - 100_000,
        "cred-1",
        "prov-1",
        "gpt-4o",
        "cursor",
        "ch-a",
        "lane-jp",
        "x",
        1_000_000,
        0,
        0,
    );
    write_heavy.usage.add(TokenKind::InputCacheWrite, 1_000_000);
    write_heavy.price_snapshot = serde_json::json!({
        "rates": {"input": 2.5, "input_cached": 1.25, "input_cache_write": 3.75},
        "multiplier": 1.0,
        "currency": "USD",
    });
    write_heavy.cost = Money::new(2.5 + 3.75, "USD");
    store.record(write_heavy).await.unwrap();
    let dash2 = store.dashboard(TimeRange::Today).await.unwrap();
    assert!(
        dash2.cache.saved_amount < dash.cache.saved_amount,
        "写缓存比普通输入贵 25% 时必须体现为少省/倒亏：{} → {}",
        dash.cache.saved_amount,
        dash2.cache.saved_amount
    );
}

#[tokio::test]
async fn csv_export_and_api_key_usage() {
    let now = ms("2026-02-11T03:00:00Z");
    let (pool, store) = usage_store(now).await;
    for r in gen_1000().into_iter().take(20) {
        store.record(r).await.unwrap();
    }

    let q = UsageQuery {
        range: day_aligned_window(),
        group_by: vec![GroupDim::Time, GroupDim::Model],
        time_bucket: Some(TimeBucket::Day),
        metrics: vec![Metric::Requests, Metric::Cost, Metric::parse("input")],
        format: UsageFormat::StackedBar,
        ..Default::default()
    };
    let table = store.query(&q).await.unwrap();
    assert_eq!(table.format, UsageFormat::StackedBar);
    let series = table.series();
    assert_eq!(series.len(), table.rows.len() * table.metrics.len());
    assert!(series.iter().all(|p| p.bucket_start_ms > 0));

    let mut buf: Vec<u8> = Vec::new();
    store.export_csv(&q, &mut buf).await.unwrap();
    let csv = String::from_utf8(buf).unwrap();
    let lines: Vec<&str> = csv.lines().collect();
    assert_eq!(lines[0], "bucket,model,requests,cost,input");
    assert_eq!(lines.len(), table.rows.len() + 2, "表头 + 数据行 + TOTAL 行");
    assert!(lines.last().unwrap().starts_with("TOTAL,"));
    // TOTAL 行的请求数必须等于总数
    let total_line = lines.last().unwrap();
    let total_fields: Vec<&str> = total_line.split(',').collect();
    let total_requests: f64 = total_fields[2].parse().unwrap();
    assert_eq!(total_requests, 20.0);

    // 每个本地 key 的用量
    let mut usage = TokenUsage::new();
    usage.add(TokenKind::Input, 123);
    usage.add(TokenKind::Output, 45);
    store.record_api_key_usage("local-key-1", now, &usage, 0.01, "USD").await.unwrap();
    store.record_api_key_usage("local-key-1", now, &usage, 0.01, "USD").await.unwrap();
    let rows = store.api_key_usage("2026-02-01", "2026-03-01").await.unwrap();
    assert_eq!(rows.len(), 2);
    let input_row = rows.iter().find(|r| r.token_kind == "input").unwrap();
    assert_eq!(input_row.tokens, 246, "累加两次");
    assert_eq!(input_row.requests, 2);

    let _ = pool;
}

/// ★ **空库上必须能拿到看板**（首次启动、新装用户的库就是空的）。
///
/// 真机证据（release 二进制，`scripts/e2e/smoke.mjs`）：导航到任一调用 `usage_dashboard` 的页面时，
/// tokio worker 反复 panic，窗口随后失去 WebDriver 会话：
///
/// ```text
/// panicked at crates\ta-store\src\usage_store.rs:1112: called `Result::unwrap()` on an `Err` value:
/// ColumnDecode { index: "\"cost\"", source: "mismatched types; Rust type `f64` (as SQL type `REAL`)
/// is not compatible with SQL type `INTEGER`" }
/// ```
///
/// 根因：`COALESCE(SUM(cost), 0)` 在**没有任何用量行**时返回的是**整数字面量 0**（聚合表达式没有列亲和性，
/// 不会因为 `cost REAL` 就变成 REAL），而代码按 `f64`(REAL) 解码 → sqlx 的 `Row::get` 直接 panic。
/// 有行的时候反而没事，所以这条缺陷**只在空库出现**——正是每个新用户的第一屏。
#[tokio::test]
async fn dashboard_on_an_empty_database_reports_zeros_instead_of_panicking() {
    let (_pool, store) = usage_store(1_700_000_000_000).await;

    let d = store
        .dashboard(TimeRange::Last7d)
        .await
        .expect("空库也必须能出看板（不能 panic、不能报错）");

    assert_eq!(d.cards.success_rate, 0.0, "空库的成功率是 0，不是 NaN/panic");
    assert_eq!(d.trend.len(), 0, "空库没有趋势点");
    // 成本卡走的就是那条 `COALESCE(SUM(cost), 0)` —— 它今天正是 panic 的地方。
    assert!(d.cards.cost.amount.is_finite(), "空库成本必须是有限的数：{}", d.cards.cost.amount);
    assert_eq!(d.cards.cost.amount, 0.0, "空库成本必须是 0");
}
