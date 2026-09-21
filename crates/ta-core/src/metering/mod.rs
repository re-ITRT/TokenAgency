//! Token 计量：把各家的 usage 归一化成 [`TokenUsage`]。
//!
//! **规格来源**：`docs/11-internal-contracts.md` §6（四家归一化表）、
//! `docs/07-ui-pages-and-cost.md` §6.1（成本中心 token 口径统一）。
//!
//! **三条铁律**：
//! 1. **子类型必须先从父总数里扣掉**，否则重复计费统计：
//!    - 输入侧：`prompt_tokens_details.cached_tokens` / `audio_tokens` 都**包含在**
//!      `prompt_tokens` 里（OpenAI 官方字段语义："present in the prompt"）；
//!      Gemini 的 `cachedContentTokenCount` / `toolUsePromptTokenCount` 同理包含在
//!      `promptTokenCount` 里（官方明确写了 "still the total effective prompt size"）。
//!    - 输出侧：`reasoning_tokens` / `audio_tokens` / 预测 token 都**包含在**
//!      `completion_tokens` 里。
//!    - Anthropic 是例外：`cache_read_input_tokens` / `cache_creation_input_tokens`
//!      **不在** `input_tokens` 里，所以 `Input` 不做扣减（§6 表如此规定）。
//!
//!    由此得到本项目**可断言的不变量**：上游给了总数时
//!    `usage.total() == upstream_total(protocol, raw)`。
//!    §6 表里 `Output = completion_tokens − reasoning − audio` 的写法是"没有预测输出
//!    时"的形态；一旦上游报了 `accepted/rejected_prediction_tokens`（它们同样是
//!    `completion_tokens` 的子集），也必须一并扣掉，否则 `total()` 会大于上游总数。
//! 2. 上游出现的新类型用 [`TokenKind::Other`] 原样保留，**不丢**；原始 usage 由调用方
//!    原样写进 `usage_raw` 用于对账。
//! 3. 上游没给细分 → 只记 `Input`/`Output`，`usage_source` 标 `UpstreamPartial`；
//!    **不估算、不猜**（估算只发生在完全拿不到 usage 时的 [`estimate_tokens`]）。

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

/// 在若干候选路径里挑出第一个"存在的对象"作为 usage 容器。
///
/// 兼容三种真实形态：
/// - OpenAI Chat / Anthropic：`{"usage": {...}}` 或直接把 usage 传进来；
/// - OpenAI Responses 事件：`{"type":"response.completed","response":{"usage":{...}}}`；
/// - Gemini：调用方传整包时是 `usageMetadata`，传进去的就是 usageMetadata 本身。
fn usage_container<'a>(raw: &'a Value, paths: &[&str]) -> &'a Value {
    for p in paths {
        if let Some(v) = raw.pointer(p) {
            if v.is_object() {
                return v;
            }
        }
    }
    raw
}

fn u(v: &Value, path: &str) -> u64 {
    v.pointer(path).and_then(|x| x.as_u64()).unwrap_or(0)
}

/// 依次尝试多个路径，取第一个**非零**的；都不非零时取第一个存在的值。
///
/// 用途：部分中转把字段名写成了另一家的（OpenAI 通道回 `input_tokens`，
/// Responses 通道回 `prompt_tokens`）。**只在主字段缺失/为 0 时才生效**，
/// 不影响正常形态。
fn u_any(v: &Value, paths: &[&str]) -> u64 {
    let mut fallback: Option<u64> = None;
    for p in paths {
        if let Some(n) = v.pointer(p).and_then(|x| x.as_u64()) {
            if n > 0 {
                return n;
            }
            if fallback.is_none() {
                fallback = Some(n);
            }
        }
    }
    fallback.unwrap_or(0)
}

/// 路径是否出现过（哪怕值是 0）——用于判断"上游给没给细分"。
fn has(v: &Value, path: &str) -> bool {
    v.pointer(path).is_some()
}

fn sat_sub(a: u64, b: u64) -> u64 {
    a.saturating_sub(b)
}

fn source_of(has_details: bool) -> UsageSource {
    if has_details {
        UsageSource::Upstream
    } else {
        UsageSource::UpstreamPartial
    }
}

