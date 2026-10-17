//! 协议适配（IR 路径）· **工作包 G3**
//!
//! 直通优先：**同协议转发永不走这里**（RAW 路径只做定点 patch，零改动）。
//! 只有客户端协议 ≠ 上游协议时才用 IR（[ADR-03]）。
//!
//! # 这一层的两条硬纪律
//!
//! 1. **无法映射就拒绝，绝不文本化**。图片映射不过去时返回
//!    `TA-2006`（415），**绝不允许**用 `"[image]"` 之类的文本顶替 —— 那会把用户"干成瞎子"，
//!    而且上游还会回 200，用户永远发现不了。
//! 2. **每一处损失都必须登记**（[`Loss`]）。转换函数不返回"改好的 JSON 就完事"，
//!    还要返回"这次转换丢了什么"。`on_loss: reject`（默认）下有任何损失就不发请求；
//!    `on_loss: warn` 只对**能继续**的损失放行（详见 [`Loss::is_fatal`]）。
//!
//! # 校验（`verify_no_loss`）怎么做
//!
//! `源请求 → IR → 目标协议编码 → **再用目标适配器解码回 IR**`，两次 IR 必须逐字段相同。
//! 这能抓住"编码器悄悄丢了一个块"这类 bug —— 那是**我们自己的** bug，用户看不见，
//! 只有把往返核对做成必经步骤才有机会在测试里红。IR 的规范形态（合并相邻同角色消息等）
//! 就是为了让这个判据不把"协议合法的表达差异"误判成损失，见 [`ir`] 模块头。
//!
//! # 已知的有意不对称
//!
//! * **请求方向**：有损失就拒发（上游**根本没被问过**， 要的就是这个）。
//! * **响应方向**：上游已经答复了，这里**永不拒绝** —— 拒绝等于把上游的答案整份丢掉，
//!   比"翻译后带损失地交出去"更糟。响应侧的损失只**记日志**（`translate_response` 返回 losses）。
//! * **跨协议流式**：** 第 326 轮起已支持**。请求方向由本模块转（与响应无关），
//!   响应方向是 SSE，逐帧走 [`session::translate_sse_stream`]（切帧 → IR → 目标协议）。
//!   上面那条不对称（请求有损就拒、响应从不拒）在流式下同样成立。

pub mod anthropic;
pub mod gemini;
pub mod ir;
pub mod loss;
pub mod openai;
pub mod responses;
pub mod session;
pub mod shared;
pub mod stream;

pub use ir::{
    IrImage, IrMessage, IrPart, IrRequest, IrResponse, IrRole, IrStopReason, IrTool, IrToolChoice,
};
pub use loss::{describe, fatal_losses, Loss};
pub use stream::{
    is_same_protocol_stream, sse_data_of, stream_adapter_for, take_sse_events,
    translate_stream_event, EncodeCtx, IrStreamEvent, IrUsage, Slot, StreamAdapter, StreamFrame,
};

use bytes::Bytes;
use serde_json::Value;
use ta_core::domain::OnLoss;
use ta_core::error::{Result, TaError};
use ta_core::types::Protocol;

/// 一个协议的编解码器。
///
/// `check_encodable` 与 `encode_request` 分开是刻意的：**致命损失在编码前就要判完**，
/// 于是编码函数可以是全函数（前置检查没过的请求根本到不了编码），
/// 也就不会有"编码到一半发现少了个必填字段"的半成品状态。
pub trait ProtocolAdapter: Send + Sync {
    fn protocol(&self) -> Protocol;

    /// 目标协议的**硬约束**检查（在编码前跑）。这些损失基本都是致命的。
    fn check_encodable(&self, ir: &IrRequest) -> Vec<Loss>;

    fn decode_request(&self, body: &Value) -> IrRequest;
    fn encode_request(&self, ir: &IrRequest, upstream_model: &str) -> Value;

    fn decode_response(&self, body: &Value) -> IrResponse;
    /// 编码给客户端的响应。响应方向不拒绝，所以把丢掉的东西**返回出来**给调用方记日志。
    fn encode_response(&self, ir: &IrResponse) -> (Value, Vec<Loss>);

    /// **这个协议的 `stream` 标记是不是写在 body 之外？**
    ///
    /// 默认 `false`：绝大多数协议把 `stream: true` 放在 body 里，往返核对**必须比它**。
    ///
    /// Gemini 是唯一的例外：流式与否由**路径**决定
    /// （`…:generateContent` vs `…:streamGenerateContent?alt=sse`），body 里根本没有这一项。
    /// 如果不声明，编码方向会永远报一条假的 `RoundTrip /stream` 损失 ——
    /// 而实际上**什么都没丢**，只是它走的不是 body。假损失比没有损失更坏：它会把
    /// 每一条流式请求在默认 `on_loss: reject` 下打回。
    fn stream_is_out_of_band(&self) -> bool {
        false
    }
}

static OPENAI: openai::OpenaiAdapter = openai::OpenaiAdapter;
static ANTHROPIC: anthropic::AnthropicAdapter = anthropic::AnthropicAdapter;
static RESPONSES: responses::ResponsesAdapter = responses::ResponsesAdapter;
static GEMINI: gemini::GeminiAdapter = gemini::GeminiAdapter;

