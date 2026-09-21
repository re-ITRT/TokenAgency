-- TokenAgency 初始 schema（P4 / ta-store）
--
-- 契约来源：
--   docs/11-internal-contracts.md §4.2  表清单 + 硬约束
--   docs/02-config-and-data-model.md §3 SQLite 表结构（草案）
--   docs/07-ui-pages-and-cost.md §6.5   用量与成本表设计
--
-- 硬约束（违反即 bug）：
--   1. usage_daily.token_kind 是**行维度**（不是列）→ 新增 token 类型无需改表。
--   2. usage_request.price_snapshot 必须写 → 改价不改历史。
--   3. trace.body* 只存 zstd 压缩 BLOB；图片只存 {type, media_type, bytes, sha256_prefix}。
--   4. key 绝不入库：secret_meta 只有元数据。
--
-- 迁移只增不改（11 §4.1）。带 “P4 增补” 注释的列是文档草案没有、但实现必需的。

-- ============================================================ 配置真源（真源是 SQLite，不是 YAML）
-- 一行一实体的 JSON；`kind='section'` 存单例子树（gateway/request_log/pricing/net/system_integration），
-- `kind='meta'` 存 version 与未知顶层字段（前向兼容，见 domain::ConfigDoc::extra）。
CREATE TABLE config (
  kind       TEXT    NOT NULL,
  entity_id  TEXT    NOT NULL,
  ord        INTEGER NOT NULL DEFAULT 0,   -- 数组顺序（policies 的拖拽顺序等）
  json       TEXT    NOT NULL,
  updated_at INTEGER NOT NULL,
  PRIMARY KEY (kind, entity_id)
);
CREATE INDEX ix_config_kind_ord ON config(kind, ord);

-- 配置快照：每次成功应用留一份带时间戳的完整文档（可回滚，最近 N 份）
CREATE TABLE config_snapshot (
  snapshot_id TEXT PRIMARY KEY,
  label       TEXT    NOT NULL,
  created_at  INTEGER NOT NULL,
  version     INTEGER NOT NULL DEFAULT 1,
  doc_json    TEXT    NOT NULL,
  yaml        TEXT                          -- 便于分享 / git 管理
);
CREATE INDEX ix_config_snapshot_created ON config_snapshot(created_at DESC);

-- 键值设置：日志模式、保留期、清理上限等运行期开关（随库持久化）
CREATE TABLE store_setting (
  key        TEXT PRIMARY KEY,
  value      TEXT    NOT NULL,
  updated_at INTEGER NOT NULL
);

-- ============================================================ 用量明细（永久保留，成本对账依据）
CREATE TABLE usage_request (
  request_id     TEXT PRIMARY KEY,
  ts             INTEGER NOT NULL,
  credential_id  TEXT NOT NULL DEFAULT '',
  provider_id    TEXT NOT NULL DEFAULT '',
  channel_id     TEXT NOT NULL DEFAULT '',
  model          TEXT NOT NULL DEFAULT '',
  upstream_model TEXT NOT NULL DEFAULT '',
  client         TEXT,
  lane_id        TEXT,
  node           TEXT,
  status         INTEGER NOT NULL DEFAULT 0,
  error_type     TEXT,
  latency_ms     INTEGER NOT NULL DEFAULT 0,
  ttfb_ms        INTEGER,
  token_kinds    TEXT NOT NULL DEFAULT '{}',   -- JSON: {"input":1204,"input_cached":51200,...}
  usage_raw      TEXT NOT NULL DEFAULT '{}',   -- JSON: 上游原始 usage，用于对账
  usage_source   TEXT NOT NULL DEFAULT 'upstream', -- P4 增补：11 §6 铁律 3（不猜、不估算）
  total_tokens   INTEGER NOT NULL DEFAULT 0,
  cost           REAL NOT NULL DEFAULT 0,
  currency       TEXT NOT NULL DEFAULT 'USD',
  price_snapshot TEXT NOT NULL DEFAULT '{}'    -- JSON: 当时生效的费率与倍率（改价不改历史）
);
CREATE INDEX ix_usage_ts ON usage_request(ts);
CREATE INDEX ix_usage_cred_ts ON usage_request(credential_id, ts);
-- P4 增补：成本中心按任意维度分组时的走索引路径
CREATE INDEX ix_usage_model_ts ON usage_request(model, ts);
CREATE INDEX ix_usage_channel_ts ON usage_request(channel_id, ts);
CREATE INDEX ix_usage_client_ts ON usage_request(client, ts);

