//! 用量存储：`usage_request` / `usage_daily` / `api_key_usage`。
//!
//! 契约：`docs/11-internal-contracts.md` §1.3 + §4.3、`docs/07-ui-pages-and-cost.md` §6。
//!
//! 硬约束：
//! - `usage_daily.token_kind` 是**行维度**（不是列）→ 新增 token 类型无需改表；
//! - `usage_request.price_snapshot` **必写** → 改价不改历史；
//! - 一次请求的写入（`usage_request` + `usage_daily`）在**单事务**内完成，
//!   任何一步失败整笔回滚（11 §4.3）；
//! - 时间一律按**本地时区**分桶（`Clock::local_offset_minutes`）。
//!
//! 选表规则（为什么有时候读 `usage_daily`，有时候扫明细）：
//!
//! | 条件 | 用哪张表 |
//! |---|---|
//! | 维度 ⊆ {time(≥day), credential, provider, model, client} 且指标不含 `requests`/`avg_latency` 且无 status 过滤 | `usage_daily`（秒开） |
//! | 其它（含 channel/lane/node、小时桶、状态过滤、平均延迟） | `usage_request`（明细） |
//!
//! `usage_daily.requests` 的语义是"包含该 token_kind 的请求数"，跨类型求和会重复计数，
//! 所以 `requests` 指标一律从明细 `COUNT(*)` 统计。

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sqlx::{Row, SqlitePool};

use ta_core::clock::{Clock, SystemClock, TimeRange};
use ta_core::domain::Policy;
use ta_core::error::Result;
use ta_core::types::{
    ChannelId, ClientId, LaneId, Money, RequestId, TokenKind, TokenUsage, UsageRecord, UsageSource,
};

use crate::timebucket::{self, TimeBucket};
use crate::util::{
    csv_field, csv_num, from_json_or_default, human_count, human_tokens, parse_json_or_null,
    store_err, to_i64, to_json, StoreResultExt,
};

/// 查询默认上限：够 UI 表格用，又不会把内存打爆。
pub const DEFAULT_LIMIT: u32 = 1000;

// ================================================================ 查询 DTO

/// 分组维度（`docs/10-interface-spec.md` §2.2 的 `UsageQuery.group_by`）。
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GroupDim {
    Time,
    Credential,
    Provider,
    Model,
    Channel,
    Client,
    Lane,
    Node,
}

impl GroupDim {
    pub fn key(&self) -> &'static str {
        match self {
            GroupDim::Time => "time",
            GroupDim::Credential => "credential",
            GroupDim::Provider => "provider",
            GroupDim::Model => "model",
            GroupDim::Channel => "channel",
            GroupDim::Client => "client",
            GroupDim::Lane => "lane",
            GroupDim::Node => "node",
        }
    }

    /// 在 `usage_request` 上的取值表达式（明细口径）。
    ///
    /// `Time` 维度不在这里处理（走 `bucket_key_sql`），所以返回 `Option`：
    /// 调用方必须显式跳过，而不是靠 `unreachable!()`。
    fn request_expr(&self) -> Option<&'static str> {
        Some(match self {
            GroupDim::Time => return None,
            GroupDim::Credential => "u.credential_id",
            GroupDim::Provider => "u.provider_id",
            GroupDim::Model => "u.model",
            GroupDim::Channel => "u.channel_id",
            GroupDim::Client => "COALESCE(u.client, '')",
            GroupDim::Lane => "COALESCE(u.lane_id, '')",
            GroupDim::Node => "COALESCE(u.node, '')",
        })
    }

    /// 在 `usage_daily` 上的取值表达式（只有 4 个维度存在于汇总表）。
    fn daily_expr(&self) -> Option<&'static str> {
        match self {
            GroupDim::Credential => Some("credential_id"),
            GroupDim::Provider => Some("provider_id"),
            GroupDim::Model => Some("model"),
            GroupDim::Client => Some("client"),
            _ => None,
        }
    }
}

/// 指标。内置指标 + **任意 `TokenKind`**（开放集合）。
///
/// specta 类型由下方**手写 `impl Type`** 提供：线上形态是一个普通字符串
/// （`"requests"` / `"input_cached"` / …），derive 会生成与线上不符的变体联合类型。
///
/// **serde 同样是手写的**（下方 `impl Serialize/Deserialize`），理由与 specta 一样：
/// derive 出来的是 Pascal（`"Requests"` / `{"Tokens":"input_cached"}`），
/// 而契约（[docs/10 §2.2]）与前端发的是 snake_case 字符串 ——
/// 真机上 `usage_query` / `usage_export_csv` 因此报 `unknown variant 'requests'`
/// （`/dashboard` 不吃 metrics 才躲过）。线上形态只有一种，就是 [`Metric::key`]。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Metric {
    Requests,
    Cost,
    CacheHitRate,
    AvgLatency,
    Tokens(TokenKind),
}

/// 线上形态 = [`Metric::key`]（snake_case 字符串，TokenKind 走 `as_key()`）。
///
/// **不能用 derive / rename_all 代替**：`Tokens(TokenKind)` 那一支无论怎么 rename
/// 都会带上枚举的外部标记（`{"Tokens":…}` 或 `{"tokens":…}`），而契约要求它
/// 直接就是 TokenKind 的字符串形态（`"input_cached"`）。
impl serde::Serialize for Metric {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.key())
    }
}

/// 从 key 反解。**只认一种形态**：Pascal 不再被接受 ——
/// 两种形态并存比"只支持错的那种"更坏，前端会永远不知道该发哪个。
impl<'de> serde::Deserialize<'de> for Metric {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Ok(Metric::parse(&s))
    }
}

/// 手写 specta 类型：线上形态是字符串（见 `Metric` 的注释）。
#[cfg(feature = "specta")]
impl specta::Type for Metric {
    fn definition(_types: &mut specta::Types) -> specta::datatype::DataType {
        specta::datatype::DataType::Primitive(specta::datatype::Primitive::str)
    }
}

impl Metric {
    pub fn key(&self) -> String {
        match self {
            Metric::Requests => "requests".to_string(),
            Metric::Cost => "cost".to_string(),
            Metric::CacheHitRate => "cache_hit_rate".to_string(),
            Metric::AvgLatency => "avg_latency".to_string(),
            Metric::Tokens(k) => k.as_key().to_string(),
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "requests" => Metric::Requests,
            "cost" => Metric::Cost,
            "cache_hit_rate" => Metric::CacheHitRate,
            "avg_latency" => Metric::AvgLatency,
            other => Metric::Tokens(TokenKind::from_key(other)),
        }
    }

    fn forces_detail(&self) -> bool {
        matches!(self, Metric::Requests | Metric::AvgLatency)
    }
}

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageFormat {
    #[default]
    Table,
    StackedBar,
    Area,
}

/// 状态过滤（`ok` | `retried` | `failed`）。
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageStatus {
    Ok,
    Retried,
    Failed,
}

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UsageFilters {
    #[serde(default)]
    pub model: Vec<String>,
    #[serde(default)]
    pub credential: Vec<String>,
    #[serde(default)]
    pub provider: Vec<String>,
    #[serde(default)]
    pub client: Vec<String>,
    /// 空 = 全部；非空时用**或**连接
    #[serde(default)]
    pub status: Vec<UsageStatus>,
}

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageQuery {
    pub range: TimeRange,
    #[serde(default)]
    pub group_by: Vec<GroupDim>,
    #[serde(default)]
    pub time_bucket: Option<TimeBucket>,
    #[serde(default)]
    pub metrics: Vec<Metric>,
    #[serde(default)]
    pub filters: Option<UsageFilters>,
    #[serde(default)]
    pub format: UsageFormat,
    #[serde(default)]
    pub limit: Option<u32>,
}

impl Default for UsageQuery {
    fn default() -> Self {
        Self {
            range: TimeRange::Today,
            group_by: Vec::new(),
            time_bucket: None,
            metrics: Vec::new(),
            filters: None,
            format: UsageFormat::Table,
            limit: None,
        }
    }
}

/// 结果表的一行。
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UsageRow {
    /// 时间维度的 bucket 起点（真实 unix ms，本地对齐）；无时间维度时 `None`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bucket_start_ms: Option<i64>,
    /// 时间维度的本地时间标签（`2026-02-11` / `2026-02-11 13:00` / `2026-02`）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bucket: Option<String>,
    /// 其它分组维度：`model` → `gpt-4o`
    pub dims: BTreeMap<String, String>,
    /// 指标 key → 值（金额为原币种数值）
    pub values: BTreeMap<String, f64>,
}

impl UsageRow {
    pub fn get(&self, metric: &str) -> f64 {
        self.values.get(metric).copied().unwrap_or(0.0)
    }
}