/// 「边缘 N 种、内部 1 种」：**每加一种协议 = 加一个适配器**，
/// 不动已有代码，也不需要 N×N 对转换器。加 `Gemini` 时照着同样三步做即可
/// （解码 + 编码 + 在下面这张表里加一行）。
///
/// 三种都有了（`Gemini` 只作为**上游**可能：网关没有 `/v1beta/...` 路由，
/// 所以它永远不会是"客户端协议"，见 [`gemini`] 模块头）。
pub fn adapter_for(protocol: Protocol) -> Option<&'static dyn ProtocolAdapter> {
    match protocol {
        Protocol::Openai | Protocol::Custom => Some(&OPENAI),
        Protocol::Anthropic => Some(&ANTHROPIC),
        Protocol::OpenaiResponses => Some(&RESPONSES),
        Protocol::Gemini => Some(&GEMINI),
    }
}

/// 这一对协议能不能走 IR 路径（两边都要有适配器）。
///
/// 管线用它来决定"跨协议时是转还是拒" —— 少了这个判断，缺适配器的组合会静默走 RAW，
/// 把 OpenAI 形状的 body 发给 Anthropic 上游（那是**必然**的 400，而用户会以为是我们坏了）。
pub fn can_translate(client: Protocol, upstream: Protocol) -> bool {
    adapter_for(client).is_some() && adapter_for(upstream).is_some()
}

/// 一次请求转换的结果。
#[derive(Debug, Clone)]
pub struct Translation {
    pub body: Bytes,
    pub losses: Vec<Loss>,
}

/// 一次响应转换的结果。
#[derive(Debug, Clone)]
pub struct TranslatedResponse {
    pub body: Bytes,
    pub losses: Vec<Loss>,
}

/// **请求方向**：客户端协议 → 上游协议。
///
/// 同协议**原样返回**（不走 IR、不重编码）：让误用也无损。
/// 跨协议时：解码 → 编码 → 往返核对 → 按 `on_loss` 决定发不发。**任何返回 `Err` 的路径
/// 都发生在拨号之前**，所以"上游根本没被问过"这条验收是结构上成立的。
pub fn translate_request(
    client: Protocol,
    upstream: Protocol,
    upstream_model: &str,
    body: &[u8],
    on_loss: OnLoss,
) -> Result<Translation> {
    if client == upstream {
        return Ok(Translation { body: Bytes::copy_from_slice(body), losses: Vec::new() });
    }
    let (Some(src), Some(dst)) = (adapter_for(client), adapter_for(upstream)) else {
        return Err(TaError::CapabilityLoss(format!(
            "跨协议转换暂不支持 {client:?} → {upstream:?}（本版有 openai-chat / anthropic / openai-responses 三个适配器）"
        )));
    };
    let value: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(err) => {
            return Err(TaError::CapabilityLoss(format!(
                "跨协议转换要求 JSON 请求体，而这是{}（{err}）",
                "非 JSON"
            )))
        }
    };

    let ir = src.decode_request(&value);
    let mut losses = ir.losses.clone();
    // 跨协议 + `stream: true` **不再在拨号前拒绝**。
    //
    // 原来这里推一条 `Loss::UnsupportedStreaming`（致命）把整条请求打回，理由是"响应方向
    // 是 SSE 状态机、还没写"。现在状态机在 [`session`] 里：上游 SSE 逐帧
    // 切帧 → IR → 目标协议，边转边发。所以**请求方向不需要因为流式而特殊对待** ——
    // body 照常转，响应方向由 session 层负责。
    losses.extend(dst.check_encodable(&ir));
    let encoded = dst.encode_request(&ir, upstream_model);
    losses.extend(verify_no_loss(&ir, dst, &encoded));

    if !losses.is_empty() {
        let fatal = losses.iter().any(Loss::is_fatal);
        if fatal || on_loss == OnLoss::Reject {
            return Err(TaError::CapabilityLoss(describe(&losses)));
        }
    }
    let encoded_bytes = serde_json::to_vec(&encoded)
        .map_err(|err| TaError::Internal(format!("跨协议转换结果无法序列化：{err}")))?;
    Ok(Translation { body: Bytes::from(encoded_bytes), losses })
}

