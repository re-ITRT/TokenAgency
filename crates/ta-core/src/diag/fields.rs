//! ★ 字段命名约定—— **全项目唯一落点**。
//!
//! 为什么不靠自觉：字段名漂移是**静默**发生的（没人会因为它报错），
//! 而它一旦漂移，"按 `request_id` 筛出一次请求的全部轨迹"这个能力就**部分失效**，
//! 且失效时不会有任何提示。所以这里给出常量 + 白名单，
//! 由 `tests/log_field_guard.rs` 机械检查（扫源码，发现白名单外的字段名即红）。
//!
//! **新增字段必须先改 这份文档，再改这里** —— 顺序反过来就会出现"代码有、契约没有"。

/// 一次客户端请求的全局 ID（`req_` 前缀）。**有它 = 属于同一次请求。**
pub const REQUEST_ID: &str = "request_id";
/// 后台任务名（取值见 [`JOBS`]）。后台任务没有 `request_id`，用 `job` 标识。
pub const JOB: &str = "job";

/// 上游服务商。
pub const PROVIDER_ID: &str = "provider_id";
/// 套餐/凭据（"字节的订阅"这种）。
pub const CREDENTIAL_ID: &str = "credential_id";
/// 通道。
pub const CHANNEL_ID: &str = "channel_id";
/// 通道名（给人读；ID 不会变、名字会变，所以两个都要有）。
pub const CHANNEL_NAME: &str = "channel_name";
/// 逻辑模型（客户端请求的）。
pub const MODEL: &str = "model";
/// 实际发给上游的模型名。
pub const UPSTREAM_MODEL: &str = "upstream_model";
/// 线路。
pub const LANE_ID: &str = "lane_id";
/// 内核节点名。
pub const NODE: &str = "node";
/// 机场订阅。
pub const SUB_ID: &str = "sub_id";

/// `TA-xxxx` 错误码。**错误日志的硬要求**：没有它就无法按码检索。
pub const CODE: &str = "code";
/// 该错误是否可重试。
pub const RETRYABLE: &str = "retryable";
/// HTTP 状态码或内核状态。
pub const STATUS: &str = "status";
/// 第几次尝试，**从 1 开始**。
pub const ATTEMPT: &str = "attempt";
/// 耗时（毫秒）。
pub const ELAPSED_MS: &str = "elapsed_ms";
/// 首字节耗时（毫秒）。
pub const TTFB_MS: &str = "ttfb_ms";
/// 字节数。
pub const BYTES: &str = "bytes";
/// 字段是否被截断。
pub const TRUNCATED: &str = "truncated";

/// §5.1–§5.4 全部约定字段名。**这就是守卫测试的白名单。**
pub const CANONICAL: &[&str] = &[
    REQUEST_ID,
    JOB,
    PROVIDER_ID,
    CREDENTIAL_ID,
    CHANNEL_ID,
    CHANNEL_NAME,
    MODEL,
    UPSTREAM_MODEL,
    LANE_ID,
    NODE,
    SUB_ID,
    CODE,
    RETRYABLE,
    STATUS,
    ATTEMPT,
    ELAPSED_MS,
    TTFB_MS,
    BYTES,
    TRUNCATED,
];

/// §5.3：后台任务必须自报家门，取值只有这几个。
pub const JOBS: &[&str] = &[
    "kernel_supervisor",
    "subscription_sync",
    "quota_flush",
    "log_rotation",
    "usage_cleanup",
    "tray",
];

