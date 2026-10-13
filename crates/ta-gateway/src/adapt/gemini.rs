//! Google Gemini 适配器（`Protocol::Gemini`，`POST /v1beta/models/{model}:generateContent`）。
//!
//! 与 `openai.rs` / `anthropic.rs` **同构**：只认文档化的形状，其余一律登记 [`Loss`]，
//! 绝不静默丢。下面只写 Gemini 与那两个协议不同、因而必须表态的地方 ——
//! 也就是回答"为什么这么定"，而不是"这行做了什么"。
//!
//! # ① 角色只有 `user` / `model`
//!
//! `contents[].role` 没有 `system` / `assistant`：系统提示在顶层 `systemInstruction`，
//! 模型侧叫 `model`。解码时把 `systemInstruction` 折进 [`IrRequest::system`]、`model` 折成
//! [`IrRole::Assistant`]，编码时反过来。`contents` 里真出现 `system` 角色时**不猜**：
//! 登记 [`Loss::UnsupportedRole`] —— 猜错等于把"用户说的话"升级成系统指令，
//! 那是安全问题，不只是内容问题。
//!
//! # ② `model` 与 `stream` 不在 body 里
//!
//! Gemini 把模型名放进 URL 路径（`…/models/{model}:generateContent`），把流式放进**方法名**
//! （`:streamGenerateContent?alt=sse`）。于是这一层的输入输出都不在 body：编码时
//! `upstream_model` **无处可放**（body 里塞 `model` 会被上游按未知字段拒掉），解码时看不到
//! URL 也认不出"这次是流式"。所以 `ProtocolAdapter::encode_request` 的 `_upstream_model`
//! 是**故意不用**的，"这次是流式"必须由看得见路径的那一层写进 IR（`ingress::upstream_path_for`）。
//!
//! # ③ 工具调用：id 与 name 各归各的
//!
//! 配对方式不同 —— OpenAI / Anthropic 用 **id**，Gemini 用 **name**（外加一个可选的 id）。
//! v1beta 的 `FunctionCall` / `FunctionResponse` 都有 **Optional 的 `id`**
//! （官方 schema：FunctionCall.id "Optional. The unique id of the function call. If populated,
//! the client to execute the function_call and return the response with the matching id."），
//! 所以 IR 里带 id 时就带上：**不带上，往返核对必然对不上**（`verify_no_loss` 逐字段比 IR），
//! 默认策略（`on_loss: reject`）下每一次跨协议的工具调用都会 415。
//! 反过来 [`IrPart::ToolResult`] 只有 id，而 `functionResponse` **必须给 name**
//! ⇒ 编码前用 id 反查 name（[`FunctionNames`]）：id 非空按 id 精确匹配，
//! **id 为空时按出现顺序的第 n 个配第 n 个**（Gemini 自己发的调用常常没有 id，
//! 全收敛到第一个名字会把"查时间的结果"说成"查天气的结果"）。查不到就
//! [`Loss::MissingRequired`]，**不编**一个名字出来。
//! `response` 必须是 JSON 对象而 IR 的工具结果是一串内容块，
//! 用 `{"result": …}` 这层壳做**互逆**映射（`encode_function_response_content`）。
//!
//! # ④ 流式：一帧里可能同时有"内容"和"收尾"
//!
//! Gemini 的流**没有** `data: [DONE]`，它以最后一个带 `finishReason` 的 chunk 收尾；
//! `usageMetadata` 是**累积**的（后一帧覆盖前一帧，最后一帧最完整 —— 见
//! `ir_usage_from_gemini`）。真实抓包里"最后一块文字 + `finishReason` + usage"
//! 常常**挤在同一帧**里，所以 [`StreamAdapter::decode_stream_event`] 返回的是**一串**事件
//! （第 328 轮）：这一帧会解出 `[TextDelta, Stop]` **两个**事件，顺序是
//! 先内容、后收尾。旧签名（`Option<IrStreamEvent>`）在这里只能二选一 ——
//! 那不是取舍，那是丢数据：丢一块文字是用户看得见的内容损失，
//! 丢一帧 stop/usage 则让客户端永远等不到终止判定。
//!
//! # ⑤ 响应侧没有 usage，也没有候选级元信息的家
//!
//! [`IrResponse`] 里没有 usage 字段（`openai.rs` / `anthropic.rs` 同样不搬响应 usage），
//! 所以 `usageMetadata` 在整包路径上**不进 IR**，也**不登记损失**（那只会给每个响应加一条
//! 噪声，账本读的是原始 body）；只有流式的 [`IrUsage`] 能带上它。`safetyRatings` /
//! `citationMetadata` / `groundingMetadata` / `logprobsResult` / `thought` 这类候选级信息
//! IR 表达不了 ⇒ **逐条**登记 [`Loss`]，绝不静默吞。`promptFeedback.blockReason`
//! （prompt 级拦截）同样登记损失，而不是当成一个"合法的空回复"交出去 ——
//! 那种失败用户永远发现不了。
//!
//! # 已知缺口（有意留着，不是遗漏）
//!
//! * **往 Gemini 发工具入参分片**：Gemini 的 `functionCall.args` 是**完整 JSON 对象**，
//!   协议里没有"入参分片"这种形态 ⇒ [`IrStreamEvent::ToolInputDelta`] 在 Gemini 方向
//!   无处安放：这里发 0 帧（发半个 JSON 就是编一个上游不会发的形状）但**留痕**。
//!   **这不丢数据**：Gemini 作上游时入参是**一次给全**的，走
//!   [`IrStreamEvent::ToolUseStart`] 的 `input` 字段（第 330 轮之前那一格不存在，
//!   整个 `args` 会静默消失）。反向（别的协议 → Gemini）才走分片，而那条路
//!   **不可达**：Gemini 永远不会是客户端协议（见 `session.rs` 模块头）。
//! * **流式 `functionCall` 的槽位**：Gemini 的 part 没有序号，适配器又是无状态的，
//!   所以槽位只能是"这一帧里的第几个 `functionCall`"。跨帧的两个调用会撞到同一个编号
//!   （上游通常把同一轮的调用放在一帧里，实际很少见）。
//! * **`usageMetadata` 里 IR 放不下的计数**：`totalTokenCount`（= prompt + candidates +
//!   **thoughts**）是**可推导量**，`toolUsePromptTokenCount` 是 `promptTokenCount` 的
//!   **子集细分**且没有别的协议有这一格 ⇒ 两个都不搬、也不拿它们去凑别的字段
//!   （凑出来的和会漏掉思考 token，是**编一个对不上的数**）。
//!   `thoughtsTokenCount` **有**槽位（[`IrUsage::reasoning_tokens`]）。

use std::collections::HashMap;

use serde_json::{json, Value};

use super::shared::{
    describe_image, f64_of, part_kind, read_usage, strings_of, u64_of, write_usage, UsageShape,
};
use crate::adapt::ir::{
    canonicalize, json_string, object, text_part, IrImage, IrMessage, IrPart, IrRequest,
    IrResponse, IrRole, IrStopReason, IrTool, IrToolChoice,
};
use crate::adapt::loss::Loss;
use crate::adapt::stream::{EncodeCtx, IrStreamEvent, IrUsage, Slot, StreamAdapter, StreamFrame};

/// Gemini 的 `inlineData` 收哪些图片格式（svg 不在其中 ⇒ 拒绝，**绝不文本化**）。
pub(crate) const IMAGE_MEDIA_TYPES: &[&str] =
    &["image/png", "image/jpeg", "image/webp", "image/heic", "image/heif"];

/// `generationConfig.stopSequences` 的协议上限（官方文档：**最多 5 条**）。
///
/// 超了上游直接 400，而它不会告诉我们哪一条多余 ⇒ 在 `check_encodable` 里先拒。
const MAX_STOP_SEQUENCES: usize = 5;

pub struct GeminiAdapter;

/// 已经映射到 IR 的顶层字段。**其余非空字段全部登记损失**（IR 路径要核对，不能装看不见）。
const KNOWN_TOP_LEVEL: &[&str] = &[
    "contents",
    "systemInstruction",
    "system_instruction",
    "generationConfig",
    "generation_config",
    "tools",
    "toolConfig",
    "tool_config",
    // 模型名与流式标志本来在 URL 上（模块头 ②）；有的中转会冗余带上，
    // 认了它们才不会报一条**假**损失（我们确实读了它们）。
    "model",
    "stream",
];

/// `generationConfig` 里 IR 表达得了的字段；其余（`topK` / `seed` / `thinkingConfig`…）逐项登记。
const KNOWN_GENERATION_CONFIG: &[&str] = &[
    "maxOutputTokens",
    "max_output_tokens",
    "temperature",
    "topP",
    "top_p",
    "stopSequences",
    "stop_sequences",
];

/// 响应候选里 IR 表达得了的键；其余（`safetyRatings` / `citationMetadata` / `groundingMetadata`…）
/// 都是候选级元信息 ⇒ 逐项登记损失。
const KNOWN_CANDIDATE: &[&str] = &["content", "finishReason", "finish_reason", "index"];

// ---------------------------------------------------------------- 小工具

/// Gemini 的 REST 同时接受 camelCase 与 snake_case（`trace_store` 认 `inline_data` 时也按这条走），
/// 所以取值一律两个拼写都看：少认一个就会把合法请求判成"没有这个字段"。
fn field<'a>(obj: &'a Value, camel: &str, snake: &str) -> Option<&'a Value> {
    obj.get(camel).or_else(|| obj.get(snake))
}

/// 上游到底说没说什么：`null` / 空数组 / 空对象都算"没说"。
/// 用它挡住"`safetyRatings: []` 也记一条损失"的噪声 —— 噪声一多，真损失就没人看了。
fn has_meaning(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Array(items) => !items.is_empty(),
        Value::Object(map) => !map.is_empty(),
        _ => true,
    }
}

/// 一个 `Part`（Gemini 的 parts 是 oneof：`text` / `inlineData` / `functionCall`…）。
///
/// 认不出来就登记损失并返回 `None`：**不猜**它是文本（：
/// 映射不过去的内容绝不用一段文字顶替 —— 客户端会以为自己看见了原文）。
fn decode_part(part: &Value, path: &str, losses: &mut Vec<Loss>) -> Option<IrPart> {
    let Some(obj) = part.as_object() else {
        losses.push(Loss::UnsupportedPart { path: path.into(), kind: "part(非对象)".into() });
        return None;
    };
    if let Some(text) = obj.get("text").and_then(Value::as_str) {
        // ★ 思维链与正文用的是**同一个** `text` 字段，靠 `thought: true` 区分。
        //   不先认它，模型会把推理过程当答案发给用户 —— 比丢掉更糟，所以登记后按丢处理。
        if obj.get("thought").and_then(Value::as_bool) == Some(true) {
            losses.push(Loss::UnsupportedPart {
                path: path.into(),
                kind: "thought（思维链在 IR 里没有位置）".into(),
            });
            return None;
        }
        return Some(text_part(text));
    }
    if let Some(inline) = field(part, "inlineData", "inline_data") {
        return decode_inline_data(inline, path, losses).map(IrPart::Image);
    }
    if field(part, "fileData", "file_data").is_some() {
        // Files API / GCS 的引用：IR 只有"内联图片"与"http(s) URL"两种形态，两者都不是 ⇒ 拒绝。
        losses.push(Loss::UnmappableImage {
            path: path.into(),
            detail: "Gemini 的 fileData（Files API / GCS 引用）没有 IR 表达（图片必须内联）".into(),
        });
        return None;
    }
    if let Some(call) = field(part, "functionCall", "function_call") {
        return Some(IrPart::ToolUse {
            // `id` 是 Optional：上游给了就带上（跨协议时它是配对的钥匙），没给就是空串 ——
            // **不编**一个（OpenAI 的老 `function_call` 也是这么处理的）。
            id: call.get("id").and_then(Value::as_str).unwrap_or("").to_string(),
            name: call.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
            input: call.get("args").cloned().unwrap_or_else(|| json!({})),
        });
    }
    if let Some(resp) = field(part, "functionResponse", "function_response") {
        return Some(IrPart::ToolResult {
            tool_use_id: resp.get("id").and_then(Value::as_str).unwrap_or("").to_string(),
            content: decode_function_response(resp.get("response")),
        });
    }
    // 认得出是 part、但 IR 表达不了（`executableCode` / `codeExecutionResult`…）：
    // 把**键名**记进损失，别用一个笼统的 "(unknown)" 让人查不出是什么。
    let keys: Vec<&str> = obj.keys().map(String::as_str).collect();
    let kind = if keys.is_empty() {
        "(空 part)".to_string()
    } else {
        format!("part({})", keys.join(","))
    };
    losses.push(Loss::UnsupportedPart { path: path.into(), kind });
    None
}

fn decode_inline_data(inline: &Value, path: &str, losses: &mut Vec<Loss>) -> Option<IrImage> {
    let media_type =
        field(inline, "mimeType", "mime_type").and_then(Value::as_str).unwrap_or("").to_string();
    let data = inline.get("data").and_then(Value::as_str).unwrap_or("").to_string();
    if media_type.is_empty() || data.is_empty() {
        losses.push(Loss::UnmappableImage {
            path: path.into(),
            detail: "inlineData 缺 mimeType 或 data".into(),
        });
        return None;
    }
    Some(IrImage::DataUrl { media_type, data })
}

fn decode_parts(parts: Option<&Value>, path: &str, losses: &mut Vec<Loss>) -> Vec<IrPart> {
    let Some(items) = parts.and_then(Value::as_array) else {
        return Vec::new(); // `parts` 缺失 = 这一段没有内容（不是"内容坏了"）
    };
    let mut out = Vec::with_capacity(items.len());
    for (i, part) in items.iter().enumerate() {
        if let Some(decoded) = decode_part(part, &format!("{path}/{i}"), losses) {
            out.push(decoded);
        }
    }
    out
}