/// **响应方向**：上游协议 → 客户端协议。
///
/// 与请求方向的三点不同（模块头"已知的有意不对称"）：不拒绝、损失只登记、非 JSON 原样返回。
pub fn translate_response(
    upstream: Protocol,
    client: Protocol,
    body: &[u8],
) -> Result<TranslatedResponse> {
    if upstream == client {
        return Ok(TranslatedResponse { body: Bytes::copy_from_slice(body), losses: Vec::new() });
    }
    let (Some(src), Some(dst)) = (adapter_for(upstream), adapter_for(client)) else {
        return Err(TaError::Internal(format!(
            "响应方向缺适配器：{upstream:?} → {client:?}（请求方向不该放它过来）"
        )));
    };
    let value: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => {
            // 上游回了非 JSON（错误页、纯文本…）：**原样交给客户端**，只登记一处损失
            return Ok(TranslatedResponse {
                body: Bytes::copy_from_slice(body),
                losses: vec![Loss::UnsupportedPart {
                    path: "/".into(),
                    kind: "响应体不是 JSON（原样转发）".into(),
                }],
            });
        }
    };
    let ir = src.decode_response(&value);
    let mut losses = ir.losses.clone();
    let (encoded, encode_losses) = dst.encode_response(&ir);
    losses.extend(encode_losses);
    losses.extend(verify_no_loss_response(&ir, dst, &encoded));
    let encoded_bytes = serde_json::to_vec(&encoded)
        .map_err(|err| TaError::Internal(format!("跨协议响应无法序列化：{err}")))?;
    Ok(TranslatedResponse { body: Bytes::from(encoded_bytes), losses })
}

/// 往返核对：目标协议编出来再解码回来，必须与源 IR **逐字段相同**。
pub fn verify_no_loss(ir: &IrRequest, dst: &dyn ProtocolAdapter, encoded: &Value) -> Vec<Loss> {
    let expect = ir.comparable();
    let actual = dst.decode_request(encoded).comparable();
    let mut losses = Vec::new();
    // `stream` 写在 body 之外的协议（Gemini）跳过这一项：它在**路径**里，
    // 编码方向没丢东西，比它只会得到一条假损失（见 `stream_is_out_of_band` 的文档）。
    diff_request(&expect, &actual, dst.stream_is_out_of_band(), &mut losses);
    losses
}

/// 响应方向的往返核对（同思路，字段更少）。
pub fn verify_no_loss_response(
    ir: &IrResponse,
    dst: &dyn ProtocolAdapter,
    encoded: &Value,
) -> Vec<Loss> {
    let actual = dst.decode_response(encoded);
    let mut losses = Vec::new();
    if ir.parts != actual.parts {
        losses.push(Loss::RoundTrip {
            path: "/content".into(),
            detail: format!(
                "响应往返后内容块不一致：期望 {} 块，实际 {} 块",
                ir.parts.len(),
                actual.parts.len()
            ),
        });
    }
    if ir.stop_reason != actual.stop_reason {
        losses.push(Loss::RoundTrip {
            path: "/stop_reason".into(),
            detail: format!(
                "停止原因往返后不一致：{:?} → {:?}",
                ir.stop_reason, actual.stop_reason
            ),
        });
    }
    losses
}

/// 逐字段比较两次 IR。**不比较 `model`**：它被有意改写成 `upstream_model`（与 RAW 路径的模型 patch 同理）。
fn diff_request(
    expect: &IrRequest,
    actual: &IrRequest,
    stream_is_out_of_band: bool,
    losses: &mut Vec<Loss>,
) {
    let mut round_trip = |path: &str, detail: String| {
        losses.push(Loss::RoundTrip { path: path.to_string(), detail })
    };
    if !stream_is_out_of_band && expect.stream != actual.stream {
        round_trip("/stream", format!("{} → {}", expect.stream, actual.stream));
    }
    if expect.system != actual.system {
        round_trip(
            "/system",
            format!("期望 {} 块，实际 {} 块", expect.system.len(), actual.system.len()),
        );
    }
    if expect.messages != actual.messages {
        let path = first_differing_message(expect, actual);
        round_trip(&path, "消息序列往返后不一致（块被丢/被改/顺序变了）".to_string());
    }
    if expect.tools != actual.tools {
        round_trip(
            "/tools",
            format!("期望 {} 个工具，实际 {} 个", expect.tools.len(), actual.tools.len()),
        );
    }
    if expect.tool_choice != actual.tool_choice {
        round_trip("/tool_choice", format!("{:?} → {:?}", expect.tool_choice, actual.tool_choice));
    }
    if expect.max_tokens != actual.max_tokens {
        round_trip("/max_tokens", format!("{:?} → {:?}", expect.max_tokens, actual.max_tokens));
    }
    if expect.temperature != actual.temperature {
        round_trip("/temperature", format!("{:?} → {:?}", expect.temperature, actual.temperature));
    }
    if expect.top_p != actual.top_p {
        round_trip("/top_p", format!("{:?} → {:?}", expect.top_p, actual.top_p));
    }
    if expect.stop != actual.stop {
        round_trip("/stop", format!("{:?} → {:?}", expect.stop, actual.stop));
    }
}

fn first_differing_message(expect: &IrRequest, actual: &IrRequest) -> String {
    let n = expect.messages.len().max(actual.messages.len());
    for i in 0..n {
        let a = expect.messages.get(i);
        let b = actual.messages.get(i);
        if a != b {
            return format!("/messages/{i}");
        }
    }
    "/messages".to_string()
}

// ================================================================ 测试

