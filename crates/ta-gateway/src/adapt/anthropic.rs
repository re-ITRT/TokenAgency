//! Anthropic Messages 适配器（`Protocol::Anthropic`）。
//!
//! 与 OpenAI 适配器**同构**：只认文档化的形状，其余登记 [`Loss`]。
//! 两处 Anthropic 特有的硬约束在 [`super::ProtocolAdapter::check_encodable`] 里：
//! `max_tokens` **必填**、图片 `source.type` 只有 `base64` / `url` 两种。

use serde_json::{json, Value};

use super::shared::{
    check_images, check_tool_ids, describe_image, f64_of, part_kind, read_usage, strings_of,
    u64_of, write_usage, UsageShape,
};
use crate::adapt::ir::{
    canonicalize, object, IrImage, IrMessage, IrPart, IrRequest, IrResponse, IrRole, IrStopReason,
    IrTool, IrToolChoice,
};
use crate::adapt::loss::Loss;
use crate::adapt::stream::{EncodeCtx, IrStreamEvent, IrUsage, Slot, StreamAdapter, StreamFrame};

/// Anthropic 官方接受的图片 media type。
pub(crate) const IMAGE_MEDIA_TYPES: &[&str] =
    &["image/jpeg", "image/png", "image/gif", "image/webp"];

pub struct AnthropicAdapter;

const KNOWN_TOP_LEVEL: &[&str] = &[
    "model",
    "stream",
    "system",
    "messages",
    "tools",
    "tool_choice",
    "max_tokens",
    "temperature",
    "top_p",
    "stop_sequences",
];

impl AnthropicAdapter {
    fn decode_system(&self, value: Option<&Value>, losses: &mut Vec<Loss>) -> Vec<IrPart> {
        match value {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::String(s)) => vec![IrPart::Text(s.clone())],
            Some(Value::Array(items)) => {
                let mut parts = Vec::new();
                for (i, item) in items.iter().enumerate() {
                    match item.get("type").and_then(Value::as_str).unwrap_or("") {
                        "text" => {
                            if let Some(t) = item.get("text").and_then(Value::as_str) {
                                parts.push(IrPart::Text(t.to_string()));
                            }
                        }
                        other => losses.push(Loss::UnsupportedPart {
                            path: format!("/system/{i}"),
                            kind: if other.is_empty() {
                                "(缺 type)".to_string()
                            } else {
                                other.to_string()
                            },
                        }),
                    }
                }
                parts
            }
            Some(_) => {
                losses.push(Loss::UnsupportedPart {
                    path: "/system".into(),
                    kind: "system(非字符串/非数组)".into(),
                });
                Vec::new()
            }
        }
    }

    fn decode_content(
        &self,
        value: Option<&Value>,
        path: &str,
        losses: &mut Vec<Loss>,
    ) -> Vec<IrPart> {
        match value {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::String(s)) => vec![IrPart::Text(s.clone())],
            Some(Value::Array(items)) => {
                let mut parts = Vec::new();
                for (i, item) in items.iter().enumerate() {
                    let ipath = format!("{path}/{i}");
                    match item.get("type").and_then(Value::as_str).unwrap_or("") {
                        "text" => {
                            if let Some(t) = item.get("text").and_then(Value::as_str) {
                                parts.push(IrPart::Text(t.to_string()));
                            }
                        }
                        "image" => {
                            if let Some(img) = decode_image(item, &ipath, losses) {
                                parts.push(img);
                            }
                        }
                        "tool_use" => parts.push(IrPart::ToolUse {
                            id: item.get("id").and_then(Value::as_str).unwrap_or("").to_string(),
                            name: item
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string(),
                            input: item.get("input").cloned().unwrap_or_else(|| json!({})),
                        }),
                        "tool_result" => {
                            let inner = self.decode_content(
                                item.get("content"),
                                &format!("{ipath}/content"),
                                losses,
                            );
                            parts.push(IrPart::ToolResult {
                                tool_use_id: item
                                    .get("tool_use_id")
                                    .and_then(Value::as_str)
                                    .unwrap_or("")
                                    .to_string(),
                                content: inner,
                            });
                        }
                        other => losses.push(Loss::UnsupportedPart {
                            path: ipath,
                            kind: if other.is_empty() {
                                "(缺 type)".to_string()
                            } else {
                                other.to_string()
                            },
                        }),
                    }
                }
                parts
            }
            Some(_) => {
                losses.push(Loss::UnsupportedPart {
                    path: path.into(),
                    kind: "content(非字符串/非数组)".into(),
                });
                Vec::new()
            }
        }
    }

    fn decode_message(&self, msg: &Value, index: usize, out: &mut IrRequest) {
        let path = format!("/messages/{index}");
        let role = msg.get("role").and_then(Value::as_str).unwrap_or("");
        let parts =
            self.decode_content(msg.get("content"), &format!("{path}/content"), &mut out.losses);
        match role {
            "user" => out.messages.push(IrMessage { role: IrRole::User, parts }),
            "assistant" => out.messages.push(IrMessage { role: IrRole::Assistant, parts }),
            other => out.losses.push(Loss::UnsupportedRole {
                path: format!("{path}/role"),
                role: other.to_string(),
            }),
        }
    }
}

fn decode_image(item: &Value, path: &str, losses: &mut Vec<Loss>) -> Option<IrPart> {
    let Some(source) = item.get("source") else {
        losses.push(Loss::UnmappableImage {
            path: path.into(),
            detail: "image 块缺 source".to_string(),
        });
        return None;
    };
    match source.get("type").and_then(Value::as_str).unwrap_or("") {
        "base64" => {
            let media_type =
                source.get("media_type").and_then(Value::as_str).unwrap_or("").to_string();
            let data = source.get("data").and_then(Value::as_str).unwrap_or("").to_string();
            if media_type.is_empty() || data.is_empty() {
                losses.push(Loss::UnmappableImage {
                    path: path.into(),
                    detail: "base64 图片缺 media_type 或 data".to_string(),
                });
                return None;
            }
            Some(IrPart::Image(IrImage::DataUrl { media_type, data }))
        }
        "url" => match source.get("url").and_then(Value::as_str) {
            Some(url) if !url.is_empty() => Some(IrPart::Image(IrImage::Url(url.to_string()))),
            _ => {
                losses.push(Loss::UnmappableImage {
                    path: path.into(),
                    detail: "url 图片缺 url".to_string(),
                });
                None
            }
        },
        other => {
            // 例如 Files API 的 `source.type = "file"`：没有跨协议对应物 → **拒绝**，绝不文本化
            losses.push(Loss::UnmappableImage {
                path: path.into(),
                detail: format!("Anthropic image source.type = `{other}` 无法映射"),
            });
            None
        }
    }
}