/// `functionResponse.response`（JSON 对象）→ IR 内容块。与
/// `encode_function_response_content` **互逆**（见模块头 ③）。
///
/// 为什么要这层壳、而不是"能解析成 JSON 对象就原样送"（那样模型更好懂）：
/// 源请求里 `{"temp": 21}`（带空格）这种文本会被 serde 压成 `{"temp":21}`，往返核对于是
/// 判成"有损"⇒ 默认策略直接 415。**工具结果的原样**比"让模型少看一层转义"重要。
fn decode_function_response(response: Option<&Value>) -> Vec<IrPart> {
    let Some(value) = response else {
        return Vec::new(); // 上游没给 response：没有内容可搬（**不编**一个空对象）
    };
    let map = match value {
        Value::Object(map) => map,
        // 不是对象（Gemini 要求是对象）：原样当 JSON 文本搬，别丢
        other => return vec![text_part(json_string(other))],
    };
    if map.is_empty() {
        return Vec::new(); // 空对象 = 空结果（编码方向对空内容写的也是它）
    }
    if map.len() == 1 {
        match map.get("result") {
            Some(Value::String(s)) => return vec![text_part(s.clone())],
            Some(Value::Array(items)) => {
                // 多个文本块编成数组（编码方向就这么写），逐个还原 ⇒ 分块边界不丢
                return items
                    .iter()
                    .map(|item| match item {
                        Value::String(s) => text_part(s.clone()),
                        other => text_part(json_string(other)),
                    })
                    .collect();
            }
            _ => {}
        }
    }
    // 不是我们写的壳（Gemini 原生的对象）：原样以 JSON 文本搬，**不改写**内容
    vec![text_part(json_string(value))]
}

/// IR 工具结果 → `functionResponse.response` 的壳。与 `decode_function_response` 互逆。
fn encode_function_response_content(content: &[IrPart]) -> Value {
    let texts: Vec<&String> = content
        .iter()
        .filter_map(|p| match p {
            IrPart::Text(t) => Some(t),
            _ => None,
        })
        .collect();
    match texts.as_slice() {
        // 空结果 → 空对象（**不是** `{"result":""}`：那会凭空多出一个空字符串）
        [] => json!({}),
        [only] => json!({ "result": only }),
        many => json!({ "result": many }),
    }
}

/// tool_use_id → 函数名 的**配对表**，给 `functionResponse` 反查用。
///
/// # 为什么不是一张 `HashMap`
///
/// Gemini 自己发的 `functionCall` **常常没有 id**（`id` 是 Optional）。两套配对规则必须同时成立：
///
/// * **id 非空**：按 id 精确匹配（跨协议往返时 id 是可靠的钥匙，同名不同调也不会串）；
/// * **id 为空**：按**出现顺序**配对 —— 第 n 个空 id 的 `ToolResult` 配第 n 个空 id 的 `ToolUse`。
///   从前这里把所有空 id 都收敛到"第一个 name"，于是同一轮里的两个无 id 调用
///   （`weather` + `time`）会让**所有**工具结果都叫 `weather`：模型拿到的"查时间结果"
///   被标成"查天气结果"，工具循环直接答错，而且不报任何损失。
struct FunctionNames<'a> {
    /// 非空 id → 名字（精确匹配，后到的同 id 不覆盖先到的）。
    by_id: HashMap<&'a str, &'a str>,
    /// 空 id 的名字，**按出现顺序**（第 n 个空 id 调用）。
    anonymous: Vec<&'a str>,
}

impl<'a> FunctionNames<'a> {
    fn collect(messages: &'a [IrMessage]) -> Self {
        let mut by_id: HashMap<&str, &str> = HashMap::new();
        let mut anonymous: Vec<&str> = Vec::new();
        for msg in messages {
            for part in &msg.parts {
                if let IrPart::ToolUse { id, name, .. } = part {
                    if id.is_empty() {
                        anonymous.push(name.as_str());
                    } else {
                        by_id.entry(id.as_str()).or_insert(name.as_str());
                    }
                }
            }
        }
        Self { by_id, anonymous }
    }
}

/// 逐个工具结果解析出的函数名，**顺序与 IR 里 `ToolResult` 的出现顺序严格对应**。
///
/// `check_encodable`（"查得到吗"）与 `encode_request`（"写成什么名字"）**必须用同一份解析结果**：
/// 两处各写一遍序号逻辑，就会出现"检查说配得上、编码时配不上"（或反过来）——
/// 那种不一致只会在线上表现为一个安静的错名字，本地怎么测都是绿的。
fn tool_result_names(messages: &[IrMessage]) -> Vec<Option<&str>> {
    let names = FunctionNames::collect(messages);
    let mut anonymous_seen = 0usize;
    let mut out = Vec::new();
    for msg in messages {
        for part in &msg.parts {
            if let IrPart::ToolResult { tool_use_id, .. } = part {
                if tool_use_id.is_empty() {
                    out.push(names.anonymous.get(anonymous_seen).copied());
                    anonymous_seen += 1;
                } else {
                    out.push(names.by_id.get(tool_use_id.as_str()).copied());
                }
            }
        }
    }
    out
}

/// Gemini 的 `finishReason` → IR。
///
/// ★ 这是**开放取值**：`SAFETY` / `RECITATION` / `MALFORMED_FUNCTION_CALL` / `IMAGE_SAFETY` …
/// 会不断增加，而 IR 只有四个语义明确的取值 ⇒ 认不出的一律 [`IrStopReason::Other`]
/// **原样带着**。把它们塞进 `EndTurn`（"正常结束"）是最坏的一种"编"：客户端会以为
/// 模型把话说完了，而内容其实是被拦掉的。
fn stop_reason_of_gemini(reason: &str) -> IrStopReason {
    match reason {
        "STOP" => IrStopReason::EndTurn,
        "MAX_TOKENS" => IrStopReason::MaxTokens,
        other => IrStopReason::Other(other.to_string()),
    }
}

/// IR 停止原因 → Gemini 的 `finishReason`（三个刻意的选择）：
///
/// * `EndTurn` / `ToolUse` / `StopSequence` 都给 `STOP`：Gemini 的枚举里**没有** tool_use 与
///   stop_sequence 这两个取值（它发函数调用时给的就是 `STOP`，工具语义由 `functionCall`
///   这个 part 表达，不由 finishReason 表达）。
/// * `Other(raw)` **原样带走**（与解码同一口径）：Gemini 的枚举是封闭的，所以外来值
///   （OpenAI 的 `content_filter`）在严格客户端眼里可能不认识 —— 但改写成 `STOP` 就是把
///   "被拦"说成"说完了"，两害相权取如实。
/// * `None`（IR 没说为什么结束）**不走这里**：调用方干脆不写这个键，而不是写一个 `STOP`。
fn gemini_finish_reason(reason: &IrStopReason) -> Value {
    match reason {
        IrStopReason::EndTurn | IrStopReason::StopSequence | IrStopReason::ToolUse => json!("STOP"),
        IrStopReason::MaxTokens => json!("MAX_TOKENS"),
        IrStopReason::Other(raw) => json!(raw),
    }
}

fn decode_tool_choice(tc: &Value, losses: &mut Vec<Loss>) -> IrToolChoice {
    if let Some(map) = tc.as_object() {
        for (k, v) in map {
            if k == "functionCallingConfig" || k == "function_calling_config" || !has_meaning(v) {
                continue;
            }
            losses.push(Loss::UnsupportedField {
                path: format!("/toolConfig/{k}"),
                field: k.clone(),
                detail: "toolConfig 子字段，IR 里没有对应物（IR 只表达 functionCallingConfig）"
                    .into(),
            });
        }
    }
    let Some(config) = field(tc, "functionCallingConfig", "function_calling_config") else {
        // 没给"怎么选工具"就是缺省 AUTO（Gemini 文档把 AUTO 定为默认）—— **不编**别的
        return IrToolChoice::Auto;
    };
    if let Some(map) = config.as_object() {
        for (k, v) in map {
            if k == "mode" || k == "allowedFunctionNames" || k == "allowed_function_names" {
                continue;
            }
            if has_meaning(v) {
                losses.push(Loss::UnsupportedField {
                    path: format!("/toolConfig/functionCallingConfig/{k}"),
                    field: k.clone(),
                    detail: "functionCallingConfig 子字段，IR 里没有对应物".into(),
                });
            }
        }
    }
    let mode = config.get("mode").and_then(Value::as_str).unwrap_or("");
    let allowed: Vec<&str> = match field(config, "allowedFunctionNames", "allowed_function_names") {
        Some(Value::Array(items)) => items.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    };
    match mode {
        // AUTO 是缺省：`allowedFunctionNames` 在 AUTO 下是"白名单子集"，IR 表达不了那个子集
        "" | "AUTO" | "MODE_UNSPECIFIED" => {
            if !allowed.is_empty() {
                losses.push(Loss::UnsupportedField {
                    path: "/toolConfig/functionCallingConfig/allowedFunctionNames".into(),
                    field: "allowedFunctionNames".into(),
                    detail: format!("AUTO 模式下的函数白名单（{} 个）IR 表达不了", allowed.len()),
                });
            }
            IrToolChoice::Auto
        }
        "NONE" => {
            if !allowed.is_empty() {
                losses.push(Loss::UnsupportedField {
                    path: "/toolConfig/functionCallingConfig/allowedFunctionNames".into(),
                    field: "allowedFunctionNames".into(),
                    detail: "NONE 模式下还带函数白名单，IR 表达不了（按「不调工具」处理）".into(),
                });
            }
            IrToolChoice::None
        }
        "ANY" => match allowed.as_slice() {
            // ANY + 恰好一个名字 == "必须调这一个"（Gemini 文档：ANY 时模型必须调其中一个）
            [only] => IrToolChoice::Tool(only.to_string()),
            [] => IrToolChoice::Required,
            many => {
                losses.push(Loss::UnsupportedField {
                    path: "/toolConfig/functionCallingConfig/allowedFunctionNames".into(),
                    field: "allowedFunctionNames".into(),
                    detail: format!(
                        "ANY 模式指定了 {} 个名字（{}），IR 只有「任意」与「某一个」两种 ⇒ 按「任意」带走",
                        many.len(),
                        many.join(" / ")
                    ),
                });
                IrToolChoice::Required
            }
        },
        other => {
            losses.push(Loss::UnsupportedField {
                path: "/toolConfig/functionCallingConfig/mode".into(),
                field: "mode".into(),
                detail: format!("取值 `{other}` 没有 IR 表达（按缺省 AUTO 处理）"),
            });
            IrToolChoice::Auto
        }
    }
}

fn decode_system_instruction(value: Option<&Value>, losses: &mut Vec<Loss>) -> Vec<IrPart> {
    match value {
        None | Some(Value::Null) => Vec::new(),
        // 有的客户端（和 `trace_store` 一样）按 OpenAI 的习惯给一个字符串：字形不对但意图明确，
        // 当文本收（**不**登记损失 —— 内容一个字节都没丢）
        Some(Value::String(s)) => vec![text_part(s.clone())],
        Some(v) => decode_parts(v.get("parts"), "/systemInstruction/parts", losses),
    }
}

// ---------------------------------------------------------------- 请求：解码

impl GeminiAdapter {
    fn decode_content(&self, content: &Value, index: usize, out: &mut IrRequest) {
        let path = format!("/contents/{index}");
        let role = content.get("role").and_then(Value::as_str).unwrap_or("");
        let parts = decode_parts(content.get("parts"), &format!("{path}/parts"), &mut out.losses);
        match role {
            // `Content.role` 在请求里是**可选**的，缺省语义就是 user 侧内容 ⇒ 空串按 user 收
            "" | "user" => out.messages.push(IrMessage { role: IrRole::User, parts }),
            "model" => out.messages.push(IrMessage { role: IrRole::Assistant, parts }),
            // 有些 SDK 在回包时用 `function`：它承载的就是 user 侧的工具结果 ⇒ 折进 User，
            // 与 OpenAI 的 `role:"tool"` 折进 User 是**同一个归一化**（`ir.rs` 的规范形态）
            "function" => out.messages.push(IrMessage { role: IrRole::User, parts }),
            other => out.losses.push(Loss::UnsupportedRole {
                path: format!("{path}/role"),
                role: other.to_string(),
            }),
        }
    }
}

// ---------------------------------------------------------------- 请求：编码辅助

/// 一个 IR part → Gemini 的 `Part`。`None` = 这个 part 在 Gemini 里没有形态
/// （例如图片 URL）：编码侧**不替它编**一个形状，损失早在 `check_encodable` 登记过了。
///
/// `names` 是 [`tool_result_names`] 的结果：**按工具结果的下标**取名字
/// （不是按 id 现查 —— 空 id 要按出现顺序配，见 [`FunctionNames`]）。
fn encode_part(part: &IrPart, names: &[Option<&str>], result_index: &mut usize) -> Option<Value> {
    match part {
        IrPart::Text(t) => Some(json!({ "text": t })),
        IrPart::Image(IrImage::DataUrl { media_type, data }) => Some(object(vec![(
            "inlineData",
            object(vec![("mimeType", json!(media_type)), ("data", json!(data))]),
        )])),
        // URL 形态：Gemini 只能内联（`fileData.fileUri` 只认 Files API / GCS 的 URI）
        IrPart::Image(IrImage::Url(_)) => None,
        IrPart::ToolUse { id, name, input } => {
            let id = if id.is_empty() { Value::Null } else { json!(id) };
            Some(object(vec![(
                "functionCall",
                object(vec![("id", id), ("name", json!(name)), ("args", input.clone())]),
            )]))
        }
        IrPart::ToolResult { tool_use_id, content } => {
            let name = names.get(*result_index).copied().flatten();
            *result_index += 1;
            Some(encode_function_response(tool_use_id, content, name))
        }
    }
}

