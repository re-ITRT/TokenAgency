//! 内置官方价目表（**per 1M tokens / USD**）。
//!
//! # ⚠️ 数据是估算，**需人工核对**
//!
//! 这些数字只是"没有别的价格来源时别让成本显示成 0"的兜底：
//!
//! - 官方会调价（本表来自公开标价的记忆值，**交付时无法联网核对**）；
//! - 同一模型常有**上下文分档**（例如 Gemini 2.5 Pro 超过 200K 输入更贵）、
//!   **Batch 折扣**、**缓存 TTL 分档**（Anthropic 1h 写入比 5m 贵）；
//! - 中转站的真实价格由倍率决定，应以用户填的 `manual` 条目为准。
//!
//! 优先级顺序（见 [`crate::pricing::engine`]）：**manual > imported > builtin**，
//! 所以本表只在用户没配价时生效。成本中心对"用内置价"的模型应当有可见提示。
//!
//! 核对方式：打开各家官方 pricing 页面逐项比对，并顺手更新本文件的注释日期。

use std::collections::BTreeMap;

use crate::types::TokenKind;

/// 一条内置价。
pub struct BuiltinPrice {
    /// 模型名通配模式（`*`）。**具体模式必须排在前面**（同 tier 内首个命中生效）
    pub pattern: &'static str,
    /// `(TokenKind key, 每 1M token 价格 USD)`
    pub rates: &'static [(&'static str, f64)],
    /// 口径备注（分档 / 特殊情况），直接给人看
    pub note: &'static str,
}

/// 内置价目表。顺序敏感：更具体的模式放前面。
///
/// 最后人工核对：**未核对（P3 交付时无外网）**。
pub static BUILTIN_PRICES: &[BuiltinPrice] = &[
    // ---------------------------------------------------------- OpenAI
    BuiltinPrice {
        pattern: "gpt-4o-mini*",
        rates: &[("input", 0.15), ("input_cached", 0.075), ("output", 0.60)],
        note: "官方标准档；缓存输入 -50%",
    },
    BuiltinPrice {
        pattern: "gpt-4o*",
        rates: &[("input", 2.50), ("input_cached", 1.25), ("output", 10.00)],
        note: "官方标准档；缓存输入 -50%。gpt-4o-audio 的音频档更贵，未单列",
    },
    BuiltinPrice {
        pattern: "gpt-4.1-nano*",
        rates: &[("input", 0.10), ("input_cached", 0.025), ("output", 0.40)],
        note: "官方标准档",
    },
    BuiltinPrice {
        pattern: "gpt-4.1-mini*",
        rates: &[("input", 0.40), ("input_cached", 0.10), ("output", 1.60)],
        note: "官方标准档",
    },
    BuiltinPrice {
        pattern: "gpt-4.1*",
        rates: &[("input", 2.00), ("input_cached", 0.50), ("output", 8.00)],
        note: "官方标准档",
    },
    BuiltinPrice {
        pattern: "o4-mini*",
        rates: &[("input", 1.10), ("input_cached", 0.275), ("output", 4.40)],
        note: "推理模型；reasoning token 按 output 计价",
    },
    BuiltinPrice {
        pattern: "o3*",
        rates: &[("input", 2.00), ("input_cached", 0.50), ("output", 8.00)],
        note: "推理模型（已调价）；reasoning token 按 output 计价",
    },
    // ---------------------------------------------------------- Anthropic
    BuiltinPrice {
        pattern: "claude-opus-4-1*",
        rates: &[
            ("input", 15.00),
            ("input_cached", 1.50),
            ("input_cache_write", 18.75),
            ("input_cache_write_1h", 30.00),
            ("output", 75.00),
        ],
        note: "缓存读 -90%；5m 写入 1.25×，1h 写入 2×",
    },
    BuiltinPrice {
        pattern: "claude-opus-4*",
        rates: &[
            ("input", 15.00),
            ("input_cached", 1.50),
            ("input_cache_write", 18.75),
            ("input_cache_write_1h", 30.00),
            ("output", 75.00),
        ],
        note: "缓存读 -90%；5m 写入 1.25×，1h 写入 2×",
    },
    BuiltinPrice {
        pattern: "claude-sonnet-4-5*",
        rates: &[
            ("input", 3.00),
            ("input_cached", 0.30),
            ("input_cache_write", 3.75),
            ("input_cache_write_1h", 6.00),
            ("output", 15.00),
        ],
        note: "缓存读 -90%；5m 写入 1.25×，1h 写入 2×",
    },
    BuiltinPrice {
        pattern: "claude-sonnet-4*",
        rates: &[
            ("input", 3.00),
            ("input_cached", 0.30),
            ("input_cache_write", 3.75),
            ("input_cache_write_1h", 6.00),
            ("output", 15.00),
        ],
        note: "缓存读 -90%；5m 写入 1.25×，1h 写入 2×",
    },
    BuiltinPrice {
        pattern: "claude-haiku-4-5*",
        rates: &[
            ("input", 1.00),
            ("input_cached", 0.10),
            ("input_cache_write", 1.25),
            ("input_cache_write_1h", 2.00),
            ("output", 5.00),
        ],
        note: "缓存读 -90%",
    },
    BuiltinPrice {
        pattern: "claude-3-5-haiku*",
        rates: &[
            ("input", 0.80),
            ("input_cached", 0.08),
            ("input_cache_write", 1.00),
            ("input_cache_write_1h", 1.60),
            ("output", 4.00),
        ],
        note: "缓存读 -90%",
    },
    // ----------------------------------------------------------- Gemini
    BuiltinPrice {
        pattern: "gemini-2.5-pro*",
        rates: &[("input", 1.25), ("input_cached", 0.3125), ("output", 10.00)],
        note: "≤200K 输入档；>200K 档约为 2×（input 2.50 / output 15.00），本表未分档",
    },
    BuiltinPrice {
        pattern: "gemini-2.5-flash*",
        rates: &[("input", 0.30), ("input_cached", 0.075), ("output", 2.50)],
        note: "含 thinking 的 output 计价；思考档另计，本表按标准档",
    },
    BuiltinPrice {
        pattern: "gemini-2.0-flash*",
        rates: &[("input", 0.10), ("input_cached", 0.025), ("output", 0.40)],
        note: "官方标准档",
    },
];