impl super::ProtocolAdapter for AnthropicAdapter {
    fn protocol(&self) -> ta_core::types::Protocol {
        ta_core::types::Protocol::Anthropic
    }

    fn check_encodable(&self, ir: &IrRequest) -> Vec<Loss> {
        let mut losses = Vec::new();
        check_tool_ids(ir, &mut losses);
        // `max_tokens` 是 Anthropic Messages 的**必填**字段；源请求没给就编不出自洽的请求
        if ir.max_tokens.is_none() {
            losses.push(Loss::MissingRequired {
                path: "/max_tokens".into(),
                detail: "Anthropic Messages 要求显式 max_tokens，而源请求没给（不会替你编一个）"
                    .into(),
            });
        }
        for (i, part) in ir.system.iter().enumerate() {
            match part {
                IrPart::Text(_) => {}
                IrPart::Image(img) => losses.push(Loss::UnsupportedPart {
                    path: format!("/system/{i}"),
                    kind: format!("system 里的图片（{}）无法表达", describe_image(img)),
                }),
                other => losses.push(Loss::UnsupportedPart {
                    path: format!("/system/{i}"),
                    kind: part_kind(other).to_string(),
                }),
            }
        }
        check_images(ir, IMAGE_MEDIA_TYPES, &mut losses);
        losses
    }

    fn decode_request(&self, body: &Value) -> IrRequest {
        let mut ir = IrRequest {
            model: body.get("model").and_then(Value::as_str).map(str::to_string),
            stream: body.get("stream").and_then(Value::as_bool).unwrap_or(false),
            max_tokens: u64_of(body, &["max_tokens"]),
            temperature: f64_of(body, "temperature"),
            top_p: f64_of(body, "top_p"),
            stop: strings_of(body.get("stop_sequences")),
            ..Default::default()
        };
        ir.system = self.decode_system(body.get("system"), &mut ir.losses);
        if let Some(messages) = body.get("messages").and_then(Value::as_array) {
            for (i, msg) in messages.iter().enumerate() {
                self.decode_message(msg, i, &mut ir);
            }
        }
        if let Some(tools) = body.get("tools").and_then(Value::as_array) {
            for tool in tools {
                ir.tools.push(IrTool {
                    name: tool.get("name").and_then(Value::as_str).unwrap_or("").into(),
                    description: tool
                        .get("description")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    parameters: tool.get("input_schema").cloned().unwrap_or_else(|| json!({})),
                });
            }
        }
        if let Some(tc) = body.get("tool_choice") {
            ir.tool_choice = decode_tool_choice(tc, &mut ir.losses);
        }
        if let Some(map) = body.as_object() {
            for (k, v) in map {
                if v.is_null() || KNOWN_TOP_LEVEL.contains(&k.as_str()) {
                    continue;
                }
                ir.losses.push(Loss::UnsupportedField {
                    path: format!("/{k}"),
                    field: k.clone(),
                    detail: "Anthropic 协议字段，目标协议没有对应物".into(),
                });
            }
        }
        ir.messages = canonicalize(ir.messages);
        ir
    }

    fn encode_request(&self, ir: &IrRequest, upstream_model: &str) -> Value {
        let system = if ir.system.is_empty() {
            Value::Null
        } else {
            Value::String(ir.system.iter().filter_map(part_text).collect::<Vec<_>>().join("\n\n"))
        };
        let messages: Vec<Value> = ir
            .messages
            .iter()
            .map(|msg| {
                object(vec![
                    ("role", json!(msg.role.as_str())),
                    ("content", encode_content(&msg.parts)),
                ])
            })
            .collect();
        let tools: Vec<Value> = ir
            .tools
            .iter()
            .map(|t| {
                object(vec![
                    ("name", json!(t.name)),
                    ("description", json!(t.description)),
                    ("input_schema", t.parameters.clone()),
                ])
            })
            .collect();
        let tool_choice = match &ir.tool_choice {
            IrToolChoice::Auto => Value::Null,
            IrToolChoice::Required => json!({ "type": "any" }),
            IrToolChoice::None => json!({ "type": "none" }),
            IrToolChoice::Tool(name) => json!({ "type": "tool", "name": name }),
        };
        object(vec![
            ("model", json!(upstream_model)),
            ("stream", json!(ir.stream)),
            ("max_tokens", ir.max_tokens.map(|n| json!(n)).unwrap_or(Value::Null)),
            ("system", system),
            ("messages", Value::Array(messages)),
            ("tools", if tools.is_empty() { Value::Null } else { Value::Array(tools) }),
            ("tool_choice", tool_choice),
            ("temperature", ir.temperature.map(|v| json!(v)).unwrap_or(Value::Null)),
            ("top_p", ir.top_p.map(|v| json!(v)).unwrap_or(Value::Null)),
            (
                "stop_sequences",
                if ir.stop.is_empty() {
                    Value::Null
                } else {
                    Value::Array(ir.stop.iter().map(|s| json!(s)).collect())
                },
            ),
        ])
    }

    fn decode_response(&self, body: &Value) -> IrResponse {
        let mut ir = IrResponse {
            id: body.get("id").and_then(Value::as_str).map(str::to_string),
            model: body.get("model").and_then(Value::as_str).map(str::to_string),
            // 第 330 轮：非流式响应也要带 usage（客户端要读 `message.usage`）
            usage: ir_usage_from_anthropic(body.get("usage")),
            ..Default::default()
        };
        ir.stop_reason = body.get("stop_reason").and_then(Value::as_str).map(|r| match r {
            "end_turn" => IrStopReason::EndTurn,
            "max_tokens" => IrStopReason::MaxTokens,
            "stop_sequence" => IrStopReason::StopSequence,
            "tool_use" => IrStopReason::ToolUse,
            other => IrStopReason::Other(other.to_string()),
        });
        if body.get("type").and_then(Value::as_str) == Some("error") {
            ir.losses.push(Loss::UnsupportedPart {
                path: "/type".into(),
                kind: "error(上游错误体不是消息)".into(),
            });
            return ir;
        }
        ir.parts = self.decode_content(body.get("content"), "/content", &mut ir.losses);
        ir
    }

