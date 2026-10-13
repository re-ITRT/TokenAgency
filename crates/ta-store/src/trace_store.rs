//! trace 存储：`trace` 表 + 清理 `gc()`。
//!
//! 契约：`docs/11-internal-contracts.md` §1.3 + §4.2、`docs/07-ui-pages-and-cost.md` §2。
//!
//! 硬约束：
//! - `body` / `resp_body` / `*_headers` **只存 zstd 压缩 BLOB**；
//! - 图片块只存 `{type, media_type, bytes, sha256_prefix}`，**绝不存 base64**；
//! - `authorization` / `x-api-key` / `api-key` / `cookie` **永不记录**（值一律 `***`）；
//! - 单字段 > 32 KB 截断并标 `truncated: true`；
//! - `LogMode::Shape` 只记字段结构（键名、层级、数组长度、图片块的 type/media_type/bytes/sha256 前缀）；
//! - `gc()`：先按 retention 过期，再按 max_rows / max_size **双限从最老删**，
//!   最后 `PRAGMA incremental_vacuum`（**不做全量 VACUUM**，07 §2.3）。
//!
//! 脱敏在**写入前**做（store 侧再兜一层），所以即使上游代码漏了脱敏，库里也不会有 key。

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{Row, SqlitePool};

use ta_core::clock::{Clock, SystemClock};
use ta_core::error::Result;
use ta_core::trace::HeaderDiff;
use ta_core::types::{
    ChannelGroupId, ChannelId, ClientId, LogMode, PolicyId, Protocol, RequestId, Retention,
};

use crate::usage_store::UsageStatus;
use crate::util::{
    compress, decompress, from_json, from_json_or_default, parse_json_or_null, sha256_prefix,
    to_i64, to_json, StoreResultExt,
};

/// `LogMode::Full` 下单字段体积上限（07 §2.2）。
pub const MAX_FIELD_BYTES: usize = 32 * 1024;

/// 搜索模式下的候选扫描行数上限（body 是压缩 BLOB，只能解压后匹配）。
const SEARCH_SCAN_ROWS: u32 = 2_000;

// ================================================================ DTO

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceAttempt {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lane: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    pub ms: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// 一条请求的日志记录。头/体由调用方原样给出，**落库前 store 统一脱敏 + 压缩**。
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone)]
pub struct TraceRecord {
    pub request_id: RequestId,
    pub ts_ms: i64,
    pub client: Option<ClientId>,
    pub model: String,
    pub protocol_in: Protocol,
    pub protocol_out: Protocol,
    pub stream: bool,
    pub policy_id: Option<PolicyId>,
    pub group_id: Option<ChannelGroupId>,
    pub chosen_channel: Option<ChannelId>,
    pub attempts: Vec<TraceAttempt>,
    /// 客户端头 vs 上游头的差异（**正常应恒为空**，非空说明网关有 bug）。
    ///
    /// 类型是 [`HeaderDiff`] 而不是 `serde_json::Value`：它的形状是**已知的**，
    /// 类型化之后前端能直接渲染成"谁多了/谁少了/谁被改了"；
    /// 顺带也避开了 specta 对 `serde_json::Value` 里 `i64` 的 BigInt 禁令。
    pub header_diff: Option<HeaderDiff>,
    pub tokens: u64,
    pub status: u16,
    pub total_ms: u32,
    pub ttfb_ms: Option<u32>,
    pub error: Option<String>,
    pub request_headers: Vec<(String, String)>,
    pub response_headers: Vec<(String, String)>,
    /// ⚠️ **任意 JSON**：请求体原文（已脱敏）。
    ///
    /// 用 `Any` 覆盖导出类型是**有意的**：它没有固定形状（用户的请求长什么样它就是什么样），
    /// 给不出具体类型；而 specta 又会拒绝导出 `serde_json::Value` 里的 `i64`。
    /// **这是"这里真的是任意 JSON"，不是"懒得想类型"。**
    #[cfg_attr(feature = "specta", specta(type = specta_typescript::Any))]
    pub request_body: Option<serde_json::Value>,
    #[cfg_attr(feature = "specta", specta(type = specta_typescript::Any))]
    pub response_body: Option<serde_json::Value>,
}

impl TraceRecord {
    pub fn new(request_id: RequestId, ts_ms: i64, model: impl Into<String>) -> Self {
        Self {
            request_id,
            ts_ms,
            client: None,
            model: model.into(),
            protocol_in: Protocol::Openai,
            protocol_out: Protocol::Openai,
            stream: false,
            policy_id: None,
            group_id: None,
            chosen_channel: None,
            attempts: Vec::new(),
            header_diff: None,
            tokens: 0,
            status: 200,
            total_ms: 0,
            ttfb_ms: None,
            error: None,
            request_headers: Vec::new(),
            response_headers: Vec::new(),
            request_body: None,
            response_body: None,
        }
    }
}

/// 日志列表行（`log_query` 的 `rows`）。
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceRow {
    pub request_id: String,
    pub ts_ms: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client: Option<String>,
    pub model: String,
    pub protocol_in: String,
    pub protocol_out: String,
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub policy_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chosen_channel: Option<String>,
    pub attempts: u32,
    pub status: u16,
    /// `ok` | `retried` | `failed`
    pub outcome: String,
    pub total_ms: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ttfb_ms: Option<u32>,
    pub tokens: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub mode: String,
    pub has_body: bool,
    pub body_bytes: u64,
}

/// 单条详情（`log_get_trace`）：尝试链 + 耗时 + header_diff + body 结构/原文。
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceDetail {
    pub row: TraceRow,
    pub attempts: Vec<TraceAttempt>,
    /// [docs/32] L6：这次请求的**按 kind token 明细**（来自用量表，`usage_request.token_kinds`）。
    ///
    /// 由**命令层**填（store 不该知道用量表）：日志的保留期可能比用量短，
    /// 所以这里是 `Option` —— `None` 表示"用量侧没有这笔账"，界面据此显示「—」，
    /// **绝不**用 `row.tokens` 凑一张分项表。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_kinds: Option<ta_core::types::TokenUsage>,
    /// 见 [`TraceRecord::header_diff`]：**有已知形状**，所以用具体类型而不是 `any`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub header_diff: Option<HeaderDiff>,
    /// ⚠️ **任意 JSON**（body 的字段结构摘要），只能用 `Any`。理由同 [`TraceRecord::request_body`]。
    #[cfg_attr(feature = "specta", specta(type = specta_typescript::Any))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_shape: Option<serde_json::Value>,
    #[cfg_attr(feature = "specta", specta(type = specta_typescript::Any))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_headers: Option<serde_json::Value>,
    #[cfg_attr(feature = "specta", specta(type = specta_typescript::Any))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_headers: Option<serde_json::Value>,
    #[cfg_attr(feature = "specta", specta(type = specta_typescript::Any))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_body: Option<serde_json::Value>,
    #[cfg_attr(feature = "specta", specta(type = specta_typescript::Any))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_body: Option<serde_json::Value>,
}

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LogQuery {
    #[serde(default)]
    pub from_ms: Option<i64>,
    #[serde(default)]
    pub to_ms: Option<i64>,
    #[serde(default)]
    pub model: Vec<String>,
    #[serde(default)]
    pub channel: Vec<String>,
    /// 凭据过滤（trace 表没有该列，走 usage_request 关联）
    #[serde(default)]
    pub credential: Vec<String>,
    #[serde(default)]
    pub client: Vec<String>,
    #[serde(default)]
    pub status: Vec<UsageStatus>,
    #[serde(default)]
    pub policy_id: Option<String>,
    /// 文本检索：请求 ID / 模型 / 错误 / 客户端 / 策略 / 通道。
    /// `full` 模式的 body 是压缩 BLOB，额外的正文匹配在解压后于内存中完成（受限扫描）。
    #[serde(default)]
    pub search: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
    /// 游标分页：`log_query` 返回的 `cursor` 原样传回
    #[serde(default)]
    pub cursor: Option<String>,
}

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogPage {
    pub rows: Vec<TraceRow>,
    pub total: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    pub has_more: bool,
}

