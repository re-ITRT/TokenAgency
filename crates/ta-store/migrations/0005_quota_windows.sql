-- 0005：多窗口额度 —— `quota_window_state` + `quota_window_bucket`（docs/24）。
--
-- 背景：一份套餐从此可以配**多个**额度窗口（「5 小时 + 周」= 2 个圈，「天 + 周 + 月」= 3 个圈），
-- 每个窗口各有一份累计口径与上限。0001 的 `quota_state` 主键是 `credential_id`（一凭据一行），
-- 装不下"一凭据多窗口"，所以另开两张表 —— **`quota_state` 一个字节都不改**：
-- 它继续写**门槛窗口**（各窗口里剩余最少的那个）的值，于是托盘/看板/所有旧读取方行为不变
-- （兼容口径见 `ta_core::domain::Credential::effective_quota_windows`）。
--
-- 两类窗口的差别决定了下面两张表为什么长这样：
--   * 日历窗口（hour/day/week/month）：有固定的窗口起止，只要一行状态；
--   * `rolling_hours` 滚动窗口：窗口是 `[now - N 小时, now]`，**随请求滑动**，
--     它的"已用"必须是窗口内所有分钟桶的和 —— 所以要有 `quota_window_bucket`。
--
-- ★ **桶必须落库**（docs/24 §4 第 3 条）：不持久化的话重启后 5 小时窗口会从零开始，
--   那是**撒谎**，比不显示更糟。粒度见 `ta_core::quota::BUCKET_MS`（1 分钟）。
--
-- ★ `window_end` / `next_expiry` 都**可空**，且语义不同，不能用 0 冒充：
--   `window_end` NULL = 滚动窗口（**没有**固定刷新时刻，不是"1970 刷新"）；
--   `next_expiry` NULL = 窗口内还没有任何用量（没有"最早一笔"可报）。
--
-- ★ `invalid` 与状态机其它列同族：401/403 的"key 失效"**窗口滚动不解除**（换周期不换 key），
--   所以它跟着窗口行走，不是跟着凭据走。
--
-- ★ **不额外建桶索引**：`PRIMARY KEY (credential_id, window_key, bucket_start)` 在 SQLite 的
--   rowid 表上会自带一个唯一索引，列序正好是"一个窗口的桶按起点排序"的最左前缀，
--   `WHERE credential_id=? AND window_key=? ORDER BY bucket_start` 与剪枝都直接走它。
--   再加一个同前缀的索引只会让每次写入多维护一棵 B 树，没有查询受益。
--
-- 迁移只增不改（docs/11-internal-contracts.md §4.1）：0001–0004 已发布，改它们是灾难。
CREATE TABLE quota_window_state (
  credential_id      TEXT    NOT NULL,
  window_key         TEXT    NOT NULL,
  window_start       INTEGER NOT NULL DEFAULT 0,
  window_end         INTEGER,                      -- NULL = 滚动窗口（没有固定刷新时刻）
  next_expiry        INTEGER,                      -- 滚动窗口：最早一笔过期时刻；NULL = 窗口内还没有用量
  used_tokens        INTEGER NOT NULL DEFAULT 0,
  used_requests      INTEGER NOT NULL DEFAULT 0,
  used_cost          REAL    NOT NULL DEFAULT 0,
  currency           TEXT    NOT NULL DEFAULT 'USD',
  reported_remaining REAL,
  reported_reset_at  INTEGER,
  used_ratio         REAL,
  state              TEXT    NOT NULL DEFAULT 'unknown', -- ok | soft | exhausted | unknown
  hard_exhausted     INTEGER NOT NULL DEFAULT 0,
  invalid            INTEGER NOT NULL DEFAULT 0,   -- 401/403：窗口滚动**不**解除
  updated_at         INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (credential_id, window_key)
);

-- 滚动窗口的分钟桶：`(凭据, 窗口, 桶起点)` 唯一，重复写入**累加**而不是覆盖。
CREATE TABLE quota_window_bucket (
  credential_id TEXT    NOT NULL,
  window_key    TEXT    NOT NULL,
  bucket_start  INTEGER NOT NULL,                  -- Unix 毫秒，按 BUCKET_MS 向下取整
  tokens        INTEGER NOT NULL DEFAULT 0,
  requests      INTEGER NOT NULL DEFAULT 0,
  cost          REAL    NOT NULL DEFAULT 0,
  PRIMARY KEY (credential_id, window_key, bucket_start)
);