/// 任意 chart 用的三元组（stacked_bar / area 直接吃这个）。
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageSeriesPoint {
    pub bucket_start_ms: i64,
    pub metric: String,
    pub value: f64,
}

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageTable {
    pub format: UsageFormat,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_bucket: Option<TimeBucket>,
    /// 指标 key，顺序与请求一致
    pub metrics: Vec<String>,
    pub group_by: Vec<GroupDim>,
    pub rows: Vec<UsageRow>,
    /// 聚合后的分组总数（截断前）
    pub total_groups: usize,
    pub truncated: bool,
}

impl UsageTable {
    /// stacked_bar / area 的绘图数据；没有时间维度时返回空。
    pub fn series(&self) -> Vec<UsageSeriesPoint> {
        let mut out = Vec::new();
        for row in &self.rows {
            let Some(bucket) = row.bucket_start_ms else { continue };
            for m in &self.metrics {
                out.push(UsageSeriesPoint {
                    bucket_start_ms: bucket,
                    metric: m.clone(),
                    value: row.get(m),
                });
            }
        }
        out
    }
}

// ================================================================ 看板 DTO（10 §2.4）

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Num {
    pub value: f64,
    /// 已排版好的展示串（`12.4M` / `1,284`）
    pub display: String,
}

impl Num {
    pub fn tokens(v: f64) -> Self {
        Self { value: v, display: human_tokens(v) }
    }
    pub fn count(v: f64) -> Self {
        Self { value: v, display: human_count(v) }
    }
}

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NamedRef {
    pub id: String,
    pub name: String,
}

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaneRef {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u32>,
}

/// 顶部状态条。`gateway` 由 Tauri 层按真实运行态覆盖（store 无从得知进程状态）。
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusBar {
    /// `running` | `paused`
    pub gateway: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub policy: Option<NamedRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lane: Option<LaneRef>,
}

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DashboardCards {
    pub requests: Num,
    pub tokens: Num,
    pub cost: Money,
    pub success_rate: f64,
}

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Sparklines {
    pub requests: Vec<f64>,
    pub tokens: Vec<f64>,
    pub cost: Vec<f64>,
    pub success_rate: Vec<f64>,
}

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrendPoint {
    pub bucket_ts: i64,
    pub input: u64,
    pub input_cached: u64,
    pub output: u64,
    pub cost: f64,
}

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaneHealthItem {
    pub lane_id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    pub latency_ms: Option<u32>,
    pub ok: bool,
}

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TopModel {
    pub model: String,
    pub tokens: u64,
    pub cost: f64,
}

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheStat {
    /// `input_cached / (input + input_cached)`
    pub hit_rate: f64,
    /// 07 §6.4 的公式；正数=省了，负数=写缓存反而亏
    pub saved_amount: f64,
    pub currency: String,
}

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DashboardData {
    pub status_bar: StatusBar,
    pub cards: DashboardCards,
    pub sparklines: Sparklines,
    pub trend: Vec<TrendPoint>,
    pub lanes: Vec<LaneHealthItem>,
    pub live: Vec<ta_core::types::RequestSummary>,
    pub top_models: Vec<TopModel>,
    pub cache: CacheStat,
    pub unpriced_models: Vec<String>,
}

// ================================================================ 额度状态 DTO

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApiKeyUsageRow {
    pub key_id: String,
    pub day: String,
    pub token_kind: String,
    pub tokens: i64,
    pub requests: i64,
    pub cost: f64,
    pub currency: String,
}

// ================================================================ trait

#[async_trait]
pub trait UsageStore: Send + Sync {
    /// 一次请求的账：`usage_request` INSERT + `usage_daily` UPSERT（按 token_kind 展开多行）
    /// ，**同一个事务**。
    async fn record(&self, rec: UsageRecord) -> Result<()>;
    /// [docs/32] L6：取某次请求的**按 kind token 明细**（`usage_request.token_kinds`）。
    ///
    /// `None` = 这次请求**没有用量记录**（日志保留期比用量长、或那笔还没落账）——
    /// 调用方据此显示「—」，**绝不用日志里的总 token 去凑一张分项表**。
    async fn token_kinds_of(&self, request_id: &str) -> Result<Option<TokenUsage>>;
    async fn query(&self, q: &UsageQuery) -> Result<UsageTable>;
    async fn dashboard(&self, range: TimeRange) -> Result<DashboardData>;
    async fn token_kinds_seen(&self) -> Result<Vec<TokenKind>>;
    async fn unpriced_models(&self) -> Result<Vec<String>>;
    async fn export_csv(&self, q: &UsageQuery, w: &mut (dyn Write + Send)) -> Result<()>;
    /// 最近请求摘要（看板实时流首次填充；`ta-gateway::GatewayStore` 也用这个）。
    async fn recent_summaries(&self, limit: u32) -> Result<Vec<ta_core::types::RequestSummary>>;
}

// ================================================================ 实现

pub struct SqliteUsageStore {
    pool: SqlitePool,
    clock: Arc<dyn Clock>,
}