/// 上游自报的 token 总数（用于对账 / 断言归一化不重复计数）。
///
/// - OpenAI Chat / Responses：`total_tokens`
/// - Gemini：`totalTokenCount`
/// - Anthropic：**不提供总数**（input / output / cache 各自独立），返回 `None`
pub fn upstream_total(protocol: Protocol, raw: &Value) -> Option<u64> {
    let us = match protocol {
        Protocol::Openai | Protocol::Custom | Protocol::OpenaiResponses => {
            usage_container(raw, &["/usage", "/response/usage"])
        }
        Protocol::Anthropic => usage_container(raw, &["/usage"]),
        Protocol::Gemini => usage_container(raw, &["/usageMetadata", "/response/usageMetadata"]),
    };
    match protocol {
        Protocol::Anthropic => None,
        Protocol::Gemini => us.pointer("/totalTokenCount").and_then(|x| x.as_u64()),
        _ => us.pointer("/total_tokens").and_then(|x| x.as_u64()),
    }
}

// ---------------------------------------------------------------- 四家实现

/// OpenAI Chat Completions（`Protocol::Custom` 的 OpenAI 兼容中转也走这里）。
///
/// 真实形态（prompt cache + reasoning 同时出现）：
/// ```json
/// {"usage":{"prompt_tokens":2000,"completion_tokens":900,"total_tokens":2900,
///           "prompt_tokens_details":{"cached_tokens":1792,"audio_tokens":0},
///           "completion_tokens_details":{"reasoning_tokens":768,"audio_tokens":0,
///               "accepted_prediction_tokens":0,"rejected_prediction_tokens":0}}}
/// ```
/// 归一化后 `total = (2000−1792) + 1792 + (900−768) + 768 = 2900` ✔
pub fn normalize_openai_chat(raw: &Value) -> MeteredUsage {
    let us = usage_container(raw, &["/usage", "/response/usage"]);
    // prompt_tokens / completion_tokens 是主字段；中转写成 Responses 字段名时兜底
    let prompt = u_any(us, &["/prompt_tokens", "/input_tokens"]);
    let completion = u_any(us, &["/completion_tokens", "/output_tokens"]);
    let cached =
        u_any(us, &["/prompt_tokens_details/cached_tokens", "/input_tokens_details/cached_tokens"]);
    let audio_in = u(us, "/prompt_tokens_details/audio_tokens");
    let reasoning = u(us, "/completion_tokens_details/reasoning_tokens");
    let audio_out = u(us, "/completion_tokens_details/audio_tokens");
    let acc = u(us, "/completion_tokens_details/accepted_prediction_tokens");
    let rej = u(us, "/completion_tokens_details/rejected_prediction_tokens");

    let mut tu = TokenUsage::new();
    // ★ 输入侧：cached / audio 都是 prompt_tokens 的子集，先减掉再作为 Input
    tu.set(TokenKind::Input, sat_sub(sat_sub(prompt, cached), audio_in));
    tu.set(TokenKind::InputCached, cached);
    tu.set(TokenKind::InputAudio, audio_in);
    // ★ 输出侧：reasoning / audio / 预测 token 都是 completion_tokens 的子集
    let output = sat_sub(sat_sub(sat_sub(sat_sub(completion, reasoning), audio_out), acc), rej);
    tu.set(TokenKind::Output, output);
    tu.set(TokenKind::OutputReasoning, reasoning);
    tu.set(TokenKind::OutputAudio, audio_out);
    tu.set(TokenKind::OutputPredictionAccepted, acc);
    tu.set(TokenKind::OutputPredictionRejected, rej);

    let has_details = has(us, "/prompt_tokens_details") || has(us, "/completion_tokens_details");
    MeteredUsage { usage: tu, source: source_of(has_details) }
}