/// [docs/32] **L1 聚合统计**：总数 / 成功 / 失败 / 平均总耗时。
///
/// 为什么单独一个类型而不是塞进 `LogPage`：日志页要显示的四张卡（总请求、成功率、失败、平均延迟）
/// 与"当前这一页的行"是**两次不同的查询** —— 混在一起会让翻页时统计跟着抖。
///
/// **搜索模式下不给统计**（见 `TraceStore::stats`）：正文是压缩体、SQL 数不准，
/// 与其给一个偏小的数字，不如如实拒绝。
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LogStats {
    pub total: u64,
    /// `outcome = 'ok'`
    pub ok: u64,
    /// `outcome <> 'ok'`（含 `retried` 与 `failed`）
    pub failed: u64,
    /// 平均总耗时（毫秒）。**一行都没有时是 `None`** —— 不编 0。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avg_total_ms: Option<u32>,
}

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StorageStats {
    /// trace 行数
    pub rows: u64,
    /// 库文件总大小（page_count × page_size）
    pub bytes: u64,
    /// trace 内容列（body / 头）压缩后合计
    pub body_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oldest_ts: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub newest_ts: Option<i64>,
}

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClearMode {
    All,
    BodiesOnly,
}

/// trace 的保留策略（07 §2.1 的 `request_log`）。
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceLimits {
    pub retention: Retention,
    pub max_rows: u64,
    /// 0 = 不限制（`request_log.max_size_mb` × 1024 × 1024）
    pub max_size_bytes: u64,
}

impl Default for TraceLimits {
    fn default() -> Self {
        Self { retention: Retention::D30, max_rows: 500_000, max_size_bytes: 2048 * 1024 * 1024 }
    }
}

// ================================================================ trait

#[async_trait]
pub trait TraceStore: Send + Sync {
    /// 写一条日志。`LogMode::Off` 时直接返回；`Meta` 只写元数据；`Shape` 只写结构；`Full` 写脱敏后的头 + 压缩体。
    async fn write(&self, t: TraceRecord) -> Result<()>;
    async fn query(&self, q: &LogQuery) -> Result<LogPage>;

    /// [docs/32] L1：按**同一套过滤**聚合出总数 / 成功 / 失败 / 平均耗时。
    ///
    /// `q.search` 非空时**如实拒绝**：正文是压缩 BLOB，SQL 层数不准；
    /// 返回一个偏小的数字比报错更糟（界面会拿它当"真的只有这么多"）。
    async fn stats(&self, q: &LogQuery) -> Result<LogStats>;
    async fn get(&self, request_id: &str) -> Result<Option<TraceDetail>>;
    async fn set_mode(&self, mode: LogMode) -> Result<()>;
    async fn set_retention(&self, r: Retention) -> Result<()>;
    async fn storage_stats(&self) -> Result<StorageStats>;
    async fn clear(&self, mode: ClearMode) -> Result<u64>;
    /// 每小时调用一次：retention → max_rows → max_size，最后 incremental_vacuum。
    async fn gc(&self, now_ms: i64) -> Result<u64>;
}

// ================================================================ 实现

pub struct SqliteTraceStore {
    pool: SqlitePool,
    clock: Arc<dyn Clock>,
}

impl SqliteTraceStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool, clock: Arc::new(SystemClock) }
    }

    pub fn with_clock(pool: SqlitePool, clock: Arc<dyn Clock>) -> Self {
        Self { pool, clock }
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    pub async fn mode(&self) -> Result<LogMode> {
        match self.setting("trace.mode").await? {
            Some(s) => Ok(from_json::<LogMode>(&s).unwrap_or_default()),
            None => Ok(LogMode::default()),
        }
    }

    pub async fn retention(&self) -> Result<Retention> {
        match self.setting("trace.retention").await? {
            Some(s) => Ok(from_json::<Retention>(&s).unwrap_or(Retention::D30)),
            None => Ok(Retention::D30),
        }
    }

    pub async fn limits(&self) -> Result<TraceLimits> {
        let defaults = TraceLimits::default();
        let retention = self.retention().await?;
        let max_rows = self
            .setting("trace.max_rows")
            .await?
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(defaults.max_rows);
        let max_size_bytes = self
            .setting("trace.max_size_bytes")
            .await?
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(defaults.max_size_bytes);
        Ok(TraceLimits { retention, max_rows, max_size_bytes })
    }

    /// 一次性设置全部限制（对应 `request_log.retention/max_rows/max_size_mb`）。
    pub async fn set_limits(&self, limits: &TraceLimits) -> Result<()> {
        self.set_setting("trace.retention", &to_json(&limits.retention)?).await?;
        self.set_setting("trace.max_rows", &limits.max_rows.to_string()).await?;
        self.set_setting("trace.max_size_bytes", &limits.max_size_bytes.to_string()).await?;
        Ok(())
    }

    async fn setting(&self, key: &str) -> Result<Option<String>> {
        let v: Option<String> = sqlx::query_scalar("SELECT value FROM store_setting WHERE key = ?")
            .bind(key)
            .fetch_optional(&self.pool)
            .await
            .store_err()?;
        Ok(v)
    }

    async fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        sqlx::query(
            "INSERT INTO store_setting (key, value, updated_at) VALUES (?, ?, ?)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        )
        .bind(key)
        .bind(value)
        .bind(self.clock.now_ms())
        .execute(&self.pool)
        .await
        .store_err()?;
        Ok(())
    }

    /// 删除最老的 N 行（gc 的分步执行单元）。
    async fn delete_oldest(&self, n: i64) -> Result<u64> {
        let res = sqlx::query(
            "DELETE FROM trace WHERE request_id IN (
                SELECT request_id FROM trace ORDER BY ts ASC, request_id ASC LIMIT ?
             )",
        )
        .bind(n)
        .execute(&self.pool)
        .await
        .store_err()?;
        Ok(res.rows_affected())
    }
}