/// **本地上下文字段**：不是关联键，只为让人读懂那一行。
///
/// 与 [`CANONICAL`] 的区别是**强制程度**：
/// - `CANONICAL` 表达对应语义时**必须**用（尤其禁止简写：`lane` ✗ → `lane_id` ✓）；
/// - 这些**允许**，但必须登记在这里 —— 登记让新增字段成为**一次有意识的决定**，
///   而不是随手敲一个名字。名字一旦散出去就再也收不回来。
///
/// **新增请同时改 的表格与本常量。**
pub const LOCAL_CONTEXT: &[&str] = &[
    "pid",
    "path",
    "port",
    "reason",
    "source",
    "entry",
    "kind",
    "mode",
    "spec",
    "pattern",
    "program",
    "version",
    "name",
    "names",
    "group",
    "key",
    "value",
    "window",
    "range",
    "delay_s",
    "delay_ms",
    "mechanism",
    "autostart",
    "config",
    "db",
    "tray",
    "menu_id",
    "op",
    "actual",
    "expected",
    "current",
    "removed",
    "failures",
    "attempts",
    "argc",
    "quitting",
    "which",
    "phase",
    "controller",
    "lane_count",
    // ---- 第 15 轮登记：命令层写命令（entity/net/settings_ext）的计数与前后值 ----
    //
    // 这些字段是**那一行日志自己的上下文**（这次改了/删了几条、写之前与写之后各是什么），
    // 不是关联键，所以按 §5.6 登记而不是塞进 §5.1–§5.4。登记本身就是这道手续的意义：
    // 新增字段是一次**有意识的决定**，不是随手敲一个名字。
    // 注意：与契约同义的简写**没有**在这里登记，而是改用了 §5.2/§5.4 的名字
    // （`lane` → `lane_id`、`policy` → `policy_id`、`state` → `status`）。
    "hint",
    "group_id",
    "released",
    "added",
    "total",
    "what",
    "provider_key",
    "accepted",
    "node_count",
    "group_count",
    "recorded_version",
    "effective_version",
    "from_version",
    "count",
    "sections",
    "policy_id",
    "enabled",
    "before_actual",
    "before_drifted",
    "after_actual",
    "kernel_found",
    "existing_config",
    "suggested",
    "providers",
    "subscriptions",
    "secrets",
    "credentials",
    "lanes",
    "rows",
    "migrated",
    "skipped",
    "traffic_known",
];

/// 是不是 §5 约定的字段名。
pub fn is_canonical(name: &str) -> bool {
    CANONICAL.contains(&name)
}

/// 是不是 §5.6 登记的本地上下文字段。
pub fn is_local_context(name: &str) -> bool {
    LOCAL_CONTEXT.contains(&name)
}

/// 守卫用的总判据：**能被 §5 接纳**的字段名。
///
/// 注意 `is_canonical` 与 `is_local_context` **分开导出**（而不是只给这一个合并函数）：
/// 迁移时需要一个"只看简写漂移"的视角，合并后就分不出"这是契约字段"还是"本地字段"了。
pub fn is_allowed_field(name: &str) -> bool {
    is_canonical(name) || is_local_context(name)
}

/// 是不是 §5.3 约定的后台任务名。
pub fn is_known_job(name: &str) -> bool {
    JOBS.contains(&name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use std::collections::BTreeSet;

    #[test]
    fn canonical_list_has_no_duplicates() {
        let uniq: BTreeSet<&str> = CANONICAL.iter().copied().collect();
        assert_eq!(uniq.len(), CANONICAL.len(), "字段名表里出现了重复项");
    }

    #[test]
    fn canonical_names_are_snake_case() {
        for n in CANONICAL {
            assert!(
                n.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'),
                "字段名 `{n}` 不是 snake_case —— 约定要求全小写 + 下划线"
            );
            assert!(!n.starts_with('_') && !n.ends_with('_'), "字段名 `{n}` 首尾不该有下划线");
        }
    }

    #[test]
    fn id_fields_are_paired_with_a_readable_name() {
        // §5.2 的理由：只有 ID 的日志排查时还要回库查名字
        assert!(CANONICAL.contains(&CHANNEL_ID) && CANONICAL.contains(&CHANNEL_NAME));
    }

    #[test]
    fn job_values_match_the_contract() {
        assert_eq!(JOBS.len(), 6);
        assert!(is_known_job("kernel_supervisor"));
        assert!(!is_known_job("kernel")); // 自创同义词会破坏检索
        assert!(is_canonical("request_id"));
        assert!(!is_canonical("rid"));
    }
}