impl SqliteUsageStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool, clock: Arc::new(SystemClock) }
    }

    pub fn with_clock(pool: SqlitePool, clock: Arc<dyn Clock>) -> Self {
        Self { pool, clock }
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    fn off_ms(&self) -> i64 {
        timebucket::offset_ms(self.clock.local_offset_minutes())
    }

    // -------------------------------------------------- 写入

    async fn record_in_tx(
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        rec: &UsageRecord,
        off_ms: i64,
    ) -> Result<()> {
        let token_kinds = to_json(&rec.usage)?;
        let usage_raw = to_json(&rec.usage_raw)?;
        let price_snapshot = to_json(&rec.price_snapshot)?;
        let client = rec.client.as_ref().map(|c| c.as_str().to_string());

        let inserted = sqlx::query(
            "INSERT INTO usage_request (
                request_id, ts, credential_id, provider_id, channel_id, model, upstream_model,
                client, lane_id, node, status, error_type, latency_ms, ttfb_ms, token_kinds,
                usage_raw, usage_source, total_tokens, cost, currency, price_snapshot
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(request_id) DO NOTHING",
        )
        .bind(rec.request_id.as_str())
        .bind(rec.ts_ms)
        .bind(rec.credential_id.as_str())
        .bind(rec.provider_id.as_str())
        .bind(rec.channel_id.as_str())
        .bind(&rec.model)
        .bind(&rec.upstream_model)
        .bind(client.clone())
        .bind(rec.lane_id.as_ref().map(|l| l.as_str().to_string()))
        .bind(rec.node.clone())
        .bind(rec.status as i64)
        .bind(rec.error_type.clone())
        .bind(rec.latency_ms as i64)
        .bind(rec.ttfb_ms.map(|v| v as i64))
        .bind(&token_kinds)
        .bind(&usage_raw)
        .bind(usage_source_str(rec.usage_source))
        .bind(to_i64(rec.usage.total()))
        .bind(rec.cost.amount)
        .bind(&rec.cost.currency)
        .bind(&price_snapshot)
        .execute(&mut **tx)
        .await
        .store_err()?;

        // 同一个 request_id 只记一次账（幂等）：重复写入不重复累加 daily。
        if inserted.rows_affected() == 0 {
            return Ok(());
        }

        let day = timebucket::local_day_str(rec.ts_ms, off_ms);
        let client_key = client.unwrap_or_default();
        let shares = per_kind_costs(&rec.usage, &rec.price_snapshot, rec.cost.amount);

        for (kind, tokens) in rec.usage.0.iter().filter(|(_, t)| **t > 0) {
            let cost = shares.get(kind).copied().unwrap_or(0.0);
            sqlx::query(
                "INSERT INTO usage_daily (
                    day, credential_id, provider_id, model, client, token_kind,
                    tokens, requests, cost, currency
                 ) VALUES (?, ?, ?, ?, ?, ?, ?, 1, ?, ?)
                 ON CONFLICT(day, credential_id, provider_id, model, client, token_kind)
                 DO UPDATE SET tokens = tokens + excluded.tokens,
                               requests = requests + excluded.requests,
                               cost = cost + excluded.cost",
            )
            .bind(&day)
            .bind(rec.credential_id.as_str())
            .bind(rec.provider_id.as_str())
            .bind(&rec.model)
            .bind(&client_key)
            .bind(kind.as_key())
            .bind(to_i64(*tokens))
            .bind(cost)
            .bind(&rec.cost.currency)
            .execute(&mut **tx)
            .await
            .store_err()?;
        }

        Ok(())
    }

    /// 单事务写入；任何一步失败 → 显式回滚，保证 `usage_request` 不落半条账。
    pub async fn record_usage(&self, rec: &UsageRecord) -> Result<()> {
        let off_ms = self.off_ms();
        let mut tx = self.pool.begin().await.store_err()?;
        match Self::record_in_tx(&mut tx, rec, off_ms).await {
            Ok(()) => tx.commit().await.store_err(),
            Err(e) => {
                let _ = tx.rollback().await;
                Err(e)
            }
        }
    }

    /// 每个本地 key 的用量（网关鉴权后调用；`key_id` 是本地 key 的标识，不是上游 key）。
    pub async fn record_api_key_usage(
        &self,
        key_id: &str,
        ts_ms: i64,
        usage: &TokenUsage,
        cost: f64,
        currency: &str,
    ) -> Result<()> {
        let day = timebucket::local_day_str(ts_ms, self.off_ms());
        let mut tx = self.pool.begin().await.store_err()?;
        for (kind, tokens) in usage.0.iter().filter(|(_, t)| **t > 0) {
            let r = sqlx::query(
                "INSERT INTO api_key_usage (key_id, day, token_kind, tokens, requests, cost, currency)
                 VALUES (?, ?, ?, ?, 1, ?, ?)
                 ON CONFLICT(key_id, day, token_kind) DO UPDATE SET
                    tokens = tokens + excluded.tokens,
                    requests = requests + excluded.requests,
                    cost = cost + excluded.cost",
            )
            .bind(key_id)
            .bind(&day)
            .bind(kind.as_key())
            .bind(to_i64(*tokens))
            .bind(cost)
            .bind(currency)
            .execute(&mut *tx)
            .await;
            if let Err(e) = r {
                let _ = tx.rollback().await;
                return Err(store_err(e));
            }
        }
        tx.commit().await.store_err()
    }

    pub async fn api_key_usage(&self, from_day: &str, to_day: &str) -> Result<Vec<ApiKeyUsageRow>> {
        let rows = sqlx::query(
            "SELECT key_id, day, token_kind, tokens, requests, cost, currency
             FROM api_key_usage WHERE day >= ? AND day < ? ORDER BY day, key_id, token_kind",
        )
        .bind(from_day)
        .bind(to_day)
        .fetch_all(&self.pool)
        .await
        .store_err()?;
        Ok(rows
            .into_iter()
            .map(|r| ApiKeyUsageRow {
                key_id: r.get("key_id"),
                day: r.get("day"),
                token_kind: r.get("token_kind"),
                tokens: r.get("tokens"),
                requests: r.get("requests"),
                cost: r.get("cost"),
                currency: r.get("currency"),
            })
            .collect())
    }

    // -------------------------------------------------- 平均粘性的用量快照

    /// **时间窗内各线路的 token 累计**（平均粘性的"平均"依据，[docs/48 §4]）。
    ///
    /// 为什么走 `usage_request` 而不是 `usage_daily`：汇总表的维度是
    /// `(day, credential_id, provider_id, model, client, token_kind)`，**没有 `channel_id`** ——
    /// 线路维度的用量只在明细表里。给汇总表加线路维度要改主键（v1 就有的核心表），
    /// 代价远大于这一条查询。
    ///
    /// **它按 `ts` 过滤 + `GROUP BY channel_id`，走 `ix_usage_channel_ts (channel_id, ts)`。**
    /// 调用方**必须**自己缓存（[`ta_core::routing::BalanceSource`] 的约定）：
    /// 这个函数每次都真的查库。
    ///
    /// 返回的 map 里**没有 0 行的线路**（不是补 0）—— 调用方要能区分
    /// "这条时间窗内没用过"（缺席）与"查询根本没数据"（空表）。
    pub async fn channel_token_usage(
        &self,
        since_ms: i64,
    ) -> Result<std::collections::BTreeMap<String, i64>> {
        let rows = sqlx::query(
            "SELECT channel_id, SUM(total_tokens) AS tokens
               FROM usage_request
              WHERE ts >= ? AND channel_id <> ''
              GROUP BY channel_id",
        )
        .bind(since_ms)
        .fetch_all(&self.pool)
        .await
        .store_err()?;
        Ok(rows
            .into_iter()
            .map(|r| (r.get::<String, _>("channel_id"), r.get::<i64, _>("tokens")))
            .collect())
    }

    // -------------------------------------------------- 查询计划与执行

    /// 该查询会走汇总表（`usage_daily`）还是明细表？纯函数，供 UI 提示与测试断言。
    pub fn plan_uses_daily(&self, q: &UsageQuery) -> bool {
        self.plan(q).0.use_daily
    }

    fn plan(&self, q: &UsageQuery) -> (Plan, i64, i64) {
        let off_ms = self.off_ms();
        let now = self.clock.now_ms();
        let (from, to) = timebucket::range_bounds(&q.range, now, off_ms);
        // 汇总表按本地日期列过滤：`All` 必须映射到全日期区间，
        // 不能拿 `i64::MAX` 去格式化日期（会溢出回退成 1970 → 查不到任何数据）。
        let (day_from, day_to) = timebucket::day_bounds(&q.range, now, off_ms);
        let has_time = q.group_by.contains(&GroupDim::Time);
        let bucket = if has_time {
            Some(q.time_bucket.unwrap_or_else(|| timebucket::default_bucket(from, to)))
        } else {
            None
        };
        let dims: Vec<GroupDim> =
            q.group_by.iter().copied().filter(|d| *d != GroupDim::Time).collect();
        let metrics: Vec<Metric> = if q.metrics.is_empty() {
            vec![Metric::Requests, Metric::Cost]
        } else {
            q.metrics.clone()
        };
        let status_filtered = q.filters.as_ref().map(|f| !f.status.is_empty()).unwrap_or(false);
        let day_aligned =
            matches!(q.range, TimeRange::All) || timebucket::is_day_aligned(from, to, off_ms);

        let use_daily = day_aligned
            && dims.iter().all(|d| d.daily_expr().is_some())
            && metrics.iter().all(|m| !m.forces_detail())
            && !status_filtered
            && !matches!(bucket, Some(TimeBucket::Hour));

        (Plan { use_daily, bucket, dims, metrics, day_from, day_to }, from, to)
    }

    async fn run_plan(
        &self,
        plan: &Plan,
        from: i64,
        to: i64,
        filters: &Option<UsageFilters>,
    ) -> Result<Vec<Agg>> {
        let off_ms = self.off_ms();
        if plan.use_daily {
            self.run_daily(plan).await
        } else {
            self.run_detail(plan, from, to, off_ms, filters).await
        }
    }

    async fn run_daily(&self, plan: &Plan) -> Result<Vec<Agg>> {
        // 维度列必须显式 AS 别名：`COALESCE(u.client,'')` 的自动列名不是 `client`。
        // SELECT 用 `expr AS alias`，GROUP BY 只能用别名（否则 `AS` 会进 GROUP BY → 语法错误）。
        let mut select_cols: Vec<String> = Vec::with_capacity(plan.dims.len() + 1);
        let mut group_cols: Vec<String> = Vec::with_capacity(plan.dims.len() + 1);
        for d in &plan.dims {
            // 汇总表没有的维度不可能走到这里（plan 已经筛过），但这里仍显式跳过而非 panic
            if let Some(expr) = d.daily_expr() {
                select_cols.push(format!("{expr} AS {}", d.key()));
                group_cols.push(d.key().to_string());
            }
        }
        // day 列总是取回：时间维度靠它在 Rust 侧换算 bucket key
        select_cols.push("day".to_string());
        group_cols.push("day".to_string());

        // [真机 E2E 抓到的崩溃] `SUM(cost)` 的**返回类型取决于数据**：`usage_daily.cost` 是
        // `REAL`，但只要这一组里所有 cost 都是整数值，SQLite 的 `SUM` 就按 INTEGER 给你
        // （聚合表达式**没有列亲和性**，与 1300 行那条 `COALESCE(SUM(cost), 0)` 同一个坑：
        // 空表返回整数字面量 0）。sqlx 按 `f64` 解码遇到 INTEGER 会直接 `ColumnDecode` 报错，
        // 而这里历史上是 `.unwrap()` → **tokio worker panic**（app 日志里那一串
        // `usage_store.rs:1112 panicked at cost is not compatible with SQL type INTEGER`）。
        //
        // 所以这一格必须**显式 CAST 成 REAL**：SQLite 的 REAL 亲和性把整数结果转成浮点，
        // 于是"全是整数"和"有小数"两条路径得到同一个类型 —— 类型不再随数据变。
        let sql = format!(
            "SELECT {cols}, token_kind, SUM(tokens) AS tokens, CAST(SUM(cost) AS REAL) AS cost
             FROM usage_daily
             WHERE day >= ? AND day < ?
             GROUP BY {group_cols}, token_kind",
            cols = select_cols.join(", "),
            group_cols = group_cols.join(", "),
        );
        let (from_day, to_day) = (plan.day_from.clone(), plan.day_to.clone());
        debug_assert!(from_day <= to_day, "日期区间必须有序：{from_day} .. {to_day}");
        let off_ms = self.off_ms();
        let rows = sqlx::query(&sql)
            .bind(&from_day)
            .bind(&to_day)
            .fetch_all(&self.pool)
            .await
            .store_err()?;

        let mut aggs: BTreeMap<String, Agg> = BTreeMap::new();
        for r in rows {
            let dims = read_dims(&r, &plan.dims);
            let day: String = r.get("day");
            let bucket_key = plan.bucket.map(|b| timebucket::day_to_bucket_key(&day, b, off_ms));
            let agg = aggs
                .entry(group_key(bucket_key, &dims))
                .or_insert_with(|| Agg::new(bucket_key, dims));
            let tokens: i64 = r.get("tokens");
            let cost: f64 = r.get("cost");
            agg.total_tokens += tokens as f64;
            agg.cost += cost;
            let kind: String = r.get("token_kind");
            *agg.tokens.entry(kind).or_insert(0.0) += tokens as f64;
        }
        Ok(aggs.into_values().collect())
    }

    async fn run_detail(
        &self,
        plan: &Plan,
        from: i64,
        to: i64,
        off_ms: i64,
        filters: &Option<UsageFilters>,
    ) -> Result<Vec<Agg>> {
        let mut dim_exprs: Vec<String> = Vec::with_capacity(plan.dims.len());
        let mut dim_aliases: Vec<String> = Vec::with_capacity(plan.dims.len());
        for d in &plan.dims {
            if let Some(expr) = d.request_expr() {
                dim_exprs.push(format!("{expr} AS {}", d.key()));
                dim_aliases.push(d.key().to_string());
            }
        }
        let bucket_expr = plan
            .bucket
            .map(|b| format!("{} AS bucket_key", timebucket::bucket_key_sql("u.ts", b, off_ms)));
        let needs_trace = filters.as_ref().map(|f| !f.status.is_empty()).unwrap_or(false);
        let trace_join =
            if needs_trace { "LEFT JOIN trace t ON t.request_id = u.request_id" } else { "" };
        let (filter_sql, params) = filters_sql(filters);

        let mut select_cols = dim_exprs.clone();
        if let Some(b) = &bucket_expr {
            select_cols.push(b.clone());
        }
        let group_cols = {
            // GROUP BY 只能引用输出别名，不能带 `AS`
            let mut g = dim_aliases.clone();
            if bucket_expr.is_some() {
                g.push("bucket_key".to_string());
            }
            g.join(", ")
        };

        // (A) 按 token_kind 汇总 → 任意 TokenKind 指标都能算，且不用动态拼 JSON 路径
        let sql_a = format!(
            "SELECT {cols}, je.key AS token_kind, SUM(CAST(je.value AS INTEGER)) AS tokens
             FROM usage_request u, json_each(u.token_kinds) je
             {join}
             WHERE u.ts >= ? AND u.ts < ? {filters}
             GROUP BY {group_cols}, je.key",
            cols = select_cols.join(", "),
            join = trace_join,
            filters = filter_sql,
            group_cols = group_cols,
        );

        // (B) 请求数 / 成本 / 延迟（每个请求只算一次，所以单独一条查询）
        let sql_b = format!(
            "SELECT {cols}, COUNT(*) AS requests, SUM(u.cost) AS cost,
                    SUM(u.latency_ms) AS latency_sum,
                    SUM(CASE WHEN u.status >= 200 AND u.status < 300 AND u.error_type IS NULL
                             THEN 1 ELSE 0 END) AS ok_requests
             FROM usage_request u
             {join}
             WHERE u.ts >= ? AND u.ts < ? {filters}
             GROUP BY {group_cols}",
            cols = select_cols.join(", "),
            join = trace_join,
            filters = filter_sql,
            group_cols = group_cols,
        );

        let mut aggs: BTreeMap<String, Agg> = BTreeMap::new();

        let mut qa = sqlx::query(&sql_a).bind(from).bind(to);
        for p in &params {
            qa = qa.bind(p.clone());
        }
        for r in qa.fetch_all(&self.pool).await.store_err()? {
            let dims = read_dims(&r, &plan.dims);
            let bucket_key = plan.bucket.map(|_| r.get::<i64, _>("bucket_key"));
            let agg = aggs
                .entry(group_key(bucket_key, &dims))
                .or_insert_with(|| Agg::new(bucket_key, dims));
            let tokens: i64 = r.get("tokens");
            agg.total_tokens += tokens as f64;
            let kind: String = r.get("token_kind");
            *agg.tokens.entry(kind).or_insert(0.0) += tokens as f64;
        }

        let mut qb = sqlx::query(&sql_b).bind(from).bind(to);
        for p in &params {
            qb = qb.bind(p.clone());
        }
        for r in qb.fetch_all(&self.pool).await.store_err()? {
            let dims = read_dims(&r, &plan.dims);
            let bucket_key = plan.bucket.map(|_| r.get::<i64, _>("bucket_key"));
            let agg = aggs
                .entry(group_key(bucket_key, &dims))
                .or_insert_with(|| Agg::new(bucket_key, dims));
            agg.requests += r.get::<i64, _>("requests") as f64;
            agg.cost += r.get::<f64, _>("cost");
            agg.latency_sum += r.get::<i64, _>("latency_sum") as f64;
            agg.latency_n += r.get::<i64, _>("requests") as f64;
            agg.ok_requests += r.get::<i64, _>("ok_requests") as f64;
        }

        Ok(aggs.into_values().collect())
    }

    // -------------------------------------------------- dashboard 辅助

    /// cache 命中率与节省额（07 §6.4）；逐请求按当时的价格快照算，改价不改历史。
    async fn cache_stat(&self, from: i64, to: i64) -> Result<CacheStat> {
        let rows = sqlx::query(
            "SELECT token_kinds, price_snapshot, currency FROM usage_request
             WHERE ts >= ? AND ts < ?",
        )
        .bind(from)
        .bind(to)
        .fetch_all(&self.pool)
        .await
        .store_err()?;

        let mut cached_total = 0.0f64;
        let mut input_total = 0.0f64;
        let mut saved = 0.0f64;
        let mut by_currency: BTreeMap<String, f64> = BTreeMap::new();
        for r in rows {
            let kinds: BTreeMap<String, f64> =
                from_json_or_default(r.get::<String, _>("token_kinds").as_str());
            let snapshot = parse_json_or_null(r.get::<String, _>("price_snapshot").as_str());
            let currency: String = r.get("currency");
            let rates = snapshot.get("rates").and_then(|v| v.as_object());
            let multiplier = snapshot.get("multiplier").and_then(|v| v.as_f64()).unwrap_or(1.0);
            let rate =
                |k: &str| -> Option<f64> { rates.and_then(|m| m.get(k)).and_then(|v| v.as_f64()) };
            let cached = kinds.get("input_cached").copied().unwrap_or(0.0);
            let input = kinds.get("input").copied().unwrap_or(0.0);
            let write = kinds.get("input_cache_write").copied().unwrap_or(0.0)
                + kinds.get("input_cache_write_1h").copied().unwrap_or(0.0);
            cached_total += cached;
            input_total += input;
            if let Some(r_in) = rate("input") {
                if let Some(r_cached) = rate("input_cached") {
                    saved += cached * (r_in - r_cached) * multiplier / 1_000_000.0;
                }
                if let Some(r_write) = rate("input_cache_write") {
                    saved += write * (r_in - r_write) * multiplier / 1_000_000.0;
                }
                *by_currency.entry(currency.clone()).or_insert(0.0) +=
                    (cached + input) * r_in * multiplier / 1_000_000.0;
            }
        }
        let hit_rate = if cached_total + input_total > 0.0 {
            cached_total / (cached_total + input_total)
        } else {
            0.0
        };
        let currency = by_currency
            .into_iter()
            .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(c, _)| c)
            .unwrap_or_else(|| "USD".to_string());
        Ok(CacheStat { hit_rate, saved_amount: saved, currency })
    }

    async fn dominant_currency(&self, from: i64, to: i64) -> Result<String> {
        let row = sqlx::query(
            "SELECT currency, SUM(cost) AS c FROM usage_request
             WHERE ts >= ? AND ts < ? GROUP BY currency ORDER BY c DESC LIMIT 1",
        )
        .bind(from)
        .bind(to)
        .fetch_optional(&self.pool)
        .await
        .store_err()?;
        Ok(row
            .and_then(|r| r.try_get::<String, _>("currency").ok())
            .unwrap_or_else(|| "USD".to_string()))
    }

    async fn cards(&self, from: i64, to: i64) -> Result<(f64, f64, f64, f64)> {
        // ★ `cost` 这一格**必须显式 CAST 成 REAL**：`COALESCE(SUM(cost), 0)` 在**空表**上返回的是
        // **整数字面量 0** —— 聚合表达式没有列亲和性，不会因为 `cost REAL`（migrations/0001 §）就变 REAL。
        // 而这里按 `f64`(REAL) 解码，sqlx 的 `Row::get` 对 INTEGER 值会**直接 panic**
        // （真机证据：release 二进制打开看板 → `ColumnDecode { index: "cost", ... Rust type f64
        // (as SQL type REAL) is not compatible with SQL type INTEGER }`，窗口随之失去会话）。
        // 有数据行时反而不触发，所以这条缺陷**只在空库出现**，正是每个新用户的第一屏。
        // 回归测试：`crates/ta-store/tests/p4_integration.rs`
        //   `dashboard_on_an_empty_database_reports_zeros_instead_of_panicking`。
        let r = sqlx::query(
            "SELECT COUNT(*) AS requests,
                    COALESCE(SUM(total_tokens), 0) AS tokens,
                    CAST(COALESCE(SUM(cost), 0.0) AS REAL) AS cost,
                    COALESCE(SUM(CASE WHEN status >= 200 AND status < 300 AND error_type IS NULL
                                      THEN 1 ELSE 0 END), 0) AS ok_count
             FROM usage_request WHERE ts >= ? AND ts < ?",
        )
        .bind(from)
        .bind(to)
        .fetch_one(&self.pool)
        .await
        .store_err()?;
        let requests = r.get::<i64, _>("requests") as f64;
        let tokens = r.get::<i64, _>("tokens") as f64;
        let cost = r.get::<f64, _>("cost");
        let ok = r.get::<i64, _>("ok_count") as f64;
        let success_rate = if requests > 0.0 { ok / requests } else { 0.0 };
        Ok((requests, tokens, cost, success_rate))
    }

    async fn status_bar(&self) -> Result<StatusBar> {
        // 策略：priority 最小的启用项；线路：net.default_lane
        let policies: Vec<Policy> = self.config_entities("policy").await?;
        let policy = policies
            .iter()
            .filter(|p| p.enabled)
            .min_by_key(|p| p.priority)
            .map(|p| NamedRef { id: p.id.as_str().to_string(), name: p.name.clone() });

        let net: Option<ta_core::domain::NetConfig> = self.config_section("net").await?;
        let lanes: Vec<ta_core::domain::Lane> = self.config_entities("lane").await?;
        let mut lane = None;
        if let Some(net) = net {
            if let Some(id) = net.default_lane {
                let name = lanes
                    .iter()
                    .find(|l| l.id == id)
                    .map(|l| l.name.clone())
                    .unwrap_or_else(|| id.as_str().to_string());
                let sample = self.latest_health(id.as_str()).await?;
                lane = Some(LaneRef {
                    id: id.as_str().to_string(),
                    name,
                    node: sample.as_ref().and_then(|s| s.node.clone()),
                    latency_ms: sample.as_ref().and_then(|s| s.latency_ms),
                });
            }
        }
        Ok(StatusBar { gateway: "running".to_string(), policy, lane })
    }

    async fn config_entities<T: serde::de::DeserializeOwned>(&self, kind: &str) -> Result<Vec<T>> {
        let rows = sqlx::query("SELECT json FROM config WHERE kind = ? ORDER BY ord")
            .bind(kind)
            .fetch_all(&self.pool)
            .await
            .store_err()?;
        let mut out = Vec::new();
        for r in rows {
            out.push(crate::util::from_json::<T>(r.get::<String, _>("json").as_str())?);
        }
        Ok(out)
    }

    async fn config_section<T: serde::de::DeserializeOwned>(
        &self,
        name: &str,
    ) -> Result<Option<T>> {
        let row = sqlx::query("SELECT json FROM config WHERE kind='section' AND entity_id = ?")
            .bind(name)
            .fetch_optional(&self.pool)
            .await
            .store_err()?;
        match row {
            Some(r) => Ok(Some(crate::util::from_json::<T>(r.get::<String, _>("json").as_str())?)),
            None => Ok(None),
        }
    }

    async fn latest_health(&self, lane_id: &str) -> Result<Option<LaneSample>> {
        let row = sqlx::query(
            "SELECT node, latency_ms, ok FROM health_sample
             WHERE lane_id = ? ORDER BY ts DESC, id DESC LIMIT 1",
        )
        .bind(lane_id)
        .fetch_optional(&self.pool)
        .await
        .store_err()?;
        Ok(row.map(|r| LaneSample {
            node: r.get::<Option<String>, _>("node"),
            latency_ms: r.get::<Option<i64>, _>("latency_ms").map(|v| v as u32),
            ok: r.get::<i64, _>("ok") != 0,
        }))
    }

    async fn lane_health(&self) -> Result<Vec<LaneHealthItem>> {
        let lanes: Vec<ta_core::domain::Lane> = self.config_entities("lane").await?;
        let mut out = Vec::new();
        for l in lanes {
            let sample = self.latest_health(l.id.as_str()).await?;
            out.push(LaneHealthItem {
                lane_id: l.id.as_str().to_string(),
                name: l.name.clone(),
                node: sample.as_ref().and_then(|s| s.node.clone()),
                latency_ms: sample.as_ref().and_then(|s| s.latency_ms),
                // 没有样本时视为"未见异常"（乐观默认），UI 可用 latency_ms=None 显示灰色
                ok: sample.as_ref().map(|s| s.ok).unwrap_or(true),
            });
        }
        Ok(out)
    }
}