/// OpenAI Responses API。
///
/// 真实形态：`{"usage":{"input_tokens":8000,"output_tokens":2000,"total_tokens":10000,
///   "input_tokens_details":{"cached_tokens":6000},
///   "output_tokens_details":{"reasoning_tokens":1500}}}`
/// （流式事件里整包是 `{"type":"response.completed","response":{"usage":{…}}}`）
pub fn normalize_openai_responses(raw: &Value) -> MeteredUsage {
    let us = usage_container(raw, &["/usage", "/response/usage"]);
    let input = u_any(us, &["/input_tokens", "/prompt_tokens"]);
    let output = u_any(us, &["/output_tokens", "/completion_tokens"]);
    let cached =
        u_any(us, &["/input_tokens_details/cached_tokens", "/prompt_tokens_details/cached_tokens"]);
    let reasoning = u_any(
        us,
        &["/output_tokens_details/reasoning_tokens", "/completion_tokens_details/reasoning_tokens"],
    );

    let mut tu = TokenUsage::new();
    tu.set(TokenKind::Input, sat_sub(input, cached));
    tu.set(TokenKind::InputCached, cached);
    tu.set(TokenKind::Output, sat_sub(output, reasoning));
    tu.set(TokenKind::OutputReasoning, reasoning);

    let has_details = has(us, "/input_tokens_details") || has(us, "/output_tokens_details");
    MeteredUsage { usage: tu, source: source_of(has_details) }
}

/// Anthropic Messages。
///
/// 真实形态（显式 5m/1h 缓存写入 + 缓存命中）：
/// ```json
/// {"usage":{"input_tokens":400,"output_tokens":250,
///   "cache_read_input_tokens":12000,"cache_creation_input_tokens":1000,
///   "cache_creation":{"ephemeral_5m_input_tokens":100,"ephemeral_1h_input_tokens":900}}}
/// ```
/// **Anthropic 的 `input_tokens` 不含 cache read/write**，所以 `Input` 原样取用，不扣减。
/// thinking token 已并入 `output_tokens`，因此没有 `OutputReasoning`（§6 表）。
pub fn normalize_anthropic(raw: &Value) -> MeteredUsage {
    let us = usage_container(raw, &["/usage"]);
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
        // 细分之和小于总数时，差额并回默认档 —— 绝不丢 token
        let detail = w5m + w1h;
        if cache_write_total > detail {
            tu.set(TokenKind::InputCacheWrite, w5m + (cache_write_total - detail));
        }
    } else {
        // 没有 5m/1h 细分时，全部算默认档
        tu.set(TokenKind::InputCacheWrite, cache_write_total);
    }
    tu.set(TokenKind::Output, output);

    let has_details =
        has(us, "/cache_read_input_tokens") || has(us, "/cache_creation_input_tokens");
    MeteredUsage { usage: tu, source: source_of(has_details) }
}

/// Google Gemini（`usageMetadata`）。
///
/// 真实形态（thinking + 缓存命中）：
/// ```json
/// {"usageMetadata":{"promptTokenCount":10000,"cachedContentTokenCount":8000,
///   "candidatesTokenCount":500,"thoughtsTokenCount":200,"totalTokenCount":10700}}
/// ```
/// `promptTokenCount` 是"含缓存内容的有效 prompt 总量"，所以 `Input` 要减掉 cached。
/// `candidatesTokenCount` 不含 thoughts（`totalTokenCount` 含），故 `Output` 不扣减。
/// `toolUsePromptTokenCount`（函数调用）同样是 prompt 的一部分：先减掉再从
/// `Other("tool_use_prompt")` 原样保留，既不重复计数也不丢数据。
pub fn normalize_gemini(raw: &Value) -> MeteredUsage {
    let us = usage_container(raw, &["/usageMetadata", "/response/usageMetadata"]);
    let prompt = u(us, "/promptTokenCount");
    let cand = u(us, "/candidatesTokenCount");
    let cached = u(us, "/cachedContentTokenCount");
    let thoughts = u(us, "/thoughtsTokenCount");
    let tool = u(us, "/toolUsePromptTokenCount");

    let mut tu = TokenUsage::new();
    tu.set(TokenKind::Input, sat_sub(sat_sub(prompt, cached), tool));
    tu.set(TokenKind::InputCached, cached);
    tu.set(TokenKind::Other("tool_use_prompt".to_string()), tool);
    tu.set(TokenKind::Output, cand);
    tu.set(TokenKind::OutputReasoning, thoughts);

    let has_details = has(us, "/cachedContentTokenCount") || has(us, "/thoughtsTokenCount");
    MeteredUsage { usage: tu, source: source_of(has_details) }
}

