//! 额度账本：四个观测源取**最保守值**。
//!
//! 观测源：
//! A. 上游响应头（`x-ratelimit-remaining-*` / `anthropic-ratelimit-*`）
//! B. 本地计量（tokenizer 估算输入 + 响应 usage 精确输出）
//! C. 手工额度（用户填的套餐总量与重置周期）
//! D. 状态码语义（429 insufficient_quota / 402 / 403 quota exceeded）
//!
//! 任一源说"没了"就按没了处理（保守优先，避免撞墙）。

use serde::{Deserialize, Serialize};

use crate::types::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema)]
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
    Header {
        remaining: Option<f64>,
        reset_at_ms: Option<i64>,
    },
    /// B：本地计量
    Local {
        tokens: u64,
        requests: u64,
        cost: f64,
    },
    /// C：用户设置（在配置里，这里用于"手动清零/充值"）
    ManualReset,
    /// D：状态码语义
    StatusCode {
        status: u16,
        error_code: Option<String>,
        message: Option<String>,
    },
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
    match status {
        402 => true,
        429 => {
            s.contains("insufficient_quota")
                || s.contains("quota")
                || s.contains("exceeded your current quota")
                || s.contains("余额")
                || s.contains("额度")
        }
        403 => s.contains("quota") || s.contains("balance") || s.contains("余额"),
        _ => false,
    }
}

/// 从上游 JSON 错误体里判断"凭据无效"。
pub fn looks_like_invalid_credential(status: u16, body: &str) -> bool {
    let s = body.to_ascii_lowercase();
    match status {
        401 => true,
        403 => {
            s.contains("invalid api key")
                || s.contains("unauthorized")
                || s.contains("api key")
                || s.contains("密钥")
        }
        _ => false,
    }
}