/// `functionResponse` 一个 part。`name` 查不到时**不写这个键**（损失已在 `check_encodable`
/// 登记成致命项）：宁可让上游明确报"缺 name"，也不编一个看起来对的名字。
fn encode_function_response(tool_use_id: &str, content: &[IrPart], name: Option<&str>) -> Value {
    let id = if tool_use_id.is_empty() { Value::Null } else { json!(tool_use_id) };
    object(vec![(
        "functionResponse",
        object(vec![
            ("id", id),
            ("name", name.map(|n| json!(n)).unwrap_or(Value::Null)),
            ("response", encode_function_response_content(content)),
        ]),
    )])
}

// ================================================================ ProtocolAdapter

impl super::ProtocolAdapter for GeminiAdapter {
    fn protocol(&self) -> ta_core::types::Protocol {
        ta_core::types::Protocol::Gemini
    }

    /// **Gemini 的 `stream` 不在 body 里，在路径里**
    /// （`…:generateContent` vs `…:streamGenerateContent?alt=sse`）。
    ///
    /// 不声明这一条，编码方向会永远报一条**假**的 `RoundTrip /stream` 损失 ——
    /// 而实际上什么都没丢，只是它走的不是 body。假损失比没有损失更坏：
    /// 它会把**每一条**流式请求在默认 `on_loss: reject` 下打回。
    /// （路径那一侧由 `ingress::upstream_path_for` 按 `stream` 拼。）
    fn stream_is_out_of_band(&self) -> bool {
        true
    }

    /// Gemini 的硬约束（在编码前跑，基本都是致命损失）：
    ///
    /// * `systemInstruction` **只收文本**（图片/工具块无处安放）；
    /// * 图片只能 `inlineData`，且 media type 在白名单里（http(s) URL 没有对应字段）；
    /// * `functionResponse` **必须带 name**，而 IR 的 `ToolResult` 只有 id ⇒ 这里先验"查得到"；
    /// * 工具结果的内容会被压成一个 JSON 对象（`{"result": …}`）⇒ 图片/嵌套块装不进去。
    ///
    /// **不查**工具 id：Gemini 的配对靠 name，空 id 对它是合法的
    /// （与 OpenAI / Anthropic 那两侧的 `check_tool_ids` 恰好相反）。
    fn check_encodable(&self, ir: &IrRequest) -> Vec<Loss> {
        let mut losses = Vec::new();
        for (i, part) in ir.system.iter().enumerate() {
            match part {
                IrPart::Text(_) => {}
                IrPart::Image(img) => losses.push(Loss::UnmappableImage {
                    path: format!("/system/{i}"),
                    detail: format!(
                        "Gemini 的 systemInstruction 目前只收文本，图片（{}）无处安放",
                        describe_image(img)
                    ),
                }),
                other => losses.push(Loss::UnsupportedPart {
                    path: format!("/system/{i}"),
                    kind: part_kind(other).to_string(),
                }),
            }
        }
        let names = tool_result_names(&ir.messages);
        let mut result_index = 0usize;
        for (i, msg) in ir.messages.iter().enumerate() {
            let path = format!("/messages/{i}/parts");
            check_parts(&msg.parts, &path, false, &mut losses);
            for (j, part) in msg.parts.iter().enumerate() {
                if let IrPart::ToolResult { tool_use_id, .. } = part {
                    // 名字查不到 ⇒ 致命缺失（**不编**一个名字出来）。
                    // 用与 `encode_request` 同一个解析结果：两边各数一遍序号必然分叉。
                    let position = result_index;
                    result_index += 1;
                    if names.get(position).copied().flatten().is_none() {
                        losses.push(Loss::MissingRequired {
                            path: format!("{path}/{j}"),
                            detail: format!(
                                "Gemini 的 functionResponse 必须带 name，而这条工具结果（id=`{tool_use_id}`）\
                                 在会话里找不到对应的 functionCall（缺 id 的老形状，或这一轮里\
                                 空 id 的调用比结果少）"
                            ),
                        });
                    }
                }
            }
        }
        // `stopSequences` 的**协议上限是 5 个**：多了上游直接 400，而且它不会告诉我们
        // 是哪一条多余的 ⇒ 必须在编码前拒（`Loss::UnsupportedField`）。
        if ir.stop.len() > MAX_STOP_SEQUENCES {
            losses.push(Loss::UnsupportedField {
                path: "/stop".into(),
                field: "stopSequences".into(),
                detail: format!(
                    "Gemini 最多接受 {MAX_STOP_SEQUENCES} 条停止序列，源请求给了 {} 条（多出来的\
                     没有表达方式，也不会替你丢掉再发）",
                    ir.stop.len()
                ),
            });
        }
        losses
    }

    fn decode_request(&self, body: &Value) -> IrRequest {
        let mut ir = IrRequest {
            model: body.get("model").and_then(Value::as_str).map(str::to_string),
            // 流式本来在 URL 方法上（模块头 ②）：body 里带了才认，否则由看得见路径的那一层写进 IR
            stream: body.get("stream").and_then(Value::as_bool).unwrap_or(false),
            ..Default::default()
        };
        ir.system = decode_system_instruction(
            field(body, "systemInstruction", "system_instruction"),
            &mut ir.losses,
        );
        if let Some(contents) = body.get("contents").and_then(Value::as_array) {
            for (i, content) in contents.iter().enumerate() {
                self.decode_content(content, i, &mut ir);
            }
        }
        if let Some(config) = field(body, "generationConfig", "generation_config") {
            ir.max_tokens = u64_of(config, &["maxOutputTokens", "max_output_tokens"]);
            ir.temperature = f64_of(config, "temperature");
            ir.top_p = f64_of(config, "topP").or_else(|| f64_of(config, "top_p"));
            ir.stop = strings_of(field(config, "stopSequences", "stop_sequences"));
            if let Some(map) = config.as_object() {
                for (k, v) in map {
                    if !has_meaning(v) || KNOWN_GENERATION_CONFIG.contains(&k.as_str()) {
                        continue;
                    }
                    ir.losses.push(Loss::UnsupportedField {
                        path: format!("/generationConfig/{k}"),
                        field: k.clone(),
                        detail: "采样/生成参数，IR 里没有对应物（丢弃前留痕）".into(),
                    });
                }
            }
        }
        if let Some(tools) = body.get("tools").and_then(Value::as_array) {
            for (i, tool) in tools.iter().enumerate() {
                let path = format!("/tools/{i}");
                let Some(decls) = field(tool, "functionDeclarations", "function_declarations")
                    .and_then(Value::as_array)
                else {
                    // 服务端工具（`googleSearch` / `codeExecution` / `urlContext` / `computerUse`）：
                    // 那是**上游自己执行**的工具，不是客户端要实现的函数 ⇒ IR 里没有对应物
                    let keys: Vec<&str> = match tool.as_object() {
                        Some(m) => m.keys().map(String::as_str).collect(),
                        None => Vec::new(),
                    };
                    ir.losses.push(Loss::UnsupportedField {
                        path,
                        field: "tools[]".into(),
                        detail: format!(
                            "工具类型 `{}` 没有 IR 表达（IR 只表达客户端的函数声明）",
                            keys.join(",")
                        ),
                    });
                    continue;
                };
                for (j, decl) in decls.iter().enumerate() {
                    ir.tools.push(IrTool {
                        name: decl.get("name").and_then(Value::as_str).unwrap_or("").into(),
                        description: decl
                            .get("description")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        parameters: decl.get("parameters").cloned().unwrap_or_else(|| json!({})),
                    });
                    if let Some(map) = decl.as_object() {
                        for (k, v) in map {
                            if matches!(k.as_str(), "name" | "description" | "parameters")
                                || !has_meaning(v)
                            {
                                continue;
                            }
                            ir.losses.push(Loss::UnsupportedField {
                                path: format!("{path}/functionDeclarations/{j}/{k}"),
                                field: k.clone(),
                                detail: "函数声明里的字段，IR 里没有对应物".into(),
                            });
                        }
                    }
                }
            }
        }
        if let Some(tc) = field(body, "toolConfig", "tool_config") {
            ir.tool_choice = decode_tool_choice(tc, &mut ir.losses);
        }
        // 其余非空顶层字段：IR 表达不了 ⇒ **登记**（默认 `on_loss: reject` 会拒发）
        if let Some(map) = body.as_object() {
            for (k, v) in map {
                if !has_meaning(v) || KNOWN_TOP_LEVEL.contains(&k.as_str()) {
                    continue;
                }
                ir.losses.push(Loss::UnsupportedField {
                    path: format!("/{k}"),
                    field: k.clone(),
                    detail: "Gemini 协议字段，目标协议没有对应物".into(),
                });
            }
        }
        ir.messages = canonicalize(ir.messages);
        ir
    }

    fn encode_request(&self, ir: &IrRequest, _upstream_model: &str) -> Value {
        // ★ `_upstream_model` **有意不用**：Gemini 的模型名在 URL 路径里，body 里塞 `model`
        //   会被上游按未知字段拒掉（模块头 ②）。同理不写 `stream`。
        //   名字表与 `check_encodable` 用**同一个** `tool_result_names`（见它的说明）。
        let names = tool_result_names(&ir.messages);
        let mut result_index = 0usize;
        let mut contents: Vec<Value> = Vec::new();
        for msg in &ir.messages {
            let role = match msg.role {
                IrRole::User => "user",
                // assistant → `model`：Gemini 里没有 assistant 这个角色
                IrRole::Assistant => "model",
            };
            let parts: Vec<Value> = msg
                .parts
                .iter()
                .filter_map(|p| encode_part(p, &names, &mut result_index))
                .collect();
            if parts.is_empty() {
                continue; // 空消息不产生占位（与 `canonicalize` 同一口径）
            }
            contents.push(object(vec![("role", json!(role)), ("parts", Value::Array(parts))]));
        }

        let system = if ir.system.is_empty() {
            Value::Null
        } else {
            // 只收文本（非文本块已在 `check_encodable` 登记），一个文本一个 part ⇒ 往返逐块相等
            let parts: Vec<Value> = ir
                .system
                .iter()
                .filter_map(|p| match p {
                    IrPart::Text(t) => Some(json!({ "text": t })),
                    _ => None,
                })
                .collect();
            json!({ "parts": parts })
        };

        let mut generation: Vec<(&str, Value)> = Vec::new();
        if let Some(n) = ir.max_tokens {
            generation.push(("maxOutputTokens", json!(n)));
        }
        if let Some(v) = ir.temperature {
            generation.push(("temperature", json!(v)));
        }
        if let Some(v) = ir.top_p {
            generation.push(("topP", json!(v)));
        }
        if !ir.stop.is_empty() {
            generation
                .push(("stopSequences", Value::Array(ir.stop.iter().map(|s| json!(s)).collect())));
        }
        let generation_config =
            if generation.is_empty() { Value::Null } else { object(generation) };

        let tools = if ir.tools.is_empty() {
            Value::Null
        } else {
            let decls: Vec<Value> = ir
                .tools
                .iter()
                .map(|t| {
                    object(vec![
                        ("name", json!(t.name)),
                        ("description", json!(t.description)),
                        ("parameters", t.parameters.clone()),
                    ])
                })
                .collect();
            // Gemini 把**所有**函数声明装在一个 tool 里（不是 OpenAI 那样一个函数一个 tool）
            json!([{ "functionDeclarations": decls }])
        };

        // Auto 时**不写** toolConfig：AUTO 就是缺省值，写出来只是噪声（解码侧也把"没有"当 Auto）
        let tool_config = match &ir.tool_choice {
            IrToolChoice::Auto => Value::Null,
            IrToolChoice::Required => json!({ "functionCallingConfig": { "mode": "ANY" } }),
            IrToolChoice::None => json!({ "functionCallingConfig": { "mode": "NONE" } }),
            IrToolChoice::Tool(name) => json!({
                "functionCallingConfig": { "mode": "ANY", "allowedFunctionNames": [name] },
            }),
        };

        object(vec![
            ("contents", Value::Array(contents)),
            ("systemInstruction", system),
            ("generationConfig", generation_config),
            ("tools", tools),
            ("toolConfig", tool_config),
        ])
    }