// ================================================================ 内部结构

struct Plan {
    use_daily: bool,
    bucket: Option<TimeBucket>,
    dims: Vec<GroupDim>,
    metrics: Vec<Metric>,
    /// 本地日期闭开区间（只有走汇总表时用）
    day_from: String,
    day_to: String,
}

impl Plan {
    /// 明细口径的计划（看板趋势、top models 等内部调用用）。
    fn detail(bucket: Option<TimeBucket>, dims: Vec<GroupDim>) -> Self {
        Self {
            use_daily: false,
            bucket,
            dims,
            metrics: Vec::new(),
            day_from: String::new(),
            day_to: String::new(),
        }
    }
}

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone)]
struct Agg {
    bucket_key: Option<i64>,
    dims: BTreeMap<String, String>,
    tokens: BTreeMap<String, f64>,
    total_tokens: f64,
    requests: f64,
    ok_requests: f64,
    cost: f64,
    latency_sum: f64,
    latency_n: f64,
}

impl Agg {
    fn new(bucket_key: Option<i64>, dims: BTreeMap<String, String>) -> Self {
        Self {
            bucket_key,
            dims,
            tokens: BTreeMap::new(),
            total_tokens: 0.0,
            requests: 0.0,
            ok_requests: 0.0,
            cost: 0.0,
            latency_sum: 0.0,
            latency_n: 0.0,
        }
    }

