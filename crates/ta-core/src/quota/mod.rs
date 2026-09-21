//! 额度账本：四个观测源取**最保守值**。
//!
//! 观测源：
//! A. 上游响应头（`x-ratelimit-remaining-*` / `anthropic-ratelimit-*`）
//! B. 本地计量（tokenizer 估算输入 + 响应 usage 精确输出）
//! C. 手工额度（用户填的套餐总量与重置周期）
//! D. 状态码语义（429 insufficient_quota / 402 / 403 quota exceeded）
//!
//! 任一源说"没了"就按没了处理（保守优先，避免撞墙）。

pub mod backend;
pub mod ledger;

pub use backend::{MemoryQuotaBackend, QuotaBackend, QuotaRecord};
pub use ledger::{window_bounds, window_bounds_local, QuotaEffect, QuotaLedgerImpl};

use serde::{Deserialize, Serialize};

use crate::domain::QuotaConfig;
use crate::types::*;

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum QuotaStateKind {
    #[default]
    Unknown,
    Ok,
    Soft,
    Exhausted,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuotaState {
    pub credential_id: CredentialId,
    pub kind: QuotaStateKind,
    pub window_start_ms: i64,
    pub window_end_ms: Option<i64>,
    pub used_tokens: u64,
    pub used_requests: u64,
    pub used_cost: f64,
    pub currency: String,
    /// 上游响应头报告的剩余（可能是 tokens 或 requests，取决于上游）
    pub reported_remaining: Option<f64>,
    pub reported_reset_at_ms: Option<i64>,
    /// 0.0–1.0；None 表示完全未知
    pub used_ratio: Option<f64>,
    pub updated_at_ms: i64,
}

impl Default for QuotaState {
    fn default() -> Self {
        Self {
            credential_id: CredentialId::new(""),
            kind: QuotaStateKind::Unknown,
            window_start_ms: 0,
            window_end_ms: None,
            used_tokens: 0,
            used_requests: 0,
            used_cost: 0.0,
            currency: "USD".into(),
            reported_remaining: None,
            reported_reset_at_ms: None,
            used_ratio: None,
            updated_at_ms: 0,
        }
    }
}

impl QuotaState {
    pub fn is_exhausted(&self) -> bool {
        matches!(self.kind, QuotaStateKind::Exhausted)
    }
    pub fn is_usable(&self) -> bool {
        !self.is_exhausted()
    }
}

/// 一次观测（四个来源之一）。
#[derive(Debug, Clone)]
pub enum QuotaObservation {
    /// A：响应头
    Header { remaining: Option<f64>, reset_at_ms: Option<i64> },
    /// B：本地计量
    Local { tokens: u64, requests: u64, cost: f64 },
    /// C：用户设置（在配置里，这里用于"手动清零/充值"）
    ManualReset,
    /// D：状态码语义
    StatusCode { status: u16, error_code: Option<String>, message: Option<String> },
}

pub trait QuotaLedger: Send + Sync {
    fn state(&self, cred: &CredentialId) -> QuotaState;
    fn observe(&self, cred: &CredentialId, obs: QuotaObservation);
    fn mark_exhausted(&self, cred: &CredentialId, hint_reset_at_ms: Option<i64>);
    fn mark_invalid(&self, cred: &CredentialId);
    /// 窗口滚动；返回本轮回升（复活）的凭据
    fn roll_window(&self, now_ms: i64) -> Vec<CredentialId>;
    /// 剩余比例，供策略 `quota_remaining_lt` 与选择器 `quota_first` 使用
    fn remaining_ratio(&self, cred: &CredentialId) -> Option<f64>;
}

/// 从上游 JSON 错误体里判断"额度用尽"（各家中转写法不一，做宽松匹配）。
pub fn looks_like_quota_exhausted(status: u16, body: &str) -> bool {
    let s = body.to_ascii_lowercase();
    let zh = s.contains("余额") || s.contains("额度") || s.contains("配额") || s.contains("欠费");
    match status {
        402 => true,
        429 => s.contains("insufficient_quota") || s.contains("quota") || zh,
        403 => s.contains("quota") || s.contains("balance") || zh,
        _ => false,
    }
}

/// 从上游 JSON 错误体里判断"凭据无效"。
///
/// 覆盖各家中转的真实措辞：OpenAI `invalid_api_key`、Anthropic `invalid x-api-key`、
/// 部分中转的 403 + `User is not authorized`、one-api/new-api 的 `密钥无效` / `无权`。
///
/// **取舍**：这里刻意**不**把 403 一律判成"key 无效"——403 也可能是地区限制、IP 限制、
/// 模型无权访问，误判会把好 key 标死（比漏判更糟）。所以要同时出现"key/鉴权"语义才算。
pub fn looks_like_invalid_credential(status: u16, body: &str) -> bool {
    let s = body.to_ascii_lowercase();
    match status {
        401 => true,
        403 => {
            s.contains("api key")
                || s.contains("api-key")
                || s.contains("api_key")
                || s.contains("apikey")
                || s.contains("unauthorized")
                || s.contains("not authorized")
                || s.contains("authentication")
                || s.contains("forbidden")
                || s.contains("permission")
                || s.contains("鉴权")
                || s.contains("无权")
                || s.contains("密钥")
        }
        _ => false,
    }
}

// ---------------------------------------------------------------- B2：未配额度的显式提醒

/// 一个可用的额度上限（`Some` 才代表"配了"）。
///
/// "可用"的定义与账本一致（[`ledger`]）：**有限的正数**。
/// `0` / 负数 / `NaN` / `inf` 都算"没配" —— 它们无法参与 `used / limit` 这个比例口径。
pub fn usable_limit(cfg: &QuotaConfig) -> Option<f64> {
    cfg.limit.filter(|l| l.is_finite() && *l > 0.0)
}

/// 套餐缺额度的那两种形态。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LimitGap {
    /// 完全没填 `limit`
    Missing,
    /// 填了，但不是可用的正数（原样带出来给 UI 显示）
    Unusable(f64),
}