// ---------------------------------------------------------------- 门面

impl TokenMeter for DefaultMeter {
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

// ---------------------------------------------------------------- 测试

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::{json, Value};

    /// 归一化并逐项断言；上游给了总数时**必须守恒**（最关键的不变量）。
    fn check(protocol: Protocol, raw: Value, expect: &[(TokenKind, u64)]) -> MeteredUsage {
        let m = DefaultMeter.normalize(protocol, &raw);
        let ctx = serde_json::to_string(&raw).unwrap();
        for (k, n) in expect {
            assert_eq!(m.usage.get(k), *n, "kind={k:?} 不符（raw={ctx}）");
        }
        if let Some(t) = upstream_total(protocol, &raw) {
            assert_eq!(
                m.usage.total(),
                t,
                "usage.total() 必须等于上游总数（cached/reasoning/audio 未扣干净就会重复计数）raw={ctx}"
            );
        }
        m
    }

    // ============================================================ OpenAI Chat

    #[test]
    fn openai_chat_01_basic_no_details() {
        // 退化形态：中转只回 input/output 总数，没有任何 details
        let raw = json!({
            "id": "chatcmpl-1", "model": "gpt-4o",
            "usage": {"prompt_tokens": 100, "completion_tokens": 40, "total_tokens": 140}
        });
        let m = check(
            Protocol::Openai,
            raw,
            &[
                (TokenKind::Input, 100),
                (TokenKind::InputCached, 0),
                (TokenKind::Output, 40),
                (TokenKind::InputAudio, 0),
                (TokenKind::OutputReasoning, 0),
            ],
        );
        assert_eq!(m.source, UsageSource::UpstreamPartial);
        assert_eq!(m.usage.input_total(), 100);
        assert_eq!(m.usage.output_total(), 40);
    }

    #[test]
    fn openai_chat_02_prompt_cache_hit() {
        let raw = json!({
            "usage": {"prompt_tokens": 2000, "completion_tokens": 150, "total_tokens": 2150,
                      "prompt_tokens_details": {"cached_tokens": 1792, "audio_tokens": 0}}
        });
        let m = check(
            Protocol::Openai,
            raw,
            &[(TokenKind::Input, 208), (TokenKind::InputCached, 1792), (TokenKind::Output, 150)],
        );
        assert_eq!(m.source, UsageSource::Upstream);
    }

    #[test]
    fn openai_chat_03_reasoning_model() {
        // o 系列：completion_tokens 里含 reasoning
        let raw = json!({
            "usage": {"prompt_tokens": 120, "completion_tokens": 900, "total_tokens": 1020,
                      "completion_tokens_details": {"reasoning_tokens": 768, "audio_tokens": 0,
                          "accepted_prediction_tokens": 0, "rejected_prediction_tokens": 0}}
        });
        let m = check(
            Protocol::Openai,
            raw,
            &[(TokenKind::Input, 120), (TokenKind::Output, 132), (TokenKind::OutputReasoning, 768)],
        );
        assert_eq!(m.source, UsageSource::Upstream);
    }

    #[test]
    fn openai_chat_04_audio_in_and_out() {
        // gpt-4o-audio-preview：prompt/completion 里都含 audio 子集
        let raw = json!({
            "usage": {"prompt_tokens": 500, "completion_tokens": 200, "total_tokens": 700,
                      "prompt_tokens_details": {"cached_tokens": 0, "audio_tokens": 300},
                      "completion_tokens_details": {"reasoning_tokens": 0, "audio_tokens": 120}}
        });
        check(
            Protocol::Openai,
            raw,
            &[
                (TokenKind::Input, 200),
                (TokenKind::InputAudio, 300),
                (TokenKind::Output, 80),
                (TokenKind::OutputAudio, 120),
                (TokenKind::InputCached, 0),
            ],
        );
    }