    fn value_of(&self, m: &Metric) -> f64 {
        match m {
            Metric::Requests => self.requests,
            Metric::Cost => self.cost,
            Metric::AvgLatency => {
                if self.latency_n > 0.0 {
                    self.latency_sum / self.latency_n
                } else {
                    0.0
                }
            }
            Metric::CacheHitRate => {
                let cached = self.tokens.get("input_cached").copied().unwrap_or(0.0);
                let input = self.tokens.get("input").copied().unwrap_or(0.0);
                if cached + input > 0.0 {
                    cached / (cached + input)
                } else {
                    0.0
                }
            }
            Metric::Tokens(k) => self.tokens.get(k.as_key()).copied().unwrap_or(0.0),
        }
    }
}

struct LaneSample {
    node: Option<String>,
    latency_ms: Option<u32>,
    ok: bool,
}

fn group_key(bucket_key: Option<i64>, dims: &BTreeMap<String, String>) -> String {
    let mut s = String::new();
    if let Some(b) = bucket_key {
        s.push_str(&b.to_string());
    }
    s.push('\u{1}');
    for (k, v) in dims {
        s.push_str(k);
        s.push('=');
        s.push_str(v);
        s.push('\u{2}');
    }
    s
}

fn read_dims(row: &sqlx::sqlite::SqliteRow, dims: &[GroupDim]) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for d in dims {
        let v: Option<String> = row.try_get(d.key()).ok().flatten();
        // 未记录的维度用 `(none)` 展示，但仍归到同一组
        out.insert(d.key().to_string(), v.unwrap_or_else(|| "(none)".to_string()));
    }
    out
}

fn usage_source_str(s: UsageSource) -> &'static str {
    match s {
        UsageSource::Upstream => "upstream",
        UsageSource::UpstreamPartial => "upstream_partial",
        UsageSource::Estimated => "estimated",
    }
}

/// `t.attempts` 的 JSON 数组长度（带 json_valid 守卫，脏数据不会让查询报错）。
const ATTEMPT_COUNT_SQL: &str = "(CASE WHEN t.attempts IS NOT NULL AND json_valid(t.attempts) \
     THEN json_array_length(t.attempts) ELSE 0 END)";

const OUTCOME_SQL: &str = "CASE \
     WHEN u.status < 200 OR u.status >= 400 OR u.error_type IS NOT NULL THEN 'failed' \
     WHEN (CASE WHEN t.attempts IS NOT NULL AND json_valid(t.attempts) \
                THEN json_array_length(t.attempts) ELSE 0 END) > 1 THEN 'retried' \
     ELSE 'ok' END";