impl LimitGap {
    /// 给用户看的说明（[15]：不写 emoji）。**不要把这条改写成"不影响使用"** ——
    /// 静默会让用户以为"用完自动切"坏了（[06 §10.1 B2] 的原话）。
    pub const fn message(&self) -> &'static str {
        match self {
            LimitGap::Missing => {
                "该套餐没有填额度上限（limit）：本地累计用量无法触发耗尽判定，只有上游响应头报告耗尽时才会自动切换；请填写 limit"
            }
            LimitGap::Unusable(_) => {
                "该套餐的额度上限（limit）不是一个有效的正数：无法按比例判定耗尽；请改成大于 0 的数值"
            }
        }
    }
}

/// 【B2】这个套餐的额度配置有没有缺口。`None` = 配了可用的 limit。
///
/// **为什么这是一个"判定函数"而不是 UI 自己判断 `limit.is_none()`**：
/// ① "0 / 负数 / NaN" 也是没配，UI 各自判断一定会漏掉这几种；
/// ② 三处 UI（套餐列表、通道矩阵、首启向导）必须给出**同一句话**（[06 §10.1 B2] 要求"三处一致提示"）。
pub fn limit_gap(cfg: &QuotaConfig) -> Option<LimitGap> {
    match cfg.limit {
        None => Some(LimitGap::Missing),
        Some(l) if l.is_finite() && l > 0.0 => None,
        Some(l) => Some(LimitGap::Unusable(l)),
    }
}

/// "用完自动切"能不能由**本地累计**驱动。
///
/// 缺 limit 时为 `false`：账本在"既没 limit 也没响应头"时按可用处理
/// （[`ledger`] 第 354 行附近），也就是说**没有可量化的耗尽信号**。
/// 上游响应头仍可单独报耗尽，所以这里是"缺一条腿"，不是"绝不可能切"。
pub fn local_exhaustion_is_drivable(cfg: &QuotaConfig) -> bool {
    limit_gap(cfg).is_none()
}

/// 一次扫出所有缺额度的凭据 —— 套餐列表 / 通道矩阵 / 首启向导三处共用同一判定。
pub fn credentials_without_usable_limit<'a>(
    items: impl IntoIterator<Item = (&'a CredentialId, &'a QuotaConfig)>,
) -> Vec<&'a CredentialId> {
    items.into_iter().filter(|(_, cfg)| limit_gap(cfg).is_some()).map(|(c, _)| c).collect()
}

