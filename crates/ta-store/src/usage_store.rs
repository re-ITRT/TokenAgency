//! 用量与账本存储。**工作包 P4 负责实现**。
//!
//! 硬约束：
//! - `usage_daily.token_kind` 是**行维度**（不是列）→ 新增 token 类型无需改表
//! - `usage_request.price_snapshot` 必写 → 改价不改历史
//! - 一次请求的写入必须在**单事务**内完成

// TODO(P4): UsageStore impl —— usage_request / usage_daily / api_key_usage / quota_state