/// 过滤条件 → SQL（` AND ...` 片段）+ 文本参数。
fn filters_sql(filters: &Option<UsageFilters>) -> (String, Vec<String>) {
    let mut sql = String::new();
    let mut params: Vec<String> = Vec::new();
    let Some(f) = filters else {
        return (sql, params);
    };
    let in_list = |sql: &mut String, params: &mut Vec<String>, col: &str, vals: &[String]| {
        if vals.is_empty() {
            return;
        }
        let holders = vec!["?"; vals.len()].join(",");
        sql.push_str(&format!(" AND {col} IN ({holders})"));
        params.extend(vals.iter().cloned());
    };
    in_list(&mut sql, &mut params, "u.model", &f.model);
    in_list(&mut sql, &mut params, "u.credential_id", &f.credential);
    in_list(&mut sql, &mut params, "u.provider_id", &f.provider);
    in_list(&mut sql, &mut params, "COALESCE(u.client, '')", &f.client);

    if !f.status.is_empty() {
        let mut conds: Vec<String> = Vec::new();
        for s in &f.status {
            match s {
                UsageStatus::Ok => conds.push(format!(
                    "(u.status >= 200 AND u.status < 300 AND u.error_type IS NULL \
                     AND {ATTEMPT_COUNT_SQL} <= 1)"
                )),
                UsageStatus::Retried => conds.push(format!(
                    "(u.status >= 200 AND u.status < 300 AND u.error_type IS NULL \
                     AND {ATTEMPT_COUNT_SQL} > 1)"
                )),
                UsageStatus::Failed => conds.push(
                    "(u.status < 200 OR u.status >= 400 OR u.error_type IS NOT NULL)".to_string(),
                ),
            }
        }
        // 去重后 OR 连接（ok+retried 覆盖全部成功请求也没问题）
        let mut seen = BTreeSet::new();
        let conds: Vec<String> = conds.into_iter().filter(|c| seen.insert(c.clone())).collect();
        sql.push_str(&format!(" AND ({})", conds.join(" OR ")));
    }
    (sql, params)
}
/// 把请求总成本分摊到各 token_kind（07 §6.5 的 `usage_daily.cost`）。
///
/// - 有价格快照且能算出份额 → 按份额分摊，残差修正到最大份额，**保证求和精确等于总成本**；
/// - 无费率（未定价）→ 整笔成本挂到 token 数最大的那一行，其余记 0（不猜单价）。
fn per_kind_costs(
    usage: &TokenUsage,
    snapshot: &serde_json::Value,
    total: f64,
) -> BTreeMap<TokenKind, f64> {
    let mut out: BTreeMap<TokenKind, f64> = BTreeMap::new();
    let rates = snapshot.get("rates").and_then(|v| v.as_object());
    let multiplier = snapshot.get("multiplier").and_then(|v| v.as_f64()).unwrap_or(1.0);

    let mut raw: Vec<(TokenKind, f64)> = Vec::new();
    let mut sum = 0.0;
    for (kind, tokens) in usage.0.iter().filter(|(_, t)| **t > 0) {
        let rate = rates.and_then(|m| m.get(kind.as_key())).and_then(|v| v.as_f64()).unwrap_or(0.0);
        let c = *tokens as f64 * rate * multiplier / 1_000_000.0;
        sum += c;
        raw.push((kind.clone(), c));
    }
    if raw.is_empty() {
        return out;
    }
    if total <= 0.0 || sum <= 0.0 {
        if total != 0.0 {
            if let Some((k, _)) = usage.0.iter().max_by_key(|(_, t)| **t) {
                out.insert(k.clone(), total);
            }
        }
        return out;
    }
    let scale = total / sum;
    let mut acc = 0.0;
    for (k, c) in raw {
        let v = c * scale;
        acc += v;
        out.insert(k, v);
    }
    let residual = total - acc;
    let max_key = out
        .iter()
        .max_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(k, _)| k.clone());
    if let Some(k) = max_key {
        if let Some(v) = out.get_mut(&k) {
            *v += residual;
        }
    }
    out
}

// ================================================================ UsageStore impl

#[async_trait]
impl UsageStore for SqliteUsageStore {
    async fn token_kinds_of(&self, request_id: &str) -> Result<Option<TokenUsage>> {
        let raw: Option<String> =
            sqlx::query_scalar("SELECT token_kinds FROM usage_request WHERE request_id = ?")
                .bind(request_id)
                .fetch_optional(&self.pool)
                .await
                .store_err()?;
        let Some(raw) = raw else {
            return Ok(None);
        };
        // 存的是一段 JSON；解不开就**如实报错**，不要假装"没有用量"
        Ok(Some(from_json_or_default::<TokenUsage>(&raw)))
    }
    async fn record(&self, rec: UsageRecord) -> Result<()> {
        self.record_usage(&rec).await
    }

    async fn query(&self, q: &UsageQuery) -> Result<UsageTable> {
        let (plan, from, to) = self.plan(q);
        let limit = q.limit.unwrap_or(DEFAULT_LIMIT) as usize;
        let mut aggs = self.run_plan(&plan, from, to, &q.filters).await?;

        let metrics: Vec<String> = plan.metrics.iter().map(|m| m.key()).collect();
        let first_metric = plan.metrics.first().cloned();

        // 排序：先时间升序，再按首个指标降序（图表与表格都稳定可预期）
        aggs.sort_by(|a, b| {
            let ka = a.bucket_key;
            let kb = b.bucket_key;
            match (ka, kb) {
                (Some(x), Some(y)) => x.cmp(&y).then_with(|| cmp_metric(b, a, &first_metric)),
                _ => cmp_metric(a, b, &first_metric),
            }
        });

        let total_groups = aggs.len();
        let truncated = total_groups > limit;
        let off_ms = self.off_ms();
        let rows: Vec<UsageRow> = aggs
            .into_iter()
            .take(limit)
            .map(|agg| {
                // bucket_key 存在 ⟺ 计划里有时间维度（bucket 也一定是 Some）；这里显式配对
                let (bucket_start_ms, bucket) = match (agg.bucket_key, plan.bucket) {
                    (Some(k), Some(b)) => (
                        Some(timebucket::bucket_start_ms(k, b, off_ms)),
                        Some(timebucket::bucket_label(k, b, off_ms)),
                    ),
                    _ => (None, None),
                };
                UsageRow {
                    bucket_start_ms,
                    bucket,
                    dims: agg.dims.clone(),
                    values: plan.metrics.iter().map(|m| (m.key(), agg.value_of(m))).collect(),
                }
            })
            .collect();

        Ok(UsageTable {
            format: q.format,
            time_bucket: plan.bucket,
            metrics,
            group_by: q.group_by.clone(),
            rows,
            total_groups,
            truncated,
        })
    }

    async fn dashboard(&self, range: TimeRange) -> Result<DashboardData> {
        let off_ms = self.off_ms();
        let now = self.clock.now_ms();
        let (from, to) = timebucket::range_bounds(&range, now, off_ms);

        let (requests, tokens, cost, success_rate) = self.cards(from, to).await?;
        let currency = self.dominant_currency(from, to).await?;

        // 趋势 / sparkline：短区间按小时，长区间按天（全部本地时区）。
        // 这里**刻意走明细口径**：sparkline 要成功率（daily 表没有状态列），
        // 且看板区间最长 30 天，明细扫描量很小；`usage_daily` 服务于成本中心的长期查询。
        let bucket = timebucket::default_bucket(from, to);
        let trend_plan = Plan::detail(Some(bucket), Vec::new());
        let mut trend_aggs = self.run_plan(&trend_plan, from, to, &None).await?;
        trend_aggs.sort_by_key(|a| a.bucket_key.unwrap_or(0));

        let mut sparklines = Sparklines::default();
        let mut trend = Vec::new();
        for agg in &trend_aggs {
            let key = agg.bucket_key.unwrap_or(0);
            let start = timebucket::bucket_start_ms(key, bucket, off_ms);
            let input = agg.tokens.get("input").copied().unwrap_or(0.0);
            let cached = agg.tokens.get("input_cached").copied().unwrap_or(0.0);
            let output = agg.tokens.get("output").copied().unwrap_or(0.0);
            sparklines.requests.push(agg.requests);
            sparklines.tokens.push(agg.total_tokens);
            sparklines.cost.push(agg.cost);
            sparklines.success_rate.push(if agg.requests > 0.0 {
                agg.ok_requests / agg.requests
            } else {
                1.0
            });
            trend.push(TrendPoint {
                bucket_ts: start,
                input: input as u64,
                input_cached: cached as u64,
                output: output as u64,
                cost: agg.cost,
            });
        }

        // top models（按花费降序，取前 5）
        let top_plan = Plan::detail(None, vec![GroupDim::Model]);
        let mut top_aggs = self.run_plan(&top_plan, from, to, &None).await?;
        top_aggs.sort_by(|a, b| {
            b.cost.partial_cmp(&a.cost).unwrap_or(std::cmp::Ordering::Equal).then_with(|| {
                b.total_tokens.partial_cmp(&a.total_tokens).unwrap_or(std::cmp::Ordering::Equal)
            })
        });
        let top_models: Vec<TopModel> = top_aggs
            .iter()
            .take(5)
            .map(|a| TopModel {
                model: a.dims.get("model").cloned().unwrap_or_default(),
                tokens: a.total_tokens as u64,
                cost: a.cost,
            })
            .collect();

        let cache = self.cache_stat(from, to).await?;

        Ok(DashboardData {
            status_bar: self.status_bar().await?,
            cards: DashboardCards {
                requests: Num::count(requests),
                tokens: Num::tokens(tokens),
                cost: Money::new(cost, currency),
                success_rate,
            },
            sparklines,
            trend,
            lanes: self.lane_health().await?,
            live: self.recent_summaries(500).await?,
            top_models,
            cache,
            unpriced_models: self.unpriced_models().await?,
        })
    }

