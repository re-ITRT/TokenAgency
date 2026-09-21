//! Token 计量：把各家的 usage 归一化成 [`TokenUsage`]。
//!
//! **三条铁律**（见 `docs/11-internal-contracts.md` §6）：
//! 1. **必须先把 cached 从 input 总数里减掉**，否则重复计费统计。
//! 2. `usage_raw` 原样保存，供对账与将来补新类型。
//! 3. 上游没给细分 → 只记 `Input`/`Output` 并标 `usage_source`，**不估算、不猜**。

use serde_json::Value;

use crate::types::{Protocol, TokenKind, TokenUsage, UsageSource};

/// 归一化结果。
#[derive(Debug, Clone)]
pub struct MeteredUsage {
    pub usage: TokenUsage,
    pub source: UsageSource,
}

pub trait TokenMeter: Send + Sync {
    /// 归一化上游原始 usage
    fn normalize(&self, protocol: Protocol, raw: &Value) -> MeteredUsage;
    /// 上游完全没给 usage 时的粗估（图片按 ~765 tokens/张 估）
    fn estimate(&self, body: &Value) -> u64;
}

// ---------------------------------------------------------------- 取值助手

fn u(v: &Value, path: &str) -> u64 {
    v.pointer(path).and_then(|x| x.as_u64()).unwrap_or(0)
}

fn sat_sub(a: u64, b: u64) -> u64 {
    a.saturating_sub(b)
}

// ---------------------------------------------------------------- 三家实现

/// OpenAI Chat Completions
pub fn normalize_openai_chat(raw: &Value) -> MeteredUsage {
    // 兼容 usage 直接在根，或包在 response.usage 里
    let us = raw.get("usage").unwrap_or(raw);
    let prompt = u(us, "/prompt_tokens");
    let completion = u(us, "/completion_tokens");
    let cached = u(us, "/prompt_tokens_details/cached_tokens");
    let audio_in = u(us, "/prompt_tokens_details/audio_tokens");
    let reasoning = u(us, "/completion_tokens_details/reasoning_tokens");
    let audio_out = u(us, "/completion_tokens_details/audio_tokens");
    let acc = u(us, "/completion_tokens_details/accepted_prediction_tokens");
    let rej = u(us, "/completion_tokens_details/rejected_prediction_tokens");

    // ★ 先减掉 cached / audio，再作为 Input
    let input = sat_sub(
        sat_sub(prompt, cached),
        audio_in,
    );
    let output = sat_sub(sat_sub(completion, reasoning), audio_out);

    let mut tu = TokenUsage::new();
    tu.set(TokenKind::Input, input);
    tu.set(TokenKind::InputCached, cached);
    tu.set(TokenKind::InputAudio, audio_in);
    tu.set(TokenKind::Output, output);
    tu.set(TokenKind::OutputReasoning, reasoning);
    tu.set(TokenKind::OutputAudio, audio_out);
    tu.set(TokenKind::OutputPredictionAccepted, acc);
    tu.set(TokenKind::OutputPredictionRejected, rej);

    let has_details = us.get("prompt_tokens_details").is_some()
        || us.get("completion_tokens_details").is_some();
    MeteredUsage {
        usage: tu,
        source: if has_details { UsageSource::Upstream } else { UsageSource::UpstreamPartial },
    }
}

/// OpenAI Responses API
pub fn normalize_openai_responses(raw: &Value) -> MeteredUsage {
    let us = raw.get("usage").unwrap_or(raw);
    let input = u(us, "/input_tokens");
    let output = u(us, "/output_tokens");
    let cached = u(us, "/input_tokens_details/cached_tokens");
    let reasoning = u(us, "/output_tokens_details/reasoning_tokens");

    let mut tu = TokenUsage::new();
    tu.set(TokenKind::Input, sat_sub(input, cached));
    tu.set(TokenKind::InputCached, cached);
    tu.set(TokenKind::Output, sat_sub(output, reasoning));
    tu.set(TokenKind::OutputReasoning, reasoning);

    let has_details = us.get("input_tokens_details").is_some()
        || us.get("output_tokens_details").is_some();
    MeteredUsage {
        usage: tu,
        source: if has_details { UsageSource::Upstream } else { UsageSource::UpstreamPartial },
    }
}