    fn encode_response(&self, ir: &IrResponse) -> (Value, Vec<Loss>) {
        let mut losses = Vec::new();
        let mut blocks: Vec<Value> = Vec::new();
        for part in &ir.parts {
            match part {
                IrPart::Text(t) => blocks.push(json!({ "type": "text", "text": t })),
                IrPart::ToolUse { id, name, input } => blocks.push(json!({
                    "type": "tool_use", "id": id, "name": name, "input": input,
                })),
                other => losses.push(Loss::UnsupportedPart {
                    path: "/content".into(),
                    kind: part_kind(other).into(),
                }),
            }
        }
        let stop_reason = ir.stop_reason.as_ref().map(|r| r.as_str().to_string());
        (
            object(vec![
                ("id", json!(ir.id)),
                ("type", json!("message")),
                ("role", json!("assistant")),
                ("model", json!(ir.model)),
                ("content", Value::Array(blocks)),
                ("stop_reason", json!(stop_reason)),
                // 第 330 轮：把 IR 里的 usage 写回去（客户端会读 `message.usage`）
                ("usage", ir.usage.as_ref().map(anthropic_usage).unwrap_or(Value::Null)),
            ]),
            losses,
        )
    }
}

// ---------------------------------------------------------------- 流式（SSE 事件）

/// Anthropic Messages 的流式映射（[`StreamAdapter`]）。
///
/// # usage 为什么同时挂在两个事件上
///
/// Anthropic 把一次请求的用量分在**两个**事件里（`ta_core::metering::usage_paths` 也记着这件事）：
///
/// * `message_start.message.usage`：`input_tokens` + `cache_read_input_tokens`
///   + `cache_creation_input_tokens`（**只在这里出现**）；
/// * `message_delta.usage`：最终的 `output_tokens`（`message_start` 里那个 1 是占位值）。
///
/// 所以两处都要带出来（[`IrStreamEvent::Start`] / [`IrStreamEvent::Stop`] 的 `usage`），
/// 由调用方用 [`IrUsage::merge`] 合并。只在其中一处取值，流式请求的输入或输出 token
/// **必丢其一** —— 而且是静默地丢：流已经发出去了，账本只会少记。
///
/// # 容器帧归这里（第 328 轮）
///
/// Anthropic 的流**必须**以 `message_start` 开头、以 `message_stop` 收尾，而别的协议
/// （OpenAI 没有 `message_start`、Gemini 什么都不发）形状完全不同。所以
/// [`begin_frames`](StreamAdapter::begin_frames) / [`close_stream`](StreamAdapter::close_stream)
/// 由这一层给：会话层因此不认识任何协议，加一个协议不用改它。
///
/// # 无状态
///
/// 适配器是全局单例（`stream.rs` 里的 `static`）：块编号（[`EncodeCtx::index_of`]）、
/// 当前块（[`EncodeCtx::enter_block`]）、"开头/收尾发过没有"全在**每请求一份**的
/// [`EncodeCtx`] 里，否则并发请求之间会串味。
impl StreamAdapter for AnthropicAdapter {
    fn decode_stream_event(&self, data: &Value) -> Vec<IrStreamEvent> {
        // 连 `type` 都没有 → 不是 Anthropic 的事件（不是对象的载荷同样落到这里）。
        // `Vec::new()`（而不是 `Other`）：`Other` 留给"认得出是协议帧、但 IR 表达不了"的情况。
        let Some(kind) = data.get("type").and_then(Value::as_str) else {
            return Vec::new();
        };
        match kind {
            "message_start" => {
                let message = data.get("message");
                vec![IrStreamEvent::Start {
                    model: message
                        .and_then(|m| m.get("model"))
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    id: message
                        .and_then(|m| m.get("id"))
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    // ★ usage 第一处（见上面 impl 的说明）：input 侧只在这里
                    usage: ir_usage_from_anthropic(message.and_then(|m| m.get("usage"))),
                }]
            }
            // `index` 是 Anthropic 的**内容块序号**（文本与工具共用一个编号空间）
            // → IR 里它就是 `Slot` 的编号，目标协议的编号由 `EncodeCtx` 另行分配。
            "content_block_start" => {
                let index = block_index(data);
                let Some(block) = data.get("content_block") else {
                    return vec![IrStreamEvent::Other];
                };
                match block.get("type").and_then(Value::as_str).unwrap_or("") {
                    "tool_use" => vec![IrStreamEvent::ToolUseStart {
                        slot: Slot::Tool(index),
                        id: block.get("id").and_then(Value::as_str).unwrap_or("").to_string(),
                        name: block.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
                        // Anthropic 正常流里这里是 `{}`（真入参走 `input_json_delta` 分片）；
                        // 非空对象说明上游**一次给全**了 —— 那就照搬，别丢。
                        input: block
                            .get("input")
                            .filter(|v| v.as_object().is_some_and(|m| !m.is_empty()))
                            .cloned(),
                    }],
                    // text / thinking 块的开始只是"下面要发 delta 了"：内容在 `content_block_delta` 里。
                    // 在这里造一个空 `TextDelta` 就是**凭空造内容**（客户端会多收到一个空增量）。
                    // 顺带：正常流里 tool_use 块的 `input` 是 `{}`，真正的入参走 `input_json_delta`。
                    _ => vec![IrStreamEvent::Other],
                }
            }
            "content_block_delta" => {
                let index = block_index(data);
                let Some(delta) = data.get("delta") else {
                    return vec![IrStreamEvent::Other];
                };
                match delta.get("type").and_then(Value::as_str).unwrap_or("") {
                    // 空串的 `text` 也照样搬：它是一条合法的增量，不是"没有内容"
                    "text_delta" => delta
                        .get("text")
                        .and_then(Value::as_str)
                        .map(|text| {
                            vec![IrStreamEvent::TextDelta {
                                slot: Slot::Text(index),
                                text: text.to_string(),
                            }]
                        })
                        // 认得出是 `text_delta` 却没有 `text`：这一帧没有东西可搬
                        .unwrap_or_else(|| vec![IrStreamEvent::Other]),
                    "input_json_delta" => delta
                        .get("partial_json")
                        .and_then(Value::as_str)
                        .map(|fragment| {
                            vec![IrStreamEvent::ToolInputDelta {
                                slot: Slot::Tool(index),
                                fragment: fragment.to_string(),
                            }]
                        })
                        .unwrap_or_else(|| vec![IrStreamEvent::Other]),
                    // thinking_delta / signature_delta / citations_delta…：IR 里没有对应物
                    _ => vec![IrStreamEvent::Other],
                }
            }
            "message_delta" => {
                // ★ usage 第二处（见上面 impl 的说明）：output 侧只在这里
                let usage = ir_usage_from_anthropic(data.get("usage"));
                let reason = data
                    .get("delta")
                    .and_then(|d| d.get("stop_reason"))
                    .and_then(Value::as_str)
                    .map(stop_reason_of_anthropic);
                if reason.is_none() && usage.is_none() {
                    return vec![IrStreamEvent::Other]; // 两样都没有：这一帧没有东西可搬
                }
                vec![IrStreamEvent::Stop { reason, usage }]
            }
            // `content_block_stop` 是"这个块写完了"，而 **IR 里没有"块结束"这个语义** ——
            // 造一个 `Stop` 就是伪事件（一次流会出现两个终止事件），造别的更是编。
            // 块该关的时候由目标适配器按 `EncodeCtx::enter_block` / `leave_block` 关。
            //
            // `message_stop` 同理：结束的**语义**（stop_reason/usage）已经在 `message_delta`
            // 里搬过了，这里再造一个终止事件，目标协议会给客户端发两套收尾帧。
            // `ping` 与将来新出的事件类型：认得出是帧，但 IR 表达不了。
            // 三者一律 `Other`（丢还是转由调用方决定）。
            _ => vec![IrStreamEvent::Other],
        }
    }