    async fn token_kinds_seen(&self) -> Result<Vec<TokenKind>> {
        let rows = sqlx::query(
            "SELECT token_kind FROM usage_daily
             UNION
             SELECT je.key AS token_kind FROM usage_request u, json_each(u.token_kinds) je
             ORDER BY token_kind",
        )
        .fetch_all(&self.pool)
        .await
        .store_err()?;
        let mut out: Vec<TokenKind> = rows
            .into_iter()
            .map(|r| TokenKind::from_key(r.get::<String, _>("token_kind").as_str()))
            .collect();
        out.sort();
        out.dedup();
        Ok(out)
    }

    async fn unpriced_models(&self) -> Result<Vec<String>> {
        let rows = sqlx::query(
            "SELECT DISTINCT model FROM usage_request
             WHERE total_tokens > 0
               AND (price_snapshot IS NULL OR price_snapshot = '' OR price_snapshot = '{}'
                    OR json_extract(price_snapshot, '$.rates') IS NULL
                    OR json_extract(price_snapshot, '$.rates') = '{}')
             ORDER BY model",
        )
        .fetch_all(&self.pool)
        .await
        .store_err()?;
        Ok(rows
            .into_iter()
            .map(|r| r.get::<String, _>("model"))
            .filter(|m| !m.is_empty())
            .collect())
    }

    async fn export_csv(&self, q: &UsageQuery, w: &mut (dyn Write + Send)) -> Result<()> {
        let table = self.query(q).await?;
        let has_time = table.time_bucket.is_some();
        let mut header: Vec<String> = Vec::new();
        if has_time {
            header.push("bucket".to_string());
        }
        for d in &table.group_by {
            if *d != GroupDim::Time {
                header.push(d.key().to_string());
            }
        }
        header.extend(table.metrics.iter().cloned());
        let line = header.iter().map(|h| csv_field(h)).collect::<Vec<_>>().join(",");
        w.write_all(line.as_bytes()).store_err()?;
        w.write_all(b"\n").store_err()?;

        let mut totals: BTreeMap<String, f64> =
            table.metrics.iter().map(|m| (m.clone(), 0.0)).collect();
        for row in &table.rows {
            let mut fields: Vec<String> = Vec::new();
            if has_time {
                fields.push(csv_field(row.bucket.as_deref().unwrap_or("")));
            }
            for d in &table.group_by {
                if *d == GroupDim::Time {
                    continue;
                }
                fields.push(csv_field(row.dims.get(d.key()).map(|s| s.as_str()).unwrap_or("")));
            }
            for m in &table.metrics {
                let v = row.get(m);
                *totals.entry(m.clone()).or_insert(0.0) += v;
                fields.push(csv_num(v));
            }
            w.write_all(fields.join(",").as_bytes()).store_err()?;
            w.write_all(b"\n").store_err()?;
        }

        let mut total_fields: Vec<String> = vec![csv_field("TOTAL")];
        for d in &table.group_by {
            if *d == GroupDim::Time {
                continue;
            }
            total_fields.push(String::new());
        }
        for m in &table.metrics {
            total_fields.push(csv_num(totals.get(m).copied().unwrap_or(0.0)));
        }
        w.write_all(total_fields.join(",").as_bytes()).store_err()?;
        w.write_all(b"\n").store_err()?;
        Ok(())
    }

    async fn recent_summaries(&self, limit: u32) -> Result<Vec<ta_core::types::RequestSummary>> {
        let sql = format!(
            "SELECT u.request_id, u.ts, u.model, u.upstream_model, u.channel_id, u.client,
                    u.lane_id, u.node, u.status, u.error_type, u.latency_ms, u.ttfb_ms,
                    u.total_tokens, u.cost, u.currency,
                    {ATTEMPT_COUNT_SQL} AS attempts, {OUTCOME_SQL} AS outcome
             FROM usage_request u
             LEFT JOIN trace t ON t.request_id = u.request_id
             ORDER BY u.ts DESC, u.request_id DESC
             LIMIT ?"
        );
        let rows = sqlx::query(&sql).bind(limit as i64).fetch_all(&self.pool).await.store_err()?;
        Ok(rows
            .into_iter()
            .map(|r| {
                let attempts = r.get::<i64, _>("attempts").max(1) as u32;
                let cost = r.get::<f64, _>("cost");
                let currency = r.get::<String, _>("currency");
                let outcome = r
                    .try_get::<Option<String>, _>("outcome")
                    .ok()
                    .flatten()
                    .unwrap_or_else(|| "ok".to_string());
                ta_core::types::RequestSummary {
                    request_id: RequestId::new(r.get::<String, _>("request_id")),
                    ts_ms: r.get("ts"),
                    model: r.get("model"),
                    upstream_model: r.get("upstream_model"),
                    channel_id: ChannelId::new(r.get::<String, _>("channel_id")),
                    client: r.get::<Option<String>, _>("client").map(ClientId::new),
                    lane_id: r.get::<Option<String>, _>("lane_id").map(LaneId::new),
                    node: r.get::<Option<String>, _>("node"),
                    status: r.get::<i64, _>("status") as u16,
                    outcome,
                    latency_ms: r.get::<i64, _>("latency_ms") as u32,
                    ttfb_ms: r.get::<Option<i64>, _>("ttfb_ms").map(|v| v as u32),
                    total_tokens: r.get::<i64, _>("total_tokens") as u64,
                    cost: Money::new(cost, currency),
                    attempts,
                    error_type: r.get::<Option<String>, _>("error_type"),
                }
            })
            .collect())
    }
}

fn cmp_metric(a: &Agg, b: &Agg, first: &Option<Metric>) -> std::cmp::Ordering {
    match first {
        Some(m) => b.value_of(m).partial_cmp(&a.value_of(m)).unwrap_or(std::cmp::Ordering::Equal),
        None => std::cmp::Ordering::Equal,
    }
}

/// 金额展示（看板用）。
pub fn money_display(amount: f64, currency: &str) -> String {
    use crate::util::fmt_amount;
    format!("{currency} {}", fmt_amount(amount))
}

#[cfg(test)]
mod token_kinds_tests {
    use super::*;
    use ta_core::types::{RequestId, TokenKind};