    fn decode_response(&self, body: &Value) -> IrResponse {
        let mut ir = IrResponse {
            // OpenAI 的 `id` / `model` 在 Gemini 里叫 `responseId` / `modelVersion`（同一个东西的另一个名字）
            id: body.get("responseId").and_then(Value::as_str).map(str::to_string),
            model: body
                .get("modelVersion")
                .and_then(Value::as_str)
                .or_else(|| body.get("model").and_then(Value::as_str))
                .map(str::to_string),
            // 第 330 轮：非流式响应也要带 usage（Gemini 叫 `usageMetadata`）
            usage: ir_usage_from_gemini(body.get("usageMetadata")),
            ..Default::default()
        };
        // prompt 级拦截：上游明确说了"这次没有内容" ⇒ **不能**当成一个正常响应交出去
        // （客户端会看到一个合法的空回复，那种失败用户永远发现不了）。
        // IR 里没有"prompt 被拦"这个维度 ⇒ 登记损失，并且**不编**候选内容。
        if let Some(reason) = body.pointer("/promptFeedback/blockReason").and_then(Value::as_str) {
            ir.losses.push(Loss::UnsupportedPart {
                path: "/promptFeedback".into(),
                kind: format!("promptFeedback.blockReason={reason}（prompt 被拦，没有候选内容）"),
            });
        }
        // `usageMetadata` 有意**不**在这里登记损失：`IrResponse` 根本没有 usage 字段
        // （`openai.rs` / `anthropic.rs` 同样不搬），账本读的是原始 body（`meter.rs`）；
        // 每个响应都记一条"usage 没进 IR"只会把真损失淹掉（模块头 ⑤）。
        let Some(candidates) = body.get("candidates").and_then(Value::as_array) else {
            ir.losses.push(Loss::UnsupportedPart {
                path: "/candidates".into(),
                kind: "candidates 缺失（没有候选内容）".into(),
            });
            return ir;
        };
        if candidates.is_empty() {
            ir.losses.push(Loss::UnsupportedPart {
                path: "/candidates".into(),
                kind: "candidates 为空（没有候选内容）".into(),
            });
            return ir;
        }
        if candidates.len() > 1 {
            // IR 只表达一个候选：多出来的候选内容会消失 ⇒ 必须留痕（不是"挑一个"就完事）
            ir.losses.push(Loss::UnsupportedField {
                path: "/candidates".into(),
                field: "candidateCount".into(),
                detail: format!(
                    "上游回了 {} 个候选，IR 只表达一个（只搬第一个）",
                    candidates.len()
                ),
            });
        }
        let candidate = &candidates[0];
        ir.stop_reason = field(candidate, "finishReason", "finish_reason")
            .and_then(Value::as_str)
            .map(stop_reason_of_gemini);
        if let Some(parts) = candidate.pointer("/content/parts") {
            ir.parts = decode_parts(Some(parts), "/candidates/0/content/parts", &mut ir.losses);
        }
        // 候选级元信息（`safetyRatings` / `citationMetadata` / `groundingMetadata` / `logprobsResult`…）：
        // IR 表达不了 ⇒ **逐项**登记，绝不静默吞（同一条纪律）
        if let Some(map) = candidate.as_object() {
            for (k, v) in map {
                if !has_meaning(v) || KNOWN_CANDIDATE.contains(&k.as_str()) {
                    continue;
                }
                ir.losses.push(Loss::UnsupportedField {
                    path: format!("/candidates/0/{k}"),
                    field: k.clone(),
                    detail: "Gemini 候选级字段，IR 里没有对应物".into(),
                });
            }
        }
        ir
    }

    fn encode_response(&self, ir: &IrResponse) -> (Value, Vec<Loss>) {
        let mut losses = Vec::new();
        // 响应里的 `functionResponse` 也要 name：只能在**这一份响应**里找同 id 的 `functionCall`
        let names: HashMap<&str, &str> = ir
            .parts
            .iter()
            .filter_map(|p| match p {
                IrPart::ToolUse { id, name, .. } => Some((id.as_str(), name.as_str())),
                _ => None,
            })
            .collect();
        let mut parts: Vec<Value> = Vec::new();
        for (i, part) in ir.parts.iter().enumerate() {
            let path = format!("/candidates/0/content/parts/{i}");
            match part {
                IrPart::Text(t) => parts.push(json!({ "text": t })),
                IrPart::Image(IrImage::DataUrl { media_type, data }) => parts.push(object(vec![(
                    "inlineData",
                    object(vec![("mimeType", json!(media_type)), ("data", json!(data))]),
                )])),
                IrPart::Image(img) => losses.push(Loss::UnmappableImage {
                    path,
                    detail: format!(
                        "Gemini 只能内联图片（inlineData），而这是 {}",
                        describe_image(img)
                    ),
                }),
                IrPart::ToolUse { id, name, input } => {
                    let id = if id.is_empty() { Value::Null } else { json!(id) };
                    parts.push(object(vec![(
                        "functionCall",
                        object(vec![("id", id), ("name", json!(name)), ("args", input.clone())]),
                    )]));
                }
                IrPart::ToolResult { tool_use_id, content } => {
                    let name = names.get(tool_use_id.as_str()).copied();
                    if name.is_none() {
                        // 响应里出现工具结果本身就反常（工具结果是**请求**侧的东西）：
                        // name 反查不到就**不编**，只留痕（`encode_response` 不拒绝，见 `adapt/mod.rs`）
                        losses.push(Loss::MissingRequired {
                            path,
                            detail: format!(
                                "响应里的工具结果（id=`{tool_use_id}`）找不到对应的 functionCall，\
                                 Gemini 的 functionResponse 必须带 name"
                            ),
                        });
                    }
                    parts.push(encode_function_response(tool_use_id, content, name));
                }
            }
        }
        let finish = match &ir.stop_reason {
            // `None` ⇒ 这个键**不写**（`object()` 丢掉 null）：IR 没说为什么结束，不编 STOP
            None => Value::Null,
            Some(reason) => gemini_finish_reason(reason),
        };
        let candidate = object(vec![
            ("content", json!({ "role": "model", "parts": parts })),
            ("finishReason", finish),
            ("index", json!(0)),
        ]);
        (
            object(vec![
                ("candidates", json!([candidate])),
                ("modelVersion", ir.model.as_ref().map(|m| json!(m)).unwrap_or(Value::Null)),
                ("responseId", ir.id.as_ref().map(|id| json!(id)).unwrap_or(Value::Null)),
                // 第 330 轮：把 IR 里的 usage 写回去（Gemini 叫 `usageMetadata`）
                ("usageMetadata", ir.usage.as_ref().map(gemini_usage).unwrap_or(Value::Null)),
            ]),
            losses,
        )
    }
}

/// 逐块检查：图片形态、工具结果里装不下的东西。
///
/// `in_tool_result` 是**关键的一位**：同一个 `Text` 在正文里合法，在 `functionResponse`
/// 的 JSON 对象里也合法；但图片/嵌套工具块在正文里有位置，在那个 JSON 对象里没有。
fn check_parts(parts: &[IrPart], path: &str, in_tool_result: bool, losses: &mut Vec<Loss>) {
    for (i, part) in parts.iter().enumerate() {
        let ipath = format!("{path}/{i}");
        match part {
            IrPart::Text(_) => {}
            IrPart::Image(img) => match img {
                IrImage::DataUrl { media_type, .. }
                    if !IMAGE_MEDIA_TYPES.iter().any(|a| a.eq_ignore_ascii_case(media_type)) =>
                {
                    losses.push(Loss::UnmappableImage {
                        path: ipath,
                        detail: format!(
                            "图片格式 {media_type} Gemini 不接受（可用：{}）",
                            IMAGE_MEDIA_TYPES.join(" / ")
                        ),
                    })
                }
                IrImage::DataUrl { .. } if in_tool_result => losses.push(Loss::UnmappableImage {
                    path: ipath,
                    detail: "Gemini 的 functionResponse 是一个 JSON 对象，装不下图片".into(),
                }),
                IrImage::DataUrl { .. } => {}
                IrImage::Url(_) => losses.push(Loss::UnmappableImage {
                    path: ipath,
                    detail: format!(
                        "Gemini 只能内联图片（inlineData），而这是 {}：\
                         fileData.fileUri 只认 Files API / GCS 的 URI，不是普通的图片 URL",
                        describe_image(img)
                    ),
                }),
            },
            IrPart::ToolUse { .. } if in_tool_result => losses.push(Loss::UnsupportedPart {
                path: ipath,
                kind: "tool_use（嵌在工具结果里的工具调用）".into(),
            }),
            IrPart::ToolUse { .. } => {}
            IrPart::ToolResult { content, .. } if in_tool_result => {
                losses.push(Loss::UnsupportedPart {
                    path: ipath.clone(),
                    kind: "tool_result（嵌在工具结果里的工具结果）".into(),
                });
                check_parts(content, &format!("{ipath}/content"), true, losses);
            }
            IrPart::ToolResult { content, .. } => {
                check_parts(content, &format!("{ipath}/content"), true, losses)
            }
        }
    }
}

// ================================================================ 流式（SSE 事件）

/// Gemini 流式的几个与别家不同的地方，都在下面：
///
/// * 收尾靠**最后一个带 `finishReason` 的 chunk**（没有 `data: [DONE]`）；
/// * `usageMetadata` 是**累积**的（见 `ir_usage_from_gemini`）；
/// * **一帧可以产出多个事件**（第 328 轮）：收尾帧常常是
///   "最后一块文字 + `finishReason` + `usageMetadata`" 挤在一起 ⇒ 解出 `[TextDelta, Stop]`。
impl StreamAdapter for GeminiAdapter {
    fn decode_stream_event(&self, data: &Value) -> Vec<IrStreamEvent> {
        if !data.is_object() {
            // 不是 GenerateContentResponse（非对象载荷同样落到这里）→ 连事件都不产出
            return Vec::new();
        }
        // **根本不是 Gemini 的帧**（别家的报文被喂到这里）：它连 Gemini 的三个顶层字段
        // 都没有。回空 `Vec`（"没有事件"），而不是 `Other`（"认得出是协议帧、但表达不了"）
        // —— 后者在这条路上是句谎话。
        //
        // 生产上不会发生（上游协议由配置决定），但把这两种情况分开能让
        // `Other` 的语义保持精确：它永远等于"这**是**一帧，只是搬不走"。
        let has_gemini_shape = data.get("candidates").is_some()
            || data.get("usageMetadata").is_some()
            || data.get("promptFeedback").is_some();
        if !has_gemini_shape {
            return Vec::new();
        }
        // ★ 累积语义：每一帧给的都是"到目前为止"的总量（后面的帧覆盖前面的），
        //   所以逐帧原样带出、由调用方用 [`IrUsage::merge`] 覆盖合并（"覆盖"恰好对上上游的语义）。
        //   两件都不能做：不能"先到为准"（那只记到第一帧的零头 → 少记账），
        //   也不能因为后面某一帧没带 usage 就把已有值抹掉（`merge` 只覆盖自己带了的字段）。
        let usage = ir_usage_from_gemini(data.get("usageMetadata"));
        let candidate =
            data.get("candidates").and_then(Value::as_array).and_then(|list| list.first());
        let Some(candidate) = candidate else {
            // prompt 级拦截：没有候选内容、也没有 finishReason。IR 事件里没有"prompt 被拦"
            // 这个维度 ⇒ [`IrStreamEvent::Other`]（认得出、表达不了），由调用方决定丢还是转。
            // **不**折成一个 Stop：那等于替上游说"模型正常结束了"。
            if data.pointer("/promptFeedback/blockReason").is_some() {
                return vec![IrStreamEvent::Other];
            }
            // 只有用量的帧：`reason: None` = "这是补充信息，不是终止判定"（与 OpenAI 的
            // `include_usage` 末帧同一处理）。既没有候选、又没有用量 ⇒ 认得出是协议帧但没东西可搬
            // ⇒ [`IrStreamEvent::Other`]（不是空 Vec：那是"连协议帧都不是"）
            return match usage {
                Some(usage) => vec![IrStreamEvent::Stop { reason: None, usage: Some(usage) }],
                None => vec![IrStreamEvent::Other],
            };
        };
        let mut events: Vec<IrStreamEvent> = Vec::new();
        // ★ 一帧里的**所有** part 都要搬：多个 text part 逐个成 `TextDelta`，
        //   `functionCall` 各自成 `ToolUseStart`。从前只有"第一个能搬的 part"活着。
        if let Some(parts) = candidate.pointer("/content/parts").and_then(Value::as_array) {
            let mut tool_calls = 0u32;
            for (i, part) in parts.iter().enumerate() {
                // 事件层**没有**损失通道（`IrStreamEvent` 里没有损失），所以这里的登记
                // 注定丢掉：思维链/未知 part 在流式路径上留不下痕 —— 这是 `IrStreamEvent`
                // 的既有缺口（会话层有原始帧，真要留痕是它的活），不是这里的取舍。
                let mut discarded = Vec::new();
                let path = format!("/candidates/0/content/parts/{i}");
                match decode_part(part, &path, &mut discarded) {
                    Some(IrPart::Text(text)) => {
                        events.push(IrStreamEvent::TextDelta { slot: Slot::Text(0), text })
                    }
                    // ★ **`functionCall.args` 是完整对象**，而 IR 的"调用开始"现在带得动它
                    //   （第 330 轮加的 `input`）—— 从前这一格不存在，
                    //   于是 Gemini 上游的工具入参**整份丢掉**。
                    //   槽位只能是"这一帧里的第几个 functionCall"：part 没有序号，
                    //   适配器又是无状态的（跨帧的相认需要每请求状态，会话层才有）。
                    Some(IrPart::ToolUse { id, name, input }) => {
                        events.push(IrStreamEvent::ToolUseStart {
                            slot: Slot::Tool(tool_calls),
                            id,
                            name,
                            // Gemini 给的**就是**完整入参（哪怕 `{}` 也是"参数为空"而不是"还没到"）
                            input: Some(input),
                        });
                        tool_calls += 1;
                    }
                    // 图片 / 工具结果在**响应流**里没有对应事件（Gemini 的流只流文本与函数调用）
                    Some(_) | None => {}
                }
            }
        }
        // 收尾帧：Gemini 用**最后一个带 finishReason 的 chunk**收尾（没有 `data: [DONE]`）。
        // ★ 与内容**同帧**时两个事件都要返回（顺序：先内容、后 Stop）—— 这正是
        // 第 328 轮把签名从 `Option` 改成 `Vec` 的原因。
        if let Some(reason) =
            field(candidate, "finishReason", "finish_reason").and_then(Value::as_str)
        {
            events.push(IrStreamEvent::Stop {
                reason: Some(stop_reason_of_gemini(reason)),
                usage: usage.clone(),
            });
            return events;
        }
        if !events.is_empty() {
            return events;
        }
        if let Some(usage) = usage {
            return vec![IrStreamEvent::Stop { reason: None, usage: Some(usage) }];
        }
        // 走到这里：对象形状、有 `candidates`、但里面没有 IR 能表达的东西
        //（空 parts、只有 `safetyRatings`、心跳…）⇒ 认得出是 Gemini 帧、却无内容可搬。
        // `Other`（而不是空 `Vec`）：空 `Vec` 留给"**根本不是** GenerateContentResponse"
        // —— 那两种情况在下面那个分支里已经分开了，判据是"有没有 Gemini 的顶层字段"。
        let _ = &events;
        vec![IrStreamEvent::Other]
    }

