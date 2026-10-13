-- 0004：`api_key` —— **我们网关对外的访问密钥**。
--
-- 背景：网关此前只认配置里的单个 `gateway.local_key`（`ta-gateway/src/ingress.rs`
-- 的 `check_local_key`）。这一轮起支持"多把 key、可启停、可看用量"，所以需要一个
-- 真源表。`api_key_usage`（0001 就有）是它的用量落点，两者靠 `key_id` 关联。
--
-- ★ **只存 `sha256(明文)`，绝不存明文**：
--   与"key 永不进 SQLite"的既有纪律同族；明文只在 `api_key_create` 的响应里
--   出现**一次**，之后**没有任何**办法取回（丢了只能新建）。
--   `key_hash` 上的 UNIQUE 不是性能优化，是**安全语义**：同一个明文不可能有两个 id。
--
-- ★ **`last_used_at` 可空**：NULL = **从未用过**，不是 0。
--   0 是"1970-01-01 用过"，会让 UI 显示成"上次使用：很久以前"而不是"从未使用"。
--   判据在 `ta-store::api_key` 与命令层都按 NULL 处理。
--
-- ★ `key_prefix` 是**展示用**（明文前 12 字符 + "…"），列表页只显示它：
--   它不足以还原明文（明文 = `ta-sk-` + 43 位 base62），但能让用户认出"是哪一把"。
--
-- 迁移只增不改：0001–0003 已发布，改它们是灾难。
CREATE TABLE api_key (
  id           TEXT PRIMARY KEY,
  name         TEXT NOT NULL,
  key_hash     TEXT NOT NULL UNIQUE,   -- sha256(明文)，只存哈希
  key_prefix   TEXT NOT NULL,          -- 展示用：前 12 字符 + "…"
  enabled      INTEGER NOT NULL DEFAULT 1,
  note         TEXT,
  created_at   INTEGER NOT NULL,
  last_used_at INTEGER
);