    fn encode_stream_event(&self, ev: &IrStreamEvent, ctx: &mut EncodeCtx) -> Vec<StreamFrame> {
        match ev {
            IrStreamEvent::Start { model, id, usage } => {
                // ★ 去重：Anthropic 的 `message_start` 只能有一次（会话层的 `begin_frames`
                //   可能已经补过一帧 —— 上游是 OpenAI 时它**根本没有**这一帧）。
                if ctx.is_opened() {
                    return Vec::new();
                }
                ctx.mark_opened();
                vec![StreamFrame::Json(anthropic_message_start(
                    // 事件里的身份优先，其次才是上游帧记下来的身份（**都不编**：取不到写 null）
                    id.as_deref().or_else(|| ctx.upstream_id()),
                    if model.is_empty() { ctx.upstream_model() } else { Some(model.as_str()) },
                    usage.as_ref(),
                ))]
            }
            // ★ 切块：Anthropic 的 `index` 空间是文本与工具**共用**的，没有嵌套块
            //   ⇒ "开新块"必须先"关旧块"。这是每请求状态，只能由 `ctx` 记（编码器无状态）。
            IrStreamEvent::TextDelta { slot, text } => {
                let mut frames = switch_block(ctx, *slot);
                frames.push(StreamFrame::Json(object(vec![
                    ("type", json!("content_block_delta")),
                    ("index", json!(ctx.index_of(*slot))),
                    ("delta", json!({ "type": "text_delta", "text": text })),
                ])));
                frames
            }
            IrStreamEvent::ToolUseStart { slot, id, name, input } => {
                let mut frames = switch_block(ctx, *slot);
                // 源协议一次给全入参时放进 `content_block.input`（**这是合法的初始值**，
                // Anthropic 自己的正常流只是习惯给 `{}` 然后发分片）。
                // 丢掉它才是错的：从前这一格不存在，Gemini 上游的 `args` 会整份消失。
                let initial = input.clone().unwrap_or_else(|| json!({}));
                frames.push(StreamFrame::Json(object(vec![
                    ("type", json!("content_block_start")),
                    ("index", json!(ctx.index_of(*slot))),
                    (
                        "content_block",
                        json!({ "type": "tool_use", "id": id, "name": name, "input": initial }),
                    ),
                ])));
                frames
            }
            IrStreamEvent::ToolInputDelta { slot, fragment } => {
                // 同一个槽位不会重复开块：`enter_block` 对"已经开着的块"返回 `(None, false)`
                let mut frames = switch_block(ctx, *slot);
                frames.push(StreamFrame::Json(object(vec![
                    ("type", json!("content_block_delta")),
                    ("index", json!(ctx.index_of(*slot))),
                    ("delta", json!({ "type": "input_json_delta", "partial_json": fragment })),
                ])));
                frames
            }
            IrStreamEvent::Stop { reason, usage } => {
                let mut frames = Vec::new();
                // ★ **收尾前必须先关掉还开着的块**（第 328 轮修）。
                //
                // Anthropic 的顺序是 `content_block_stop` → `message_delta` → `message_stop`。
                // 漏掉关块，客户端会认为最后一个 `content_block_start` 后面缺了
                // `content_block_stop` —— 那是一个**半截的流**（Anthropic 自家 SDK 会挂着等）。
                // 这条只在"上游的收尾帧直接带 stop_reason"时才会走到（OpenAI 的
                // `finish_reason` 就是），所以它很容易漏 —— 由
                // `session::tests::upstream_done_is_not_translated_but_still_terminates_the_stream` 钉住。
                if reason.is_some() {
                    if let Some(slot) = ctx.leave_block() {
                        frames.push(StreamFrame::Json(object(vec![
                            ("type", json!("content_block_stop")),
                            ("index", json!(ctx.index_of(slot))),
                        ])));
                    }
                }
                if reason.is_some() || usage.is_some() {
                    // `delta` 必须是对象（Anthropic 的形状）。没有 stop_reason 时给一个空对象：
                    // **不编** `end_turn` —— "上游没说为什么结束"与"上游说正常结束"是两回事。
                    let delta = match reason {
                        Some(reason) => {
                            json!({ "stop_reason": reason.as_str(), "stop_sequence": null })
                        }
                        None => json!({}),
                    };
                    frames.push(StreamFrame::Json(object(vec![
                        ("type", json!("message_delta")),
                        ("delta", delta),
                        ("usage", usage.as_ref().map(anthropic_usage).unwrap_or(Value::Null)),
                    ])));
                }
                if reason.is_some() {
                    // 一次流只有**一个**终止帧：只带 usage 的那一帧（OpenAI `include_usage` 的末帧
                    // 翻译过来的）不再补 `message_stop`，否则客户端会看到两个"流结束"。
                    ctx.mark_closed();
                    frames.push(StreamFrame::Json(json!({ "type": "message_stop" })));
                }
                frames
            }
            // 没有对应物：**什么都不发**，而不是发一个假的心跳
            IrStreamEvent::Other => Vec::new(),
        }
    }

    /// **解码侧的内容损失**：IR 里没有位置的、**带内容**的增量。
    ///
    /// Anthropic 的 `content_block_delta` 有三种"IR 表达不了但装着内容"的形态：
    ///
    /// * `thinking_delta` —— 推理正文（extended thinking 的产物）；
    /// * `signature_delta` —— 推理块的签名（跟上面成对，缺了它内容不完整）；
    /// * `citations_delta` —— 引用片段。
    ///
    /// 它们**不是心跳**：丢的是内容，必须留痕。
    /// 反过来 `ping` / `content_block_stop` / `message_stop` **不记** ——
    /// 那些是流程帧，记了就是噪音，会把真损失淹没。
    fn decode_losses(&self, data: &Value) -> Vec<Loss> {
        if data.get("type").and_then(Value::as_str) != Some("content_block_delta") {
            return Vec::new();
        }
        let kind = data.pointer("/delta/type").and_then(Value::as_str).unwrap_or("");
        if !matches!(kind, "thinking_delta" | "signature_delta" | "citations_delta") {
            return Vec::new();
        }
        vec![Loss::UnsupportedPart {
            path: format!("/delta/{kind}"),
            kind: format!("{kind}（IR 里没有推理/引用增量的位置，内容被丢弃）"),
        }]
    }