    #[test]
    fn openai_chat_05_cached_plus_predicted_output() {
        // 预测输出（Predicted Outputs）：accepted/rejected 也是 completion 的子集
        let raw = json!({
            "usage": {"prompt_tokens": 1000, "completion_tokens": 300, "total_tokens": 1300,
                      "prompt_tokens_details": {"cached_tokens": 512, "audio_tokens": 0},
                      "completion_tokens_details": {"reasoning_tokens": 0, "audio_tokens": 0,
                          "accepted_prediction_tokens": 128, "rejected_prediction_tokens": 0}}
        });
        let m = check(
            Protocol::Openai,
            raw,
            &[
                (TokenKind::Input, 488),
                (TokenKind::InputCached, 512),
                (TokenKind::Output, 172),
                (TokenKind::OutputPredictionAccepted, 128),
                (TokenKind::OutputPredictionRejected, 0),
            ],
        );
        assert_eq!(m.usage.total(), 1300);
    }

    #[test]
    fn openai_chat_tolerates_anthropic_style_keys() {
        // 中转把字段名写错（OpenAI 通道回 input_tokens/output_tokens）
        let raw = json!({"usage": {"input_tokens": 60, "output_tokens": 20}});
        check(Protocol::Openai, raw, &[(TokenKind::Input, 60), (TokenKind::Output, 20)]);
    }

    // ======================================================= OpenAI Responses

    #[test]
    fn openai_responses_01_basic_no_details() {
        let raw = json!({"usage": {"input_tokens": 50, "output_tokens": 20, "total_tokens": 70}});
        let m = check(
            Protocol::OpenaiResponses,
            raw,
            &[(TokenKind::Input, 50), (TokenKind::Output, 20), (TokenKind::InputCached, 0)],
        );
        assert_eq!(m.source, UsageSource::UpstreamPartial);
    }

    #[test]
    fn openai_responses_02_cached_input() {
        let raw = json!({
            "usage": {"input_tokens": 3000, "output_tokens": 60, "total_tokens": 3060,
                      "input_tokens_details": {"cached_tokens": 2048}}
        });
        check(
            Protocol::OpenaiResponses,
            raw,
            &[(TokenKind::Input, 952), (TokenKind::InputCached, 2048), (TokenKind::Output, 60)],
        );
    }

    #[test]
    fn openai_responses_03_reasoning() {
        let raw = json!({
            "usage": {"input_tokens": 40, "output_tokens": 1500, "total_tokens": 1540,
                      "output_tokens_details": {"reasoning_tokens": 1200}}
        });
        check(
            Protocol::OpenaiResponses,
            raw,
            &[(TokenKind::Input, 40), (TokenKind::Output, 300), (TokenKind::OutputReasoning, 1200)],
        );
    }

    #[test]
    fn openai_responses_04_wrapped_in_completed_event() {
        // 流式事件整包形态（退化：没有 details）
        let raw = json!({
            "type": "response.completed",
            "response": {"id": "resp_1",
                "usage": {"input_tokens": 10, "output_tokens": 5, "total_tokens": 15}}
        });
        let m = check(
            Protocol::OpenaiResponses,
            raw,
            &[(TokenKind::Input, 10), (TokenKind::Output, 5)],
        );
        assert_eq!(m.source, UsageSource::UpstreamPartial);
    }

    #[test]
    fn openai_responses_05_cached_plus_reasoning() {
        let raw = json!({
            "usage": {"input_tokens": 8000, "output_tokens": 2000, "total_tokens": 10000,
                      "input_tokens_details": {"cached_tokens": 6000, "text_tokens": 2000},
                      "output_tokens_details": {"reasoning_tokens": 1500}}
        });
        check(
            Protocol::OpenaiResponses,
            raw,
            &[
                (TokenKind::Input, 2000),
                (TokenKind::InputCached, 6000),
                (TokenKind::Output, 500),
                (TokenKind::OutputReasoning, 1500),
            ],
        );
    }

    // ============================================================ Anthropic

    #[test]
    fn anthropic_01_basic_no_details() {
        // 退化形态：没有 cache 字段
        let raw = json!({"id": "msg_1", "usage": {"input_tokens": 120, "output_tokens": 45}});
        let m = check(
            Protocol::Anthropic,
            raw,
            &[
                (TokenKind::Input, 120),
                (TokenKind::Output, 45),
                (TokenKind::InputCached, 0),
                (TokenKind::InputCacheWrite, 0),
                (TokenKind::OutputReasoning, 0),
            ],
        );
        assert_eq!(m.source, UsageSource::UpstreamPartial);
    }