#[async_trait]
impl TraceStore for SqliteTraceStore {
    async fn write(&self, t: TraceRecord) -> Result<()> {
        let mode = self.mode().await?;
        if !mode.enabled() {
            // off：什么都不记（但聚合用量照记，那在 UsageStore 里）
            return Ok(());
        }

        let attempts_json = to_json(&t.attempts)?;
        let header_diff = match &t.header_diff {
            Some(v) => Some(to_json(v)?),
            None => None,
        };

        let mut body: Option<Vec<u8>> = None;
        let mut resp_body: Option<Vec<u8>> = None;
        let mut req_headers: Option<Vec<u8>> = None;
        let mut resp_headers: Option<Vec<u8>> = None;
        let mut body_shape: Option<String> = None;
        let mut body_bytes: i64 = 0;

        if mode.records_shape() {
            if let Some(v) = &t.request_body {
                body_shape = Some(to_json(&body_shape_of(v))?);
            }
        }
        if mode.records_body() {
            if let Some(v) = &t.request_body {
                let sanitized = sanitize_body(v);
                let json = to_json(&sanitized)?;
                let blob = compress(json.as_bytes())?;
                body_bytes += blob.len() as i64;
                body = Some(blob);
            }
            if let Some(v) = &t.response_body {
                let sanitized = sanitize_body(v);
                let json = to_json(&sanitized)?;
                let blob = compress(json.as_bytes())?;
                body_bytes += blob.len() as i64;
                resp_body = Some(blob);
            }
            if !t.request_headers.is_empty() {
                let json = to_json(&sanitize_headers(&t.request_headers))?;
                let blob = compress(json.as_bytes())?;
                body_bytes += blob.len() as i64;
                req_headers = Some(blob);
            }
            if !t.response_headers.is_empty() {
                let json = to_json(&sanitize_headers(&t.response_headers))?;
                let blob = compress(json.as_bytes())?;
                body_bytes += blob.len() as i64;
                resp_headers = Some(blob);
            }
        }

        sqlx::query(
            "INSERT INTO trace (
                request_id, ts, client, model, protocol_in, protocol_out, stream, policy_id,
                group_id, chosen_channel, attempts, header_diff, tokens, status, total_ms,
                ttfb_ms, error, mode, body, resp_body, req_headers, resp_headers, body_shape,
                body_meta, body_bytes
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, NULL, ?)
             ON CONFLICT(request_id) DO UPDATE SET
                ts = excluded.ts, client = excluded.client, model = excluded.model,
                protocol_in = excluded.protocol_in, protocol_out = excluded.protocol_out,
                stream = excluded.stream, policy_id = excluded.policy_id,
                group_id = excluded.group_id, chosen_channel = excluded.chosen_channel,
                attempts = excluded.attempts, header_diff = excluded.header_diff,
                tokens = excluded.tokens, status = excluded.status, total_ms = excluded.total_ms,
                ttfb_ms = excluded.ttfb_ms, error = excluded.error, mode = excluded.mode,
                body = excluded.body, resp_body = excluded.resp_body,
                req_headers = excluded.req_headers, resp_headers = excluded.resp_headers,
                body_shape = excluded.body_shape, body_bytes = excluded.body_bytes",
        )
        .bind(t.request_id.as_str())
        .bind(t.ts_ms)
        .bind(t.client.as_ref().map(|c| c.as_str().to_string()))
        .bind(&t.model)
        .bind(protocol_str(t.protocol_in))
        .bind(protocol_str(t.protocol_out))
        .bind(if t.stream { 1_i64 } else { 0_i64 })
        .bind(t.policy_id.as_ref().map(|p| p.as_str().to_string()))
        .bind(t.group_id.as_ref().map(|g| g.as_str().to_string()))
        .bind(t.chosen_channel.as_ref().map(|c| c.as_str().to_string()))
        .bind(&attempts_json)
        .bind(header_diff)
        .bind(to_i64(t.tokens))
        .bind(t.status as i64)
        .bind(t.total_ms as i64)
        .bind(t.ttfb_ms.map(|v| v as i64))
        .bind(t.error.clone())
        // 与 `mode()` 的读取格式保持一致（`"meta"` 带引号）
        .bind(to_json(&mode)?)
        .bind(body)
        .bind(resp_body)
        .bind(req_headers)
        .bind(resp_headers)
        .bind(body_shape)
        .bind(body_bytes)
        .execute(&self.pool)
        .await
        .store_err()?;
        Ok(())
    }

    async fn stats(&self, q: &LogQuery) -> Result<LogStats> {
        if q.search.as_deref().is_some_and(|s| !s.trim().is_empty()) {
            return Err(ta_core::error::TaError::MalformedRequest(
                "搜索模式下不提供聚合统计：正文是压缩体，SQL 数不准".to_string(),
            ));
        }
        let (where_sql, params) = trace_filter_sql(q, false);
        let sql = format!(
            // 口径**必须与 `map_trace_row` 一模一样**：表里**没有** `outcome` 列，
            // 它是读时按 `status` / `error` / 尝试次数推导的（"ok" 还要求没有重试）。
            // 这里用同一条规则在 SQL 里数，避免出现"列表说成功、统计说失败"。
            "SELECT COUNT(*) AS total,
                    SUM(CASE WHEN {ok_cond} THEN 1 ELSE 0 END) AS ok,
                    SUM(CASE WHEN NOT ({ok_cond}) THEN 1 ELSE 0 END) AS failed,
                    AVG(t.total_ms) AS avg_ms
             FROM trace t WHERE 1=1 {where_sql}",
            ok_cond = OK_CONDITION
        );
        let mut query = sqlx::query(&sql);
        for p in &params {
            query = query.bind(p.clone());
        }
        let row = query.fetch_one(&self.pool).await.store_err()?;
        let total: i64 = row.try_get("total").unwrap_or(0);
        let ok: i64 = row.try_get("ok").unwrap_or(0);
        let failed: i64 = row.try_get("failed").unwrap_or(0);
        let avg_ms: Option<f64> = row.try_get("avg_ms").ok();
        Ok(LogStats {
            total: total.max(0) as u64,
            ok: ok.max(0) as u64,
            failed: failed.max(0) as u64,
            // **没有行就没有平均**：不依赖 `AVG` 的 NULL 语义（不同 sqlite 版本/driver
            // 解出来可能是 `Some(0.0)`），先看 total —— 这条是"不编 0"的硬保证。
            avg_total_ms: if total <= 0 {
                None
            } else {
                avg_ms.filter(|v| v.is_finite() && *v >= 0.0).map(|v| v.round() as u32)
            },
        })
    }

    async fn query(&self, q: &LogQuery) -> Result<LogPage> {
        let limit = q.limit.unwrap_or(100).clamp(1, 1000);
        // 游标只影响"取哪一页"，绝不影响 total（否则翻页时 total 会越来越小）
        let (where_sql, params) = trace_filter_sql(q, false);
        let (page_sql, page_params) = trace_filter_sql(q, true);

        if let Some(needle) = q.search.as_deref().map(|s| s.to_lowercase()) {
            // body 是压缩 BLOB，SQL 匹配不了正文 → 取候选（最多 SEARCH_SCAN_ROWS 条）解压后过滤。
            // 搜索模式不做游标分页（结果集本身有界），frontend 也只在"日志页"用。
            let sql = format!(
                "SELECT {cols} FROM trace t WHERE 1=1 {where_sql}
                 ORDER BY t.ts DESC, t.request_id DESC LIMIT ?",
                cols = COLS
            );
            let mut query = sqlx::query(&sql);
            for p in &params {
                query = query.bind(p.clone());
            }
            let rows =
                query.bind(SEARCH_SCAN_ROWS as i64).fetch_all(&self.pool).await.store_err()?;
            let mut out = Vec::new();
            for r in rows {
                if row_matches_search(&r, &needle) {
                    out.push(map_trace_row(&r));
                }
            }
            let total = out.len() as u64;
            out.truncate(limit as usize);
            return Ok(LogPage { has_more: false, rows: out, total, cursor: None });
        }

        let total: i64 = {
            let sql = format!("SELECT COUNT(*) FROM trace t WHERE 1=1 {where_sql}");
            let mut query = sqlx::query_scalar(&sql);
            for p in &params {
                query = query.bind(p.clone());
            }
            query.fetch_one(&self.pool).await.store_err()?
        };

        let sql = format!(
            "SELECT {cols} FROM trace t WHERE 1=1 {page_sql}
             ORDER BY t.ts DESC, t.request_id DESC LIMIT ?",
            cols = COLS,
            page_sql = page_sql
        );
        let mut query = sqlx::query(&sql);
        for p in &page_params {
            query = query.bind(p.clone());
        }
        // 多取一行来判断"还有没有下一页"：比用 total - offset 更可靠（游标分页没有 offset）
        let mut rows = query.bind(limit as i64 + 1).fetch_all(&self.pool).await.store_err()?;

        let has_more = rows.len() as u32 > limit;
        if has_more {
            rows.truncate(limit as usize);
        }
        let cursor = if has_more {
            rows.last()
                .map(|r| format!("{}|{}", r.get::<i64, _>("ts"), r.get::<String, _>("request_id")))
        } else {
            None
        };

        Ok(LogPage {
            rows: rows.iter().map(map_trace_row).collect(),
            total: total.max(0) as u64,
            cursor,
            has_more,
        })
    }

    async fn get(&self, request_id: &str) -> Result<Option<TraceDetail>> {
        let row = sqlx::query(&format!(
            "SELECT {cols}, header_diff, body_shape, body, resp_body, req_headers, resp_headers
             FROM trace t WHERE t.request_id = ?",
            cols = COLS
        ))
        .bind(request_id)
        .fetch_optional(&self.pool)
        .await
        .store_err()?;
        let Some(r) = row else { return Ok(None) };

        let blob_to_value = |col: &str| -> Result<Option<serde_json::Value>> {
            let blob: Option<Vec<u8>> = r.try_get(col).ok().flatten();
            match blob {
                Some(b) => {
                    let raw = decompress(&b)?;
                    Ok(Some(from_json_or_default(std::str::from_utf8(&raw).unwrap_or("null"))))
                }
                None => Ok(None),
            }
        };

        let attempts: Vec<TraceAttempt> = r
            .try_get::<Option<String>, _>("attempts")
            .ok()
            .flatten()
            .map(|s| from_json_or_default(&s))
            .unwrap_or_default();
        // `header_diff` 是**有类型的**（[`HeaderDiff`]），所以这里解析成它而不是 `Value`。
        // 老行若形态对不上，`from_json_or_default` 会退化成默认值（空差异）——
        // 对"自检字段"来说这是安全的降级方向：**宁可看起来没有差异，也不要让整条详情读不出来**。
        let header_diff = r
            .try_get::<Option<String>, _>("header_diff")
            .ok()
            .flatten()
            .map(|s| from_json_or_default::<HeaderDiff>(&s));
        let body_shape = r
            .try_get::<Option<String>, _>("body_shape")
            .ok()
            .flatten()
            .map(|s| parse_json_or_null(&s));

        Ok(Some(TraceDetail {
            row: map_trace_row(&r),
            attempts,
            // store 层不查用量表：由命令层补上（见字段注释）
            token_kinds: None,
            header_diff,
            body_shape,
            request_headers: blob_to_value("req_headers")?,
            response_headers: blob_to_value("resp_headers")?,
            request_body: blob_to_value("body")?,
            response_body: blob_to_value("resp_body")?,
        }))
    }

    async fn set_mode(&self, mode: LogMode) -> Result<()> {
        self.set_setting("trace.mode", &to_json(&mode)?).await
    }

    async fn set_retention(&self, r: Retention) -> Result<()> {
        self.set_setting("trace.retention", &to_json(&r)?).await
    }

    async fn storage_stats(&self) -> Result<StorageStats> {
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM trace")
            .fetch_one(&self.pool)
            .await
            .store_err()?;
        let span = sqlx::query(
            "SELECT COALESCE(SUM(body_bytes), 0) AS body_bytes, MIN(ts) AS oldest, MAX(ts) AS newest
             FROM trace",
        )
        .fetch_one(&self.pool)
        .await
        .store_err()?;
        let page_count: i64 =
            sqlx::query_scalar("PRAGMA page_count").fetch_one(&self.pool).await.store_err()?;
        let page_size: i64 =
            sqlx::query_scalar("PRAGMA page_size").fetch_one(&self.pool).await.store_err()?;
        Ok(StorageStats {
            rows: rows.max(0) as u64,
            bytes: (page_count.max(0) * page_size.max(0)) as u64,
            body_bytes: span.get::<i64, _>("body_bytes").max(0) as u64,
            oldest_ts: span.get::<Option<i64>, _>("oldest"),
            newest_ts: span.get::<Option<i64>, _>("newest"),
        })
    }

    async fn clear(&self, mode: ClearMode) -> Result<u64> {
        let res = match mode {
            ClearMode::All => sqlx::query("DELETE FROM trace").execute(&self.pool).await,
            ClearMode::BodiesOnly => {
                sqlx::query(
                    "UPDATE trace SET body = NULL, resp_body = NULL, req_headers = NULL,
                                      resp_headers = NULL, body_bytes = 0
                     WHERE body_bytes > 0 OR body IS NOT NULL OR resp_body IS NOT NULL",
                )
                .execute(&self.pool)
                .await
            }
        }
        .store_err()?;
        // 只清 body 也会释放空间，做一次增量回收
        let _ = sqlx::query("PRAGMA incremental_vacuum").execute(&self.pool).await;
        Ok(res.rows_affected())
    }

    async fn gc(&self, now_ms: i64) -> Result<u64> {
        let limits = self.limits().await?;
        let mut deleted: u64 = 0;

        // 1) retention 过期
        if let Some(ms) = limits.retention.as_ms() {
            let cutoff = now_ms.saturating_sub(ms);
            let res = sqlx::query("DELETE FROM trace WHERE ts < ?")
                .bind(cutoff)
                .execute(&self.pool)
                .await
                .store_err()?;
            deleted += res.rows_affected();
        }

        // 2) max_rows 上限：从最老删
        if limits.max_rows > 0 {
            loop {
                let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM trace")
                    .fetch_one(&self.pool)
                    .await
                    .store_err()?;
                if rows as u64 <= limits.max_rows {
                    break;
                }
                let over = (rows as u64 - limits.max_rows).min(1_000) as i64;
                let n = self.delete_oldest(over).await?;
                deleted += n;
                if n == 0 {
                    break;
                }
            }
        }

        // 3) max_size 上限：按 trace 内容占用（body_bytes 合计）从最老删。
        //    这里按"需要释放的字节数"精确挑选要删的行 —— 固定批量会一次删掉远超需要的数据。
        if limits.max_size_bytes > 0 {
            loop {
                let bytes: i64 =
                    sqlx::query_scalar("SELECT COALESCE(SUM(body_bytes), 0) FROM trace")
                        .fetch_one(&self.pool)
                        .await
                        .store_err()?;
                let bytes = bytes.max(0) as u64;
                if bytes <= limits.max_size_bytes {
                    break;
                }
                let need = (bytes - limits.max_size_bytes) as i64;
                let candidates = sqlx::query(
                    "SELECT request_id, body_bytes FROM trace ORDER BY ts ASC, request_id ASC LIMIT 4096",
                )
                .fetch_all(&self.pool)
                .await
                .store_err()?;
                if candidates.is_empty() {
                    break;
                }
                let mut acc: i64 = 0;
                let mut ids: Vec<String> = Vec::new();
                for r in &candidates {
                    acc += r.get::<i64, _>("body_bytes").max(0);
                    ids.push(r.get::<String, _>("request_id"));
                    if acc >= need {
                        break; // 删到刚好够（最多多删最后一行）
                    }
                }
                let holders = vec!["?"; ids.len()].join(",");
                let sql = format!("DELETE FROM trace WHERE request_id IN ({holders})");
                let mut del = sqlx::query(&sql);
                for id in &ids {
                    del = del.bind(id.clone());
                }
                let n = del.execute(&self.pool).await.store_err()?.rows_affected();
                deleted += n;
                if n == 0 {
                    break;
                }
            }
        }

        // 4) 增量回收（不做全量 VACUUM）
        sqlx::query("PRAGMA incremental_vacuum").execute(&self.pool).await.store_err()?;
        Ok(deleted)
    }
}