    fn encode_stream_event(&self, ev: &IrStreamEvent, ctx: &mut EncodeCtx) -> Vec<StreamFrame> {
        match ev {
            // Gemini 的流**没有"开始"帧**（也没有 `data: [DONE]`）：`id` / `model` 在 SSE 帧里
            // 没有可放的位置 ⇒ **不造一帧**。用法写在 [`EncodeCtx::remember_upstream`]：
            // 跨协议时身份由 `ctx` 记着，别的协议（Anthropic 的 `message_start`）要用。
            IrStreamEvent::Start { .. } => Vec::new(),
            IrStreamEvent::TextDelta { text, .. } => {
                vec![StreamFrame::Json(gemini_candidate_part(json!({ "text": text })))]
            }
            IrStreamEvent::ToolUseStart { slot: _, id, name, input } => {
                let id = if id.is_empty() { Value::Null } else { json!(id) };
                // 源协议如果一次给全了入参（Gemini↔Gemini、或别的协议把整份 JSON 放在
                // `ToolUseStart.input` 里），就写进 `args` —— **不丢**。
                // 只有"入参靠分片"的源协议（OpenAI/Anthropic）才是空对象 + 后面无分片可发
                //（Gemini 没有分片形态，那一路由 `ToolInputDelta` 记损失）。
                let args = input.clone().unwrap_or_else(|| json!({}));
                vec![StreamFrame::Json(gemini_candidate_part(object(vec![(
                    "functionCall",
                    object(vec![("id", id), ("name", json!(name)), ("args", args)]),
                )])))]
            }
            // ★ Gemini 的 `functionCall.args` 是**完整 JSON 对象**：协议里没有"入参分片"这种形态。
            //   把片段塞进 `args` 就是编一个上游不会发的形状（客户端的 JSON 解析器会拿到半个对象）
            //   ⇒ 什么都不发。
            //
            //   ★ 但"什么都不发"不等于"当没发生"：从前这一层**没有损失通道**，于是
            //     "跨协议流式把工具入参整段丢了"是静默发生的 —— 客户端只会看到一个
            //     `args: {}` 的空工具调用，查不出是网关丢的。现在登记一条，
            //     由会话层写进日志（[`EncodeCtx::take_notes`]）。
            //   真正支持得让会话层按目标协议缓冲合并（模块头"已知缺口"）。
            IrStreamEvent::ToolInputDelta { slot: _, fragment } => {
                ctx.note(Loss::UnsupportedPart {
                    path: "/candidates/0/content/parts".into(),
                    kind: format!(
                        "流式工具入参分片在 Gemini 里没有形态（`args` 是完整对象），已丢弃：{fragment}"
                    ),
                });
                Vec::new()
            }
            IrStreamEvent::Stop { reason, usage } => {
                if let Some(reason) = reason {
                    // 一次流只有**一个**终止帧：finishReason 与（累积的）用量同帧发出去 ——
                    // Gemini 的形状允许两者同帧，而它**没有**额外的 `[DONE]` 要补。
                    let candidate = object(vec![
                        ("content", json!({ "role": "model", "parts": [] })),
                        ("finishReason", gemini_finish_reason(reason)),
                        ("index", json!(0)),
                    ]);
                    let with_usage = usage.as_ref().map(gemini_usage).unwrap_or(Value::Null);
                    return vec![StreamFrame::Json(object(vec![
                        ("candidates", json!([candidate])),
                        ("usageMetadata", with_usage),
                    ]))];
                }
                // `reason: None` = "这是流结尾的补充信息（用量），不是终止判定" ⇒ 只发用量，
                // **不编** finishReason（编了这一帧就会被客户端当成收尾，后面的内容全丢）
                match usage {
                    Some(usage) => {
                        vec![StreamFrame::Json(json!({ "usageMetadata": gemini_usage(usage) }))]
                    }
                    None => Vec::new(),
                }
            }
            // 没有对应物：**什么都不发**，而不是发一个假的心跳 / 空候选
            IrStreamEvent::Other => Vec::new(),
        }
    }

    /// ★ **空**：Gemini 的流就是"最后一帧带 `finishReason`"，它**没有任何收尾标记**
    /// （没有 `data: [DONE]`，也没有 `message_stop` 那类东西）。
    ///
    /// 于是这里**必须什么都不补**：补一个空候选帧会让客户端以为上游又发了一帧；
    /// 补一个 `[DONE]` 更糟 —— 那不是 Gemini 的线格式，严格客户端会把它当成坏数据。
    /// 收尾语义由 `Stop { reason: Some(_) }`（带 `finishReason` 的那一帧）承担。
    fn close_stream(&self, ctx: &mut EncodeCtx) -> Vec<StreamFrame> {
        let _ = ctx;
        Vec::new()
    }

    /// Gemini 没有"流开始"这种东西：**空**。
    fn begin_frames(&self, _ctx: &mut EncodeCtx) -> Vec<StreamFrame> {
        Vec::new()
    }

    /// Gemini 的 SSE 帧把身份放在 `responseId` / `modelVersion` 里（顶层**没有**
    /// `id` / `model`）。不覆写这个方法，跨协议流式合成别的协议的开头帧时
    /// 模型名会永远是 `null` —— 上游明明每帧都在说。
    fn frame_identity(&self, data: &Value) -> (Option<String>, Option<String>) {
        (
            field(data, "responseId", "response_id").and_then(Value::as_str).map(str::to_string),
            field(data, "modelVersion", "model_version")
                .and_then(Value::as_str)
                .map(str::to_string),
        )
    }

    /// **解码侧的内容损失**：思维链（`thought: true`）与认不出的 part。
    ///
    /// 这两样是**内容**被丢掉，不是心跳 —— 不留痕就是静默丢数据（口径）。
    /// 从前 `decode_part` 已经把损失记进了一个局部 vec，而那个 vec 在返回时被丢了；
    /// 通道补上之后它才真的有出口。
    ///
    /// **候选级元信息不记**（`citationMetadata` / `groundingMetadata` / `safetyRatings` /
    /// `logprobsResult`）：那是元信息，丢了不损失内容。记了就是噪音，
    /// 而噪音会把真损失淹没 —— 留痕是为了**可行动**，不是为了好看。
    fn decode_losses(&self, data: &Value) -> Vec<Loss> {
        let mut notes = Vec::new();
        let parts = data
            .get("candidates")
            .and_then(Value::as_array)
            .and_then(|list| list.first())
            .and_then(|candidate| candidate.pointer("/content/parts"))
            .and_then(Value::as_array);
        if let Some(parts) = parts {
            for (i, part) in parts.iter().enumerate() {
                let path = format!("/candidates/0/content/parts/{i}");
                // 用**同一个**解析器算损失：两处各写一遍判断，就会出现"解码说丢了这个、
                // 损失说丢了那个" —— 那种不一致只在线上表现为**安静地少内容**，
                // 本地怎么测都是绿的。
                let _ = decode_part(part, &path, &mut notes);
            }
        }
        notes
    }
}

/// 一个增量帧的骨架：`candidates[0].content.parts[]`。
/// `index` 显式给 0：单候选流里它恒为 0，Gemini 自己也发它。
fn gemini_candidate_part(part: Value) -> Value {
    json!({ "candidates": [{ "content": { "role": "model", "parts": [part] }, "index": 0 }] })
}

/// Gemini 的 `usageMetadata` → [`IrUsage`]。
///
/// 有 IR 位置的是 `promptTokenCount` / `candidatesTokenCount` / `cachedContentTokenCount` 三个；
/// `totalTokenCount` **故意不搬**：IR 没有它的槽位，而它 = prompt + candidates + **思考 token**，
/// 塞进任何现有字段都是**编一个对不上的数**。`thoughtsTokenCount` / `toolUsePromptTokenCount`
/// 同理。账本的权威来源是原始帧（`ta_core::metering::normalize_gemini` 直接读 `usageMetadata`）。
/// Gemini 的 usage 字段表（第 330 轮：读/写由 `shared` 的表驱动）。
///
/// * 每格给了 camelCase 与 snake_case 两个候选 —— Gemini 的两种上游实现都有；
/// * 口径与 Anthropic 不同：`promptTokenCount` **含**缓存命中
///   （`metering::normalize_gemini` 要减掉 cached 才是 Input）。本层只搬字段、**不换算** ——
///   换算会引入"我们凭猜调整过的数字"，那是 `ta_core::metering` 的唯一职责；
/// * `cache_creation` 是**空**的（Gemini 不报缓存写入）；
/// * `reasoning` = `thoughtsTokenCount`。它已包含在 `candidatesTokenCount` 里，
///   所以搬它不会重复计数；
/// * `totalTokenCount` / `toolUsePromptTokenCount` **不在表里**：前者可推导，
///   后者是 `promptTokenCount` 的子集细分且没有别的协议有这一格。
pub(crate) const USAGE: UsageShape = UsageShape {
    input: &["promptTokenCount", "prompt_token_count"],
    output: &["candidatesTokenCount", "candidates_token_count"],
    cache_read: &["cachedContentTokenCount", "cached_content_token_count"],
    cache_creation: &[],
    reasoning: &["thoughtsTokenCount", "thoughts_token_count"],
};
fn ir_usage_from_gemini(usage: Option<&Value>) -> Option<IrUsage> {
    read_usage(usage, &USAGE)
}
/// [`IrUsage`] → Gemini 的 `usageMetadata`。**没说的字段不写**（不补 0）。
fn gemini_usage(usage: &IrUsage) -> Value {
    write_usage(usage, &USAGE)
}
// ================================================================ 测试

#[cfg(test)]
mod tests {
    //! 判据与 `adapt::tests` 一致：一条测试只有在"把被测的那件事破坏掉之后会红"时才算证据。
    //! 每条都同时钉住"该搬过去的东西"与"**不该**出现的东西"（编出来的值 / 形状）。

    use super::*;
    use crate::adapt::ProtocolAdapter;
    use pretty_assertions::assert_eq;

    /// 一小段 base64（内容无所谓，测的是"字节有没有原样穿过"）。
    const PNG: &str = "iVBORw0KGgoAAAANSUhEUg==";

    fn gemini() -> GeminiAdapter {
        GeminiAdapter
    }

    /// 恰好一个事件（多一个少一个都是契约被破坏）。
    fn only(events: Vec<IrStreamEvent>) -> IrStreamEvent {
        let mut events = events;
        if events.len() != 1 {
            panic!("应当恰好一个事件，实际 {events:?}");
        }
        events.remove(0)
    }

    /// 取 JSON 帧的载荷（`Literal` 帧在 Gemini 侧根本不该出现）。
    fn json_of(frame: &StreamFrame) -> &Value {
        match frame {
            StreamFrame::Json(value) => value,
            StreamFrame::Literal(text) => panic!("Gemini 侧不该有字面量帧（{text}）"),
        }
    }

    /// ① 角色映射（双向）：`system` ↔ `systemInstruction`、`assistant` ↔ `model`。
    #[test]
    fn system_and_assistant_roles_map_both_ways() {
        let body = json!({
            "systemInstruction": { "parts": [{ "text": "be nice" }] },
            "contents": [
                { "role": "user", "parts": [{ "text": "hi" }] },
                { "role": "model", "parts": [{ "text": "hello" }] },
                { "role": "user", "parts": [{ "text": "bye" }] },
            ],
        });
        let ir = gemini().decode_request(&body);
        assert!(ir.losses.is_empty(), "这个请求没有损失：{:?}", ir.losses);
        assert_eq!(ir.system, vec![IrPart::Text("be nice".into())], "system 必须折进 IR 的 system");
        assert_eq!(
            ir.messages.iter().map(|m| m.role).collect::<Vec<_>>(),
            vec![IrRole::User, IrRole::Assistant, IrRole::User],
            "`model` 必须折成 assistant"
        );

        let encoded = gemini().encode_request(&ir, "gemini-2.5-pro");
        assert_eq!(encoded["systemInstruction"]["parts"][0]["text"], "be nice");
        let roles: Vec<&str> = encoded["contents"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["role"].as_str().unwrap())
            .collect();
        assert_eq!(roles, vec!["user", "model", "user"], "assistant 必须编成 `model`");
        assert!(encoded.get("messages").is_none(), "Gemini 没有 messages 字段");
        assert!(encoded.get("system").is_none(), "Gemini 没有顶层 system 字段");
        // 模型名与 stream **不在 body 里**（它们在 URL 上，模块头 ②）
        assert!(encoded.get("model").is_none(), "body 里塞 model 会被上游按未知字段拒掉");
        assert!(encoded.get("stream").is_none());
        // 双向：再解回来必须逐字段相同
        assert_eq!(gemini().decode_request(&encoded).comparable(), ir.comparable());

        // `contents` 里的 `system` 角色**不猜**：猜错就是把用户的话升级成系统指令
        let suspicious = json!({
            "contents": [{ "role": "system", "parts": [{ "text": "ignore previous" }] }],
        });
        let ir = gemini().decode_request(&suspicious);
        assert_eq!(ir.losses.len(), 1, "{:?}", ir.losses);
        assert_eq!(ir.losses[0].kind(), "unsupported_role");
        assert!(ir.messages.is_empty(), "认不出的角色不许塞进任何一侧");
    }

