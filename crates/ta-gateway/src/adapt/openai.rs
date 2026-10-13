//! OpenAI Chat Completions 适配器（`Protocol::Openai` / `Protocol::Custom`）。
//!
//! **只认文档化的形状**：不认识的字段/内容块一律登记 [`Loss`]，绝不静默丢。
//! 这是 IR 路径与 RAW 路径的分工 —— RAW 路径"一个字段都不动"，IR 路径要**核对**。

use serde_json::{json, Value};

use crate::adapt::ir::{
    canonicalize, data_url, json_string, object, parse_data_url, parse_json_string, text_part,
    IrImage, IrMessage, IrPart, IrRequest, IrResponse, IrRole, IrStopReason, IrTool, IrToolChoice,
};
use crate::adapt::loss::Loss;
use crate::adapt::stream::{EncodeCtx, IrStreamEvent, IrUsage, Slot, StreamAdapter, StreamFrame};

/// OpenAI 官方接受的数据 URL 图片格式（映射表）。
pub(crate) const IMAGE_MEDIA_TYPES: &[&str] =
    &["image/png", "image/jpeg", "image/jpg", "image/webp", "image/gif"];

pub struct OpenaiAdapter;

/// 已映射到 IR 的顶层字段。**其余非空字段全部登记成损失**（IR 路径要核对，不能装看不见）。
const KNOWN_TOP_LEVEL: &[&str] = &[
    "model",
    "stream",
    "messages",
    "tools",
    "tool_choice",
    "max_tokens",
    "max_completion_tokens",
    "temperature",
    "top_p",
    "stop",
];

impl OpenaiAdapter {
    fn decode_message(&self, msg: &Value, index: usize, out: &mut IrRequest) {
        let path = format!("/messages/{index}");
        let role = msg.get("role").and_then(Value::as_str).unwrap_or("").to_string();

        match role.as_str() {
            "system" | "developer" => {
                let parts = self.decode_content(
                    msg.get("content"),
                    &format!("{path}/content"),
                    &mut out.losses,
                );
                out.system.extend(parts);
            }
            "user" => {
                let parts = self.decode_content(
                    msg.get("content"),
                    &format!("{path}/content"),
                    &mut out.losses,
                );
                out.messages.push(IrMessage { role: IrRole::User, parts });
            }
            "assistant" => {
                let mut parts = self.decode_content(
                    msg.get("content"),
                    &format!("{path}/content"),
                    &mut out.losses,
                );
                if let Some(calls) = msg.get("tool_calls").and_then(Value::as_array) {
                    for (ci, call) in calls.iter().enumerate() {
                        let cpath = format!("{path}/tool_calls/{ci}");
                        match call.get("function") {
                            Some(func) => parts.push(self.decode_tool_call(
                                call,
                                func,
                                &cpath,
                                &mut out.losses,
                            )),
                            None => out.losses.push(Loss::UnsupportedPart {
                                path: cpath,
                                kind: "tool_call(no function)".to_string(),
                            }),
                        }
                    }
                } else if let Some(func) = msg.get("function_call") {
                    // 老形状：没有 `id`，而两侧协议都要 id 才能配对 → 交给目标协议检查报缺失
                    let name = func.get("name").and_then(Value::as_str).unwrap_or("").to_string();
                    let input = func
                        .get("arguments")
                        .and_then(Value::as_str)
                        .and_then(parse_json_string)
                        .unwrap_or_else(|| json!({}));
                    parts.push(IrPart::ToolUse { id: String::new(), name, input });
                }
                out.messages.push(IrMessage { role: IrRole::Assistant, parts });
            }
            "tool" => {
                // OpenAI 的工具结果是**独立消息**；IR 里是 user 消息里的 tool_result 块
                let id = msg.get("tool_call_id").and_then(Value::as_str).unwrap_or("").to_string();
                let content = self.decode_content(
                    msg.get("content"),
                    &format!("{path}/content"),
                    &mut out.losses,
                );
                out.messages.push(IrMessage {
                    role: IrRole::User,
                    parts: vec![IrPart::ToolResult { tool_use_id: id, content }],
                });
            }
            other => out.losses.push(Loss::UnsupportedRole {
                path: format!("{path}/role"),
                role: other.to_string(),
            }),
        }
    }

    fn decode_tool_call(
        &self,
        call: &Value,
        func: &Value,
        path: &str,
        losses: &mut Vec<Loss>,
    ) -> IrPart {
        let id = call.get("id").and_then(Value::as_str).unwrap_or("").to_string();
        let name = func.get("name").and_then(Value::as_str).unwrap_or("").to_string();
        let input = match func.get("arguments").and_then(Value::as_str) {
            Some(raw) => match parse_json_string(raw) {
                Some(v) => v,
                None => {
                    losses.push(Loss::UnsupportedPart {
                        path: format!("{path}/function/arguments"),
                        kind: "arguments(非 JSON)".to_string(),
                    });
                    json!({})
                }
            },
            None => json!({}),
        };
        IrPart::ToolUse { id, name, input }
    }