    /// Anthropic 的流**必须**以 `message_start` 开头，而上游是 OpenAI 时那条流里
    /// **根本没有这一帧**（Gemini 也没有）⇒ 在写出第一批真实帧之前由这里补一帧。
    ///
    /// 身份只能用 `ctx` 里记下的上游 `id` / `model`（`frame_identity` 那一步存进去的）；
    /// **取不到就写 `null`，绝不编**（假 Model 名会被下游当成上游的原话）。
    /// 已经在 `Start` 里开过（上游本来就是 Anthropic）时不重复发。
    fn begin_frames(&self, ctx: &mut EncodeCtx) -> Vec<StreamFrame> {
        if ctx.is_opened() {
            return Vec::new();
        }
        ctx.mark_opened();
        vec![StreamFrame::Json(anthropic_message_start(
            ctx.upstream_id(),
            ctx.upstream_model(),
            None,
        ))]
    }

    /// 上游流结束：先把**还开着的块**关掉，再发 `message_stop`。
    ///
    /// 不关块就发 `message_stop`，客户端会认为最后一个 `content_block_start`
    /// 后面缺了 `content_block_stop` —— 那是一个半截的流（Anthropic 自己的 SDK 会挂着等）。
    /// 已经发过终止帧（`Stop { reason: Some(_) }`）时一个字节都不补：一次流一个终止帧。
    fn close_stream(&self, ctx: &mut EncodeCtx) -> Vec<StreamFrame> {
        if ctx.is_closed() {
            return Vec::new();
        }
        let mut frames = Vec::new();
        if let Some(slot) = ctx.leave_block() {
            frames.push(StreamFrame::Json(object(vec![
                ("type", json!("content_block_stop")),
                ("index", json!(ctx.index_of(slot))),
            ])));
        }
        ctx.mark_closed();
        frames.push(StreamFrame::Json(json!({ "type": "message_stop" })));
        frames
    }
}

/// 上游帧里的内容块序号（`index`）。缺失时按 0 处理 —— 它是**槽位身份**，
/// 认不出来就只能落到第一个槽位（编一个别的编号更糟：那会把增量接到别的块上）。
fn block_index(data: &Value) -> u32 {
    data.get("index").and_then(Value::as_u64).unwrap_or(0) as u32
}

/// 切到某个槽位对应的块：必要时**先关旧块、再开新块**，返回要写的那几帧。
///
/// 返回的帧数可能是 0（块没变：后续增量直接写 `content_block_delta`）、
/// 1（只开了新块，前面没有开着的块）、2（先关旧再开新）。
fn switch_block(ctx: &mut EncodeCtx, slot: Slot) -> Vec<StreamFrame> {
    let (previous, fresh) = ctx.enter_block(slot);
    if !fresh {
        return Vec::new();
    }
    let mut frames = Vec::new();
    if let Some(previous) = previous {
        frames.push(StreamFrame::Json(object(vec![
            ("type", json!("content_block_stop")),
            ("index", json!(ctx.index_of(previous))),
        ])));
    }
    // 文本块也要显式 `content_block_start`：Anthropic 的 `content_block_delta`
    // 必须落在一个已经开着的块上（'先开块、再发增量'是它的形状，不是可选装饰）。
    let block = match slot {
        Slot::Text(_) => json!({ "type": "text", "text": "" }),
        // 工具块的 `content_block_start` 由 `ToolUseStart` 发（只有它带 id / name）。
        // 走到这里说明这一帧是"没见到开始的入参分片"：那就**只关旧块**，不为它编一个
        // 空 id / 空 name 的 `content_block_start`（编出来的块客户端会当成一个真的工具调用）。
        // 分片本身照样按这个槽位的编号发出去，客户端至少不会拿到"半个 JSON"之外的假信息。
        Slot::Tool(_) => return frames,
    };
    frames.push(StreamFrame::Json(object(vec![
        ("type", json!("content_block_start")),
        ("index", json!(ctx.index_of(slot))),
        ("content_block", block),
    ])));
    frames
}

/// `message_start` 一帧。`id` / `model` 取不到就写 `null`（**不编**），usage 同理。
fn anthropic_message_start(
    id: Option<&str>,
    model: Option<&str>,
    usage: Option<&IrUsage>,
) -> Value {
    object(vec![
        ("type", json!("message_start")),
        (
            "message",
            object(vec![
                ("id", id.map(|id| json!(id)).unwrap_or(Value::Null)),
                ("type", json!("message")),
                ("role", json!("assistant")),
                ("model", model.map(|m| json!(m)).unwrap_or(Value::Null)),
                ("content", json!([])),
                ("stop_reason", Value::Null),
                ("stop_sequence", Value::Null),
                ("usage", usage.map(anthropic_usage).unwrap_or(Value::Null)),
            ]),
        ),
    ])
}

/// Anthropic 的 usage → [`IrUsage`]。没出现的字段**不写**（`None` ≠ `Some(0)`）。
///
/// `cache_creation.ephemeral_5m/1h` 的细分**不在这里展开**：那是 Anthropic 独有的计费维度
/// （OpenAI 的 usage 形状里根本没有），口径归一化是 `ta_core::metering` 的活。
/// Anthropic 的 usage 字段表（第 330 轮：读/写由 `shared` 的表驱动）。
///
/// * Anthropic 的 `input_tokens` **不含**缓存读/写（与 OpenAI 的 `prompt_tokens` 口径不同）；
/// * `reasoning` 是**空**的 —— Anthropic 的 usage 形状里没有"思考 token"这一格。
///   空数组是"这个协议没有这一格"的**显式声明**，不是漏填。
pub(crate) const USAGE: UsageShape = UsageShape {
    input: &["input_tokens"],
    output: &["output_tokens"],
    cache_read: &["cache_read_input_tokens"],
    cache_creation: &["cache_creation_input_tokens"],
    reasoning: &[],
};
fn ir_usage_from_anthropic(usage: Option<&Value>) -> Option<IrUsage> {
    read_usage(usage, &USAGE)
}
/// [`IrUsage`] → Anthropic 的 usage 对象。**没说的字段不写**（不补 0）。
fn anthropic_usage(usage: &IrUsage) -> Value {
    write_usage(usage, &USAGE)
}
/// Anthropic 的 `stop_reason` → IR（与 `decode_response` 同一口径，不另立一张表）。
fn stop_reason_of_anthropic(reason: &str) -> IrStopReason {
    match reason {
        "end_turn" => IrStopReason::EndTurn,
        "max_tokens" => IrStopReason::MaxTokens,
        "stop_sequence" => IrStopReason::StopSequence,
        "tool_use" => IrStopReason::ToolUse,
        other => IrStopReason::Other(other.to_string()),
    }
}