    #[test]
    fn anthropic_02_cache_read_and_write() {
        // ★ input_tokens 不含 cache read/write，所以不对 Input 做扣减
        let raw = json!({
            "usage": {"input_tokens": 400, "output_tokens": 250,
                      "cache_read_input_tokens": 12000, "cache_creation_input_tokens": 800}
        });
        let m = check(
            Protocol::Anthropic,
            raw,
            &[
                (TokenKind::Input, 400),
                (TokenKind::InputCached, 12000),
                (TokenKind::InputCacheWrite, 800),
                (TokenKind::InputCacheWrite1h, 0),
                (TokenKind::Output, 250),
            ],
        );
        assert_eq!(m.source, UsageSource::Upstream);
        assert_eq!(m.usage.total(), 400 + 12000 + 800 + 250);
    }

    #[test]
    fn anthropic_03_cache_creation_ttl_breakdown() {
        let raw = json!({
            "usage": {"input_tokens": 30, "output_tokens": 10,
                      "cache_read_input_tokens": 0, "cache_creation_input_tokens": 1000,
                      "cache_creation": {"ephemeral_5m_input_tokens": 100,
                                         "ephemeral_1h_input_tokens": 900}}
        });
        check(
            Protocol::Anthropic,
            raw,
            &[
                (TokenKind::Input, 30),
                (TokenKind::InputCached, 0),
                (TokenKind::InputCacheWrite, 100),
                (TokenKind::InputCacheWrite1h, 900),
            ],
        );
    }

    #[test]
    fn anthropic_04_thinking_tokens_stay_in_output() {
        // extended thinking：thinking token 并入 output_tokens（§6 表：OutputReasoning 为 —）
        let raw = json!({
            "usage": {"input_tokens": 30, "output_tokens": 2000, "cache_read_input_tokens": 0}
        });
        let m = check(
            Protocol::Anthropic,
            raw,
            &[(TokenKind::Input, 30), (TokenKind::Output, 2000), (TokenKind::OutputReasoning, 0)],
        );
        assert_eq!(m.source, UsageSource::Upstream); // cache_read 字段出现过（值为 0）
    }

    #[test]
    fn anthropic_05_ttl_breakdown_smaller_than_total_keeps_every_token() {
        // 奇葩中转：细分之和 < cache_creation_input_tokens，差额不能丢
        let raw = json!({
            "usage": {"input_tokens": 5, "output_tokens": 5, "cache_creation_input_tokens": 500,
                      "cache_creation": {"ephemeral_5m_input_tokens": 100,
                                         "ephemeral_1h_input_tokens": 300}}
        });
        check(
            Protocol::Anthropic,
            raw,
            &[
                (TokenKind::Input, 5),
                (TokenKind::InputCacheWrite, 200), // 100 (5m) + 100 (总数与细分之和的差额)
                (TokenKind::InputCacheWrite1h, 300),
            ],
        );
    }

    // =============================================================== Gemini

    #[test]
    fn gemini_01_basic_no_details() {
        let raw = json!({
            "usageMetadata": {"promptTokenCount": 100, "candidatesTokenCount": 50,
                              "totalTokenCount": 150}
        });
        let m = check(
            Protocol::Gemini,
            raw,
            &[
                (TokenKind::Input, 100),
                (TokenKind::Output, 50),
                (TokenKind::InputCached, 0),
                (TokenKind::OutputReasoning, 0),
            ],
        );
        assert_eq!(m.source, UsageSource::UpstreamPartial);
    }

    #[test]
    fn gemini_02_cached_content() {
        let raw = json!({
            "usageMetadata": {"promptTokenCount": 5000, "cachedContentTokenCount": 4096,
                              "candidatesTokenCount": 200, "totalTokenCount": 5200}
        });
        check(
            Protocol::Gemini,
            raw,
            &[(TokenKind::Input, 904), (TokenKind::InputCached, 4096), (TokenKind::Output, 200)],
        );
    }