/// 同 [`credentials_without_usable_limit`]，但连**缺口形态**一起给出，供 IPC/UI 直接渲染。
///
/// **不让 UI 自己去判**：一旦前端也写一遍 `limit.is_none()`，`0` / 负数 / `NaN` / `inf`
/// 这几种就一定会漏（那是两个口径）。文案统一取 [`LimitGap::message`] 原文 ——
/// 它是一句完整的话，**不含凭据名**（名字由调用方从自己的数据里带，ID 与名字都要有，[14 §5.2]）。
pub fn credential_limit_gaps<'a>(
    items: impl IntoIterator<Item = (&'a CredentialId, &'a QuotaConfig)>,
) -> Vec<(&'a CredentialId, LimitGap)> {
    items.into_iter().filter_map(|(c, cfg)| limit_gap(cfg).map(|g| (c, g))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ------------------------------------------------ 额度耗尽（D 源）

    /// 各家中转的奇葩文案：只要判定为"额度没了"，账本就会把凭据标 exhausted 并换通道。
    #[test]
    fn quota_exhausted_covers_real_world_wording() {
        // OpenAI 官方
        assert!(looks_like_quota_exhausted(
            429,
            r#"{"error":{"message":"You exceeded your current quota, please check your plan and billing details.","type":"insufficient_quota","code":"insufficient_quota"}}"#
        ));
        assert!(looks_like_quota_exhausted(429, r#"{"error":{"code":"insufficient_quota"}}"#));
        // 部分中转：402 Payment Required
        assert!(looks_like_quota_exhausted(402, "Payment Required"));
        assert!(looks_like_quota_exhausted(402, ""));
        // ★ 中文文案（one-api / new-api 系）
        assert!(looks_like_quota_exhausted(
            429,
            r#"{"error":{"message":"当前分组余额不足，请充值","type":"one_api_error"}}"#
        ));
        assert!(looks_like_quota_exhausted(429, "余额不足"));
        assert!(looks_like_quota_exhausted(429, "额度已用完，请充值"));
        assert!(looks_like_quota_exhausted(429, "配额不足"));
        assert!(looks_like_quota_exhausted(403, "账户欠费，请充值"));
        // 403 + balance / quota
        assert!(looks_like_quota_exhausted(
            403,
            r#"{"error":{"message":"Your account balance is insufficient."}}"#
        ));
        assert!(looks_like_quota_exhausted(
            403,
            r#"{"error":{"code":"insufficient_user_quota","message":"用户额度不足"}}"#
        ));
        assert!(looks_like_quota_exhausted(429, "exceeded your current quota"));
    }

    /// 反例：RPM/TPM 限流**不是**额度耗尽，不能把好凭据标死。
    #[test]
    fn plain_rate_limit_is_not_quota_exhaustion() {
        assert!(!looks_like_quota_exhausted(
            429,
            r#"{"error":{"message":"Rate limit reached for gpt-4o in organization org-x on requests per min (RPM): Limit 500, Used 500.","type":"requests","code":"rate_limit_exceeded"}}"#
        ));
        assert!(!looks_like_quota_exhausted(429, "server is busy"));
        // 状态码不在白名单里 → 一律不判
        assert!(!looks_like_quota_exhausted(400, "quota"));
        assert!(!looks_like_quota_exhausted(500, "internal error"));
        assert!(!looks_like_quota_exhausted(200, "quota"));
    }

    // ------------------------------------------------ 凭据无效

    #[test]
    fn invalid_credential_covers_real_world_wording() {
        assert!(looks_like_invalid_credential(401, ""));
        assert!(looks_like_invalid_credential(
            401,
            r#"{"error":{"message":"Incorrect API key provided: sk-***","code":"invalid_api_key"}}"#
        ));
        // Anthropic 的 401 文案（注意 "x-api-key" 里没有 "api key" 这个子串）
        assert!(looks_like_invalid_credential(
            401,
            r#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}}"#
        ));
        assert!(looks_like_invalid_credential(403, r#"{"error":{"message":"invalid api key"}}"#));
        assert!(looks_like_invalid_credential(403, "invalid x-api-key"));
        assert!(looks_like_invalid_credential(403, r#"{"error":{"code":"invalid_api_key"}}"#));
        assert!(looks_like_invalid_credential(403, "User is not authorized"));
        assert!(looks_like_invalid_credential(403, "Forbidden"));
        assert!(looks_like_invalid_credential(
            403,
            "The API key does not have permission to access this resource"
        ));
        assert!(looks_like_invalid_credential(403, "密钥无效"));
        assert!(looks_like_invalid_credential(403, "鉴权失败，请检查 API Key"));
        assert!(looks_like_invalid_credential(403, "该令牌无权访问此模型"));
        // 403 + 余额 是额度问题，不是 key 问题
        assert!(!looks_like_invalid_credential(403, "余额不足"));
        assert!(!looks_like_invalid_credential(403, "quota exceeded"));
        // 429 永远不是"凭据无效"
        assert!(!looks_like_invalid_credential(429, "api key"));
        assert!(!looks_like_invalid_credential(500, "api key"));
    }

    /// 负例：403 也可能是地区/IP 限制，**绝不能**把有效 key 误标成失效。
    #[test]
    fn forbidden_cases_that_are_not_credential_problems() {
        assert!(!looks_like_invalid_credential(403, "region not allowed"));
        assert!(!looks_like_invalid_credential(
            403,
            r#"{"error":{"message":"Access denied from your region (CN)"}}"#
        ));
        assert!(!looks_like_invalid_credential(403, "Your IP is not in the allowlist"));
        assert!(!looks_like_invalid_credential(403, "Request blocked by WAF"));
        assert!(!looks_like_invalid_credential(403, ""));
    }

    /// 状态对象的基本语义（策略与 UI 都依赖）。
    #[test]
    fn quota_state_helpers() {
        let mut st = QuotaState::default();
        assert_eq!(st.kind, QuotaStateKind::Unknown);
        assert!(st.is_usable());
        st.kind = QuotaStateKind::Exhausted;
        assert!(st.is_exhausted());
        assert!(!st.is_usable());
        st.kind = QuotaStateKind::Soft;
        assert!(st.is_usable(), "soft 只是告警，仍可路由");
    }

    // ------------------------------------------------ B2：未配额度的显式提醒

    fn cfg_with_limit(limit: Option<f64>) -> QuotaConfig {
        QuotaConfig { limit, ..Default::default() }
    }

    #[test]
    fn missing_limit_is_reported_not_silently_ignored() {
        let gap = limit_gap(&cfg_with_limit(None)).expect("不填 limit 必须有缺口");
        assert_eq!(gap, LimitGap::Missing);
        assert!(gap.message().contains("limit"), "{}", gap.message());
        assert!(gap.message().contains("自动切换") || gap.message().contains("耗尽"));
        assert!(!local_exhaustion_is_drivable(&cfg_with_limit(None)));
    }

    #[test]
    fn zero_negative_and_non_finite_limits_count_as_missing() {
        // 这三种 UI 各自判断 `is_none()` 时一定会漏 —— 所以判定必须在这里
        for bad in [0.0, -1.0, -0.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let cfg = cfg_with_limit(Some(bad));
            assert!(limit_gap(&cfg).is_some(), "{bad} 应当算作没有可用额度");
            assert!(!local_exhaustion_is_drivable(&cfg));
            assert_eq!(usable_limit(&cfg), None);
        }
        assert!(matches!(limit_gap(&cfg_with_limit(Some(0.0))), Some(LimitGap::Unusable(0.0))));
    }

    #[test]
    fn a_usable_limit_closes_the_gap() {
        let cfg = cfg_with_limit(Some(1_000_000.0));
        assert_eq!(limit_gap(&cfg), None);
        assert_eq!(usable_limit(&cfg), Some(1_000_000.0));
        assert!(local_exhaustion_is_drivable(&cfg));
    }

    #[test]
    fn the_three_ui_places_share_one_listing_helper() {
        let a = CredentialId::new("c-a");
        let b = CredentialId::new("c-b");
        let c = CredentialId::new("c-c");
        let good = cfg_with_limit(Some(100.0));
        let missing = cfg_with_limit(None);
        let zero = cfg_with_limit(Some(0.0));
        let listed = credentials_without_usable_limit([(&a, &good), (&b, &missing), (&c, &zero)]);
        assert_eq!(listed.len(), 2);
        assert!(listed.contains(&&b) && listed.contains(&&c));
        assert!(!listed.contains(&&a));
    }

    /// IPC/UI 用的那一版：一次拿到 `(凭据, 缺口)`，文案仍出自同一个 `message()`。
    #[test]
    fn ipc_listing_carries_the_reason_variant() {
        let a = CredentialId::new("c-a");
        let b = CredentialId::new("c-b");
        let c = CredentialId::new("c-c");
        let good = cfg_with_limit(Some(100.0));
        let missing = cfg_with_limit(None);
        let zero = cfg_with_limit(Some(0.0));

        let rows = credential_limit_gaps([(&a, &good), (&b, &missing), (&c, &zero)]);
        assert_eq!(rows.len(), 2, "只列有缺口的：{rows:?}");
        assert_eq!(rows[0], (&b, LimitGap::Missing));
        assert_eq!(rows[1], (&c, LimitGap::Unusable(0.0)));
        // 两个入口的成员判定必须一致（同一个 limit_gap，不许分叉）
        let ids: Vec<&CredentialId> = rows.iter().map(|(c, _)| *c).collect();
        assert_eq!(
            ids,
            credentials_without_usable_limit([(&a, &good), (&b, &missing), (&c, &zero)])
        );
        for (_, gap) in rows {
            assert!(!gap.message().is_empty(), "每一行都要能给出一句完整的话");
        }
    }
}