/// Anthropic Messages
pub fn normalize_anthropic(raw: &Value) -> MeteredUsage {
    let us = raw.get("usage").unwrap_or(raw);
    let input = u(us, "/input_tokens");
    let output = u(us, "/output_tokens");
    let cache_read = u(us, "/cache_read_input_tokens");
    let cache_write_total = u(us, "/cache_creation_input_tokens");
    let w5m = u(us, "/cache_creation/ephemeral_5m_input_tokens");
    let w1h = u(us, "/cache_creation/ephemeral_1h_input_tokens");

    let mut tu = TokenUsage::new();
    tu.set(TokenKind::Input, input);
    tu.set(TokenKind::InputCached, cache_read);
    if w5m > 0 || w1h > 0 {
        tu.set(TokenKind::InputCacheWrite, w5m);
        tu.set(TokenKind::InputCacheWrite1h, w1h);
    } else {
        // 没有 5m/1h 细分时，全部算默认档
        tu.set(TokenKind::InputCacheWrite, cache_write_total);
    }
    tu.set(TokenKind::Output, output);

    let has_details = us.get("cache_read_input_tokens").is_some()
        || us.get("cache_creation_input_tokens").is_some();
    MeteredUsage {
        usage: tu,
        source: if has_details { UsageSource::Upstream } else { UsageSource::UpstreamPartial },
    }
}

/// Google Gemini
pub fn normalize_gemini(raw: &Value) -> MeteredUsage {
    let us = raw.get("usageMetadata").unwrap_or(raw);
    let prompt = u(us, "/promptTokenCount");
    let cand = u(us, "/candidatesTokenCount");
    let cached = u(us, "/cachedContentTokenCount");
    let thoughts = u(us, "/thoughtsTokenCount");
    let tool = u(us, "/toolUsePromptTokenCount");

    let mut tu = TokenUsage::new();
    tu.set(TokenKind::Input, sat_sub(sat_sub(prompt, cached), tool));
    tu.set(TokenKind::InputCached, cached);
    tu.set(TokenKind::Output, cand);
    tu.set(TokenKind::OutputReasoning, thoughts);

    let has_details = us.get("cachedContentTokenCount").is_some()
        || us.get("thoughtsTokenCount").is_some();
    MeteredUsage {
        usage: tu,
        source: if has_details { UsageSource::Upstream } else { UsageSource::UpstreamPartial },
    }
}

impl TokenMeter for crate::metering::DefaultMeter {
    fn normalize(&self, protocol: Protocol, raw: &Value) -> MeteredUsage {
        match protocol {
            Protocol::Openai | Protocol::Custom => normalize_openai_chat(raw),
            Protocol::OpenaiResponses => normalize_openai_responses(raw),
            Protocol::Anthropic => normalize_anthropic(raw),
            Protocol::Gemini => normalize_gemini(raw),
        }
    }

    fn estimate(&self, body: &Value) -> u64 {
        estimate_tokens(body)
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct DefaultMeter;

/// 极简估算：按字符数 / 4 粗估，图片按 765 tokens/张。
/// **只在完全拿不到 usage 时使用**，并会把 `usage_source` 标成 `Estimated`。
pub fn estimate_tokens(body: &Value) -> u64 {
    let mut total: u64 = 0;
    fn walk(v: &Value, total: &mut u64) {
        match v {
            Value::String(s) => *total += (s.chars().count() as u64) / 4 + 1,
            Value::Array(a) => a.iter().for_each(|x| walk(x, total)),
            Value::Object(m) => {
                if m.get("type").and_then(|t| t.as_str()) == Some("image_url")
                    || m.get("type").and_then(|t| t.as_str()) == Some("image")
                {
                    *total += 765;
                }
                m.values().for_each(|x| walk(x, total));
            }
            _ => {}
        }
    }
    walk(body, &mut total);
    total
}