    #[test]
    fn gemini_03_thinking_tokens() {
        let raw = json!({
            "usageMetadata": {"promptTokenCount": 260, "candidatesTokenCount": 35,
                              "thoughtsTokenCount": 249, "totalTokenCount": 544}
        });
        check(
            Protocol::Gemini,
            raw,
            &[(TokenKind::Input, 260), (TokenKind::Output, 35), (TokenKind::OutputReasoning, 249)],
        );
    }

    #[test]
    fn gemini_04_tool_use_prompt_tokens() {
        // 函数调用：toolUsePromptTokenCount 属于 prompt，扣掉后原样保留为 Other
        let raw = json!({
            "usageMetadata": {"promptTokenCount": 800, "candidatesTokenCount": 40,
                              "toolUsePromptTokenCount": 120, "totalTokenCount": 840}
        });
        let m = check(
            Protocol::Gemini,
            raw,
            &[
                (TokenKind::Input, 680),
                (TokenKind::Output, 40),
                (TokenKind::Other("tool_use_prompt".into()), 120),
            ],
        );
        assert_eq!(m.usage.total(), 840);
    }

    #[test]
    fn gemini_05_cached_plus_thoughts() {
        let raw = json!({
            "usageMetadata": {"promptTokenCount": 10000, "cachedContentTokenCount": 8000,
                              "candidatesTokenCount": 500, "thoughtsTokenCount": 200,
                              "totalTokenCount": 10700}
        });
        check(
            Protocol::Gemini,
            raw,
            &[
                (TokenKind::Input, 2000),
                (TokenKind::InputCached, 8000),
                (TokenKind::Output, 500),
                (TokenKind::OutputReasoning, 200),
            ],
        );
    }

    // ============================================================ 其它

    /// ★ 硬约束回归（D 块第 1 条）：**cached 必须从 input 总数里减掉，
    /// reasoning 必须从 output 总数里减掉**，否则会重复计费统计。
    ///
    /// 四家一起跑同一组数字，逐条断言"父总数 = 各子类型之和"。
    #[test]
    fn hard_constraint_sub_kinds_are_subtracted_from_their_parent_totals() {
        struct Case {
            proto: Protocol,
            raw: Value,
            /// 上游口径的输入总数（OpenAI: prompt_tokens / Anthropic: input_tokens / Gemini: promptTokenCount）
            input_total: u64,
            output_total: u64,
            cached: u64,
            reasoning: u64,
            /// cached 是否包含在输入总数里（Anthropic 是唯一不包含的）
            cached_in_input: bool,
            /// reasoning 是否包含在输出总数里（Anthropic 并入 output、Gemini 另计）
            reasoning_in_output: bool,
        }

        let cases = vec![
            Case {
                proto: Protocol::Openai,
                raw: json!({"usage": {"prompt_tokens": 1000, "completion_tokens": 400,
                    "total_tokens": 1400,
                    "prompt_tokens_details": {"cached_tokens": 768, "audio_tokens": 0},
                    "completion_tokens_details": {"reasoning_tokens": 256, "audio_tokens": 0}}}),
                input_total: 1000,
                output_total: 400,
                cached: 768,
                reasoning: 256,
                cached_in_input: true,
                reasoning_in_output: true,
            },
            Case {
                proto: Protocol::OpenaiResponses,
                raw: json!({"usage": {"input_tokens": 1000, "output_tokens": 400,
                    "total_tokens": 1400,
                    "input_tokens_details": {"cached_tokens": 768},
                    "output_tokens_details": {"reasoning_tokens": 256}}}),
                input_total: 1000,
                output_total: 400,
                cached: 768,
                reasoning: 256,
                cached_in_input: true,
                reasoning_in_output: true,
            },
            Case {
                proto: Protocol::Anthropic,
                raw: json!({"usage": {"input_tokens": 1000, "output_tokens": 400,
                    "cache_read_input_tokens": 768}}),
                input_total: 1000,
                output_total: 400,
                cached: 768,
                reasoning: 0,
                cached_in_input: false,
                reasoning_in_output: false,
            },
            Case {
                proto: Protocol::Gemini,
                raw: json!({"usageMetadata": {"promptTokenCount": 1000,
                    "cachedContentTokenCount": 768, "candidatesTokenCount": 400,
                    // Gemini 的 totalTokenCount 含 thoughts：1000 + 400 + 256
                    "thoughtsTokenCount": 256, "totalTokenCount": 1656}}),
                input_total: 1000,
                output_total: 400,
                cached: 768,
                reasoning: 256,
                cached_in_input: true,
                reasoning_in_output: false,
            },
        ];

        for c in cases {
            let label = format!("{:?}", c.proto);
            let m = DefaultMeter.normalize(c.proto, &c.raw);
            let i = m.usage.get(&TokenKind::Input);
            let o = m.usage.get(&TokenKind::Output);

            // 子类型必须单独出现（不能被折进父类型里）
            assert_eq!(
                m.usage.get(&TokenKind::InputCached),
                c.cached,
                "{label}: cached 要单独记一类"
            );
            assert_eq!(
                m.usage.get(&TokenKind::OutputReasoning),
                c.reasoning,
                "{label}: reasoning 要单独记一类"
            );

            if c.cached_in_input {
                assert_eq!(
                    i,
                    c.input_total - c.cached,
                    "{label}: Input 必须是输入总数 − cached（否则重复计费统计）"
                );
                assert_eq!(m.usage.input_total(), c.input_total, "{label}: 输入侧合计要守恒");
            } else {
                assert_eq!(i, c.input_total, "{label}: Anthropic 的 input_tokens 本来就不含 cache");
            }

            if c.reasoning_in_output {
                assert_eq!(
                    o,
                    c.output_total - c.reasoning,
                    "{label}: Output 必须是输出总数 − reasoning（否则重复计费统计）"
                );
                assert_eq!(m.usage.output_total(), c.output_total, "{label}: 输出侧合计要守恒");
            } else {
                assert_eq!(o, c.output_total, "{label}: 该家的 output 总数不含 reasoning");
            }

            if let Some(t) = upstream_total(c.proto, &c.raw) {
                assert_eq!(m.usage.total(), t, "{label}: 归一化后总 token 必须等于上游总数");
            }
        }
    }