#[cfg(test)]
mod tests {
    //! # 这套测试**自己出过一次假绿**，写在最前面免得下一个人重犯
    //!
    //! "规范形态"（[`crate::adapt::ir::canonicalize`]：合并相邻同角色消息、`tool_result` 排前）
    //! 我当成必需来实现，理由也对（Anthropic 要求 user/assistant 交替）。但把 `canonicalize`
    //! 改成 no-op 之后，**13 条测试全绿** —— 因为原来的夹具里**根本没有相邻同角色消息**：
    //! 我验证的是"我实现了 X"，不是"X 是必需的"。
    //!
    //! 补上 `parallel_tool_results_are_merged_into_one_user_message`（并行工具调用时 OpenAI 会
    //! 连发多条 `role:"tool"`，这正是真实流量里最常见的相邻同角色形态）之后，同一个改动才变红
    //! （13 passed / 1 failed）。
    //!
    //! **判据**：一条测试只有在"把被测的那件事破坏掉之后会红"时才算证据。
    //! 加测试时要问"什么改动会让它红"，答不上来就说明它只是在**复述实现**。
    //! （同一族的另一个实例见 `runtime.rs::runtime_has_no_second_router_assembly`：
    //! 它第一版扫整份文件，被测试段里的 `Router::with_sticky` 喂饱，改成 `Router::new` 照样绿。）

    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    const PNG: &str = "data:image/png;base64,AAAB";

    /// 一个"什么都用上了"的 OpenAI 请求：system、文本+图片、工具调用、工具结果、工具定义、调参。
    fn rich_openai() -> Value {
        json!({
            "model": "gpt-4o",
            "max_tokens": 1024,
            "temperature": 0.2,
            "top_p": 0.9,
            "stop": ["END"],
            "messages": [
                { "role": "system", "content": "be nice" },
                { "role": "user", "content": [
                    { "type": "text", "text": "look at this" },
                    { "type": "image_url", "image_url": { "url": PNG, "detail": "high" } }
                ]},
                { "role": "assistant", "content": null, "tool_calls": [
                    { "id": "call_1", "type": "function",
                      "function": { "name": "weather", "arguments": "{\"city\":\"SH\"}" } }
                ]},
                { "role": "tool", "tool_call_id": "call_1", "content": "{\"temp\":21}" }
            ],
            "tools": [
                { "type": "function", "function": {
                    "name": "weather", "description": "查天气",
                    "parameters": { "type": "object", "properties": { "city": { "type": "string" } } }
                }}
            ],
            "tool_choice": "auto"
        })
    }