-- 日汇总：token_kind 作为**行维度**，任意维度组合都能 GROUP BY
--
-- `requests` 列语义 = "包含该 token_kind 的请求数"，**跨类型求和会重复计数**；
-- 因此 `requests` 指标一律从 usage_request 统计（COUNT(*)），见 usage_store 的选表规则。
-- `cost` 列 = 该 token_kind 的分摊成本（按 price_snapshot 费率分摊），同一请求所有行求和 = 请求总成本。
CREATE TABLE usage_daily (
  day           TEXT    NOT NULL,           -- 本地时区日期 YYYY-MM-DD
  credential_id TEXT    NOT NULL DEFAULT '',
  provider_id   TEXT    NOT NULL DEFAULT '',
  model         TEXT    NOT NULL DEFAULT '',
  client        TEXT    NOT NULL DEFAULT '',
  token_kind    TEXT    NOT NULL,
  tokens        INTEGER NOT NULL DEFAULT 0,
  requests      INTEGER NOT NULL DEFAULT 0,
  cost          REAL    NOT NULL DEFAULT 0,
  currency      TEXT    NOT NULL DEFAULT 'USD',
  PRIMARY KEY (day, credential_id, provider_id, model, client, token_kind)
);
CREATE INDEX ix_usage_daily_dim ON usage_daily(day, credential_id, provider_id, model, client);

-- 每个本地 key 的用量（成本中心"按 key 分组"用；由网关在鉴权后调用 record_api_key_usage 写入）
CREATE TABLE api_key_usage (
  key_id     TEXT    NOT NULL,
  day        TEXT    NOT NULL,
  token_kind TEXT    NOT NULL,
  tokens     INTEGER NOT NULL DEFAULT 0,
  requests   INTEGER NOT NULL DEFAULT 0,
  cost       REAL    NOT NULL DEFAULT 0,
  currency   TEXT    NOT NULL DEFAULT 'USD',
  PRIMARY KEY (key_id, day, token_kind)
);
CREATE INDEX ix_api_key_usage_day ON api_key_usage(day, key_id);

-- 额度状态：由账本 + 响应头 + 手工额度合成（见 02 §2.2 quota）
CREATE TABLE quota_state (
  credential_id      TEXT PRIMARY KEY,
  window_start       INTEGER NOT NULL DEFAULT 0,
  window_end         INTEGER NOT NULL DEFAULT 0,
  used_tokens        INTEGER NOT NULL DEFAULT 0,
  used_requests      INTEGER NOT NULL DEFAULT 0,
  used_cost          REAL    NOT NULL DEFAULT 0,
  reported_remaining REAL,
  reported_reset_at  INTEGER,
  state              TEXT    NOT NULL DEFAULT 'unknown', -- ok | soft | exhausted | unknown
  updated_at         INTEGER NOT NULL DEFAULT 0
);