// ================================================================ SQL 片段 / 映射

/// 与 `map_trace_row` **同一口径**的"这次算成功"：2xx/3xx、没有 `error`、且**没有重试**。
///
/// 写成一处常量，是为了"统计卡片"和"列表里的徽章"永远说同一件事
/// （[docs/32] 的硬要求：不许出现列表说成功、统计说失败）。
const OK_CONDITION: &str = "(t.status >= 200 AND t.status < 400 AND t.error IS NULL \
     AND json_array_length(COALESCE(t.attempts, '[]')) <= 1)";

const COLS: &str = "t.request_id, t.ts, t.client, t.model, t.protocol_in, t.protocol_out, \
     t.stream, t.policy_id, t.group_id, t.chosen_channel, t.attempts, t.tokens, t.status, \
     t.total_ms, t.ttfb_ms, t.error, t.mode, t.body_bytes, \
     (t.body IS NOT NULL OR t.resp_body IS NOT NULL) AS has_body";

/// trace 内部口径的尝试次数（`t.attempts` 是 JSON 数组）。
const T_ATTEMPTS: &str = "(CASE WHEN t.attempts IS NOT NULL AND json_valid(t.attempts) \
     THEN json_array_length(t.attempts) ELSE 0 END)";

/// 过滤条件 → SQL（` AND ...` 片段）+ 文本参数。
///
/// `with_cursor` 区分"算 total"和"取这一页"：游标不能进 COUNT，否则翻页时 total 会递减。
fn trace_filter_sql(q: &LogQuery, with_cursor: bool) -> (String, Vec<String>) {
    let mut sql = String::new();
    let mut params: Vec<String> = Vec::new();
    if let Some(from) = q.from_ms {
        sql.push_str(" AND t.ts >= ?");
        params.push(from.to_string());
    }
    if let Some(to) = q.to_ms {
        sql.push_str(" AND t.ts < ?");
        params.push(to.to_string());
    }
    let in_list = |sql: &mut String, params: &mut Vec<String>, col: &str, vals: &[String]| {
        if vals.is_empty() {
            return;
        }
        let holders = vec!["?"; vals.len()].join(",");
        sql.push_str(&format!(" AND {col} IN ({holders})"));
        params.extend(vals.iter().cloned());
    };
    in_list(&mut sql, &mut params, "t.model", &q.model);
    in_list(&mut sql, &mut params, "t.chosen_channel", &q.channel);
    in_list(&mut sql, &mut params, "COALESCE(t.client, '')", &q.client);
    if !q.credential.is_empty() {
        let holders = vec!["?"; q.credential.len()].join(",");
        sql.push_str(&format!(
            " AND EXISTS (SELECT 1 FROM usage_request u WHERE u.request_id = t.request_id \
              AND u.credential_id IN ({holders}))"
        ));
        params.extend(q.credential.iter().cloned());
    }
    if let Some(p) = &q.policy_id {
        sql.push_str(" AND t.policy_id = ?");
        params.push(p.clone());
    }
    if !q.status.is_empty() {
        let mut conds: Vec<String> = Vec::new();
        for s in &q.status {
            match s {
                UsageStatus::Ok => conds.push(format!(
                    "(t.status >= 200 AND t.status < 300 AND t.error IS NULL AND {T_ATTEMPTS} <= 1)"
                )),
                UsageStatus::Retried => conds.push(format!(
                    "(t.status >= 200 AND t.status < 300 AND t.error IS NULL AND {T_ATTEMPTS} > 1)"
                )),
                UsageStatus::Failed => {
                    conds.push("(t.status < 200 OR t.status >= 400 OR t.error IS NOT NULL)".into())
                }
            }
        }
        conds.sort();
        conds.dedup();
        sql.push_str(&format!(" AND ({})", conds.join(" OR ")));
    }
    if with_cursor {
        if let Some(cursor) = &q.cursor {
            if let Some((ts, rid)) = cursor.split_once('|') {
                sql.push_str(" AND (t.ts < ? OR (t.ts = ? AND t.request_id < ?))");
                params.push(ts.to_string());
                params.push(ts.to_string());
                params.push(rid.to_string());
            }
        }
    }
    (sql, params)
}