    /// [docs/32] L6：按 request_id 取**按 kind 的 token 明细**。
    ///
    /// 三条断言对应界面的三条路：有记录 → 显示分项；没记录 → 「—」（不是 0）；
    /// 存量数据解不开 → 如实报错（不假装"没有用量"）。
    #[tokio::test]
    async fn reads_per_kind_tokens_and_reports_absence_honestly() {
        let pool = crate::open_memory().await.expect("开内存库");
        let store = SqliteUsageStore::new(pool);
        let rid = RequestId::new("req_usage_1");

        let mut usage = TokenUsage::new();
        usage.add(TokenKind::Input, 100);
        usage.add(TokenKind::Output, 25);
        let _ = &usage;
        let rec = UsageRecord {
            request_id: rid.clone(),
            ts_ms: 1_000,
            credential_id: ta_core::types::CredentialId::new("cred-1"),
            provider_id: ta_core::types::ProviderId::new("prov-1"),
            channel_id: ta_core::types::ChannelId::new("ch-1"),
            model: "m".to_string(),
            upstream_model: "m-2026".to_string(),
            client: None,
            lane_id: None,
            node: None,
            status: 200,
            error_type: None,
            latency_ms: 100,
            ttfb_ms: Some(30),
            usage,
            usage_raw: serde_json::Value::Null,
            usage_source: UsageSource::Upstream,
            cost: ta_core::types::Money::new(0.0, "USD"),
            price_snapshot: serde_json::Value::Null,
        };
        store.record_usage(&rec).await.expect("落账");

        let got = store.token_kinds_of(rid.as_str()).await.expect("读得到");
        let got = got.expect("应当有分项");
        assert_eq!(got.get(&TokenKind::Input), 100);
        assert_eq!(got.get(&TokenKind::Output), 25);

        // 没记过账的请求 → `None`（界面显示「—」，**不许**显示 0）
        assert!(store.token_kinds_of("req_never_seen").await.expect("查询本身要成功").is_none());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn usage(pairs: &[(TokenKind, u64)]) -> TokenUsage {
        let mut u = TokenUsage::new();
        for (k, v) in pairs {
            u.add(k.clone(), *v);
        }
        u
    }

    #[test]
    fn per_kind_cost_sums_to_total() {
        let u = usage(&[
            (TokenKind::Input, 1_000_000),
            (TokenKind::InputCached, 1_000_000),
            (TokenKind::Output, 100_000),
        ]);
        let snap = json!({
            "rates": {"input": 2.5, "input_cached": 1.25, "output": 10.0},
            "multiplier": 1.0,
            "currency": "USD"
        });
        let shares = per_kind_costs(&u, &snap, 4.0);
        let sum: f64 = shares.values().sum();
        assert!((sum - 4.0).abs() < 1e-9, "分摊必须精确等于总成本：{sum}");
        // 未改价前：input 2.5 + cached 1.25 + output 1.0 = 4.75 → 按比例缩到 4.0
        assert!(
            *shares.get(&TokenKind::Input).unwrap() > *shares.get(&TokenKind::InputCached).unwrap()
        );
    }

    #[test]
    fn metric_roundtrip_keeps_open_token_kinds() {
        for s in ["requests", "cost", "cache_hit_rate", "avg_latency", "input", "output_reasoning"]
        {
            assert_eq!(Metric::parse(s).key(), s);
        }
        assert_eq!(Metric::parse("brand_new_kind").key(), "brand_new_kind");
    }

    // ---------------------------------------------------------------- 线上形态（task-24）

    /// ★ `Metric` 的**线上形态必须是 snake_case 字符串**。
    ///
    /// 依据 [docs/10 §2.2]（`metrics: ("requests"|"cost"|…|TokenKind)[]`）与本文件
    /// `Metric` 上方那段注释。derive 出来的形态是 Pascal（`"Requests"`），
    /// 真机上 `usage_query` / `usage_export_csv` 就是因为这个直接报
    /// `unknown variant 'requests'` —— `/dashboard` 不吃 metrics 才躲过。
    #[test]
    fn metric_online_form_is_snake_case_not_pascal() {
        // 序列化：走 key()，不是变体名
        assert_eq!(json!(Metric::Requests), json!("requests"));
        assert_eq!(json!(Metric::Cost), json!("cost"));
        assert_eq!(json!(Metric::CacheHitRate), json!("cache_hit_rate"));
        assert_eq!(json!(Metric::AvgLatency), json!("avg_latency"));
        // `Tokens(TokenKind)` 要落到 TokenKind 自己的字符串形态 ——
        // **不是** {"Tokens":…}，也不是 {"tokens":…}（外部标记两种都不对）
        assert_eq!(json!(Metric::Tokens(TokenKind::InputCached)), json!("input_cached"));

        // 反序列化：前端按契约发的 snake_case 必须能解出来
        assert_eq!(serde_json::from_value::<Metric>(json!("requests")).unwrap(), Metric::Requests);
        assert_eq!(serde_json::from_value::<Metric>(json!("cost")).unwrap(), Metric::Cost);
        assert_eq!(
            serde_json::from_value::<Metric>(json!("cache_hit_rate")).unwrap(),
            Metric::CacheHitRate
        );
        assert_eq!(
            serde_json::from_value::<Metric>(json!("avg_latency")).unwrap(),
            Metric::AvgLatency
        );
        assert_eq!(
            serde_json::from_value::<Metric>(json!("input_cached")).unwrap(),
            Metric::Tokens(TokenKind::InputCached)
        );
        // 开放集合：没见过的 key 也要能收（不报错）
        assert_eq!(
            serde_json::from_value::<Metric>(json!("brand_new_kind")).unwrap(),
            Metric::Tokens(TokenKind::from_key("brand_new_kind"))
        );

        // 开放集合的直接后果：**任何字符串都是合法的 TokenKind 键**，
        // 所以 "Requests" 会被当成一个**新的 token 类型**，而不是 `Metric::Requests`。
        // 这不是缺陷（契约的 TokenKind 本来就是开放集合），但它说明
        // "Pascal 不再等于内置指标"这件事必须钉住 —— 否则前端发 Pascal
        // 会静默变成"查询一个叫 Requests 的 token 类型"，拿回一堆 0。
        assert_ne!(
            serde_json::from_value::<Metric>(json!("Requests")).unwrap(),
            Metric::Requests,
            "Pascal 形态绝不能落到内置指标上"
        );
    }

    /// 端到端入参路径：`usage_query` / `usage_export_csv` 收到的就是 `UsageQuery`。
    ///
    /// 这里**不从零拼 JSON**：`range` 的形态是另一个任务的范围（它现在是
    /// 外部标记的变体名，曾漂移成 `{"preset":…}`）。从 `UsageQuery::default()`
    /// 出发、只替换 `metrics`，就能只测 metric 这一件事、不被 range 的形态牵着走。
    #[test]
    fn usage_query_accepts_contract_shaped_metrics() {
        // 默认 `UsageQuery` 的 metrics 是**空的**，所以显式给一份真实查询
        let q = UsageQuery {
            metrics: vec![Metric::Requests, Metric::Cost, Metric::Tokens(TokenKind::InputCached)],
            ..Default::default()
        };
        let mut v = serde_json::to_value(&q).expect("查询可序列化");

        // 序列化出来的**必须**是 snake_case 字符串；`Tokens(..)` 不能带外部标记
        let as_text = v["metrics"].to_string();
        assert!(as_text.contains("requests"), "线上形态必须是 snake_case：{as_text}");
        assert!(as_text.contains("input_cached"), "TokenKind 要用自己的字符串形态：{as_text}");
        assert!(!as_text.contains("Requests"), "不该出现 Pascal：{as_text}");
        assert!(!as_text.contains("Tokens"), "Tokens 那支不该带枚举标记：{as_text}");

        // 再按前端实际会发的契约形态反序列化回来
        v["metrics"] = json!(["requests", "cost", "input_cached"]);
        let back: UsageQuery = serde_json::from_value(v)
            .expect("契约形态的 metrics 必须能解出来（这就是真机上的失败点）");
        assert_eq!(
            back.metrics,
            vec![Metric::Requests, Metric::Cost, Metric::Tokens(TokenKind::InputCached)]
        );
    }

    /// **守卫：凡按 `f64` 解码的 `SUM(cost)` 必须显式 `CAST(... AS REAL)`。**
    ///
    /// 为什么用**源码扫描**而不是"造数据跑一遍"：`usage_daily.cost` 声明成 `REAL`，
    /// 而 SQLite 的 REAL 亲和性会把写进去的整数**转成浮点存**，所以单元测试里造不出
    /// "聚合结果变成 INTEGER"那一刻 —— 我试过，修与不修都绿（那是**假覆盖**）。
    /// 而 app 日志里出现过真实的
    /// `ColumnDecode { index: "cost", ... Rust type f64 (as SQL type REAL) is not compatible
    ///  with SQL type INTEGER }` 崩溃（tokio worker panic）。
    ///
    /// SQLite 的语义本来就写着：**聚合表达式没有列亲和性**，`SUM` 的结果类型由输入值决定
    /// （"全是整数"→ INTEGER，空表上的 `COALESCE(SUM(cost), 0)` → 整数字面量 0）。
    /// 所以这条纪律不能靠"跑一遍看看"，只能钉住 SQL 的形状。
    ///
    /// 已知的例外：`dominant_currency` 里那句 `SUM(cost) AS c` **只用于 `ORDER BY`**，
    /// 结果被当成字符串读，不涉 `f64` 解码 —— 它在下面的白名单里，且**只有**这一处。
    #[test]
    fn cost_aggregations_are_cast_to_real() {
        let src = include_str!("usage_store.rs");
        // ★ 只扫**生产代码**那一段：本函数自己的断言文案里就写着 `SUM(cost)`，
        //   整文件扫会自匹配（第一次写就是这么红的）。
        let prod = src.split("#[cfg(test)]").next().unwrap_or(src);
        // 白名单：这一处 SUM(cost) 只做排序，不解码成 f64（改它就得改这个测试的意图）
        const ORDER_BY_ONLY: &str = "SELECT currency, SUM(cost) AS c FROM usage_request";
        let mut bare: Vec<(usize, String)> = Vec::new();
        for (i, line) in prod.lines().enumerate() {
            // 只看**真的 SQL 行**：注释里提到 `SUM(cost)`（本改动自己的说明、以及 1300 行
            // 附近那条既有注释）不是查询，不该被当成违规。
            let code = line.trim_start();
            if code.starts_with("//") || !line.contains('"') {
                continue;
            }
            if !line.contains("SUM(cost)") {
                continue;
            }
            if line.contains(ORDER_BY_ONLY) {
                continue;
            }
            if !line.contains("CAST(") {
                bare.push((i + 1, line.trim().to_string()));
            }
        }
        assert!(
            bare.is_empty(),
            "这些 SUM(cost) 没有 CAST 成 REAL —— 聚合结果可能是 INTEGER，\
             按 f64 解码会直接 panic（真机上出过）：{bare:#?}"
        );
    }
}