fn part_text(part: &IrPart) -> Option<String> {
    match part {
        IrPart::Text(t) => Some(t.clone()),
        _ => None,
    }
}

fn decode_tool_choice(tc: &Value, losses: &mut Vec<Loss>) -> IrToolChoice {
    match tc.get("type").and_then(Value::as_str).unwrap_or("") {
        "auto" => IrToolChoice::Auto,
        "any" => IrToolChoice::Required,
        "none" => IrToolChoice::None,
        "tool" => match tc.get("name").and_then(Value::as_str) {
            Some(name) => IrToolChoice::Tool(name.to_string()),
            None => {
                losses.push(Loss::UnsupportedField {
                    path: "/tool_choice".into(),
                    field: "tool_choice".into(),
                    detail: "type=tool 但没有 name".into(),
                });
                IrToolChoice::Auto
            }
        },
        other => {
            losses.push(Loss::UnsupportedField {
                path: "/tool_choice".into(),
                field: "tool_choice".into(),
                detail: format!("取值 `{other}` 没有 IR 表达"),
            });
            IrToolChoice::Auto
        }
    }
}

fn encode_content(parts: &[IrPart]) -> Value {
    if let [IrPart::Text(t)] = parts {
        return json!(t);
    }
    let blocks: Vec<Value> = parts
        .iter()
        .map(|p| match p {
            IrPart::Text(t) => json!({ "type": "text", "text": t }),
            IrPart::Image(IrImage::DataUrl { media_type, data }) => json!({
                "type": "image",
                "source": { "type": "base64", "media_type": media_type, "data": data },
            }),
            IrPart::Image(IrImage::Url(url)) => json!({
                "type": "image",
                "source": { "type": "url", "url": url },
            }),
            IrPart::ToolUse { id, name, input } => json!({
                "type": "tool_use", "id": id, "name": name, "input": input,
            }),
            IrPart::ToolResult { tool_use_id, content } => json!({
                "type": "tool_result",
                "tool_use_id": tool_use_id,
                "content": encode_content(content),
            }),
        })
        .collect();
    Value::Array(blocks)
}

// ================================================================ 测试

#[cfg(test)]
mod tests {
    //! 判据与 `stream.rs::tests` 一致：一条测试只有在"把被测的那件事破坏掉之后会红"时才算证据。
    //! 每条都同时钉住"该搬过去的东西"与"**不该**出现的东西"（伪事件 / 伪帧 / 伪编号）。

    use super::*;
    use pretty_assertions::assert_eq;

    fn anthropic() -> AnthropicAdapter {
        AnthropicAdapter
    }

    /// 恰好一个事件（多一个少一个都是契约被破坏）。
    fn only(events: Vec<IrStreamEvent>) -> IrStreamEvent {
        let mut events = events;
        if events.len() != 1 {
            panic!("应当恰好一个事件，实际 {events:?}");
        }
        events.remove(0)
    }

    /// 取 JSON 帧的载荷（`Literal` 帧在 Anthropic 侧根本不该出现）。
    fn json_of(frame: &StreamFrame) -> &Value {
        match frame {
            StreamFrame::Json(value) => value,
            StreamFrame::Literal(text) => panic!("Anthropic 侧不该有字面量帧：{text}"),
        }
    }

    /// 一帧里所有 `(type, index)`（测试里用来断言"先关旧块再开新块"的顺序）。
    fn kinds_and_indices(frames: &[StreamFrame]) -> Vec<(&str, u64)> {
        frames
            .iter()
            .map(|f| {
                let value = json_of(f);
                (value["type"].as_str().unwrap_or(""), value["index"].as_u64().unwrap_or(0))
            })
            .collect()
    }

    /// ★ usage：Anthropic 把用量分在 `message_start`（input 侧）与 `message_delta`
    /// （output 侧）两处 —— 一处都不许丢，而且两处都要能从编出来的帧再解回来。
    #[test]
    fn usage_split_across_two_events_is_never_lost() {
        // 第一处：input 侧（含缓存）只在 message_start
        let start = json!({ "type": "message_start", "message": {
            "id": "msg_1", "model": "claude-x",
            "usage": { "input_tokens": 25, "cache_read_input_tokens": 100,
                       "cache_creation_input_tokens": 7, "output_tokens": 1 },
        }});
        let (model, id, head_usage) = match only(anthropic().decode_stream_event(&start)) {
            IrStreamEvent::Start { model, id, usage } => {
                (model, id, usage.expect("message_start 必须带上 input 侧 usage"))
            }
            other => panic!("message_start 应当解成 Start：{other:?}"),
        };
        assert_eq!(model, "claude-x");
        assert_eq!(id, Some("msg_1".into()));
        assert_eq!(head_usage.input_tokens, Some(25));
        assert_eq!(head_usage.cache_read_input_tokens, Some(100));
        assert_eq!(head_usage.cache_creation_input_tokens, Some(7));

        // 第二处：output_tokens 只在 message_delta（stop_reason 也在这里，**原样带走**）
        let delta = json!({ "type": "message_delta", "delta": { "stop_reason": "max_tokens" },
                            "usage": { "output_tokens": 15 } });
        let (reason, tail_usage) = match only(anthropic().decode_stream_event(&delta)) {
            IrStreamEvent::Stop { reason, usage } => {
                (reason, usage.expect("message_delta 必须带上 output 侧 usage"))
            }
            other => panic!("message_delta 应当解成 Stop：{other:?}"),
        };
        assert_eq!(reason, Some(IrStopReason::MaxTokens));
        assert_eq!(tail_usage.output_tokens, Some(15));

        // 两处合并：一个数都不能少，且 output 必须被真值覆盖（`message_start` 里那个 1 是占位）
        let mut merged = head_usage.clone();
        merged.merge(&tail_usage);
        assert_eq!(merged.input_tokens, Some(25));
        assert_eq!(merged.cache_read_input_tokens, Some(100));
        assert_eq!(merged.cache_creation_input_tokens, Some(7));
        assert_eq!(merged.output_tokens, Some(15), "占位的 1 必须被 message_delta 的真值覆盖");

        // 编码方向：Start 带 usage 时，四个计数一个都不许丢（解回来必须相等）
        let mut ctx = EncodeCtx::new();
        let frames = anthropic().encode_stream_event(
            &IrStreamEvent::Start {
                model: "claude-x".into(),
                id: Some("msg_1".into()),
                usage: Some(head_usage.clone()),
            },
            &mut ctx,
        );
        assert_eq!(frames.len(), 1);
        assert_eq!(json_of(&frames[0])["type"], "message_start");
        assert_eq!(
            only(anthropic().decode_stream_event(json_of(&frames[0]))),
            IrStreamEvent::Start {
                model: "claude-x".into(),
                id: Some("msg_1".into()),
                usage: Some(head_usage),
            },
            "usage 三计数在往返里蒸发就是静默少记账"
        );

        // 认不出的 stop_reason **原样带走**（不硬塞 end_turn）
        let weird = json!({ "type": "message_delta", "delta": { "stop_reason": "pause_turn" } });
        assert_eq!(
            only(anthropic().decode_stream_event(&weird)),
            IrStreamEvent::Stop {
                reason: Some(IrStopReason::Other("pause_turn".into())),
                usage: None,
            }
        );
    }