fn map_trace_row(r: &sqlx::sqlite::SqliteRow) -> TraceRow {
    let attempts_json: String = r.try_get("attempts").unwrap_or_else(|_| "[]".into());
    let attempts = from_json_or_default::<Vec<TraceAttempt>>(&attempts_json).len() as u32;
    let status = r.get::<i64, _>("status") as u16;
    let error: Option<String> = r.get("error");
    let outcome = if !(200..400).contains(&status) || error.is_some() {
        "failed"
    } else if attempts > 1 {
        "retried"
    } else {
        "ok"
    };
    TraceRow {
        request_id: r.get("request_id"),
        ts_ms: r.get("ts"),
        client: r.get("client"),
        model: r.get("model"),
        protocol_in: r.get("protocol_in"),
        protocol_out: r.get("protocol_out"),
        stream: r.get::<i64, _>("stream") != 0,
        policy_id: r.get("policy_id"),
        group_id: r.get("group_id"),
        chosen_channel: r.get("chosen_channel"),
        attempts,
        status,
        outcome: outcome.to_string(),
        total_ms: r.get::<i64, _>("total_ms") as u32,
        ttfb_ms: r.get::<Option<i64>, _>("ttfb_ms").map(|v| v as u32),
        tokens: r.get::<i64, _>("tokens") as u64,
        error,
        mode: r.get("mode"),
        has_body: r.try_get::<i64, _>("has_body").map(|v| v != 0).unwrap_or(false),
        body_bytes: r.get::<i64, _>("body_bytes").max(0) as u64,
    }
}