/// 查内置价：**先精确匹配，再按数组顺序通配**（与引擎的匹配语义一致）。
///
/// 顺带容忍 `openai/gpt-4o`、`anthropic/claude-sonnet-4-5` 这类带前缀的写法
/// （取最后一个 `/` 之后的部分再试一次）。
pub fn builtin_lookup(model: &str) -> Option<&'static BuiltinPrice> {
    lookup_one(model).or_else(|| model.rsplit_once('/').and_then(|(_, tail)| lookup_one(tail)))
}

fn lookup_one(model: &str) -> Option<&'static BuiltinPrice> {
    if model.is_empty() {
        return None;
    }
    if let Some(e) = BUILTIN_PRICES.iter().find(|e| e.pattern == model) {
        return Some(e);
    }
    BUILTIN_PRICES
        .iter()
        .find(|e| e.pattern.contains('*') && crate::pricing::glob_match(e.pattern, model))
}

/// 把内置价转成结算用的 `TokenKind` 费率表。
pub fn rates_of(price: &BuiltinPrice) -> BTreeMap<TokenKind, f64> {
    price
        .rates
        .iter()
        .filter(|(_, v)| v.is_finite())
        .map(|(k, v)| (TokenKind::from_key(k), *v))
        .collect()
}

/// 内置价一律是 USD / per_1m。
pub const BUILTIN_CURRENCY: &str = "USD";
pub const BUILTIN_DIVISOR: f64 = 1_000_000.0;

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn required_models_are_present() {
        // P3 完成定义点名的五个模型必须有内置价
        for m in ["gpt-4o", "gpt-4o-mini", "claude-sonnet-4-5", "claude-opus-4-1", "gemini-2.5-pro"]
        {
            let e = builtin_lookup(m).unwrap_or_else(|| panic!("缺少内置价：{m}"));
            assert!(!e.rates.is_empty(), "{m} 的费率不能为空");
            assert!(rates_of(e).contains_key(&TokenKind::Input), "{m} 缺少 input 价");
            assert!(rates_of(e).contains_key(&TokenKind::Output), "{m} 缺少 output 价");
        }
    }

    #[test]
    fn exact_match_wins_over_wildcard() {
        // gpt-4o-mini 必须命中 mini，而不是 gpt-4o*
        let mini = builtin_lookup("gpt-4o-mini").unwrap();
        assert_eq!(mini.pattern, "gpt-4o-mini*");
        assert!(rates_of(mini)[&TokenKind::Input] < 1.0);

        let full = builtin_lookup("gpt-4o").unwrap();
        assert_eq!(full.pattern, "gpt-4o*");
    }

    #[test]
    fn dated_aliases_fall_into_the_same_bucket() {
        assert_eq!(builtin_lookup("gpt-4o-2024-11-20").unwrap().pattern, "gpt-4o*");
        assert_eq!(builtin_lookup("gpt-4o-mini-2024-07-18").unwrap().pattern, "gpt-4o-mini*");
        assert_eq!(
            builtin_lookup("claude-sonnet-4-5-20250929").unwrap().pattern,
            "claude-sonnet-4-5*"
        );
        // 带 provider 前缀的写法
        assert_eq!(builtin_lookup("openai/gpt-4o-mini").unwrap().pattern, "gpt-4o-mini*");
    }

    #[test]
    fn unknown_models_are_not_invented() {
        assert!(builtin_lookup("some-local-llama-3").is_none());
        assert!(builtin_lookup("").is_none());
    }

    #[test]
    fn anthropic_cache_rates_are_consistent() {
        let e = builtin_lookup("claude-sonnet-4-5").unwrap();
        let r = rates_of(e);
        assert!(r[&TokenKind::InputCached] < r[&TokenKind::Input]);
        assert!(r[&TokenKind::InputCacheWrite] > r[&TokenKind::Input]);
        assert!(r[&TokenKind::InputCacheWrite1h] > r[&TokenKind::InputCacheWrite]);
    }
}
