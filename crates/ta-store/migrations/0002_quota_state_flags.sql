-- 0002：补齐 `quota_state` 与 `QuotaRecord` 的字段缺口（P3 ↔ P4 跨包接口）
--
-- 背景：`ta_core::quota::backend::QuotaRecord` 在冻结的 `QuotaState` 之外还带两个
-- **状态机内部标记**：
--   - `hard_exhausted`：D 源/手动标记"额度没了"，窗口滚动 / 手动清零 / 响应头回升可解除；
--   - `invalid`：401/403 "key 失效"语义，**窗口滚动不解除**（换周期不换 key）。
-- 没有这两列，失效的 key 会在每次窗口滚动后被反复启用，白白烧掉尝试次数。
-- 同时补 `currency` / `used_ratio`，让 QuotaRecord ↔ 行 的往返**无损**。
--
-- 迁移只增不改（docs/11-internal-contracts.md §4.1）：0001 已发布，对已有库改它是灾难。
ALTER TABLE quota_state ADD COLUMN hard_exhausted INTEGER NOT NULL DEFAULT 0;
ALTER TABLE quota_state ADD COLUMN invalid INTEGER NOT NULL DEFAULT 0;
ALTER TABLE quota_state ADD COLUMN currency TEXT NOT NULL DEFAULT 'USD';
ALTER TABLE quota_state ADD COLUMN used_ratio REAL;