/// 搜索模式下对单行做匹配（含 `full` 模式 body 的解压匹配）。
fn row_matches_search(r: &sqlx::sqlite::SqliteRow, needle: &str) -> bool {
    let text_fields = [
        r.try_get::<String, _>("request_id").unwrap_or_default(),
        r.try_get::<String, _>("model").unwrap_or_default(),
        r.try_get::<Option<String>, _>("client").ok().flatten().unwrap_or_default(),
        r.try_get::<Option<String>, _>("error").ok().flatten().unwrap_or_default(),
        r.try_get::<Option<String>, _>("policy_id").ok().flatten().unwrap_or_default(),
        r.try_get::<Option<String>, _>("chosen_channel").ok().flatten().unwrap_or_default(),
    ];
    if text_fields.iter().any(|f| f.to_lowercase().contains(needle)) {
        return true;
    }
    // 注意：这里**只在内存里**解压比对，不落库、不外传
    for col in ["body", "resp_body"] {
        let blob: Option<Vec<u8>> = r.try_get(col).ok().flatten();
        if let Some(b) = blob {
            if let Ok(raw) = decompress(&b) {
                let s = String::from_utf8_lossy(&raw);
                if s.to_lowercase().contains(needle) {
                    return true;
                }
            }
        }
    }
    false
}

fn protocol_str(p: Protocol) -> &'static str {
    match p {
        Protocol::Openai => "openai",
        Protocol::OpenaiResponses => "openai_responses",
        Protocol::Anthropic => "anthropic",
        Protocol::Gemini => "gemini",
        Protocol::Custom => "custom",
    }
}

// ================================================================ 脱敏 / 结构

const SENSITIVE_HEADERS: [&str; 7] = [
    "authorization",
    "x-api-key",
    "api-key",
    "cookie",
    "set-cookie",
    "proxy-authorization",
    "x-goog-api-key",
];

const SENSITIVE_BODY_KEYS: [&str; 6] =
    ["authorization", "x-api-key", "api-key", "cookie", "api_key", "secret"];

/// 头脱敏：**保留键名**，值替换为 `***`（07 §2.2）。
pub fn sanitize_headers(headers: &[(String, String)]) -> Vec<(String, String)> {
    headers
        .iter()
        .map(|(k, v)| {
            let lk = k.to_ascii_lowercase();
            if SENSITIVE_HEADERS.contains(&lk.as_str()) {
                (k.clone(), "***".to_string())
            } else {
                (k.clone(), v.clone())
            }
        })
        .collect()
}

