//! trace 存储。**工作包 P4 负责实现**。
//!
//! 硬约束：`body` 只存压缩 Blob；图片只存 `{type, media_type, bytes, sha256_prefix}`；
//! **绝不记录** `authorization` / `x-api-key` / `cookie`。

// TODO(P4): TraceStore impl —— trace 表 + 清理 gc()