    /// 内容：字符串或块数组。`detail` 是**文档化的可忽略项**（：`detail` ↔ 忽略）。
    fn decode_content(
        &self,
        content: Option<&Value>,
        path: &str,
        losses: &mut Vec<Loss>,
    ) -> Vec<IrPart> {
        match content {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::String(s)) => vec![text_part(s.clone())],
            Some(Value::Array(items)) => {
                let mut parts = Vec::with_capacity(items.len());
                for (i, item) in items.iter().enumerate() {
                    let ipath = format!("{path}/{i}");
                    match item.get("type").and_then(Value::as_str).unwrap_or("") {
                        "text" => {
                            if let Some(t) = item.get("text").and_then(Value::as_str) {
                                parts.push(text_part(t));
                            }
                        }
                        "image_url" => {
                            if let Some(img) = decode_image(item, &ipath, losses) {
                                parts.push(img);
                            }
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
            Some(other) => {
                losses.push(Loss::UnsupportedPart { path: path.into(), kind: kind_of(other) });
                Vec::new()
            }
        }
    }
}

/// `{"type":"image_url","image_url":{"url":…}}` → IR 图片。
fn decode_image(item: &Value, path: &str, losses: &mut Vec<Loss>) -> Option<IrPart> {
    let url =
        item.get("image_url").and_then(|v| v.get("url")).and_then(Value::as_str).unwrap_or("");
    if url.is_empty() {
        losses.push(Loss::UnmappableImage {
            path: path.into(),
            detail: "image_url.url 缺失或不是字符串".to_string(),
        });
        return None;
    }
    Some(IrPart::Image(match parse_data_url(url) {
        Some(img) => img,
        None if url.starts_with("http://") || url.starts_with("https://") => {
            IrImage::Url(url.to_string())
        }
        None => {
            losses.push(Loss::UnmappableImage {
                path: path.into(),
                detail: format!("图片 URL 既不是 http(s) 也不是 data:…;base64（{}）", head(url)),
            });
            return None;
        }
    }))
}

impl super::ProtocolAdapter for OpenaiAdapter {
    fn protocol(&self) -> ta_core::types::Protocol {
        ta_core::types::Protocol::Openai
    }

    /// OpenAI 的硬约束：图片 media type 在白名单里、system 里不能有图片（规范只收文本）、
    /// 工具 id 不能为空（空 id 的调用在目标协议里无法配对 tool_result）。
    fn check_encodable(&self, ir: &IrRequest) -> Vec<Loss> {
        let mut losses = Vec::new();
        check_tool_ids(ir, &mut losses);
        for (i, part) in ir.system.iter().enumerate() {
            match part {
                IrPart::Text(_) => {}
                IrPart::Image(img) => losses.push(Loss::UnmappableImage {
                    path: format!("/system/{i}"),
                    detail: format!(
                        "OpenAI 的 system 消息只收文本，图片（{}）无处安放",
                        describe_image(img)
                    ),
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
            max_tokens: u64_of(body, &["max_tokens", "max_completion_tokens"]),
            temperature: f64_of(body, "temperature"),
            top_p: f64_of(body, "top_p"),
            stop: strings_of(body.get("stop")),
            ..Default::default()
        };
        if let Some(messages) = body.get("messages").and_then(Value::as_array) {
            for (i, msg) in messages.iter().enumerate() {
                self.decode_message(msg, i, &mut ir);
            }
        }
        if let Some(tools) = body.get("tools").and_then(Value::as_array) {
            for (i, tool) in tools.iter().enumerate() {
                let path = format!("/tools/{i}");
                match tool.get("type").and_then(Value::as_str) {
                    Some("function") => match tool.get("function") {
                        Some(func) => ir.tools.push(IrTool {
                            name: func.get("name").and_then(Value::as_str).unwrap_or("").into(),
                            description: func
                                .get("description")
                                .and_then(Value::as_str)
                                .map(str::to_string),
                            parameters: func
                                .get("parameters")
                                .cloned()
                                .unwrap_or_else(|| json!({})),
                        }),
                        None => ir
                            .losses
                            .push(Loss::UnsupportedPart { path, kind: "tool(no function)".into() }),
                    },
                    other => ir.losses.push(Loss::UnsupportedField {
                        path,
                        field: "tools[].type".into(),
                        detail: format!("工具类型 `{}` 没有 IR 表达", other.unwrap_or("")),
                    }),
                }
            }
        }
        if let Some(tc) = body.get("tool_choice") {
            ir.tool_choice = decode_tool_choice(tc, &mut ir.losses);
        }
        // 其余非空顶层字段：IR 表达不了 → **登记**（默认 on_loss: reject 会拒发）
        if let Some(map) = body.as_object() {
            for (k, v) in map {
                if v.is_null() || KNOWN_TOP_LEVEL.contains(&k.as_str()) {
                    continue;
                }
                ir.losses.push(Loss::UnsupportedField {
                    path: format!("/{k}"),
                    field: k.clone(),
                    detail: "OpenAI 协议字段，目标协议没有对应物".into(),
                });
            }
        }
        ir.messages = canonicalize(ir.messages);
        ir
    }

    fn encode_request(&self, ir: &IrRequest, upstream_model: &str) -> Value {
        let mut messages: Vec<Value> = Vec::new();
        if !ir.system.is_empty() {
            messages.push(object(vec![
                ("role", json!("system")),
                ("content", encode_content(&ir.system)),
            ]));
        }
        for msg in &ir.messages {
            match msg.role {
                IrRole::User => {
                    // 工具结果在 OpenAI 里必须是**独立消息**，且排在文本之前（IR 规范形态已保证顺序）
                    let mut rest: Vec<IrPart> = Vec::new();
                    for part in &msg.parts {
                        match part {
                            IrPart::ToolResult { tool_use_id, content } => {
                                messages.push(object(vec![
                                    ("role", json!("tool")),
                                    ("tool_call_id", json!(tool_use_id)),
                                    ("content", encode_content(content)),
                                ]));
                            }
                            other => rest.push(other.clone()),
                        }
                    }
                    if !rest.is_empty() {
                        messages.push(object(vec![
                            ("role", json!("user")),
                            ("content", encode_content(&rest)),
                        ]));
                    }
                }
                IrRole::Assistant => {
                    let text: Vec<IrPart> = msg
                        .parts
                        .iter()
                        .filter(|p| !matches!(p, IrPart::ToolUse { .. }))
                        .cloned()
                        .collect();
                    let calls: Vec<Value> = msg
                        .parts
                        .iter()
                        .filter_map(|p| match p {
                            IrPart::ToolUse { id, name, input } => Some(json!({
                                "id": id,
                                "type": "function",
                                "function": { "name": name, "arguments": json_string(input) },
                            })),
                            _ => None,
                        })
                        .collect();
                    let content = if text.is_empty() { Value::Null } else { encode_content(&text) };
                    let mut fields = vec![("role", json!("assistant")), ("content", content)];
                    if !calls.is_empty() {
                        fields.push(("tool_calls", Value::Array(calls)));
                    }
                    messages.push(object(fields));
                }
            }
        }

        let tools: Vec<Value> = ir
            .tools
            .iter()
            .map(|t| {
                object(vec![
                    ("type", json!("function")),
                    (
                        "function",
                        object(vec![
                            ("name", json!(t.name)),
                            ("description", json!(t.description)),
                            ("parameters", t.parameters.clone()),
                        ]),
                    ),
                ])
            })
            .collect();
        let tool_choice = match &ir.tool_choice {
            IrToolChoice::Auto => Value::Null,
            IrToolChoice::Required => json!("required"),
            IrToolChoice::None => json!("none"),
            IrToolChoice::Tool(name) => json!({ "type": "function", "function": { "name": name } }),
        };

        object(vec![
            ("model", json!(upstream_model)),
            ("stream", json!(ir.stream)),
            ("messages", Value::Array(messages)),
            ("tools", if tools.is_empty() { Value::Null } else { Value::Array(tools) }),
            ("tool_choice", tool_choice),
            ("max_tokens", ir.max_tokens.map(|n| json!(n)).unwrap_or(Value::Null)),
            ("temperature", ir.temperature.map(|v| json!(v)).unwrap_or(Value::Null)),
            ("top_p", ir.top_p.map(|v| json!(v)).unwrap_or(Value::Null)),
            (
                "stop",
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
            // 第 330 轮：非流式响应也要带 usage —— 不带的话，客户端看到的是
            // `usage: null`（上游明明给了）。**计费不读它**（账本读上游原始字节）。
            usage: ir_usage_from_openai(body.get("usage")),
            ..Default::default()
        };
        let Some(choice) = body.get("choices").and_then(Value::as_array).and_then(|c| c.first())
        else {
            ir.losses.push(Loss::UnsupportedPart {
                path: "/choices".into(),
                kind: "choices 为空".into(),
            });
            return ir;
        };
        ir.stop_reason = choice.get("finish_reason").and_then(Value::as_str).map(|r| match r {
            "stop" => IrStopReason::EndTurn,
            "length" => IrStopReason::MaxTokens,
            "tool_calls" => IrStopReason::ToolUse,
            other => IrStopReason::Other(other.to_string()),
        });
        if let Some(message) = choice.get("message") {
            ir.parts = self.decode_content(
                message.get("content"),
                "/choices/0/message/content",
                &mut ir.losses,
            );
            if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
                for (i, call) in calls.iter().enumerate() {
                    let func = call.get("function").cloned().unwrap_or_else(|| json!({}));
                    let part = self.decode_tool_call(
                        call,
                        &func,
                        &format!("/choices/0/message/tool_calls/{i}"),
                        &mut ir.losses,
                    );
                    ir.parts.push(part);
                }
            }
        }
        ir
    }

    fn encode_response(&self, ir: &IrResponse) -> (Value, Vec<Loss>) {
        let mut losses = Vec::new();
        let mut content = String::new();
        let mut calls: Vec<Value> = Vec::new();
        for part in &ir.parts {
            match part {
                IrPart::Text(t) => content.push_str(t),
                IrPart::ToolUse { id, name, input } => calls.push(json!({
                    "id": id,
                    "type": "function",
                    "function": { "name": name, "arguments": json_string(input) },
                })),
                other => losses.push(Loss::UnsupportedPart {
                    path: "/choices/0/message".into(),
                    kind: part_kind(other).into(),
                }),
            }
        }
        let finish = match &ir.stop_reason {
            Some(IrStopReason::MaxTokens) => "length",
            Some(IrStopReason::ToolUse) => "tool_calls",
            Some(IrStopReason::EndTurn) | Some(IrStopReason::StopSequence) | None => "stop",
            Some(IrStopReason::Other(reason)) => {
                losses.push(Loss::UnsupportedField {
                    path: "/choices/0/finish_reason".into(),
                    field: "finish_reason".into(),
                    detail: format!(
                        "上游停止原因 `{reason}` 在 OpenAI 里没有对应值（按 stop 给出）"
                    ),
                });
                "stop"
            }
        };
        let message = object(vec![
            ("role", json!("assistant")),
            ("content", if content.is_empty() { Value::Null } else { json!(content) }),
            ("tool_calls", if calls.is_empty() { Value::Null } else { Value::Array(calls) }),
        ]);
        (
            object(vec![
                ("id", json!(ir.id)),
                ("object", json!("chat.completion")),
                ("model", json!(ir.model)),
                ("choices", json!([{ "index": 0, "message": message, "finish_reason": finish }])),
                // 第 330 轮：把 IR 里的 usage 写回客户端。
                // IR 没有就传 `Value::Null` —— `object()` 会**丢掉 null 键**，所以
                // 结果是"这一格不写"，而不是写一个 `null`（"上游没说" ≠ "上游说了 0"）。
                ("usage", ir.usage.as_ref().map(openai_usage).unwrap_or(Value::Null)),
            ]),
            losses,
        )
    }
}

// ---------------------------------------------------------------- 流式（SSE 事件）

/// OpenAI Chat 的流式映射（[`StreamAdapter`]）。
///
/// **无状态是刻意的**：适配器是全局单例（`stream.rs` 里的 `static`），
/// 任何跨事件的记忆（"这是第几个工具调用"、"`[DONE]` 发过没有"）都由**每请求一份**的
/// [`EncodeCtx`] 持有，否则并发请求之间会串味 —— 那类 bug 只在线上并发时出现，
/// 本地单请求测不出来。
///
/// # 一帧可以产出**多个**事件（第 328 轮）
///
/// 旧签名返回 `Option<IrStreamEvent>`，逼着这一层"一帧里只能挑一件事搬"：
/// 真实上游的末帧常常同时带内容与 `finish_reason`（`delta.content` 的最后一截
/// 与 `finish_reason: "stop"` 同帧），旧形状只能二选一 —— 那不是取舍，那是丢数据。
/// 现在逐项搬，一帧里有什么就返回什么。
impl StreamAdapter for OpenaiAdapter {
    fn decode_stream_event(&self, data: &Value) -> Vec<IrStreamEvent> {
        // 不是 JSON 对象（`data: [DONE]` 由上层按字面处理）→ 这里根本没有事件。
        // `Vec::new()`（而不是 `Other`）：`Other` 留给"认得出是协议帧、但 IR 表达不了"的情况。
        if !data.is_object() {
            return Vec::new();
        }
        let usage = ir_usage_from_openai(data.get("usage"));
        let Some(choice) = data.get("choices").and_then(Value::as_array).and_then(|c| c.first())
        else {
            // `stream_options.include_usage` 的**末帧**：`choices` 是空数组，只有 usage。
            // ★ 这一帧是流式请求 output_tokens 的唯一来源，丢了它账本只能去估算
            //   （铁律：不猜不估算）。`usage: null` 不算数据 ——
            //   "上游说没有"和"上游说 0"是两回事。
            return match usage {
                Some(usage) => vec![IrStreamEvent::Stop { reason: None, usage: Some(usage) }],
                // `choices` 为空、usage 也没有：认得出是协议帧，但没东西可搬
                None => vec![IrStreamEvent::Other],
            };
        };
        let mut events: Vec<IrStreamEvent> = Vec::new();
        if let Some(delta) = choice.get("delta") {
            // 首帧带 `delta.role`，后面所有帧都不带 —— 这是 OpenAI 侧"流开始"的唯一标记。
            // 不能靠 `model`（它在**每一帧**都重复出现，按它发 Start 会发出几十个 Start）。
            if let Some(role) = delta.get("role").and_then(Value::as_str) {
                if !role.is_empty() {
                    events.push(IrStreamEvent::Start {
                        model: data.get("model").and_then(Value::as_str).unwrap_or("").to_string(),
                        id: data.get("id").and_then(Value::as_str).map(str::to_string),
                        usage: usage.clone(),
                    });
                }
            }
            if let Some(text) = delta.get("content").and_then(Value::as_str) {
                events.push(IrStreamEvent::TextDelta { slot: Slot::Text(0), text: text.into() });
            }
            // 新形状：`tool_calls` 数组。每一条**各自**成事件（并行工具调用靠 `slot` 区分），
            // 而且同一条里"开始"（`id` / `function.name`）与入参片（`function.arguments`）
            // 可以同时出现 —— 两个事件都要返回（旧签名在这里也只能二选一）。
            let calls = delta.get("tool_calls").and_then(Value::as_array);
            for (i, call) in calls.into_iter().flatten().enumerate() {
                // `index` 是上游给的"这是第几个调用"；缺了才退回数组下标（不编一个假的语义）
                let slot = Slot::Tool(
                    call.get("index").and_then(Value::as_u64).unwrap_or(i as u64) as u32,
                );
                let id = call.get("id").and_then(Value::as_str).filter(|id| !id.is_empty());
                let name = call
                    .get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(Value::as_str)
                    .filter(|name| !name.is_empty());
                if id.is_some() || name.is_some() {
                    events.push(IrStreamEvent::ToolUseStart {
                        slot,
                        // 缺的那一半给空串（**不编**一个 id / 名字）
                        id: id.unwrap_or("").to_string(),
                        name: name.unwrap_or("").to_string(),
                        input: None,
                    });
                }
                let args = call
                    .get("function")
                    .and_then(|f| f.get("arguments"))
                    .and_then(Value::as_str)
                    .filter(|args| !args.is_empty());
                if let Some(fragment) = args {
                    events.push(IrStreamEvent::ToolInputDelta {
                        slot,
                        fragment: fragment.to_string(),
                    });
                }
            }
            // 老形状 `function_call`（没有 tool_calls 数组）：它**没有 id**，
            // 而两侧协议都要 id 才能配对 tool_result → 这里老实给空 id，
            // 由目标协议那侧的 `check_tool_ids` 规则去拒（不在这里替它编一个）。
            // 同样地，"有 name"与"有 arguments"各自成事件，一帧里两样都有就出两个。
            if let Some(func) = delta.get("function_call") {
                let name = func.get("name").and_then(Value::as_str).filter(|n| !n.is_empty());
                if let Some(name) = name {
                    events.push(IrStreamEvent::ToolUseStart {
                        slot: Slot::Tool(0),
                        id: String::new(),
                        name: name.to_string(),
                        input: None,
                    });
                }
                let args =
                    func.get("arguments").and_then(Value::as_str).filter(|args| !args.is_empty());
                if let Some(fragment) = args {
                    events.push(IrStreamEvent::ToolInputDelta {
                        slot: Slot::Tool(0),
                        fragment: fragment.to_string(),
                    });
                }
            }
        }
        // 终帧标记：**与内容同帧也要返回**（旧签名在这里只能二选一，把内容或收尾丢掉一个）
        if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
            events.push(IrStreamEvent::Stop {
                reason: Some(stop_reason_of_openai(reason)),
                usage: usage.clone(),
            });
            return events;
        }
        if !events.is_empty() {
            return events;
        }
        if let Some(usage) = usage {
            // `delta` 与 `finish_reason` 都没有，但这一帧带了 usage（少数上游这么发）：
            // 它是"用量补充"，不是终止判定（`reason: None` 的既有语义）
            return vec![IrStreamEvent::Stop { reason: None, usage: Some(usage) }];
        }
        // 认得出是 OpenAI 的帧，但里面没有 IR 能表达的东西（keepalive、`delta:{}`、
        // 只有 `reasoning_content` 之类的新字段）→ `Other`：**不造内容**，丢还是转由调用方决定。
        vec![IrStreamEvent::Other]
    }

    fn encode_stream_event(&self, ev: &IrStreamEvent, ctx: &mut EncodeCtx) -> Vec<StreamFrame> {
        match ev {
            // `usage` 在起点没有可放的位置：OpenAI 只在 `include_usage` 的**末帧**给 usage，
            // 塞进首帧就是造一个上游不会发的形状。需要 input_tokens 的调用方应当把
            // `Start.usage` 合并进 `Stop.usage`（[`IrUsage::merge`]）——那是会话层的活。
            IrStreamEvent::Start { model, id, usage: _ } => {
                let mut chunk =
                    openai_chunk(json!({ "role": "assistant", "content": "" }), Value::Null);
                // `id` / `model` 有才写：**不编 id，也不编 created** —— 假时间戳/假 id
                // 会被下游当成上游的原话，比缺字段更难查。
                if let Some(id) = id {
                    chunk["id"] = json!(id);
                }
                if !model.is_empty() {
                    chunk["model"] = json!(model);
                }
                vec![StreamFrame::Json(chunk)]
            }
            IrStreamEvent::TextDelta { text, .. } => {
                vec![StreamFrame::Json(openai_chunk(json!({ "content": text }), Value::Null))]
            }
            // ★ `tool_calls[].index` 是 OpenAI 关联**分片**的键（同一个调用的入参分片共用它）。
            //   编号由每请求一份的 [`EncodeCtx::index_of`] 分配 —— 并行工具调用因此各有各的 index，
            //   不会互相污染（从前固定写 0，两个调用会挤进同一个槽）。
            IrStreamEvent::ToolUseStart { slot, id, name, input } => {
                let index = ctx.index_of(*slot);
                // 源协议一次给全入参时（Gemini 的 `args`），这里把它**序列化成一片** ——
                // OpenAI 只认 `arguments` 字符串，分不分片是上游的自由；
                // 丢掉它才是错的（从前那一格不存在，于是整份入参消失）。
                let arguments = input.as_ref().map(json_string).unwrap_or_default();
                vec![StreamFrame::Json(openai_chunk(
                    json!({ "tool_calls": [{
                        "index": index,
                        "id": id,
                        "type": "function",
                        "function": { "name": name, "arguments": arguments },
                    }]}),
                    Value::Null,
                ))]
            }
            IrStreamEvent::ToolInputDelta { slot, fragment } => {
                let index = ctx.index_of(*slot);
                vec![StreamFrame::Json(openai_chunk(
                    json!({ "tool_calls": [{
                        "index": index,
                        "function": { "arguments": fragment },
                    }]}),
                    Value::Null,
                ))]
            }
            IrStreamEvent::Stop { reason, usage } => {
                let mut frames = Vec::new();
                if let Some(reason) = reason {
                    frames.push(StreamFrame::Json(openai_chunk(
                        json!({}),
                        openai_finish_reason(reason),
                    )));
                }
                // usage 只能放在"choices 为空"的末帧里（OpenAI 的形状）。
                // 客户端没开 `stream_options.include_usage` 时这一帧是多余的 —— 但"多一帧"
                // 远好过"静默少记"；要不要丢由**看得见请求体**的会话层决定。
                //
                // ★ `reason: None`（只有用量的补充帧）**不在这里补一个终止帧**：
                //   一次流只能有一个终止帧，终止帧由带 `finish_reason` 的那一帧或
                //   `close_stream` 的 `[DONE]` 负责。
                if let Some(usage) = usage {
                    frames.push(StreamFrame::Json(json!({
                        "object": "chat.completion.chunk",
                        "choices": [],
                        "usage": openai_usage(usage),
                    })));
                }
                frames
            }
            // 没有对应物：**什么都不发**，而不是发一个假的心跳/空帧
            IrStreamEvent::Other => Vec::new(),
        }
    }

    /// OpenAI 家族的收尾是 `data: [DONE]` —— 一个**字面量**，不是 JSON。
    ///
    /// 从前它由会话层硬编码补（`if to == Anthropic {…} else {[DONE]}`），
    /// 第 328 轮起收尾形状归目标适配器：会话层不认识任何协议。
    /// 已经发过就不再补（一次流只能有一个终止帧）。
    fn close_stream(&self, ctx: &mut EncodeCtx) -> Vec<StreamFrame> {
        if ctx.is_closed() {
            return Vec::new();
        }
        ctx.mark_closed();
        vec![StreamFrame::Literal("[DONE]")]
    }
}

/// 一个增量 chunk 的骨架。
///
/// `finish_reason` 显式给 `null` 而不是省略：OpenAI 的每一帧都带这个键，
/// 缺键会让严格解析的客户端（pydantic 那种 required 字段）直接报错。
fn openai_chunk(delta: Value, finish_reason: Value) -> Value {
    json!({
        "object": "chat.completion.chunk",
        "choices": [{ "index": 0, "delta": delta, "finish_reason": finish_reason }],
    })
}

/// OpenAI 的 `finish_reason` → IR。
///
/// 与 `decode_response` 里那张表**同一口径**（这里不抽公共函数是为了不动既有路径）。
/// 认不出来的值走 [`IrStopReason::Other`] 原样带着，绝不硬塞成 `stop`。
fn stop_reason_of_openai(reason: &str) -> IrStopReason {
    match reason {
        "stop" => IrStopReason::EndTurn,
        "length" => IrStopReason::MaxTokens,
        "tool_calls" => IrStopReason::ToolUse,
        other => IrStopReason::Other(other.to_string()),
    }
}

/// IR 停止原因 → OpenAI 的 `finish_reason`。
///
/// 认不出来的值**原样带走**：OpenAI 的 `finish_reason` 也是开放字符串，
/// 硬塞成 `stop` 会把"被内容过滤截断"说成"正常结束"（用户会以为模型说完了）。
fn openai_finish_reason(reason: &IrStopReason) -> Value {
    match reason {
        IrStopReason::EndTurn | IrStopReason::StopSequence => json!("stop"),
        IrStopReason::MaxTokens => json!("length"),
        IrStopReason::ToolUse => json!("tool_calls"),
        IrStopReason::Other(raw) => json!(raw),
    }
}

/// OpenAI 家族的 usage 字段表（第 330 轮：读/写由 `shared` 的表驱动）。
///
/// * `total_tokens` **不搬** —— 它是 prompt+completion 的可推导量，搬它反而要在目标协议里
///   维护一个可能对不上的和；
/// * `cache_creation` 是**空**的 —— OpenAI 只报缓存**命中**、不报写入。
///   空数组是"这个协议没有这一格"的**显式声明**，不是漏填；
/// * `reasoning` 是 `output_tokens` 的**子集**（不是另一份总量），搬它不会重复计数。
pub(crate) const USAGE: UsageShape = UsageShape {
    input: &["prompt_tokens"],
    output: &["completion_tokens"],
    cache_read: &["prompt_tokens_details/cached_tokens"],
    cache_creation: &[],
    reasoning: &["completion_tokens_details/reasoning_tokens"],
};

/// ★ OpenAI 的 `prompt_tokens` **含**缓存命中，Anthropic 的 `input_tokens` **不含** ——
/// 口径不同是协议事实：本层只照搬字段，换算交给 `ta_core::metering` 的唯一口径。
fn ir_usage_from_openai(usage: Option<&Value>) -> Option<IrUsage> {
    read_usage(usage, &USAGE)
}

/// [`IrUsage`] → OpenAI 的 usage 对象。**没说的字段不写**（不补 0）。
fn openai_usage(usage: &IrUsage) -> Value {
    write_usage(usage, &USAGE)
}

fn decode_tool_choice(tc: &Value, losses: &mut Vec<Loss>) -> IrToolChoice {
    match tc {
        Value::String(s) => match s.as_str() {
            "auto" => IrToolChoice::Auto,
            "required" => IrToolChoice::Required,
            "none" => IrToolChoice::None,
            other => {
                losses.push(Loss::UnsupportedField {
                    path: "/tool_choice".into(),
                    field: "tool_choice".into(),
                    detail: format!("取值 `{other}` 没有 IR 表达"),
                });
                IrToolChoice::Auto
            }
        },
        Value::Object(_) => {
            match tc.get("function").and_then(|f| f.get("name")).and_then(Value::as_str) {
                Some(name) => IrToolChoice::Tool(name.to_string()),
                None => {
                    losses.push(Loss::UnsupportedField {
                        path: "/tool_choice".into(),
                        field: "tool_choice".into(),
                        detail: "对象形态里没有 function.name".into(),
                    });
                    IrToolChoice::Auto
                }
            }
        }
        _ => {
            losses.push(Loss::UnsupportedField {
                path: "/tool_choice".into(),
                field: "tool_choice".into(),
                detail: "不是字符串也不是对象".into(),
            });
            IrToolChoice::Auto
        }
    }
}

/// 文本内容：单块文本 → 字符串（兼容老中转的严格解析），否则用块数组。
fn encode_content(parts: &[IrPart]) -> Value {
    if let [IrPart::Text(t)] = parts {
        return json!(t);
    }
    let items: Vec<Value> = parts
        .iter()
        .filter_map(|p| match p {
            IrPart::Text(t) => Some(json!({ "type": "text", "text": t })),
            IrPart::Image(IrImage::DataUrl { media_type, data }) => Some(
                json!({ "type": "image_url", "image_url": { "url": data_url(media_type, data) } }),
            ),
            IrPart::Image(IrImage::Url(url)) => {
                Some(json!({ "type": "image_url", "image_url": { "url": url } }))
            }
            IrPart::ToolUse { .. } | IrPart::ToolResult { .. } => None,
        })
        .collect();
    Value::Array(items)
}

// ---------------------------------------------------------------- 公共检查/取值
//
// 第 328 轮：这些函数原来就住在这里，然后 `anthropic.rs` / `responses.rs` /
// `gemini.rs` 都写着 `use crate::adapt::openai::{…}`。那是一处**依赖方向错误**：
// `openai.rs` 是协议适配器、不是工具库；三个兄弟协议依赖它，意味着改一个取值辅助
// 会牵动另外三个协议，而且新写协议的人不可能想到通用约束住在**兄弟协议**里。
//
// 现在它们住在 `super::shared`（判据：**四个协议都会用到的函数不属于其中任何一个**）。
// 只有一项例外留在本文件：`IMAGE_MEDIA_TYPES` —— 那是**协议事实**，
// 而 Responses 与 chat 由同一家定义同一份图片白名单（所以它按"OpenAI 家族"共享是对的）。

use super::shared::{
    check_images, check_tool_ids, describe_image, f64_of, head, kind_of, part_kind, read_usage,
    strings_of, u64_of, write_usage, UsageShape,
};

// ================================================================ 测试

#[cfg(test)]
mod tests {
    //! 判据与 `stream.rs::tests` 一致：一条测试只有在"把被测的那件事破坏掉之后会红"时才算证据。
    //! 所以每条都同时钉住"该搬过去的东西"与"**不该**出现的东西"（编出来的值 / 形状）。

    use super::*;
    use pretty_assertions::assert_eq;

    fn openai() -> OpenaiAdapter {
        OpenaiAdapter
    }

    /// 取 JSON 帧的载荷。`Literal`（`[DONE]`）**不是 JSON**：拿它当 JSON 用就是测试自己写错了。
    fn json_of(frame: &StreamFrame) -> &Value {
        match frame {
            StreamFrame::Json(value) => value,
            StreamFrame::Literal(text) => panic!("这是字面量帧（{text}），不是 JSON"),
        }
    }

    /// ★ 第 328 轮的核心：**一帧里同时有内容与 `finish_reason` 时，两个事件都要解出来**。
    ///
    /// 旧签名返回 `Option<IrStreamEvent>`，这种帧只能二选一 —— 要么丢掉最后一块文字
    /// （用户少看见一段话），要么丢掉终止判定（客户端一直等下一条）。把 `decode_stream_event`
    /// 改回"只返回一个事件"，这条立刻红。
    #[test]
    fn one_frame_with_content_and_finish_reason_yields_two_events() {
        let frame = json!({
            "id": "chatcmpl-1", "model": "gpt-4o",
            "choices": [{ "index": 0, "delta": { "content": "。" }, "finish_reason": "stop" }],
        });
        assert_eq!(
            openai().decode_stream_event(&frame),
            vec![
                IrStreamEvent::TextDelta { slot: Slot::Text(0), text: "。".into() },
                IrStreamEvent::Stop { reason: Some(IrStopReason::EndTurn), usage: None },
            ],
            "内容与收尾同帧 ⇒ 顺序是「先内容、后 Stop」，两件都不许丢"
        );
        // usage 也搭在同一帧上时：终止事件必须**带上它**（output token 的唯一来源之一）
        let with_usage = json!({
            "choices": [{ "index": 0, "delta": { "content": "x" }, "finish_reason": "tool_calls" }],
            "usage": { "prompt_tokens": 9, "completion_tokens": 3 },
        });
        assert_eq!(
            openai().decode_stream_event(&with_usage),
            vec![
                IrStreamEvent::TextDelta { slot: Slot::Text(0), text: "x".into() },
                IrStreamEvent::Stop {
                    reason: Some(IrStopReason::ToolUse),
                    usage: Some(IrUsage {
                        input_tokens: Some(9),
                        output_tokens: Some(3),
                        ..Default::default()
                    }),
                },
            ]
        );
    }

    /// 文本增量：槽位固定 `Slot::Text(0)`（OpenAI 的文本增量本来就没有编号）。
    #[test]
    fn text_delta_uses_the_text_slot_and_round_trips() {
        let chunk = json!({ "choices": [{ "index": 0, "delta": { "content": "你好" },
                                          "finish_reason": null }] });
        assert_eq!(
            openai().decode_stream_event(&chunk),
            vec![IrStreamEvent::TextDelta { slot: Slot::Text(0), text: "你好".into() }]
        );
        let frames = openai().encode_stream_event(
            &IrStreamEvent::TextDelta { slot: Slot::Text(0), text: "你好".into() },
            &mut EncodeCtx::new(),
        );
        assert_eq!(frames.len(), 1);
        assert_eq!(json_of(&frames[0])["choices"][0]["delta"]["content"], "你好");
        assert_eq!(json_of(&frames[0])["choices"][0]["index"], 0);
        assert_eq!(
            openai().decode_stream_event(json_of(&frames[0])),
            vec![IrStreamEvent::TextDelta { slot: Slot::Text(0), text: "你好".into() }],
            "往返必须回到同一个事件"
        );
    }

    /// 工具调用：`index` → `Slot::Tool(index)`；开始与入参片可以同帧，两条都要出。
    #[test]
    fn tool_call_slots_come_from_the_upstream_index() {
        // 首片：带 id + name，arguments 为空 ⇒ 只有"开始"
        let head = json!({ "choices": [{ "delta": { "tool_calls": [
            { "index": 0, "id": "call_a", "type": "function",
              "function": { "name": "weather", "arguments": "" } }
        ]}}]});
        assert_eq!(
            openai().decode_stream_event(&head),
            vec![IrStreamEvent::ToolUseStart {
                slot: Slot::Tool(0),
                id: "call_a".into(),
                name: "weather".into(),
                input: None
            }]
        );
        // 后续片：只有 arguments ⇒ 只有入参增量，且**不重复** name
        let tail = json!({ "choices": [{ "delta": { "tool_calls": [
            { "index": 0, "function": { "arguments": "{\"city\":" } }
        ]}}]});
        assert_eq!(
            openai().decode_stream_event(&tail),
            vec![IrStreamEvent::ToolInputDelta {
                slot: Slot::Tool(0),
                fragment: "{\"city\":".into(),
            }]
        );
        // 首片里两样都有（id+name 与 arguments 同帧）⇒ **两条事件**
        let both = json!({ "choices": [{ "delta": { "tool_calls": [
            { "index": 2, "id": "call_b", "function": { "name": "time", "arguments": "{}" } }
        ]}}]});
        assert_eq!(
            openai().decode_stream_event(&both),
            vec![
                IrStreamEvent::ToolUseStart {
                    slot: Slot::Tool(2),
                    id: "call_b".into(),
                    name: "time".into(),
                    input: None
                },
                IrStreamEvent::ToolInputDelta { slot: Slot::Tool(2), fragment: "{}".into() },
            ],
            "上游给的 index=2 必须原样成为槽位（并行调用靠它区分）"
        );
        // 同一帧里的两个并行调用：各自一条事件，槽位不同
        let parallel = json!({ "choices": [{ "delta": { "tool_calls": [
            { "index": 0, "id": "c0", "function": { "name": "a" } },
            { "index": 1, "id": "c1", "function": { "name": "b" } },
        ]}}]});
        assert_eq!(
            openai().decode_stream_event(&parallel),
            vec![
                IrStreamEvent::ToolUseStart {
                    slot: Slot::Tool(0),
                    id: "c0".into(),
                    name: "a".into(),
                    input: None
                },
                IrStreamEvent::ToolUseStart {
                    slot: Slot::Tool(1),
                    id: "c1".into(),
                    name: "b".into(),
                    input: None
                },
            ]
        );
    }

    /// 老形状 `function_call` 同样处理，槽位固定 `Slot::Tool(0)`，id **不编**（空串）。
    #[test]
    fn legacy_function_call_shape_maps_to_the_first_tool_slot() {
        let frame = json!({ "choices": [{ "delta": { "function_call": {
            "name": "weather", "arguments": "{\"city\":\"SH\"}" } }}]});
        assert_eq!(
            openai().decode_stream_event(&frame),
            vec![
                IrStreamEvent::ToolUseStart {
                    slot: Slot::Tool(0),
                    id: String::new(), // 老形状没有 id：**不编**
                    name: "weather".into(),
                    input: None
                },
                IrStreamEvent::ToolInputDelta {
                    slot: Slot::Tool(0),
                    fragment: "{\"city\":\"SH\"}".into(),
                },
            ]
        );
    }

    /// usage：`include_usage` 的末帧（`choices: []`）落成 `Stop { reason: None }`，**不编**终止原因。
    #[test]
    fn usage_only_frame_never_invents_a_stop_reason() {
        let usage_only = json!({ "choices": [],
            "usage": { "prompt_tokens": 9, "completion_tokens": 3, "total_tokens": 12 } });
        assert_eq!(
            openai().decode_stream_event(&usage_only),
            vec![IrStreamEvent::Stop {
                reason: None,
                usage: Some(IrUsage {
                    input_tokens: Some(9),
                    output_tokens: Some(3),
                    ..Default::default()
                }),
            }],
            "只有 usage 的末帧必须落成 Stop.usage（reason 不许编）"
        );
        // `usage: null` 是"上游说没有"，**不是**"上游说 0" ⇒ 搬不出事件。
        // 统一规则：**认得出是协议帧、但这一帧没有可搬的内容 → `Other`**；
        // 空 `Vec` 留给"根本不是协议帧"（非对象载荷）。
        assert_eq!(
            openai().decode_stream_event(&json!({ "choices": [], "usage": null })),
            vec![IrStreamEvent::Other]
        );
        // 反过来：编码 `reason: None` 的 Stop **不发终止帧**（一次流一个终止帧），
        // 但用量一个数都不许丢
        let frames = openai().encode_stream_event(
            &IrStreamEvent::Stop {
                reason: None,
                usage: Some(IrUsage { output_tokens: Some(2), ..Default::default() }),
            },
            &mut EncodeCtx::new(),
        );
        assert_eq!(frames.len(), 1);
        assert_eq!(json_of(&frames[0])["choices"], json!([]));
        assert_eq!(json_of(&frames[0])["usage"]["completion_tokens"], 2);
        assert!(json_of(&frames[0])["usage"]["prompt_tokens"].is_null(), "上游没说的字段不许补 0");
    }

    /// 停止原因：映射不过去的值**原样带走**，不硬塞成"正常结束"。
    #[test]
    fn stop_reasons_are_carried_verbatim() {
        for (raw, expected) in [
            ("stop", IrStopReason::EndTurn),
            ("length", IrStopReason::MaxTokens),
            ("tool_calls", IrStopReason::ToolUse),
            ("content_filter", IrStopReason::Other("content_filter".into())),
        ] {
            let frame = json!({ "choices": [{ "delta": {}, "finish_reason": raw }] });
            assert_eq!(
                openai().decode_stream_event(&frame),
                vec![IrStreamEvent::Stop { reason: Some(expected), usage: None }],
                "finish_reason `{raw}` 的映射不许猜"
            );
        }
        // 编码方向：认不出的值原样带走（OpenAI 的 finish_reason 也是开放字符串）
        let frames = openai().encode_stream_event(
            &IrStreamEvent::Stop {
                reason: Some(IrStopReason::Other("content_filter".into())),
                usage: None,
            },
            &mut EncodeCtx::new(),
        );
        assert_eq!(frames.len(), 1, "有 reason 才发终止帧：{frames:?}");
        assert_eq!(json_of(&frames[0])["choices"][0]["finish_reason"], "content_filter");
    }

    /// 认得出是协议的帧、但 IR 表达不了 → `Other`；压根不是协议帧 → **0 个事件**。
    ///
    /// 两者不是一回事：前者要能被调用方看见（丢还是转由它决定），后者什么都不是。
    #[test]
    fn unrecognisable_payloads_yield_no_events_but_known_frames_yield_other() {
        assert!(openai().decode_stream_event(&json!(null)).is_empty());
        assert!(openai().decode_stream_event(&json!("[DONE]")).is_empty());
        // keepalive：`delta: {}`、没有 finish_reason、没有 usage
        let keepalive = json!({ "choices": [{ "index": 0, "delta": {}, "finish_reason": null }] });
        assert_eq!(openai().decode_stream_event(&keepalive), vec![IrStreamEvent::Other]);
        // `Other` 编码成**零帧**（不是假心跳）
        assert!(openai()
            .encode_stream_event(&IrStreamEvent::Other, &mut EncodeCtx::new())
            .is_empty());
    }

    /// ★ `close_stream`：OpenAI 家族的收尾是**字面量** `[DONE]`（不是 JSON），而且只能发一次。
    ///
    /// 第 328 轮之前它由会话层硬编码补；现在归目标适配器 ——
    /// 会话层因此不认识任何协议。把 `close_stream` 改成返回空/返回 JSON，这条立刻红。
    #[test]
    fn close_stream_emits_the_done_literal_exactly_once() {
        let mut ctx = EncodeCtx::new();
        assert_eq!(
            openai().close_stream(&mut ctx),
            vec![StreamFrame::Literal("[DONE]")],
            "OpenAI 的收尾是字面量帧"
        );
        assert!(ctx.is_closed(), "发过收尾就要标记，避免第二个终止帧");
        assert!(openai().close_stream(&mut ctx).is_empty(), "第二次调用不许再产出");
    }

    /// `[DONE]` 必须在真正的内容帧**之后**（这里验的是"它有内容载荷的最后出口"）。
    #[test]
    fn done_literal_is_not_a_json_frame() {
        let mut ctx = EncodeCtx::new();
        let frames = openai().close_stream(&mut ctx);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].payload(), "[DONE]", "载荷就是字面量本身");
        assert!(
            matches!(frames[0], StreamFrame::Literal(_)),
            "不许把它编成一个 `\"DONE\"` 字符串帧 —— 那会把协议字面量与 JSON 混为一谈"
        );
    }
}