/// body 脱敏。四道闸门，**任何一道都能独立拦住 base64 / key**：
///
/// 1. 敏感键（authorization / x-api-key / cookie / secret / …）→ `***`；
/// 2. 图片块（四种协议形状，以及裸的 `data:image/...;base64,` 字符串）→
///    只留 `{type, media_type, bytes, sha256_prefix}`；
/// 3. 单字段 > 32 KB → 只留 `{truncated, bytes, sha256_prefix}`；
/// 4. ≥4 KB 且整串是 base64 字符集 → 视为内联二进制，只留哈希（兜底：形状不认识时也不会漏）；
///    疑似明文 key（`sk-…`）→ `***`。
pub fn sanitize_body(v: &serde_json::Value) -> serde_json::Value {
    match v {
        Value::Object(map) => {
            if let Some(meta) = image_meta(v) {
                return meta;
            }
            let mut out = serde_json::Map::new();
            for (k, val) in map {
                let lk = k.to_ascii_lowercase();
                if SENSITIVE_BODY_KEYS.contains(&lk.as_str()) {
                    out.insert(k.clone(), Value::String("***".into()));
                } else {
                    out.insert(k.clone(), sanitize_body(val));
                }
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(sanitize_body).collect()),
        Value::String(s) => sanitize_string(s),
        other => other.clone(),
    }
}

fn sanitize_string(s: &str) -> serde_json::Value {
    // 闸门 2：data URL（哪怕短到 4KB 以下也不会漏）
    if let Some(meta) = data_url_meta(s) {
        return meta;
    }
    // 闸门 4a：疑似明文 key
    if looks_like_plaintext_key(s) {
        return Value::String("***".into());
    }
    // 闸门 3：单字段体积
    if s.len() > MAX_FIELD_BYTES {
        return truncated_marker(s);
    }
    // 闸门 4b：内联二进制兜底（长且整串落在 base64 字符集）
    if looks_like_base64_payload(s) {
        return serde_json::json!({
            "type": "binary",
            "encoding": "base64?",
            "bytes": s.len(),
            "sha256_prefix": sha256_prefix(s.as_bytes(), 8),
        });
    }
    Value::String(s.to_string())
}

/// 裸字符串里的 `data:<media_type>;base64,<payload>`。
fn data_url_meta(s: &str) -> Option<serde_json::Value> {
    let start = s.find("data:")?;
    let rest = &s[start + 5..];
    let (head, payload) = rest.split_once(";base64,")?;
    if !head.starts_with("image/") {
        return None;
    }
    Some(image_meta_from_payload(Some(payload), Some(head), "image"))
}

/// 明文上游 key 的特征（`sk-` 后跟 ≥16 个 key 字符）。宁可多脱敏一次，也不把 key 写进库。
fn looks_like_plaintext_key(s: &str) -> bool {
    if s.len() > 512 {
        return false;
    }
    let bytes = s.as_bytes();
    let mut i = 0;
    while i + 3 <= bytes.len() {
        if &bytes[i..i + 3] == b"sk-" {
            let run = bytes[i + 3..]
                .iter()
                .take_while(|c| c.is_ascii_alphanumeric() || **c == b'-' || **c == b'_')
                .count();
            if run >= 16 {
                return true;
            }
        }
        i += 1;
    }
    false
}

/// 长且整串落在 base64 字符集（允许换行/空格）→ 当作内联二进制。
fn looks_like_base64_payload(s: &str) -> bool {
    if s.len() < 4096 {
        return false;
    }
    s.bytes().all(|c| {
        c.is_ascii_alphanumeric()
            || c == b'+'
            || c == b'/'
            || c == b'='
            || c == b'\n'
            || c == b'\r'
            || c == b' '
    })
}

fn truncated_marker(s: &str) -> serde_json::Value {
    serde_json::json!({
        "truncated": true,
        "bytes": s.len(),
        "sha256_prefix": sha256_prefix(s.as_bytes(), 8),
    })
}

/// `LogMode::Shape`：只记键名、层级、数组长度、图片块的元数据，**不记任何内容**。
pub fn body_shape_of(v: &serde_json::Value) -> serde_json::Value {
    match v {
        Value::Object(map) => {
            if let Some(meta) = image_meta(v) {
                return meta;
            }
            let mut keys = serde_json::Map::new();
            for (k, val) in map {
                keys.insert(k.clone(), body_shape_of(val));
            }
            serde_json::json!({"type": "object", "keys": keys})
        }
        Value::Array(items) => {
            let mut merged: Option<Value> = None;
            for it in items {
                let s = body_shape_of(it);
                merged = Some(match merged {
                    None => s,
                    Some(prev) => merge_shapes(&prev, &s),
                });
            }
            serde_json::json!({
                "type": "array",
                "len": items.len(),
                "items": merged.unwrap_or(Value::Null),
            })
        }
        Value::String(s) => serde_json::json!({"type": "string", "bytes": s.len()}),
        Value::Number(_) => serde_json::json!({"type": "number"}),
        Value::Bool(_) => serde_json::json!({"type": "bool"}),
        Value::Null => serde_json::json!({"type": "null"}),
    }
}

fn merge_shapes(a: &serde_json::Value, b: &serde_json::Value) -> serde_json::Value {
    if a.get("type").and_then(|t| t.as_str()) == Some("object")
        && b.get("type").and_then(|t| t.as_str()) == Some("object")
    {
        let mut keys = a.get("keys").and_then(|k| k.as_object()).cloned().unwrap_or_default();
        if let Some(bk) = b.get("keys").and_then(|k| k.as_object()) {
            for (k, v) in bk {
                match keys.get(k) {
                    Some(prev) => {
                        let merged = merge_shapes(prev, v);
                        keys.insert(k.clone(), merged);
                    }
                    None => {
                        keys.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        return serde_json::json!({"type": "object", "keys": keys});
    }
    a.clone()
}

/// 识别图片块并抽取**元数据**（绝不返回内容）。
///
/// 覆盖四种常见形状：
/// - OpenAI Chat：`{type:"image_url", image_url:{url:"data:image/png;base64,..."}}`
/// - OpenAI Responses：`{type:"input_image", image_url:"data:..."}`
/// - Anthropic：`{type:"image", source:{type:"base64", media_type, data}}`
/// - Gemini：`{inline_data:{mime_type, data}}` / `{inlineData:{...}}`
fn image_meta(v: &serde_json::Value) -> Option<serde_json::Value> {
    let obj = v.as_object()?;
    let kind = obj.get("type").and_then(|t| t.as_str()).unwrap_or("");
    let is_image_kind = matches!(kind, "image_url" | "image" | "input_image" | "image_base64");
    let has_inline = obj.contains_key("inline_data") || obj.contains_key("inlineData");
    let has_b64_source = obj
        .get("source")
        .and_then(|s| s.as_object())
        .map(|s| s.get("type").and_then(|t| t.as_str()) == Some("base64") || s.contains_key("data"))
        .unwrap_or(false);
    if !(is_image_kind || has_inline || has_b64_source) {
        return None;
    }

    // 找出 base64 载荷与 media_type
    let mut payload: Option<&str> = None;
    let mut media_type: Option<String> = None;
    if let Some(src) = obj.get("source").and_then(|s| s.as_object()) {
        media_type = src.get("media_type").and_then(|m| m.as_str()).map(|s| s.to_string());
        payload = src.get("data").and_then(|d| d.as_str());
    }
    if payload.is_none() {
        let inline =
            obj.get("inline_data").or_else(|| obj.get("inlineData")).and_then(|i| i.as_object());
        if let Some(inline) = inline {
            media_type = inline
                .get("mime_type")
                .or_else(|| inline.get("mimeType"))
                .and_then(|m| m.as_str())
                .map(|s| s.to_string());
            payload = inline.get("data").and_then(|d| d.as_str());
        }
    }
    if payload.is_none() {
        if let Some(url) = obj.get("image_url") {
            let url = url
                .as_str()
                .map(|s| s.to_string())
                .or_else(|| url.get("url").and_then(|u| u.as_str()).map(|s| s.to_string()));
            if let Some(url) = url {
                if let Some(rest) = url.strip_prefix("data:") {
                    let (mt, b64) = rest.split_once(";base64,").unwrap_or(("", rest));
                    media_type = Some(mt.to_string());
                    // 只借用字符串切片，不做任何拷贝到库里的动作
                    let meta = image_meta_from_payload(Some(b64), media_type.as_deref(), kind);
                    return Some(meta);
                }
                // 非 data URL（远端图片）：只记形状，不抓取
                return Some(serde_json::json!({
                    "type": if kind.is_empty() { "image" } else { kind },
                    "kind": kind,
                    "media_type": media_type,
                    "url": "***",
                }));
            }
        }
    }

    Some(image_meta_from_payload(payload, media_type.as_deref(), kind))
}

fn image_meta_from_payload(
    payload: Option<&str>,
    media_type: Option<&str>,
    kind: &str,
) -> serde_json::Value {
    use base64::Engine;
    let bytes = payload.map(|p| p.len()).unwrap_or(0);
    let mut out = serde_json::json!({
        "type": if kind.is_empty() { "image" } else { kind },
        "kind": kind,
        "media_type": media_type,
        "bytes": bytes,
    });
    if let Some(p) = payload {
        // 解码后按**原始字节**算哈希；解不开就按字符串算（同样不落内容）
        let digest = base64::engine::general_purpose::STANDARD
            .decode(p)
            .map(|raw| sha256_prefix(&raw, 8))
            .unwrap_or_else(|_| sha256_prefix(p.as_bytes(), 8));
        out["sha256_prefix"] = serde_json::Value::String(digest);
    }
    out
}

/// 供 UI 展示的 body 结构摘要（`messages[3] · content[2] = [text, image_url{...}]` 这类）。
pub fn shape_summary(shape: &serde_json::Value) -> String {
    fn walk(v: &serde_json::Value, out: &mut String, depth: usize) {
        if depth > 3 {
            return;
        }
        match v.get("type").and_then(|t| t.as_str()) {
            Some("array") => {
                out.push('[');
                out.push_str(&v.get("len").and_then(|l| l.as_u64()).unwrap_or(0).to_string());
                out.push(']');
            }
            Some("object") => {
                let keys = v.get("keys").and_then(|k| k.as_object());
                if let Some(keys) = keys {
                    out.push('{');
                    for (i, (k, val)) in keys.iter().enumerate() {
                        if i > 0 {
                            out.push_str(", ");
                        }
                        out.push_str(k);
                        walk(val, out, depth + 1);
                    }
                    out.push('}');
                }
            }
            Some(t) => {
                out.push('<');
                out.push_str(t);
                out.push('>');
            }
            None => {}
        }
    }
    let mut s = String::new();
    walk(shape, &mut s, 0);
    s
}

/// 库里是否出现明文 key / base64（自检用；正常恒为 0）。
///
/// 注意 body 是编译后压缩的 BLOB，这里直接对原始字节做匹配 —— 匹配不到才说明
/// **连压缩前的明文都没有入库的机会**（压缩后仍命中说明脱敏漏了）。
/// [docs/32] L1 聚合统计的**真库测试**：用真的 SQLite，插入三条不同结果的记录，看四个数对不对。
#[cfg(test)]
mod stats_tests {
    use super::*;
    use ta_core::types::RequestId;

    /// **每个测试一个内存库**：用临时目录 + 进程号命名会让同一进程里的三个测试
    /// 互相看到对方插入的行（这条我第一版就踩了，被 `empty_result_is_none_not_zero` 抓住）。
    async fn store() -> SqliteTraceStore {
        let pool = crate::open_memory().await.expect("开内存库");
        SqliteTraceStore::new(pool)
    }

    /// `outcome` **不是** `TraceRecord` 的字段：它由 `status` / `error` / 尝试次数**推导**后落列
    /// （见 `write` 里那三行）。所以这里按同一条规则造记录 —— 测试与生产用同一个口径。
    fn rec(id: &str, ok: bool, total_ms: u32, ts: i64) -> TraceRecord {
        let mut r = TraceRecord::new(RequestId::new(id), ts, "m");
        r.status = if ok { 200 } else { 502 };
        r.total_ms = total_ms;
        r
    }

    #[tokio::test]
    async fn counts_ok_failed_and_average() {
        let s = store().await;
        s.write(rec("a", true, 100, 1_000)).await.unwrap();
        s.write(rec("b", true, 300, 2_000)).await.unwrap();
        s.write(rec("c", false, 200, 3_000)).await.unwrap();

        let all = s.stats(&LogQuery::default()).await.unwrap();
        assert_eq!((all.total, all.ok, all.failed), (3, 2, 1));
        // (100+300+200)/3 = 200
        assert_eq!(all.avg_total_ms, Some(200));

        // 时间窗过滤也走同一套 WHERE：只算第一条
        let window = LogQuery { from_ms: Some(0), to_ms: Some(1_500), ..Default::default() };
        let one = s.stats(&window).await.unwrap();
        assert_eq!((one.total, one.ok, one.failed, one.avg_total_ms), (1, 1, 0, Some(100)));
    }

    #[tokio::test]
    async fn empty_result_is_none_not_zero() {
        let s = store().await;
        let none = s.stats(&LogQuery::default()).await.unwrap();
        assert_eq!((none.total, none.ok, none.failed), (0, 0, 0));
        // **没有行 → 平均延迟是 `None`**：界面据此显示"—"，不是编一个 0ms
        assert_eq!(none.avg_total_ms, None);
    }

    #[tokio::test]
    async fn search_mode_refuses_instead_of_lying() {
        let s = store().await;
        s.write(rec("a", true, 100, 1_000)).await.unwrap();
        let q = LogQuery { search: Some("a".into()), ..Default::default() };
        // 正文在压缩体里，SQL 数不准 → **如实拒绝**，不给一个偏小的数字
        assert!(s.stats(&q).await.is_err());
    }
}
pub async fn count_plaintext_secret_rows(pool: &SqlitePool, needle: &str) -> Result<i64> {
    let n: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM trace WHERE instr(CAST(body AS TEXT), ?) > 0
            OR instr(CAST(resp_body AS TEXT), ?) > 0
            OR instr(CAST(req_headers AS TEXT), ?) > 0",
    )
    .bind(needle)
    .bind(needle)
    .bind(needle)
    .fetch_one(pool)
    .await
    .store_err()?;
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn headers_are_redacted_but_kept() {
        let h = vec![
            ("Authorization".to_string(), "Bearer sk-abc".to_string()),
            ("x-api-key".to_string(), "sk-xyz".to_string()),
            ("content-type".to_string(), "application/json".to_string()),
        ];
        let s = sanitize_headers(&h);
        assert_eq!(s[0].1, "***");
        assert_eq!(s[1].1, "***");
        assert_eq!(s[2].1, "application/json");
        assert_eq!(s[0].0, "Authorization", "键名要保留（否则看不出来被脱敏了）");
    }

    #[test]
    fn images_become_hash_blocks_only() {
        let body = json!({
            "messages": [{
                "role": "user",
                "content": [
                    {"type": "text", "text": "看图"},
                    {"type": "image_url", "image_url": {"url": "data:image/png;base64,iVBORw0KGgoAAAANSUhEUg=="}}
                ]
            }],
            "authorization": "Bearer sk-secret"
        });
        let s = sanitize_body(&body);
        let dumped = s.to_string();
        assert!(!dumped.contains("iVBORw0KGgo"), "绝不能存 base64");
        assert!(!dumped.contains("sk-secret"), "绝不能存 key");
        let img = &s["messages"][0]["content"][1];
        assert_eq!(img["media_type"], "image/png");
        assert_eq!(img["sha256_prefix"].as_str().unwrap().len(), 8);
        assert!(img["bytes"].as_u64().unwrap() > 0);
    }

    #[test]
    fn shape_records_structure_not_content() {
        let body = json!({
            "model": "gpt-4o",
            "messages": [
                {"role": "user", "content": "第一句"},
                {"role": "assistant", "content": "第二句"}
            ]
        });
        let shape = body_shape_of(&body);
        let dumped = shape.to_string();
        assert!(!dumped.contains("第一句"));
        assert!(!dumped.contains("gpt-4o"));
        assert_eq!(shape["keys"]["messages"]["len"], 2);
        assert_eq!(shape["keys"]["messages"]["type"], "array");
        assert_eq!(shape["keys"]["messages"]["items"]["type"], "object");
    }

    #[test]
    fn big_fields_are_truncated_with_marker() {
        let big = "x".repeat(MAX_FIELD_BYTES + 10);
        let s = sanitize_body(&json!({"content": big}));
        assert_eq!(s["content"]["truncated"], true);
        assert_eq!(s["content"]["bytes"], MAX_FIELD_BYTES + 10);
    }
}