    fn openai_adapter() -> &'static dyn ProtocolAdapter {
        adapter_for(Protocol::Openai).expect("必须有 OpenAI 适配器")
    }

    fn anthropic_adapter() -> &'static dyn ProtocolAdapter {
        adapter_for(Protocol::Anthropic).expect("必须有 Anthropic 适配器")
    }

    /// 并行工具调用：OpenAI 会回**连续多条** `role:"tool"` 消息，而 Anthropic 要求
    /// user/assistant **交替** —— 不合并就会编出一个 Anthropic 必然 400 的请求。
    ///
    /// 这条同时钉住"user 消息里 tool_result 排在文本之前"这条规范形态。
    #[test]
    fn parallel_tool_results_are_merged_into_one_user_message() {
        let body = json!({
            "model": "gpt-4o",
            "max_tokens": 256,
            "messages": [
                { "role": "user", "content": "两个城市" },
                { "role": "assistant", "content": null, "tool_calls": [
                    { "id": "call_a", "type": "function",
                      "function": { "name": "weather", "arguments": "{\"city\":\"SH\"}" } },
                    { "id": "call_b", "type": "function",
                      "function": { "name": "weather", "arguments": "{\"city\":\"BJ\"}" } }
                ]},
                { "role": "tool", "tool_call_id": "call_a", "content": "21" },
                { "role": "tool", "tool_call_id": "call_b", "content": "19" },
                { "role": "user", "content": "谢谢" }
            ]
        });
        let out = translate_request(
            Protocol::Openai,
            Protocol::Anthropic,
            "claude-x",
            &serde_json::to_vec(&body).unwrap(),
            OnLoss::Reject,
        )
        .expect("并行工具结果必须能无损转换");
        let mid: Value = serde_json::from_slice(&out.body).unwrap();
        let roles: Vec<&str> = mid["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["role"].as_str().unwrap())
            .collect();
        assert_eq!(roles, vec!["user", "assistant", "user"], "Anthropic 必须交替：{roles:?}");
        let blocks = mid["messages"][2]["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 3, "两个 tool_result + 一段文本必须在同一条 user 消息里");
        assert_eq!(blocks[0]["type"], "tool_result");
        assert_eq!(blocks[0]["tool_use_id"], "call_a");
        assert_eq!(blocks[1]["type"], "tool_result");
        assert_eq!(blocks[1]["tool_use_id"], "call_b");
        assert_eq!(blocks[2]["type"], "text", "文本排在工具结果之后（两个协议都接受的顺序）");

        // 回程：两条 tool 消息与那段文本都还在
        let back = translate_request(
            Protocol::Anthropic,
            Protocol::Openai,
            "gpt-4o",
            &out.body,
            OnLoss::Reject,
        )
        .expect("回程无损");
        let back_json: Value = serde_json::from_slice(&back.body).unwrap();
        let roles: Vec<&str> = back_json["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["role"].as_str().unwrap())
            .collect();
        assert_eq!(roles, vec!["user", "assistant", "tool", "tool", "user"]);
        assert_eq!(back_json["messages"][2]["tool_call_id"], "call_a");
        assert_eq!(back_json["messages"][3]["tool_call_id"], "call_b");
        assert_eq!(back_json["messages"][4]["content"], "谢谢");
        assert_eq!(
            openai_adapter().decode_request(&body).comparable(),
            openai_adapter().decode_request(&back_json).comparable(),
            "并行工具结果的往返必须逐字段相同"
        );
    }

    /// ★ DoD：`openai → anthropic → openai` 的图片块与工具调用**语义不变**。
    #[test]
    fn openai_to_anthropic_round_trip_keeps_images_and_tools() {
        let original = rich_openai();
        let to_anthropic = translate_request(
            Protocol::Openai,
            Protocol::Anthropic,
            "claude-sonnet-4-5",
            &serde_json::to_vec(&original).unwrap(),
            OnLoss::Reject,
        )
        .expect("这个请求应当在两个方向都能无损转换");
        assert!(to_anthropic.losses.is_empty(), "无损路径不该有损失：{:?}", to_anthropic.losses);

        let mid: Value = serde_json::from_slice(&to_anthropic.body).unwrap();
        assert_eq!(mid["model"], "claude-sonnet-4-5", "模型必须换成通道的 upstream_model");
        assert_eq!(mid["system"], "be nice", "system 必须提到顶层");
        assert_eq!(mid["max_tokens"], 1024);
        assert_eq!(mid["stop_sequences"], json!(["END"]));
        // 图片：data URL → base64 source（**不是**文本，也不是别的形状）
        let user_content = &mid["messages"][0]["content"];
        assert_eq!(user_content[0]["type"], "text");
        assert_eq!(user_content[1]["type"], "image");
        assert_eq!(user_content[1]["source"]["type"], "base64");
        assert_eq!(user_content[1]["source"]["media_type"], "image/png");
        assert_eq!(user_content[1]["source"]["data"], "AAAB");
        // 工具调用
        assert_eq!(mid["messages"][1]["content"][0]["type"], "tool_use");
        assert_eq!(mid["messages"][1]["content"][0]["id"], "call_1");
        assert_eq!(mid["messages"][1]["content"][0]["input"]["city"], "SH");
        // 工具结果：OpenAI 的独立 `role:"tool"` 消息 → Anthropic 的 user + tool_result
        assert_eq!(mid["messages"][2]["role"], "user");
        assert_eq!(mid["messages"][2]["content"][0]["type"], "tool_result");
        assert_eq!(mid["messages"][2]["content"][0]["tool_use_id"], "call_1");
        // 工具定义：function.parameters → input_schema
        assert_eq!(mid["tools"][0]["name"], "weather");
        assert_eq!(mid["tools"][0]["input_schema"]["properties"]["city"]["type"], "string");

        // 再转回 OpenAI：语义必须完全回到原样（IR 逐字段相等）
        let back = translate_request(
            Protocol::Openai,
            Protocol::Openai,
            "gpt-4o",
            &serde_json::to_vec(&json!({ "ignored": true })).unwrap(),
            OnLoss::Reject,
        );
        assert!(back.is_ok(), "同协议必须原样通过");
        let back = translate_request(
            Protocol::Anthropic,
            Protocol::Openai,
            "gpt-4o",
            &to_anthropic.body,
            OnLoss::Reject,
        )
        .expect("回程也必须无损");
        let back_json: Value = serde_json::from_slice(&back.body).unwrap();
        assert_eq!(back_json["tools"][0]["function"]["name"], "weather");
        // 注意编号：OpenAI 里 system 也是 messages 的一员（Anthropic 是顶层字段），所以这里是 2/3
        assert_eq!(back_json["messages"][2]["role"], "assistant");
        assert_eq!(back_json["messages"][2]["tool_calls"][0]["id"], "call_1");
        let args: Value = serde_json::from_str(
            back_json["messages"][2]["tool_calls"][0]["function"]["arguments"].as_str().unwrap(),
        )
        .unwrap();
        assert_eq!(args["city"], "SH");
        assert_eq!(back_json["messages"][3]["role"], "tool");
        assert_eq!(back_json["messages"][3]["tool_call_id"], "call_1");
        assert_eq!(back_json["messages"][1]["content"][1]["image_url"]["url"], PNG);

        // **语义等价的最终判据**：两次解码出来的 IR 逐字段相同
        let first = openai_adapter().decode_request(&original).comparable();
        let second = openai_adapter().decode_request(&back_json).comparable();
        assert_eq!(first, second, "openai → anthropic → openai 之后 IR 必须逐字段相同");
    }

    /// ★ DoD：无法映射的图片 → `TA-2006`（415），且**绝不文本化**。
    #[test]
    fn unmappable_image_is_rejected_and_never_textified() {
        let mut body = rich_openai();
        body["messages"][1]["content"][1]["image_url"]["url"] =
            json!("data:image/svg+xml;base64,PHN2Zy8+");
        let err = translate_request(
            Protocol::Openai,
            Protocol::Anthropic,
            "claude-sonnet-4-5",
            &serde_json::to_vec(&body).unwrap(),
            OnLoss::Reject,
        )
        .expect_err("svg 不在 Anthropic 的白名单里，必须拒绝");
        assert_eq!(err.code(), "TA-2006");
        assert_eq!(err.http_status(), 415);
        let message = err.to_string();
        assert!(message.contains("image/svg+xml"), "错误里要写清是哪个格式：{message}");
        // 路径按**源请求/IR 坐标**给出：图片在 IR 的第 0 条消息里（system 被单独提出去，不占 messages 序号）
        assert!(message.contains("content/1"), "要给出精确位置：{message}");
        assert!(!message.contains("[image]"), "**绝不允许**文本化图片");

        // `warn` 也不行：不能编码的损失**任何模式**都拒绝
        let err = translate_request(
            Protocol::Openai,
            Protocol::Anthropic,
            "claude-sonnet-4-5",
            &serde_json::to_vec(&body).unwrap(),
            OnLoss::Warn,
        )
        .expect_err("warn 只放过'丢一点可选字段'，图片不可映射属于编不出请求");
        assert_eq!(err.code(), "TA-2006");
    }

    /// Anthropic 的 `source.type = "file"`（Files API 引用）没有跨协议对应物 → 同样拒绝。
    #[test]
    fn anthropic_file_image_is_rejected() {
        let body = json!({
            "model": "claude-sonnet-4-5",
            "max_tokens": 100,
            "messages": [{ "role": "user", "content": [
                { "type": "image", "source": { "type": "file", "file_id": "file_1" } }
            ]}]
        });
        let err = translate_request(
            Protocol::Anthropic,
            Protocol::Openai,
            "gpt-4o",
            &serde_json::to_vec(&body).unwrap(),
            OnLoss::Reject,
        )
        .expect_err("file 引用无法映射");
        assert_eq!(err.code(), "TA-2006");
        assert!(err.to_string().contains("file"), "错误要说清是 source.type=file：{err}");
    }

    /// Anthropic Messages **必填** `max_tokens`：源请求没给就编不出自洽请求（**不替你编一个**）。
    #[test]
    fn anthropic_target_requires_explicit_max_tokens() {
        let mut body = rich_openai();
        body.as_object_mut().unwrap().remove("max_tokens");
        let err = translate_request(
            Protocol::Openai,
            Protocol::Anthropic,
            "claude-sonnet-4-5",
            &serde_json::to_vec(&body).unwrap(),
            OnLoss::Reject,
        )
        .expect_err("缺 max_tokens 必须拒绝");
        assert_eq!(err.code(), "TA-2006");
        assert!(err.to_string().contains("max_tokens"), "要说清缺哪个字段：{err}");
    }

    /// 没有对应物的调参字段：默认拒绝（响亮），`on_loss: warn` 才放行并**留痕**。
    #[test]
    fn optional_field_loss_rejects_by_default_and_warns_when_configured() {
        let mut body = rich_openai();
        body["presence_penalty"] = json!(0.5);
        let raw = serde_json::to_vec(&body).unwrap();

        let err = translate_request(
            Protocol::Openai,
            Protocol::Anthropic,
            "claude-x",
            &raw,
            OnLoss::Reject,
        )
        .expect_err("默认 reject：有损失就不发");
        assert_eq!(err.code(), "TA-2006");
        assert!(err.to_string().contains("presence_penalty"), "{err}");

        let ok = translate_request(
            Protocol::Openai,
            Protocol::Anthropic,
            "claude-x",
            &raw,
            OnLoss::Warn,
        )
        .expect("warn：用户已显式接受丢这一项");
        assert_eq!(ok.losses.len(), 1, "损失必须登记出来：{:?}", ok.losses);
        assert_eq!(ok.losses[0].kind(), "unsupported_field");
        assert!(!ok.losses[0].is_fatal(), "调参字段是'可继续'的损失");
        let sent: Value = serde_json::from_slice(&ok.body).unwrap();
        assert!(sent.get("presence_penalty").is_none(), "丢掉的东西不该出现在目标请求里");
    }

    /// 跨协议 + 流式：**第 326 轮起不再在拨号前拒绝**。
    ///
    /// 原来这一条断言的是"拒绝"（TA-2006），理由是响应方向是 SSE 状态机、还没写。
    /// 现在状态机在 [`session`] 里（上游 SSE 逐帧切帧 → IR → 目标协议），
    /// 所以请求方向**不能**因为 `stream: true` 就把整条请求打回 ——
    /// 那会让 Claude Code（默认流式）接 OpenAI 中转站**永远用不了**。
    ///
    /// 注意这条只验"请求方向放行"：响应方向的逐帧翻译由 `session::tests` 覆盖。
    #[test]
    fn cross_protocol_streaming_is_no_longer_refused() {
        let mut body = rich_openai();
        body["stream"] = json!(true);
        let out = translate_request(
            Protocol::Openai,
            Protocol::Anthropic,
            "claude-x",
            &serde_json::to_vec(&body).unwrap(),
            OnLoss::Warn,
        )
        .expect("跨协议流式现在应当放行（响应方向由 session 层逐帧翻译）");
        let sent: Value = serde_json::from_slice(&out.body).unwrap();
        // Anthropic 的请求体里必须有 `messages` —— 证明它真的被转成了目标协议
        assert!(sent.get("messages").is_some(), "应当已转成 Anthropic 形状：{sent}");
    }

    /// 同协议：**原样返回**（不重编码，一个字节都不动）—— 误用也无损。
    #[test]
    fn same_protocol_is_returned_byte_identical() {
        let raw = br#"{ "model" : "gpt-4o" ,  "messages" : [ ] }"#;
        let out =
            translate_request(Protocol::Openai, Protocol::Openai, "whatever", raw, OnLoss::Reject)
                .expect("同协议不走 IR");
        assert_eq!(out.body.as_ref(), raw, "同协议必须逐字节原样返回");
        assert!(out.losses.is_empty());
    }

    /// 非 JSON 请求体跨协议：编不出目标协议的请求 → 拒绝（**绝不**把原样字节发给错误协议的上游）。
    #[test]
    fn non_json_body_is_refused_cross_protocol() {
        let err = translate_request(
            Protocol::Openai,
            Protocol::Anthropic,
            "claude-x",
            b"not json at all",
            OnLoss::Reject,
        )
        .expect_err("非 JSON 无法走 IR");
        assert_eq!(err.code(), "TA-2006");
    }

    /// 响应方向：Anthropic 的 `tool_use` + `stop_reason` → OpenAI 的 `tool_calls` + `finish_reason`。
    #[test]
    fn anthropic_response_to_openai_keeps_tool_calls_and_stop_reason() {
        let upstream = json!({
            "id": "msg_1",
            "type": "message",
            "role": "assistant",
            "model": "claude-sonnet-4-5",
            "content": [
                { "type": "text", "text": "让我查一下" },
                { "type": "tool_use", "id": "toolu_1", "name": "weather", "input": { "city": "SH" } }
            ],
            "stop_reason": "tool_use"
        });
        let out = translate_response(
            Protocol::Anthropic,
            Protocol::Openai,
            &serde_json::to_vec(&upstream).unwrap(),
        )
        .expect("响应方向不该拒绝");
        let body: Value = serde_json::from_slice(&out.body).unwrap();
        assert_eq!(body["id"], "msg_1");
        assert_eq!(body["choices"][0]["message"]["content"], "让我查一下");
        assert_eq!(body["choices"][0]["message"]["tool_calls"][0]["id"], "toolu_1");
        assert_eq!(body["choices"][0]["message"]["tool_calls"][0]["function"]["name"], "weather");
        assert_eq!(body["choices"][0]["finish_reason"], "tool_calls");
        assert!(out.losses.is_empty(), "无损响应不该有损失：{:?}", out.losses);
    }

    /// 响应方向的非 JSON（上游错误页）：原样交给客户端，只登记一处损失。
    #[test]
    fn non_json_response_is_forwarded_verbatim() {
        let out = translate_response(Protocol::Anthropic, Protocol::Openai, b"<html>502</html>")
            .expect("响应方向不该拒绝");
        assert_eq!(out.body.as_ref(), b"<html>502</html>");
        assert_eq!(out.losses.len(), 1);
        assert_eq!(out.losses[0].kind(), "unsupported_part");
    }

    /// **往返核对本身是承重的**：把编码结果里的一块内容删掉，它必须报出来。
    ///
    /// 这条是"校验器会红"的证据 —— 一条从不红的校验等于没有校验。
    #[test]
    fn round_trip_verifier_detects_a_dropping_encoder() {
        let ir = openai_adapter().decode_request(&rich_openai());
        let encoded = anthropic_adapter().encode_request(&ir, "claude-x");
        // 先确认好编码是无损的
        assert!(verify_no_loss(&ir, anthropic_adapter(), &encoded).is_empty());

        // 模拟"编码器少搬了一块"：把 user 消息里的图片删掉
        let mut broken = encoded.clone();
        let content = broken["messages"][0]["content"].as_array_mut().unwrap();
        content.retain(|b| b["type"] != "image");
        let losses = verify_no_loss(&ir, anthropic_adapter(), &broken);
        assert!(!losses.is_empty(), "丢了图片必须被报出来");
        assert_eq!(losses[0].kind(), "round_trip_mismatch");
        assert_eq!(losses[0].path(), "/messages/0");
    }

    /// 损失的分类与描述：致命/非致命要有明确边界，描述要带路径与原因。
    #[test]
    fn loss_classification_and_description() {
        assert!(Loss::UnsupportedStreaming { detail: String::new() }.is_fatal());
        assert!(Loss::UnmappableImage { path: "/x".into(), detail: String::new() }.is_fatal());
        assert!(Loss::MissingRequired { path: "/x".into(), detail: String::new() }.is_fatal());
        assert!(!Loss::UnsupportedField {
            path: "/x".into(),
            field: "seed".into(),
            detail: String::new()
        }
        .is_fatal());

        let text = describe(&[Loss::UnsupportedField {
            path: "/presence_penalty".into(),
            field: "presence_penalty".into(),
            detail: "目标协议没有对应物".into(),
        }]);
        assert!(text.contains("unsupported_field"));
        assert!(text.contains("/presence_penalty"));
        assert!(text.contains("目标协议没有对应物"));
        assert!(fatal_losses(&[]).is_empty());
    }

    /// 第 330 轮：**非流式响应的 usage 也要跨协议带过去**。
    ///
    /// 从前 `IrResponse` 里没有 usage 这一格，于是翻译过的非流式响应交出的是
    /// **空的用量** —— OpenAI 客户端看到 `usage: null`、Anthropic 客户端连
    /// `message.usage` 都没有，而上游明明给了。
    ///
    /// **不影响计费**（账本读上游原始字节），这条钉的是**保真**。
    #[test]
    fn non_streaming_response_usage_survives_cross_protocol() {
        // OpenAI chat → Anthropic
        let openai_body = br#"{"id":"c1","model":"m",
            "choices":[{"message":{"role":"assistant","content":"hi"},"finish_reason":"stop"}],
            "usage":{"prompt_tokens":11,"completion_tokens":4,
                     "prompt_tokens_details":{"cached_tokens":3}}}"#;
        let out = translate_response(Protocol::Openai, Protocol::Anthropic, openai_body)
            .expect("openai → anthropic 应当能转");
        let sent: Value = serde_json::from_slice(&out.body).unwrap();
        assert_eq!(sent["usage"]["input_tokens"], json!(11), "输入用量丢了：{sent}");
        assert_eq!(sent["usage"]["output_tokens"], json!(4), "输出用量丢了：{sent}");
        assert_eq!(sent["usage"]["cache_read_input_tokens"], json!(3), "缓存命中数丢了：{sent}");
        // Anthropic 没有"缓存写入"这一格 → 不写（不是写 0）
        assert!(sent["usage"].get("cache_creation_input_tokens").is_none(), "{sent}");

        // 反向：Anthropic → OpenAI
        let anthropic_body = br#"{"id":"m1","type":"message","model":"claude-x",
            "content":[{"type":"text","text":"hi"}],"stop_reason":"end_turn",
            "usage":{"input_tokens":7,"output_tokens":2}}"#;
        let out = translate_response(Protocol::Anthropic, Protocol::Openai, anthropic_body)
            .expect("anthropic → openai 应当能转");
        let sent: Value = serde_json::from_slice(&out.body).unwrap();
        assert_eq!(sent["usage"]["prompt_tokens"], json!(7), "{sent}");
        assert_eq!(sent["usage"]["completion_tokens"], json!(2), "{sent}");
    }

    /// 「边缘 N 种、内部 1 种」的**可用性表**。
    ///
    /// 加一个适配器就要改这张表 —— 它是"哪几种协议之间真的能转"的唯一声明。
    /// 缺适配器的组合必须**显式拒绝**（`TA-2006`），而不是静默把源协议的 body 发给目标协议：
    /// 那是**必然**的 400，而用户会以为是我们坏了。
    #[test]
    fn protocol_pair_availability_is_declared_not_guessed() {
        // **三种适配器都齐了** ⇒ 任意两种协议之间都能走 IR 中转。
        // 这张表就是「边缘 N 种」那句话的可执行版本：加/删一个适配器，它立刻变红。
        for (a, b) in [
            (Protocol::Openai, Protocol::Anthropic),
            (Protocol::Anthropic, Protocol::Openai),
            (Protocol::Openai, Protocol::OpenaiResponses),
            (Protocol::OpenaiResponses, Protocol::Openai),
            (Protocol::Anthropic, Protocol::OpenaiResponses),
            (Protocol::OpenaiResponses, Protocol::Anthropic),
            (Protocol::Openai, Protocol::Gemini),
            (Protocol::Gemini, Protocol::Openai),
            (Protocol::Anthropic, Protocol::Gemini),
            (Protocol::Gemini, Protocol::Anthropic),
            (Protocol::OpenaiResponses, Protocol::Gemini),
            (Protocol::Gemini, Protocol::OpenaiResponses),
        ] {
            assert!(can_translate(a, b), "{a:?} → {b:?} 应当可转（两边都有适配器）");
        }
        // `Custom` 的线格式由配置决定，但适配器与 OpenAI chat 是同一份（既有约定）
        assert!(can_translate(Protocol::Custom, Protocol::Anthropic));
        // 请求适配器与流式适配器**必须同生同死**：只有一边会让客户端收到空流或坏流
        for p in
            [Protocol::Openai, Protocol::Anthropic, Protocol::OpenaiResponses, Protocol::Gemini]
        {
            assert_eq!(
                adapter_for(p).is_some(),
                stream_adapter_for(p).is_some(),
                "{p:?} 的请求/流式适配器必须同时存在"
            );
        }
    }
}