-- 会话粘性：key → 通道（一个 key 同时只粘一个组，与 02 §3 草案一致）
CREATE TABLE sticky (
  key        TEXT PRIMARY KEY,
  group_id   TEXT    NOT NULL,
  channel_id TEXT    NOT NULL,
  created_at INTEGER NOT NULL,
  expires_at INTEGER NOT NULL,
  hits       INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX ix_sticky_group ON sticky(group_id);
CREATE INDEX ix_sticky_expires ON sticky(expires_at);

-- 请求日志：每个请求一行（含失败尝试链）
--
-- body* 列只存 zstd 压缩 BLOB；请求体列名沿用文档的 `body`。
-- 图片块绝不存 base64，只存 {type, media_type, bytes, sha256_prefix}（07 §2.2）。
CREATE TABLE trace (
  request_id     TEXT PRIMARY KEY,
  ts             INTEGER NOT NULL,
  client         TEXT,
  model          TEXT NOT NULL DEFAULT '',
  protocol_in    TEXT NOT NULL DEFAULT '',
  protocol_out   TEXT NOT NULL DEFAULT '',
  stream         INTEGER NOT NULL DEFAULT 0,
  policy_id      TEXT,
  group_id       TEXT,
  chosen_channel TEXT,
  attempts       TEXT NOT NULL DEFAULT '[]',   -- JSON: [{channel,lane,node,status,ms,reason}]
  header_diff    TEXT,                         -- JSON: 客户端头 vs 上游头差异（正常恒为空）
  tokens         INTEGER NOT NULL DEFAULT 0,
  status         INTEGER NOT NULL DEFAULT 0,
  total_ms       INTEGER NOT NULL DEFAULT 0,
  ttfb_ms        INTEGER,
  error          TEXT,
  -- P4 增补（body 体积控制）
  mode           TEXT    NOT NULL DEFAULT 'meta',
  body           BLOB,                         -- 请求体（zstd 压缩）
  resp_body      BLOB,                         -- 响应体（zstd 压缩）
  req_headers    BLOB,                         -- 请求头 JSON（已脱敏，zstd 压缩）
  resp_headers   BLOB,                         -- 响应头 JSON（已脱敏，zstd 压缩）
  body_shape     TEXT,                         -- shape 模式：字段结构（不含内容）
  body_meta      TEXT,                         -- JSON: {images:[...], truncated:[...]} 等
  body_bytes     INTEGER NOT NULL DEFAULT 0    -- 本行 body 列压缩后总字节（供 max_size 清理）
);
CREATE INDEX ix_trace_ts ON trace(ts);
CREATE INDEX ix_trace_model_ts ON trace(model, ts);
CREATE INDEX ix_trace_channel_ts ON trace(chosen_channel, ts);
CREATE INDEX ix_trace_client_ts ON trace(client, ts);

-- 线路健康样本（保留 24h）
CREATE TABLE health_sample (
  id         INTEGER PRIMARY KEY AUTOINCREMENT,
  ts         INTEGER NOT NULL,
  lane_id    TEXT    NOT NULL,
  node       TEXT,
  latency_ms INTEGER,
  ok         INTEGER NOT NULL DEFAULT 0,
  status     INTEGER
);
CREATE INDEX ix_health_lane_ts ON health_sample(lane_id, ts);
CREATE INDEX ix_health_ts ON health_sample(ts);

-- key 的元数据（**值在系统钥匙串，绝不入库**）
CREATE TABLE secret_meta (
  name       TEXT PRIMARY KEY,
  created_at INTEGER NOT NULL,
  last_used  INTEGER,
  hint       TEXT NOT NULL DEFAULT ''
);

-- ============================================================ 订阅（表由 P6 使用；P4 只建表）
CREATE TABLE subscription (
  id             TEXT PRIMARY KEY,
  name           TEXT    NOT NULL DEFAULT '',
  url            TEXT    NOT NULL DEFAULT '',
  user_agent     TEXT    NOT NULL DEFAULT '',
  format         TEXT    NOT NULL DEFAULT 'auto',
  interval_s     INTEGER NOT NULL DEFAULT 21600,
  keep_versions  INTEGER NOT NULL DEFAULT 3,
  enabled        INTEGER NOT NULL DEFAULT 1,
  provider_key   TEXT,
  node_count     INTEGER NOT NULL DEFAULT 0,
  group_count    INTEGER NOT NULL DEFAULT 0,
  unrecognized   TEXT,                          -- JSON array: 未识别协议
  last_parsed_at INTEGER,
  parser_version TEXT,
  userinfo       TEXT,                          -- JSON: upload/download/total/expire
  rules_raw      TEXT,                          -- 只读展示，不执行
  updated_at     INTEGER NOT NULL DEFAULT 0
);

-- 订阅版本（坏更新可回滚；raw 为压缩后的原文）
CREATE TABLE subscription_version (
  subscription_id TEXT    NOT NULL,
  version_index   INTEGER NOT NULL,
  fetched_at      INTEGER NOT NULL,
  format          TEXT    NOT NULL DEFAULT 'unknown',
  raw             BLOB,
  raw_bytes       INTEGER NOT NULL DEFAULT 0,
  node_count      INTEGER NOT NULL DEFAULT 0,
  group_count     INTEGER NOT NULL DEFAULT 0,
  userinfo        TEXT,
  note            TEXT,
  PRIMARY KEY (subscription_id, version_index)
);
CREATE INDEX ix_subscription_version_fetched ON subscription_version(subscription_id, fetched_at DESC);
