//! 定价与成本计算。
//!
//! `cost = Σ_k tokens[k] × rate[k] × multiplier / divisor`
//!
//! **价格快照是硬要求**：每次请求把当时生效的费率整份存进记录，
//! 之后改价不得追溯改写历史成本。

use std::collections::BTreeMap;

use crate::types::*;

/// 匹配到的一组费率（已解析，可直接参与计算）。
#[derive(Debug, Clone)]
pub struct ResolvedPrice {
    /// 每 `unit` 个 token 的价格
    pub rates: BTreeMap<TokenKind, f64>,
    pub multiplier: f64,
    pub currency: String,
    /// 除数：per_1m → 1_000_000
    pub divisor: f64,
    pub source: PriceSourceKind,
    pub plan_fee: Option<PlanFeeResolved>,
    /// 命中的条目下标（-1 = 未命中，即"未定价"）
    pub entry_index: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PriceSourceKind {
    Manual,
    Imported,
    Builtin,
    /// 未定价 —— 成本记 0，UI 高亮提醒补价
    None,
}

#[derive(Debug, Clone)]
pub struct PlanFeeResolved {
    pub amount: f64,
    pub currency: String,
}

impl ResolvedPrice {
    pub fn unpriced(currency: &str, divisor: f64) -> Self {
        Self {
            rates: BTreeMap::new(),
            multiplier: 1.0,
            currency: currency.to_string(),
            divisor,
            source: PriceSourceKind::None,
            plan_fee: None,
            entry_index: -1,
        }
    }

    pub fn is_priced(&self) -> bool {
        self.source != PriceSourceKind::None
    }

    /// 单次请求成本
    pub fn cost_of(&self, usage: &TokenUsage) -> Money {
        if !self.is_priced() || self.divisor <= 0.0 {
            return Money::zero(self.currency.clone());
        }
        let mut total = 0.0f64;
        for (kind, n) in usage.0.iter() {
            if let Some(rate) = self.rates.get(kind) {
                total += (*n as f64) * rate;
            }
        }
        Money::new(total * self.multiplier / self.divisor, self.currency.clone())
    }

    /// 序列化成快照（写进 `usage_request.price_snapshot`）
    pub fn to_snapshot(&self) -> serde_json::Value {
        serde_json::json!({
            "rates": self.rates.iter().map(|(k, v)| (k.as_key().to_string(), *v))
                       .collect::<BTreeMap<_, _>>(),
            "multiplier": self.multiplier,
            "currency": self.currency,
            "divisor": self.divisor,
            "source": format!("{:?}", self.source),
            "entry_index": self.entry_index,
        })
    }
}

pub trait PricingEngine: Send + Sync {
    /// 按 **manual > imported > builtin** 的优先级解析；精确匹配优先于通配
    fn resolve(&self, model: &str, provider: &ProviderId, cred: &CredentialId) -> ResolvedPrice;
    fn cost(&self, usage: &TokenUsage, price: &ResolvedPrice) -> Money {
        price.cost_of(usage)
    }
}

/// 缓存收益：`input_cached × (全价 − 缓存价) + cache_write × (全价 − 写入价)`
/// 正数=省了，**负数=缓存写多了反而亏**。
pub fn cache_savings(usage: &TokenUsage, price: &ResolvedPrice) -> Money {
    if !price.is_priced() || price.divisor <= 0.0 {
        return Money::zero(&price.currency);
    }
    let base = price.rates.get(&TokenKind::Input).copied().unwrap_or(0.0);
    let cached = price.rates.get(&TokenKind::InputCached).copied().unwrap_or(base);
    let write = price.rates.get(&TokenKind::InputCacheWrite).copied().unwrap_or(base);
    let c = usage.get(&TokenKind::InputCached) as f64;
    let w = usage.get(&TokenKind::InputCacheWrite) as f64;
    let saved = c * (base - cached) + w * (base - write);
    Money::new(saved * price.multiplier / price.divisor, price.currency.clone())
}

/// 简单的通配匹配：支持 `*`，大小写敏感。
pub fn glob_match(pattern: &str, s: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if !pattern.contains('*') {
        return pattern == s;
    }
    let parts: Vec<&str> = pattern.split('*').collect();
    let mut pos = 0usize;
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        if i == 0 {
            if !s[pos..].starts_with(part) {
                return false;
            }
            pos += part.len();
        } else if i == parts.len() - 1 {
            return s[pos..].ends_with(part);
        } else if let Some(idx) = s[pos..].find(part) {
            pos += idx + part.len();
        } else {
            return false;
        }
    }
    true
}