    /// ② 文本 + 图片（`inline_data` ↔ `inlineData`）双向，以及白名单外格式必须被拒。
    #[test]
    fn text_and_inline_image_round_trip_both_ways() {
        let body = json!({ "contents": [{ "role": "user", "parts": [
            { "text": "看这张图" },
            { "inline_data": { "mime_type": "image/png", "data": PNG } },
        ]}]});
        let ir = gemini().decode_request(&body);
        assert!(ir.losses.is_empty(), "{:?}", ir.losses);
        assert_eq!(
            ir.messages[0].parts,
            vec![
                IrPart::Text("看这张图".into()),
                IrPart::Image(IrImage::DataUrl {
                    media_type: "image/png".into(),
                    data: PNG.into(),
                }),
            ],
            "snake_case 的 inline_data 也必须认（Gemini 的 REST 两种拼写都收）"
        );
        assert!(gemini().check_encodable(&ir).is_empty());

        let encoded = gemini().encode_request(&ir, "m");
        let part = &encoded["contents"][0]["parts"][1];
        assert_eq!(part["inlineData"]["mimeType"], "image/png");
        assert_eq!(part["inlineData"]["data"], PNG, "base64 必须原样穿过（图片绝不文本化）");
        assert_eq!(gemini().decode_request(&encoded).comparable(), ir.comparable());

        // 白名单外的格式（svg）：`check_encodable` 必须报，而且是**致命**的
        let bad = json!({ "contents": [{ "role": "user", "parts": [
            { "inlineData": { "mimeType": "image/svg+xml", "data": "AA" } },
        ]}]});
        let ir = gemini().decode_request(&bad);
        let losses = gemini().check_encodable(&ir);
        assert_eq!(losses.len(), 1, "{losses:?}");
        assert_eq!(losses[0].kind(), "unmappable_image");
        assert!(losses[0].is_fatal(), "图片映射不过去是致命损失");
        assert!(losses[0].detail().contains("image/svg+xml"), "{}", losses[0].detail());
    }

    /// ③ `functionCall` / `functionResponse` 双向：id 与 name 一个都不许丢、也不许编。
    #[test]
    fn function_call_and_response_round_trip_with_id_and_name() {
        let ir = IrRequest {
            messages: vec![
                IrMessage { role: IrRole::User, parts: vec![IrPart::Text("查天气".into())] },
                IrMessage {
                    role: IrRole::Assistant,
                    parts: vec![IrPart::ToolUse {
                        id: "call_1".into(),
                        name: "weather".into(),
                        input: json!({ "city": "SH" }),
                    }],
                },
                IrMessage {
                    role: IrRole::User,
                    parts: vec![IrPart::ToolResult {
                        tool_use_id: "call_1".into(),
                        content: vec![IrPart::Text("{\"temp\": 21}".into())],
                    }],
                },
            ],
            ..Default::default()
        };
        let losses = gemini().check_encodable(&ir);
        assert!(losses.is_empty(), "{losses:?}");

        let encoded = gemini().encode_request(&ir, "m");
        let call = &encoded["contents"][1]["parts"][0]["functionCall"];
        assert_eq!(call["name"], "weather");
        assert_eq!(call["args"]["city"], "SH");
        // v1beta 的 `FunctionCall.id` 是 Optional 的：带上它，跨协议的工具循环才能往返无损
        // （不带 ⇒ 往返核对必然对不上 ⇒ 默认策略 415）
        assert_eq!(call["id"], "call_1");
        let resp = &encoded["contents"][2]["parts"][0]["functionResponse"];
        assert_eq!(resp["name"], "weather", "functionResponse 必须带 name（IR 只有 id ⇒ 反查）");
        assert_eq!(resp["id"], "call_1");
        assert_eq!(resp["response"]["result"], "{\"temp\": 21}", "工具结果**原样**进壳");

        let back = gemini().decode_request(&encoded);
        assert!(back.losses.is_empty(), "{:?}", back.losses);
        assert_eq!(back.comparable(), ir.comparable(), "IR → Gemini → IR 必须逐字段相同");

        // 反方向：Gemini 自己发的形状（没有 id）→ IR：**不编** id，内容照搬
        let native = json!({ "contents": [
            { "role": "user", "parts": [{ "text": "查天气" }] },
            { "role": "model", "parts": [
                { "functionCall": { "name": "weather", "args": { "city": "SH" } } },
            ]},
            { "role": "function", "parts": [
                { "functionResponse": { "name": "weather", "response": { "result": "晴" } } },
            ]},
        ]});
        let ir = gemini().decode_request(&native);
        assert!(ir.losses.is_empty(), "{:?}", ir.losses);
        assert_eq!(
            ir.messages[1],
            IrMessage {
                role: IrRole::Assistant,
                parts: vec![IrPart::ToolUse {
                    id: String::new(),
                    name: "weather".into(),
                    input: json!({ "city": "SH" }),
                }],
            },
            "`function` 角色是 user 侧的工具结果 ⇒ 折进 User；函数调用折进 assistant"
        );
        assert_eq!(
            ir.messages[2].parts,
            vec![IrPart::ToolResult {
                tool_use_id: String::new(),
                content: vec![IrPart::Text("晴".into())],
            }],
            "「result」这层壳必须被拆掉（与编码方向互逆）"
        );

        // 配不上 functionCall 的工具结果：name 反查不到 ⇒ **不编**名字，登记致命缺失
        let orphan = IrRequest {
            messages: vec![IrMessage {
                role: IrRole::User,
                parts: vec![IrPart::ToolResult {
                    tool_use_id: "call_9".into(),
                    content: Vec::new(),
                }],
            }],
            ..Default::default()
        };
        let losses = gemini().check_encodable(&orphan);
        assert_eq!(losses.len(), 1, "{losses:?}");
        assert_eq!(losses[0].kind(), "missing_required");
        assert!(losses[0].is_fatal());
        assert!(losses[0].detail().contains("call_9"), "{}", losses[0].detail());
    }

    /// ④ `usageMetadata` ↔ [`IrUsage`]：三个有位置的计数一个不丢，没有位置的**不编**。
    #[test]
    fn usage_metadata_maps_three_counts_and_never_invents_total() {
        let usage = json!({
            "promptTokenCount": 11,
            "candidatesTokenCount": 5,
            "totalTokenCount": 17,
            "cachedContentTokenCount": 3,
            "thoughtsTokenCount": 1,
        });
        let ir = ir_usage_from_gemini(Some(&usage)).expect("usageMetadata 是对象就必须认");
        assert_eq!(ir.input_tokens, Some(11), "promptTokenCount → input");
        assert_eq!(ir.output_tokens, Some(5), "candidatesTokenCount → output");
        assert_eq!(ir.cache_read_input_tokens, Some(3), "cachedContentTokenCount → cache_read");
        assert_eq!(
            ir.cache_creation_input_tokens, None,
            "Gemini 不报缓存写入 ⇒ None（不是 Some(0)）"
        );

        // total=17 而 prompt+candidates 只有 16：差的那 1 个是思考 token。
        // IR 没有 total 的槽位 ⇒ 不搬、也不拿它去凑别的字段（凑出来会漏掉 thoughts = 编一个对不上的数）
        let encoded = gemini_usage(&ir);
        assert_eq!(encoded["totalTokenCount"], json!(null), "不许编 totalTokenCount");
        assert_eq!(encoded["promptTokenCount"], json!(11));
        assert_eq!(encoded["candidatesTokenCount"], json!(5));
        assert_eq!(encoded["cachedContentTokenCount"], json!(3));

        // `usage` 缺失/null = 上游说没有 ⇒ None；只有"上游说了 0"才是 Some(0)
        assert_eq!(ir_usage_from_gemini(Some(&Value::Null)), None);
        assert_eq!(ir_usage_from_gemini(None), None);
        let zeros = ir_usage_from_gemini(Some(&json!({ "promptTokenCount": 0 }))).unwrap();
        assert_eq!(zeros.input_tokens, Some(0));
        assert_eq!(zeros.output_tokens, None, "没说的字段不许补 0");
    }

    /// ⑤ 流式：带 `finishReason` 的帧 → `Stop`；累积 usage 不会被前面的帧吃掉。
    #[test]
    fn stream_finish_reason_becomes_stop_and_cumulative_usage_survives() {
        let first = json!({
            "candidates": [{ "content": { "role": "model", "parts": [{ "text": "你" }] }, "index": 0 }],
            "usageMetadata": { "promptTokenCount": 7, "candidatesTokenCount": 1, "totalTokenCount": 8 },
        });
        let second = json!({
            "candidates": [{ "content": { "role": "model", "parts": [{ "text": "好" }] }, "index": 0 }],
            "usageMetadata": { "promptTokenCount": 7, "candidatesTokenCount": 2, "totalTokenCount": 9 },
        });
        assert_eq!(
            only(gemini().decode_stream_event(&first)),
            IrStreamEvent::TextDelta { slot: Slot::Text(0), text: "你".into() }
        );
        assert_eq!(
            only(gemini().decode_stream_event(&second)),
            IrStreamEvent::TextDelta { slot: Slot::Text(0), text: "好".into() }
        );

        // ★ 收尾：Gemini 的流**不用** `data: [DONE]`，靠这个带 finishReason 的 chunk 收尾
        let last = json!({
            "candidates": [{ "content": { "role": "model", "parts": [] }, "finishReason": "STOP", "index": 0 }],
            "usageMetadata": { "promptTokenCount": 7, "candidatesTokenCount": 2, "totalTokenCount": 9 },
        });
        let (reason, usage) = match only(gemini().decode_stream_event(&last)) {
            IrStreamEvent::Stop { reason, usage } => {
                (reason, usage.expect("收尾帧的 usage 不许丢"))
            }
            other => panic!("应当是 Stop：{other:?}"),
        };
        assert_eq!(reason, Some(IrStopReason::EndTurn));
        assert_eq!(usage.input_tokens, Some(7));
        assert_eq!(usage.output_tokens, Some(2), "累积语义：最后一个数才是最终用量");

        // 合并：后一帧覆盖前一帧带的字段，而**没带**的字段（这里是 input）不许被抹掉
        let mut merged = IrUsage::default();
        for frame in [&first, &second, &last] {
            if let Some(fragment) = ir_usage_from_gemini(frame.get("usageMetadata")) {
                merged.merge(&fragment);
            }
        }
        assert_eq!(merged.input_tokens, Some(7), "前面的帧给的 input 不许被后面的帧吃掉");
        assert_eq!(merged.output_tokens, Some(2));
        let mut partial = IrUsage { input_tokens: Some(7), ..Default::default() };
        partial.merge(&IrUsage { output_tokens: Some(2), ..Default::default() });
        assert_eq!(partial, merged, "累积 = 逐帧 merge（`IrUsage::merge` 的覆盖语义与上游一致）");

        // 只有用量的帧：reason **不许编**
        let usage_only =
            json!({ "usageMetadata": { "promptTokenCount": 3, "candidatesTokenCount": 1 } });
        assert_eq!(
            only(gemini().decode_stream_event(&usage_only)),
            IrStreamEvent::Stop {
                reason: None,
                usage: Some(IrUsage {
                    input_tokens: Some(3),
                    output_tokens: Some(1),
                    ..Default::default()
                }),
            }
        );

        // 编码方向：Stop（reason + usage）→ **一帧**（Gemini 没有 `[DONE]` 要补）
        let frames = gemini().encode_stream_event(
            &IrStreamEvent::Stop {
                reason: Some(IrStopReason::EndTurn),
                usage: Some(IrUsage {
                    input_tokens: Some(3),
                    output_tokens: Some(1),
                    ..Default::default()
                }),
            },
            &mut EncodeCtx::new(),
        );
        assert_eq!(frames.len(), 1, "一次流只有一个终止帧：{frames:?}");
        assert_eq!(json_of(&frames[0])["candidates"][0]["finishReason"], "STOP");
        assert_eq!(json_of(&frames[0])["candidates"][0]["content"]["role"], "model");
        assert_eq!(json_of(&frames[0])["usageMetadata"]["candidatesTokenCount"], 1);
        assert_eq!(
            json_of(&frames[0])["usageMetadata"]["totalTokenCount"],
            json!(null),
            "不许编 total"
        );

        // 文本增量：一帧 candidate part
        let frames = gemini().encode_stream_event(
            &IrStreamEvent::TextDelta { slot: Slot::Text(0), text: "hi".into() },
            &mut EncodeCtx::new(),
        );
        assert_eq!(frames.len(), 1);
        assert_eq!(json_of(&frames[0])["candidates"][0]["content"]["parts"][0]["text"], "hi");
        // 只有用量（reason: None）⇒ 只发用量帧，**不编** finishReason
        let frames = gemini().encode_stream_event(
            &IrStreamEvent::Stop {
                reason: None,
                usage: Some(IrUsage { output_tokens: Some(2), ..Default::default() }),
            },
            &mut EncodeCtx::new(),
        );
        assert_eq!(frames.len(), 1);
        assert!(json_of(&frames[0]).get("candidates").is_none(), "{frames:?}");
    }