    #[test]
    fn degenerate_empty_usage_is_all_zero_and_not_invented() {
        let m = check(Protocol::Openai, json!({"usage": {}}), &[]);
        assert_eq!(m.usage.total(), 0);
        assert_eq!(m.source, UsageSource::UpstreamPartial);
    }

    #[test]
    fn saturating_subtraction_never_underflows_on_bogus_relays() {
        // 中转给了比总数还大的 cached（数据脏），不能 panic/回绕
        let raw = json!({
            "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15,
                      "prompt_tokens_details": {"cached_tokens": 99}}
        });
        let m = DefaultMeter.normalize(Protocol::Openai, &raw);
        assert_eq!(m.usage.get(&TokenKind::Input), 0);
        assert_eq!(m.usage.get(&TokenKind::InputCached), 99);
    }

    #[test]
    fn custom_protocol_follows_openai_chat() {
        let raw =
            json!({"usage": {"prompt_tokens": 7, "completion_tokens": 3, "total_tokens": 10}});
        check(Protocol::Custom, raw, &[(TokenKind::Input, 7), (TokenKind::Output, 3)]);
    }

    #[test]
    fn estimate_is_rough_but_image_aware() {
        let body = json!({"messages": [{"role": "user", "content": [
            {"type": "text", "text": "hello world"},
            {"type": "image_url", "image_url": {"url": "data:image/png;base64,AAAA"}}
        ]}]});
        let n = DefaultMeter.estimate(&body);
        assert!(n >= 765, "图片至少要按 ~765 tokens 估，实际 {n}");
    }

    #[test]
    fn upstream_total_shape_is_protocol_specific() {
        let chat =
            json!({"usage": {"prompt_tokens": 1, "completion_tokens": 2, "total_tokens": 3}});
        assert_eq!(upstream_total(Protocol::Openai, &chat), Some(3));
        let anth = json!({"usage": {"input_tokens": 1, "output_tokens": 2}});
        assert_eq!(upstream_total(Protocol::Anthropic, &anth), None);
        let gem = json!({"usageMetadata": {"totalTokenCount": 9}});
        assert_eq!(upstream_total(Protocol::Gemini, &gem), Some(9));
    }
}