    /// ★ `begin_frames`：上游是 OpenAI 时那条流里**根本没有** `message_start`，
    /// 而 Anthropic 的流必须以它开头 ⇒ 由目标适配器补一帧；已经在 `Start` 里开过就不重复。
    ///
    /// 把 `begin_frames` 改成返回空，会话层的
    /// `anthropic_target_always_opens_with_message_start` 会红；把去重去掉，这里会红。
    #[test]
    fn begin_frames_synthesises_message_start_exactly_once() {
        // 上游身份取得到 ⇒ 用真身份
        let mut ctx = EncodeCtx::new();
        ctx.remember_upstream(Some("chatcmpl-1".into()), Some("gpt-4o".into()));
        let frames = anthropic().begin_frames(&mut ctx);
        assert_eq!(frames.len(), 1);
        assert_eq!(json_of(&frames[0])["type"], "message_start");
        assert_eq!(json_of(&frames[0])["message"]["id"], "chatcmpl-1");
        assert_eq!(json_of(&frames[0])["message"]["model"], "gpt-4o");
        assert_eq!(json_of(&frames[0])["message"]["content"], json!([]));
        assert!(ctx.is_opened());
        // 第二次调用不许再补一帧（`message_start` 只能有一次）
        assert!(anthropic().begin_frames(&mut ctx).is_empty());

        // 身份取不到 ⇒ 写 `null`，**不编** id/model
        let mut unknown = EncodeCtx::new();
        let frames = anthropic().begin_frames(&mut unknown);
        assert_eq!(json_of(&frames[0])["message"]["id"], Value::Null);
        assert_eq!(json_of(&frames[0])["message"]["model"], Value::Null);

        // 已经在 `Start` 里开过（上游本来就是 Anthropic）⇒ 不重复
        let mut opened = EncodeCtx::new();
        let start = anthropic().encode_stream_event(
            &IrStreamEvent::Start {
                model: "claude-x".into(),
                id: Some("msg_1".into()),
                usage: None,
            },
            &mut opened,
        );
        assert_eq!(start.len(), 1, "第一次要真的发出去");
        assert!(anthropic().begin_frames(&mut opened).is_empty(), "开过就不许再开");
        // 再收到一个 `Start`（上游重复发了 message_start）也**不许**再发一帧
        assert!(anthropic()
            .encode_stream_event(
                &IrStreamEvent::Start { model: "claude-x".into(), id: None, usage: None },
                &mut opened,
            )
            .is_empty());
    }

    /// ★ `close_stream`：还开着的块**先关掉**再发 `message_stop`；已经终止过则一个字节都不补。
    ///
    /// 不关块就发 `message_stop`，客户端会认为流在半截处断了。把 `leave_block` 那段删掉，
    /// 这条与 `session.rs` 的 `upstream_done_is_not_translated_but_still_terminates_the_stream`
    /// 都会红。
    #[test]
    fn close_stream_closes_the_open_block_before_message_stop() {
        let mut ctx = EncodeCtx::new();
        // 先真的开一个文本块（上游给了内容）
        let frames = anthropic().encode_stream_event(
            &IrStreamEvent::TextDelta { slot: Slot::Text(0), text: "hi".into() },
            &mut ctx,
        );
        assert_eq!(
            kinds_and_indices(&frames),
            vec![("content_block_start", 0), ("content_block_delta", 0)]
        );

        let tail = anthropic().close_stream(&mut ctx);
        assert_eq!(
            kinds_and_indices(&tail),
            vec![("content_block_stop", 0), ("message_stop", 0)],
            "顺序必须是 关块 → message_stop"
        );
        assert!(ctx.is_closed());
        assert!(anthropic().close_stream(&mut ctx).is_empty(), "第二次调用不许再补");

        // 上游已经给过终止语义（`Stop { reason: Some(_) }`）⇒ 收尾不补任何东西
        let mut stopped = EncodeCtx::new();
        let frames = anthropic().encode_stream_event(
            &IrStreamEvent::Stop { reason: Some(IrStopReason::EndTurn), usage: None },
            &mut stopped,
        );
        assert_eq!(
            kinds_and_indices(&frames),
            vec![("message_delta", 0), ("message_stop", 0)],
            "终止帧是 message_delta + message_stop 两帧"
        );
        assert_eq!(json_of(&frames[0])["delta"]["stop_reason"], "end_turn");
        assert!(stopped.is_closed());
        assert!(anthropic().close_stream(&mut stopped).is_empty(), "一次流只能有一个终止帧");

        // `reason: None` 的用量补充帧**不发** message_stop（它不是终止判定）
        let mut usage_only = EncodeCtx::new();
        let frames = anthropic().encode_stream_event(
            &IrStreamEvent::Stop {
                reason: None,
                usage: Some(IrUsage { output_tokens: Some(5), ..Default::default() }),
            },
            &mut usage_only,
        );
        assert_eq!(kinds_and_indices(&frames), vec![("message_delta", 0)]);
        assert!(!usage_only.is_closed(), "补用量不是收尾");
    }