    /// ★ 第 328 轮的核心：**收尾帧里"最后一块文字 + finishReason + usage"三样同帧**
    /// 时，必须解出**两个**事件（内容是 `TextDelta`、收尾是带 usage 的 `Stop`），
    /// 顺序是先内容、后收尾。
    ///
    /// 旧签名返回 `Option<IrStreamEvent>`，这种帧只能二选一 —— 那就是丢数据。
    /// 把 `decode_stream_event` 改回"只返回一个事件"，这条立刻红。
    #[test]
    fn one_frame_with_text_finish_reason_and_usage_yields_two_events() {
        let last = json!({
            "candidates": [{ "content": { "role": "model", "parts": [{ "text": "。" }] },
                             "finishReason": "MAX_TOKENS", "index": 0 }],
            "usageMetadata": { "promptTokenCount": 7, "candidatesTokenCount": 9 },
        });
        assert_eq!(
            gemini().decode_stream_event(&last),
            vec![
                IrStreamEvent::TextDelta { slot: Slot::Text(0), text: "。".into() },
                IrStreamEvent::Stop {
                    reason: Some(IrStopReason::MaxTokens),
                    usage: Some(IrUsage {
                        input_tokens: Some(7),
                        output_tokens: Some(9),
                        ..Default::default()
                    }),
                },
            ],
            "同帧的内容与收尾都不许丢（顺序：先内容、后 Stop）"
        );

        // 一帧里多个 text part 逐个成事件（从前只有第一个活下来）
        let multi = json!({
            "candidates": [{ "content": { "role": "model",
                "parts": [{ "text": "甲" }, { "text": "乙" }] }, "index": 0 }],
        });
        assert_eq!(
            gemini().decode_stream_event(&multi),
            vec![
                IrStreamEvent::TextDelta { slot: Slot::Text(0), text: "甲".into() },
                IrStreamEvent::TextDelta { slot: Slot::Text(0), text: "乙".into() },
            ]
        );
        // 一帧里两个 functionCall 各有各的槽位（并行调用不许挤进同一个块），
        // 而且**入参要带上**（第 330 轮：从前这一格不存在，`args` 整份丢掉）
        let calls = json!({
            "candidates": [{ "content": { "role": "model", "parts": [
                { "functionCall": { "name": "weather", "args": { "city": "SH" } } },
                { "functionCall": { "name": "time", "args": {} } },
            ]}, "index": 0 }],
        });
        assert_eq!(
            gemini().decode_stream_event(&calls),
            vec![
                IrStreamEvent::ToolUseStart {
                    slot: Slot::Tool(0),
                    id: String::new(),
                    name: "weather".into(),
                    input: Some(json!({ "city": "SH" }))
                },
                IrStreamEvent::ToolUseStart {
                    slot: Slot::Tool(1),
                    id: String::new(),
                    name: "time".into(),
                    // `{}` 是"参数就是空对象"，**不是**"入参还没到" —— Gemini 一次给全，
                    // 所以这里必须是 `Some({})`（`None` 才是"入参走分片"）
                    input: Some(json!({}))
                },
            ]
        );
    }

    /// ★ `frame_identity`：Gemini 的 SSE 帧把身份放在 `responseId` / `modelVersion` 里
    /// （顶层没有 `id` / `model`）。不覆写它，跨协议流式合成的 `message_start.model`
    /// 会永远是 `null`。把覆写删掉，这条立刻红。
    #[test]
    fn frame_identity_reads_response_id_and_model_version() {
        let frame = json!({
            "responseId": "resp_1", "modelVersion": "gemini-2.5-pro",
            "candidates": [{ "content": { "parts": [{ "text": "hi" }] } }],
        });
        assert_eq!(
            gemini().frame_identity(&frame),
            (Some("resp_1".to_string()), Some("gemini-2.5-pro".to_string()))
        );
        // 取不到就是 None（**不编**）
        assert_eq!(gemini().frame_identity(&json!({ "usageMetadata": {} })), (None, None));
    }

    /// ★ `ToolInputDelta`：Gemini 的 `args` 是完整对象 ⇒ **0 帧**，但必须**留一条 note**。
    ///
    /// 从前这一层没有损失通道，"跨协议流式把工具入参整段丢掉"是**静默**发生的
    /// （客户端只看到一个 `args: {}` 的空工具调用）。把 `ctx.note` 删掉，这条立刻红。
    #[test]
    fn tool_input_delta_is_dropped_but_never_silently() {
        let mut ctx = EncodeCtx::new();
        let frames = gemini().encode_stream_event(
            &IrStreamEvent::ToolInputDelta { slot: Slot::Tool(0), fragment: "{\"city\":".into() },
            &mut ctx,
        );
        assert!(frames.is_empty(), "Gemini 的 args 没有分片形态 ⇒ 不编半个 JSON：{frames:?}");
        let notes = ctx.take_notes();
        assert_eq!(notes.len(), 1, "丢掉的东西必须留痕：{notes:?}");
        assert_eq!(notes[0].kind(), "unsupported_part");
        assert!(
            notes[0].detail().contains("{\"city\":"),
            "留痕要能看出丢的是哪一段：{}",
            notes[0].detail()
        );
        // 取走之后不再重复登记（`take_notes` 是**取走**语义）
        assert!(ctx.take_notes().is_empty());
    }

    /// ★ `close_stream`：**空** —— Gemini 的流就是"最后一帧带 `finishReason`"，
    /// 它没有任何收尾标记（没有 `[DONE]`，也没有 `message_stop`）。
    ///
    /// 补一个空候选帧会让客户端以为上游又发了一帧；补 `[DONE]` 更糟（那不是它的线格式）。
    #[test]
    fn close_stream_is_empty_because_gemini_has_no_terminator() {
        let mut ctx = EncodeCtx::new();
        assert!(
            gemini().close_stream(&mut ctx).is_empty(),
            "Gemini 的流没有任何收尾标记 —— 补一帧就是替上游编一个它不会发的形状"
        );
        // `begin_frames` 同理：Gemini 没有"流开始"这种东西
        assert!(gemini().begin_frames(&mut ctx).is_empty());
    }

    /// ⑥ `finishReason` 是开放取值：认不出的**原样带走**，绝不折成 STOP / end_turn。
    #[test]
    fn unknown_finish_reasons_are_carried_verbatim_never_turned_into_stop() {
        for (raw, expected) in [
            ("STOP", IrStopReason::EndTurn),
            ("MAX_TOKENS", IrStopReason::MaxTokens),
            ("SAFETY", IrStopReason::Other("SAFETY".into())),
            ("RECITATION", IrStopReason::Other("RECITATION".into())),
            ("MALFORMED_FUNCTION_CALL", IrStopReason::Other("MALFORMED_FUNCTION_CALL".into())),
            ("WHAT_IS_THIS", IrStopReason::Other("WHAT_IS_THIS".into())),
        ] {
            let body = json!({
                "candidates": [{ "content": { "role": "model", "parts": [] }, "finishReason": raw }],
            });
            assert_eq!(
                gemini().decode_response(&body).stop_reason,
                Some(expected),
                "finishReason `{raw}` 的映射不许猜"
            );
        }
        // ★ 关键：被拦的值**不许**落成"正常结束"（用户会以为模型把话说完了）
        let blocked = json!({
            "candidates": [{ "content": { "role": "model", "parts": [] }, "finishReason": "SAFETY" }],
        });
        assert_ne!(gemini().decode_response(&blocked).stop_reason, Some(IrStopReason::EndTurn));

        // 编码方向：认不出的值原样带走；IR 没说为什么结束 ⇒ **不写**这个键（不编 STOP）
        assert_eq!(gemini_finish_reason(&IrStopReason::Other("SAFETY".into())), json!("SAFETY"));
        assert_eq!(gemini_finish_reason(&IrStopReason::EndTurn), json!("STOP"));
        assert_eq!(gemini_finish_reason(&IrStopReason::MaxTokens), json!("MAX_TOKENS"));
        let (body, _) = gemini().encode_response(&IrResponse {
            parts: vec![IrPart::Text("x".into())],
            ..Default::default()
        });
        assert!(
            body["candidates"][0].get("finishReason").is_none(),
            "IR 没说结束原因 ⇒ 不写这个键"
        );
        // 流式同理：认不出的值原样带走
        let frame = json!({
            "candidates": [{ "content": { "parts": [] }, "finishReason": "IMAGE_SAFETY" }],
        });
        assert_eq!(
            only(gemini().decode_stream_event(&frame)),
            IrStreamEvent::Stop {
                reason: Some(IrStopReason::Other("IMAGE_SAFETY".into())),
                usage: None,
            },
            "未知取值不许编成 STOP（编了用户会以为模型把话说完了）"
        );
    }

    /// ⑦ `check_encodable`：Gemini 表达不了的 IR 字段必须**逐条**报损失（不许静默降级）。
    #[test]
    fn check_encodable_reports_what_gemini_cannot_express() {
        // ① systemInstruction 只收文本：图片没有位置
        let ir = IrRequest {
            system: vec![IrPart::Image(IrImage::DataUrl {
                media_type: "image/png".into(),
                data: PNG.into(),
            })],
            ..Default::default()
        };
        let losses = gemini().check_encodable(&ir);
        assert_eq!(losses.len(), 1, "{losses:?}");
        assert_eq!(losses[0].kind(), "unmappable_image");
        assert_eq!(losses[0].path(), "/system/0");
        assert!(losses[0].is_fatal());

        // ② http(s) 图片 URL：Gemini 只能内联（fileData.fileUri 只认 Files API / GCS 的 URI）
        let ir = IrRequest {
            messages: vec![IrMessage {
                role: IrRole::User,
                parts: vec![IrPart::Image(IrImage::Url("https://x/y.png".into()))],
            }],
            ..Default::default()
        };
        let losses = gemini().check_encodable(&ir);
        assert_eq!(losses.len(), 1, "{losses:?}");
        assert_eq!(losses[0].kind(), "unmappable_image");
        assert_eq!(losses[0].path(), "/messages/0/parts/0");
        assert!(losses[0].is_fatal());
        assert_eq!(
            gemini().encode_request(&ir, "m")["contents"],
            json!([]),
            "编不出来的块不许变成文本或别的形状（宁可空着，也不替它编）"
        );

        // ③ 工具结果里的图片：`functionResponse.response` 是 JSON 对象，装不下图片
        let ir = IrRequest {
            messages: vec![
                IrMessage {
                    role: IrRole::Assistant,
                    parts: vec![IrPart::ToolUse {
                        id: "call_1".into(),
                        name: "shot".into(),
                        input: json!({}),
                    }],
                },
                IrMessage {
                    role: IrRole::User,
                    parts: vec![IrPart::ToolResult {
                        tool_use_id: "call_1".into(),
                        content: vec![IrPart::Image(IrImage::DataUrl {
                            media_type: "image/png".into(),
                            data: PNG.into(),
                        })],
                    }],
                },
            ],
            ..Default::default()
        };
        let losses = gemini().check_encodable(&ir);
        assert_eq!(losses.len(), 1, "{losses:?}");
        assert_eq!(losses[0].kind(), "unmappable_image");
        assert!(losses[0].path().contains("content/0"), "要指出是哪一块：{}", losses[0].path());

        // ④ `generationConfig` 里 IR 表达不了的调参项：**逐项**登记（不是整块丢掉）
        let body = json!({
            "contents": [],
            "generationConfig": {
                "temperature": 0.5,
                "topK": 40,
                "thinkingConfig": { "thinkingBudget": 1024 },
            },
        });
        let ir = gemini().decode_request(&body);
        assert_eq!(ir.temperature, Some(0.5));
        // 排序后比：**损失的顺序不是契约**（它跟 JSON 对象的遍历顺序走），
        // 契约是"这两项各登记了一条"
        let mut paths: Vec<&str> = ir.losses.iter().map(|l| l.path()).collect();
        paths.sort_unstable();
        assert_eq!(paths, vec!["/generationConfig/thinkingConfig", "/generationConfig/topK"]);
        assert!(!ir.losses[0].is_fatal(), "调参项是可继续的损失（`on_loss: warn` 能放行）");

        // ⑤ 候选级元信息（safetyRatings / citationMetadata / groundingMetadata）：逐条登记
        let body = json!({
            "candidates": [{
                "content": { "role": "model", "parts": [{ "text": "hi" }] },
                "finishReason": "STOP",
                "safetyRatings": [{ "category": "HARM_CATEGORY_HATE_SPEECH", "probability": "LOW" }],
                "citationMetadata": { "citations": [{ "uri": "https://x" }] },
                "groundingMetadata": { "webSearchQueries": ["x"] },
            }],
        });
        let ir = gemini().decode_response(&body);
        assert_eq!(ir.text(), "hi");
        // 同上：顺序不是契约，集合才是
        let mut paths: Vec<&str> = ir.losses.iter().map(|l| l.path()).collect();
        paths.sort_unstable();
        assert_eq!(
            paths,
            vec![
                "/candidates/0/citationMetadata",
                "/candidates/0/groundingMetadata",
                "/candidates/0/safetyRatings",
            ],
            "候选级元信息一条都不许静默吞：{:?}",
            ir.losses
        );
        // 思维链（`thought: true`）与正文共用一个 text 字段：不许当成答案交出去
        let body = json!({
            "candidates": [{ "content": { "role": "model", "parts": [
                { "text": "我在想…", "thought": true },
                { "text": "答案" },
            ]}}],
        });
        let ir = gemini().decode_response(&body);
        assert_eq!(ir.text(), "答案", "思维链不是答案");
        assert_eq!(ir.losses.len(), 1);
        assert_eq!(ir.losses[0].kind(), "unsupported_part");
    }

