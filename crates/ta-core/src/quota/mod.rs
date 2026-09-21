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
}