    /// 文本块与工具块**交替**时：index 空间共用 ⇒ 必须先关旧块再开新块，且编号稳定。
    #[test]
    fn switching_between_blocks_closes_the_previous_one_first() {
        let mut ctx = EncodeCtx::new();
        // 文本块
        let first = anthropic().encode_stream_event(
            &IrStreamEvent::TextDelta { slot: Slot::Text(0), text: "先说一句".into() },
            &mut ctx,
        );
        assert_eq!(
            kinds_and_indices(&first),
            vec![("content_block_start", 0), ("content_block_delta", 0)]
        );
        // 切到工具块：必须【关文本块 → 开工具块 → 增量】
        let second = anthropic().encode_stream_event(
            &IrStreamEvent::ToolUseStart {
                slot: Slot::Tool(0),
                id: "call_a".into(),
                name: "f".into(),
                input: None,
            },
            &mut ctx,
        );
        assert_eq!(
            kinds_and_indices(&second),
            vec![("content_block_stop", 0), ("content_block_start", 1)],
            "文本与工具共用一个 index 空间：先关旧块再开新块"
        );
        assert_eq!(json_of(&second[1])["content_block"]["type"], "tool_use");
        assert_eq!(json_of(&second[1])["content_block"]["id"], "call_a");
        assert_eq!(json_of(&second[1])["content_block"]["input"], json!({}));

        // 同一槽位的后续分片：**不重复开块**，编号也不变
        let third = anthropic().encode_stream_event(
            &IrStreamEvent::ToolInputDelta { slot: Slot::Tool(0), fragment: "{}".into() },
            &mut ctx,
        );
        assert_eq!(
            kinds_and_indices(&third),
            vec![("content_block_delta", 1)],
            "同一个工具槽位的分片不许再开一个块"
        );

        // 回到文本（新槽位）：先关工具块再开文本块，编号继续往前排
        let fourth = anthropic().encode_stream_event(
            &IrStreamEvent::TextDelta { slot: Slot::Text(1), text: "再说一句".into() },
            &mut ctx,
        );
        assert_eq!(
            kinds_and_indices(&fourth),
            vec![("content_block_stop", 1), ("content_block_start", 2), ("content_block_delta", 2)]
        );
        assert_eq!(json_of(&fourth[1])["content_block"]["type"], "text");
    }

    /// 并行工具调用：两个槽位拿到**不同的 index**，分片回到各自的块。
    #[test]
    fn parallel_tool_slots_get_distinct_block_indices() {
        let mut ctx = EncodeCtx::new();
        let a = anthropic().encode_stream_event(
            &IrStreamEvent::ToolUseStart {
                slot: Slot::Tool(0),
                id: "c0".into(),
                name: "a".into(),
                input: None,
            },
            &mut ctx,
        );
        let b = anthropic().encode_stream_event(
            &IrStreamEvent::ToolUseStart {
                slot: Slot::Tool(1),
                id: "c1".into(),
                name: "b".into(),
                input: None,
            },
            &mut ctx,
        );
        let a_index = json_of(&a[0])["index"].as_u64().unwrap();
        let b_index = json_of(b.last().unwrap())["index"].as_u64().unwrap();
        assert_ne!(a_index, b_index, "两个调用不许挤进同一个块");
        // 回到第一个调用的分片：必须落在它自己的编号上
        let tail = anthropic().encode_stream_event(
            &IrStreamEvent::ToolInputDelta { slot: Slot::Tool(0), fragment: "{}".into() },
            &mut ctx,
        );
        let last = json_of(tail.last().unwrap());
        assert_eq!(last["index"].as_u64().unwrap(), a_index);
    }

    /// `content_block_start` / `content_block_delta` 的槽位来自上游的 `index`
    /// （文本与工具**共用**编号空间，所以 `index: 1` 的工具不能落成 `Slot::Tool(0)`）。
    #[test]
    fn upstream_block_index_becomes_the_slot() {
        let tool = json!({ "type": "content_block_start", "index": 1,
            "content_block": { "type": "tool_use", "id": "toolu_1", "name": "weather",
                               "input": {} } });
        assert_eq!(
            only(anthropic().decode_stream_event(&tool)),
            IrStreamEvent::ToolUseStart {
                slot: Slot::Tool(1),
                id: "toolu_1".into(),
                name: "weather".into(),
                input: None
            }
        );
        let text = json!({ "type": "content_block_delta", "index": 0,
            "delta": { "type": "text_delta", "text": "你好" } });
        assert_eq!(
            only(anthropic().decode_stream_event(&text)),
            IrStreamEvent::TextDelta { slot: Slot::Text(0), text: "你好".into() }
        );
        let fragment = json!({ "type": "content_block_delta", "index": 3,
            "delta": { "type": "input_json_delta", "partial_json": "{\"city\":" } });
        assert_eq!(
            only(anthropic().decode_stream_event(&fragment)),
            IrStreamEvent::ToolInputDelta { slot: Slot::Tool(3), fragment: "{\"city\":".into() }
        );
        // 空串的 text 是**合法增量**，不是"没有内容"
        let empty = json!({ "type": "content_block_delta", "index": 0,
            "delta": { "type": "text_delta", "text": "" } });
        assert_eq!(
            only(anthropic().decode_stream_event(&empty)),
            IrStreamEvent::TextDelta { slot: Slot::Text(0), text: String::new() }
        );
    }

    /// 没有对应物的事件 → `Other`（**不造伪事件**）；不是本协议的载荷 → **0 个事件**。
    ///
    /// `content_block_stop` 特别重要：IR 里**没有**"块结束"这个语义，
    /// 为它造一个事件就是伪事件（客户端会收到一个上游没发过的语义）。
    #[test]
    fn frames_without_a_counterpart_yield_other_never_a_fake_event() {
        for frame in [
            json!({ "type": "ping" }),
            json!({ "type": "content_block_stop", "index": 0 }),
            json!({ "type": "message_stop" }),
            json!({ "type": "content_block_start", "index": 0,
                    "content_block": { "type": "text", "text": "" } }),
            json!({ "type": "content_block_delta", "index": 0,
                    "delta": { "type": "thinking_delta", "thinking": "…" } }),
            json!({ "type": "content_block_delta", "index": 0,
                    "delta": { "type": "text_delta" } }), // 认得出是 text_delta 却没有 text
            json!({ "type": "message_delta", "delta": {} }), // reason 与 usage 都没有
        ] {
            assert_eq!(
                anthropic().decode_stream_event(&frame),
                vec![IrStreamEvent::Other],
                "{frame} 在 IR 里没有对应物，必须是 Other（不是伪造的事件）"
            );
        }
        // 不是 Anthropic 的事件（非对象载荷、没有 type、别家的帧）→ 连事件都不产出
        assert!(anthropic().decode_stream_event(&json!(null)).is_empty());
        assert!(anthropic().decode_stream_event(&json!("[DONE]")).is_empty());
        assert!(anthropic().decode_stream_event(&json!({ "no_type": 1 })).is_empty());
        assert!(anthropic()
            .decode_stream_event(&json!({ "choices": [{ "delta": { "content": "x" } }] }))
            .is_empty());
        // `Other` 编码成**零帧**（不是假心跳）
        assert!(anthropic()
            .encode_stream_event(&IrStreamEvent::Other, &mut EncodeCtx::new())
            .is_empty());
    }
}