    /// ⑧ `toolConfig`：模式双向映射；IR 表达不了的白名单子集必须留痕。
    #[test]
    fn tool_config_modes_map_and_multi_name_restrictions_are_registered() {
        let tools = json!({ "tools": [{ "functionDeclarations": [
            { "name": "weather", "description": "查天气",
              "parameters": { "type": "object", "properties": { "city": { "type": "string" } } } },
        ]}]});
        let base = gemini().decode_request(&tools);
        assert_eq!(base.tools.len(), 1);
        assert_eq!(base.tools[0].name, "weather");
        // 没有 toolConfig = 缺省 AUTO（不编别的）
        assert_eq!(base.tool_choice, IrToolChoice::Auto);
        // Auto ⇒ **不写** toolConfig（缺省值不写出来，解码侧也把"没有"当 Auto）
        assert!(gemini().encode_request(&base, "m").get("toolConfig").is_none());

        for (choice, mode, allowed) in [
            (IrToolChoice::Required, "ANY", Value::Null),
            (IrToolChoice::None, "NONE", Value::Null),
            (IrToolChoice::Tool("weather".into()), "ANY", json!(["weather"])),
        ] {
            let ir = IrRequest {
                tools: base.tools.clone(),
                tool_choice: choice.clone(),
                ..Default::default()
            };
            let encoded = gemini().encode_request(&ir, "m");
            assert_eq!(encoded["toolConfig"]["functionCallingConfig"]["mode"], mode);
            assert_eq!(
                encoded["toolConfig"]["functionCallingConfig"]
                    .get("allowedFunctionNames")
                    .cloned()
                    .unwrap_or(Value::Null),
                allowed
            );
            assert_eq!(gemini().decode_request(&encoded).tool_choice, choice, "模式必须往返一致");
        }

        // ANY + 多个名字：IR 只有"任意"与"某一个" ⇒ 按"任意"带走，但必须留痕
        let multi = json!({ "toolConfig": { "functionCallingConfig": {
            "mode": "ANY", "allowedFunctionNames": ["a", "b"],
        }}});
        let ir = gemini().decode_request(&multi);
        assert_eq!(ir.tool_choice, IrToolChoice::Required);
        assert_eq!(ir.losses.len(), 1, "{:?}", ir.losses);
        assert_eq!(ir.losses[0].path(), "/toolConfig/functionCallingConfig/allowedFunctionNames");
        // 认不出的 mode：留痕 + 落回缺省 AUTO（不猜一个像样的模式）
        let weird = json!({ "toolConfig": { "functionCallingConfig": { "mode": "SOMETIMES" } } });
        let ir = gemini().decode_request(&weird);
        assert_eq!(ir.tool_choice, IrToolChoice::Auto);
        assert_eq!(ir.losses.len(), 1, "{:?}", ir.losses);
        assert_eq!(ir.losses[0].path(), "/toolConfig/functionCallingConfig/mode");
    }

    /// ⑨ prompt 级拦截：登记损失，**不**当成一个合法的空回复。
    #[test]
    fn prompt_feedback_block_is_a_loss_not_a_normal_empty_response() {
        let body = json!({ "promptFeedback": { "blockReason": "SAFETY" } });
        let ir = gemini().decode_response(&body);
        assert!(ir.parts.is_empty(), "被拦的 prompt 没有内容可搬");
        assert_eq!(ir.stop_reason, None, "没有候选就没有 finishReason（不许编 STOP）");
        assert_eq!(
            ir.losses.len(),
            2,
            "promptFeedback 与「candidates 缺失」各一条：{:?}",
            ir.losses
        );
        assert!(ir.losses.iter().any(|l| l.detail().contains("SAFETY")), "{:?}", ir.losses);
        // 流式：同样不许折成一个 Stop（那等于替上游说"模型正常结束了"）
        assert_eq!(gemini().decode_stream_event(&body), vec![IrStreamEvent::Other]);
    }

    /// ⑪ `check_encodable`：`stopSequences` 的协议上限是 **5 条**。
    ///
    /// 超了上游直接 400，而且它不会告诉我们哪一条多余 ⇒ 必须在编码前拒。
    /// 把这条检查删掉，本测试立刻红（默认 `on_loss: reject` 下每一次超限请求都会打到上游）。
    #[test]
    fn stop_sequences_over_the_protocol_limit_are_refused_up_front() {
        // 恰好 5 条：合法，不许报损失
        let ok = IrRequest {
            stop: (0..MAX_STOP_SEQUENCES).map(|i| format!("S{i}")).collect(),
            ..Default::default()
        };
        assert!(
            !gemini().check_encodable(&ok).iter().any(|l| l.path() == "/stop"),
            "5 条是上限内，不该报"
        );
        // 6 条：报一条，且指出是哪个字段
        let too_many = IrRequest {
            stop: (0..=MAX_STOP_SEQUENCES).map(|i| format!("S{i}")).collect(),
            ..Default::default()
        };
        let losses = gemini().check_encodable(&too_many);
        let stop_loss = losses
            .iter()
            .find(|l| l.path() == "/stop")
            .unwrap_or_else(|| panic!("超限必须报损失：{losses:?}"));
        assert_eq!(stop_loss.kind(), "unsupported_field");
        assert!(stop_loss.detail().contains("6"), "要说清源请求给了几条：{}", stop_loss.detail());
    }

    /// ⑫ **空 id 的多个工具调用按序号配对**（不是全部收敛到第一个名字）。
    ///
    /// Gemini 自己发的 `functionCall` 常常没有 id：从前 `tool_names_by_id` 把空 id 全映射到
    /// 第一个 name，于是"查时间的结果"被标成"查天气的结果"—— 工具循环静默答错，
    /// 而且一条损失都不报。改成按出现顺序配对之后：
    /// 第 n 个空 id 的 `ToolResult` 配第 n 个空 id 的 `ToolUse`。
    #[test]
    fn anonymous_tool_calls_pair_by_position_not_by_the_first_name() {
        let ir = IrRequest {
            messages: vec![
                IrMessage {
                    role: IrRole::Assistant,
                    parts: vec![
                        IrPart::ToolUse {
                            id: String::new(),
                            name: "weather".into(),
                            input: json!({}),
                        },
                        IrPart::ToolUse {
                            id: String::new(),
                            name: "time".into(),
                            input: json!({}),
                        },
                    ],
                },
                IrMessage {
                    role: IrRole::User,
                    parts: vec![
                        IrPart::ToolResult {
                            tool_use_id: String::new(),
                            content: vec![IrPart::Text("晴".into())],
                        },
                        IrPart::ToolResult {
                            tool_use_id: String::new(),
                            content: vec![IrPart::Text("12:00".into())],
                        },
                    ],
                },
            ],
            ..Default::default()
        };
        // 两个都能配上 ⇒ **一条损失都没有**
        assert!(gemini().check_encodable(&ir).is_empty(), "{:?}", gemini().check_encodable(&ir));

        let encoded = gemini().encode_request(&ir, "m");
        let calls: Vec<&str> = encoded["contents"][0]["parts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["functionCall"]["name"].as_str().unwrap())
            .collect();
        assert_eq!(calls, vec!["weather", "time"], "两个调用各自带上自己的名字");
        let responses: Vec<&str> = encoded["contents"][1]["parts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["functionResponse"]["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            responses,
            vec!["weather", "time"],
            "第 n 个空 id 的结果必须配第 n 个空 id 的调用（不许全收敛到第一个名字）"
        );
        assert_eq!(
            encoded["contents"][1]["parts"][1]["functionResponse"]["response"]["result"], "12:00",
            "内容也要落在配得上的那一条上"
        );

        // 反过来：结果比调用多 ⇒ 多出来的那条**查不到名字**（致命缺失），不是安静地用第一个名字
        let extra_result = IrRequest {
            messages: vec![
                IrMessage {
                    role: IrRole::Assistant,
                    parts: vec![IrPart::ToolUse {
                        id: String::new(),
                        name: "weather".into(),
                        input: json!({}),
                    }],
                },
                IrMessage {
                    role: IrRole::User,
                    parts: vec![
                        IrPart::ToolResult {
                            tool_use_id: String::new(),
                            content: vec![IrPart::Text("晴".into())],
                        },
                        IrPart::ToolResult {
                            tool_use_id: String::new(),
                            content: vec![IrPart::Text("多出来的".into())],
                        },
                    ],
                },
            ],
            ..Default::default()
        };
        let losses = gemini().check_encodable(&extra_result);
        assert_eq!(losses.len(), 1, "{losses:?}");
        assert_eq!(losses[0].kind(), "missing_required");
        assert!(losses[0].is_fatal());
        // 非空 id 仍然按 id 精确匹配（不受序号影响）
        let mixed = IrRequest {
            messages: vec![
                IrMessage {
                    role: IrRole::Assistant,
                    parts: vec![
                        IrPart::ToolUse {
                            id: "call_1".into(),
                            name: "weather".into(),
                            input: json!({}),
                        },
                        IrPart::ToolUse {
                            id: String::new(),
                            name: "time".into(),
                            input: json!({}),
                        },
                    ],
                },
                IrMessage {
                    role: IrRole::User,
                    parts: vec![
                        IrPart::ToolResult {
                            tool_use_id: String::new(),
                            content: vec![IrPart::Text("12:00".into())],
                        },
                        IrPart::ToolResult {
                            tool_use_id: "call_1".into(),
                            content: vec![IrPart::Text("晴".into())],
                        },
                    ],
                },
            ],
            ..Default::default()
        };
        assert!(
            gemini().check_encodable(&mixed).is_empty(),
            "{:?}",
            gemini().check_encodable(&mixed)
        );
        let encoded = gemini().encode_request(&mixed, "m");
        let responses: Vec<&str> = encoded["contents"][1]["parts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["functionResponse"]["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            responses,
            vec!["time", "weather"],
            "空 id 走序号、非空 id 走精确匹配，两条规则互不干扰"
        );
    }

    /// ⑬ 认不出的流式帧 → `Other`；不是协议帧 → **0 个事件**。
    #[test]
    fn stream_frames_without_a_counterpart_yield_other_not_a_fake_event() {
        // 空候选、心跳、只有 safetyRatings：认得出是 Gemini 帧但没东西可搬
        for frame in [
            json!({ "candidates": [{ "content": { "role": "model", "parts": [] }, "index": 0 }] }),
            json!({ "usageMetadata": {} }),
            json!({ "candidates": [{ "safetyRatings": [{ "category": "HARM" }] }] }),
        ] {
            assert_eq!(
                gemini().decode_stream_event(&frame),
                vec![IrStreamEvent::Other],
                "{frame} 没有可搬的东西 ⇒ Other（不是伪事件）"
            );
        }
        // 不是协议帧（非对象、别家的帧）⇒ 连事件都不产出
        assert!(gemini().decode_stream_event(&json!(null)).is_empty());
        assert!(gemini().decode_stream_event(&json!("[DONE]")).is_empty());
        assert!(gemini().decode_stream_event(&json!([])).is_empty());
        assert!(gemini()
            .decode_stream_event(&json!({ "choices": [{ "delta": { "content": "x" } }] }))
            .is_empty());
        // `Other` 编码成**零帧**
        assert!(gemini()
            .encode_stream_event(&IrStreamEvent::Other, &mut EncodeCtx::new())
            .is_empty());
        // `Start` 编码成零帧：Gemini 的帧里没有 `id` / `model` 的落脚处，
        // 而且身份已经由 `ctx.remember_upstream` 记着（别的协议要用）——
        // **不造**一帧假的 `modelVersion`
        assert!(gemini()
            .encode_stream_event(
                &IrStreamEvent::Start {
                    model: "gemini-2.5-pro".into(),
                    id: Some("resp_1".into()),
                    usage: None,
                },
                &mut EncodeCtx::new(),
            )
            .is_empty());
    }

    /// ⑩ **与 `openai.rs` 的跨协议往返**（这才是 IR 路径真正要过的那道门）：
    /// OpenAI 形状的富请求（system + 文本 + 图片 + 工具调用 + 工具结果 + 函数声明 + 调参）
    /// → IR → Gemini → 再解回 IR，必须逐字段相同（`adapt::verify_no_loss` 的判据）。
    ///
    /// 这条是"`FunctionCall.id` 必须带上"的**证据**：把 `encode_part` 里的 `("id", id)` 去掉，
    /// 往返回来 `ToolUse.id` 会变成空串 ⇒ 这里立刻红（默认 `on_loss: reject` 下就是 415）。
    #[test]
    fn openai_shaped_request_round_trips_through_gemini_without_loss() {
        use crate::adapt::openai::OpenaiAdapter;

        let openai = OpenaiAdapter;
        let body = json!({
            "model": "gpt-4o",
            "max_tokens": 256,
            "temperature": 0.2,
            "top_p": 0.9,
            "stop": ["END"],
            "messages": [
                { "role": "system", "content": "be nice" },
                { "role": "user", "content": [
                    { "type": "text", "text": "看这张图" },
                    { "type": "image_url",
                      "image_url": { "url": format!("data:image/png;base64,{PNG}") } },
                ]},
                { "role": "assistant", "content": null, "tool_calls": [
                    { "id": "call_1", "type": "function",
                      "function": { "name": "weather", "arguments": "{\"city\":\"SH\"}" } },
                ]},
                { "role": "tool", "tool_call_id": "call_1", "content": "{\"temp\": 21}" },
            ],
            "tools": [
                { "type": "function", "function": {
                    "name": "weather", "description": "查天气",
                    "parameters": { "type": "object", "properties": { "city": { "type": "string" } } },
                }},
            ],
            "tool_choice": "auto",
        });
        let ir = openai.decode_request(&body);
        assert!(ir.losses.is_empty(), "源侧（OpenAI 解码）不该有损失：{:?}", ir.losses);
        let losses = gemini().check_encodable(&ir);
        assert!(losses.is_empty(), "Gemini 作为目标不该有硬约束损失：{losses:?}");

        let encoded = gemini().encode_request(&ir, "gemini-2.5-pro");
        let back = gemini().decode_request(&encoded);
        assert!(back.losses.is_empty(), "回程（Gemini 解码）不该有损失：{:?}", back.losses);

        // `model` **有意不比**：Gemini 的模型名在 URL 路径里（模块头 ②），body 里根本没有它；
        // `adapt::mod::diff_request` 也同样不比较 `model`（它被有意改写成 `upstream_model`）。
        let mut expected = ir.comparable();
        expected.model = None;
        assert_eq!(back.comparable(), expected, "openai → IR → gemini → IR 必须逐字段相同");
    }
}
