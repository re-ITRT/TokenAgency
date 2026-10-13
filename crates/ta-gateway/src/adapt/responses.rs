//! OpenAI Responses API 适配器（`POST /v1/responses`，`Protocol::OpenaiResponses`）。
//!
//! 与 [`crate::adapt::openai`]（Chat Completions）**同构**：只认文档化的形状，其余登记 [`Loss`]；
//! 差别全在这三个"结构性的地方"，它们不是改个字段名就能糊过去的：
//!
//! 1. **对话是一个扁平 item 数组**（`input`），而不是 `messages` + `message.tool_calls`。
//!    `function_call` / `function_call_output` / `reasoning` 与 `message` **平级**。
//!    所以解码是"逐 item 解出 IR 片段 → [`canonicalize`] 合并相邻同角色消息" ——
//!    直接照搬 Chat 的"按消息解"会在工具调用那一轮上得到两条永远对不上的 IR。
//! 2. **`instructions` 是顶层字段**（system 不占 input 的位置），与 Anthropic 的 `system`
//!    同构，因此 IR 的 `system` 与 `messages` 天然分开。
//! 3. **函数工具是扁平的**：`{type:"function", name, description, parameters}`，
//!    不套 Chat 的 `function: {…}` 那一层；`tool_choice` 同理（`{type:"function", name}`）。
//!
//! `usage` 的字段名也不同（`input_tokens` / `output_tokens`，而不是 `prompt_tokens` /
//! `completion_tokens`，见 [`ir_usage_from_responses`]）。
//!
//! # 为什么响应方向没有 usage
//!
//! [`IrResponse`] **没有** usage 字段：账本（`ta_core::metering`）直接读上游原始报文
//! （`normalize_openai_responses`，含 `input_tokens_details` / `output_tokens_details` 的细分），
//! 在 IR 里再放一份就是第二套口径。所以 `decode_response` 不把 usage 塞进 IR、也不为它记损失
//! （给每个响应都挂一条"假损失"会让 `on_loss: reject` 形同虚设）；流式那一侧 usage 有位置
//! （[`IrUsage`]），本文件里**只有**流式路径搬 usage。
//!
//! # 不编造（与两个既有适配器同一条纪律）
//!
//! * `status` 只映射成"上游真说过的"停止原因；IR 里没有停止原因时（`None`）**不写** `status`
//!   —— 填一个 `completed` 就是把"上游没说"冒充成"上游说正常结束"。
//! * 认不出的 item / content part / 事件，一律 [`Loss`] 或 [`IrStreamEvent::Other`]，**不伪造**。
//! * item 上的信封字段（`id` / `status` / `sequence_number`）**不搬也不登记**：IR 里没有位置，
//!   且它们不是内容（与 `openai.rs` 忽略消息里未知标量字段同一口径）。
//!
//! # 已知缺口（诚实列出）
//!
//! * 非流式的 `previous_response_id`（服务端会话续写）、`store`、`reasoning`、`include`、
//!   `metadata`、`text.format` 等 IR 表达不了的顶层字段：解码时登记成损失（默认 `reject` 会拒发），
//!   绝不静默吞掉。
//! * 流式**不累积** `output`：终止帧里只有 `status` / `incomplete_details` / `usage`，
//!   （`usage` 现在带得动：非流式走 [`crate::adapt::ir::IrResponse::usage`]，
//!   流式走 [`IrStreamEvent::Stop`] 的 `usage`），
//!   没有把已经发过的增量重新拼一遍的 `output` 数组 —— 本层是**无状态**的（[`StreamAdapter`] 是
//!   全局单例），拼装需要每请求一份状态，属于会话层。
//! * 内置工具（`web_search` / `file_search` / `computer_use` / `mcp` …）没有 IR 表达 → 登记损失。

use serde_json::{json, Value};

use super::shared::{
    check_images, check_tool_ids, describe_image, f64_of, head, part_kind, read_usage, u64_of,
    write_usage, UsageShape,
};
use crate::adapt::ir::{
    canonicalize, data_url, json_string, object, parse_data_url, parse_json_string, text_part,
    IrImage, IrMessage, IrPart, IrRequest, IrResponse, IrRole, IrStopReason, IrTool, IrToolChoice,
};
use crate::adapt::loss::Loss;
// `IMAGE_MEDIA_TYPES` 是**唯一**允许跨适配器共享的东西：Responses 与 chat/completions
// 由同一家定义同一份图片白名单，所以它按"OpenAI 家族"共享是对的（`shared.rs` 里不放进来的
// 理由也在这里 —— 它不是"通用工具"，是协议事实）。
use crate::adapt::openai::IMAGE_MEDIA_TYPES;
use crate::adapt::stream::{EncodeCtx, IrStreamEvent, IrUsage, Slot, StreamAdapter, StreamFrame};

pub struct ResponsesAdapter;

/// 已映射到 IR 的顶层字段。**其余非空字段全部登记成损失**
/// （`reasoning` / `store` / `include` / `metadata` / `text` / `previous_response_id` / `user` …）。
const KNOWN_TOP_LEVEL: &[&str] = &[
    "model",
    "input",
    "instructions",
    "stream",
    "max_output_tokens",
    "temperature",
    "top_p",
    "tools",
    "tool_choice",
];

impl ResponsesAdapter {
    /// `instructions` → IR 的 system。字符串是文档形态；数组形态（少数 SDK 这么发）按
    /// content part 解 —— 两种都认，认不出才登记损失。
    fn decode_system(&self, value: Option<&Value>, losses: &mut Vec<Loss>) -> Vec<IrPart> {
        match value {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::String(s)) => vec![text_part(s.clone())],
            Some(Value::Array(_)) => self.decode_content(value, "/instructions", losses),
            Some(_) => {
                losses.push(Loss::UnsupportedPart {
                    path: "/instructions".into(),
                    kind: "instructions(非字符串/非数组)".into(),
                });
                Vec::new()
            }
        }
    }

    /// `input`：**字符串**（= 一条 user 文本消息）或 **item 数组**。
    fn decode_input(
        &self,
        input: Option<&Value>,
        messages: &mut Vec<IrMessage>,
        system: &mut Vec<IrPart>,
        losses: &mut Vec<Loss>,
    ) {
        match input {
            None | Some(Value::Null) => {}
            Some(Value::String(s)) => {
                messages.push(IrMessage { role: IrRole::User, parts: vec![text_part(s.clone())] })
            }
            Some(Value::Array(items)) => {
                for (i, item) in items.iter().enumerate() {
                    self.decode_item(item, &format!("/input/{i}"), messages, system, losses);
                }
            }
            Some(_) => losses.push(Loss::UnsupportedPart {
                path: "/input".into(),
                kind: "input(非字符串/非数组)".into(),
            }),
        }
    }

    /// 一个 `input` item。工具调用/工具结果是**平级 item**（不是消息里的块），
    /// 所以这里直接产出 IR 消息，交给 [`canonicalize`] 合并成规范形态。
    fn decode_item(
        &self,
        item: &Value,
        path: &str,
        messages: &mut Vec<IrMessage>,
        system: &mut Vec<IrPart>,
        losses: &mut Vec<Loss>,
    ) {
        if !item.is_object() {
            losses.push(Loss::UnsupportedPart {
                path: path.into(), kind: "item(不是对象)".into()
            });
            return;
        }
        let kind = item.get("type").and_then(Value::as_str).unwrap_or("");
        let role = item.get("role").and_then(Value::as_str).unwrap_or("");
        match kind {
            // 有的客户端发消息时不写 `type`（只写 `role`）；文档化形态是带 `type` 的。
            // 两者都解成消息，缺 `type` 又缺 `role` 才算"认不出的 item"。
            "message" | "" => {
                if kind.is_empty() && role.is_empty() {
                    losses.push(Loss::UnsupportedPart {
                        path: path.into(),
                        kind: "(缺 type 与 role)".into(),
                    });
                    return;
                }
                let parts =
                    self.decode_content(item.get("content"), &format!("{path}/content"), losses);
                match role {
                    // `developer` 与 `system` 同义（都是"比用户更高优先级的指令"）；
                    // Anthropic 侧只有顶层 system，IR 也只有这一处能放。
                    "system" | "developer" => system.extend(parts),
                    "user" => messages.push(IrMessage { role: IrRole::User, parts }),
                    "assistant" => messages.push(IrMessage { role: IrRole::Assistant, parts }),
                    other => losses.push(Loss::UnsupportedRole {
                        path: format!("{path}/role"),
                        role: other.to_string(),
                    }),
                }
            }
            "function_call" => messages.push(IrMessage {
                role: IrRole::Assistant,
                parts: vec![decode_function_call(item, path, losses)],
            }),
            "function_call_output" => {
                let content = match item.get("output") {
                    // 字符串形态：工具输出**原样**是一个字符串（多半是 JSON 文本，但**不解析** ——
                    // 解析再序列化会改字节，而它就是上游/客户端给的原始载荷）
                    Some(Value::String(s)) => vec![text_part(s.clone())],
                    Some(Value::Array(_)) => {
                        self.decode_content(item.get("output"), &format!("{path}/output"), losses)
                    }
                    None | Some(Value::Null) => Vec::new(),
                    Some(_) => {
                        losses.push(Loss::UnsupportedPart {
                            path: format!("{path}/output"),
                            kind: "output(非字符串/非数组)".into(),
                        });
                        Vec::new()
                    }
                };
                messages.push(IrMessage {
                    role: IrRole::User,
                    parts: vec![IrPart::ToolResult {
                        tool_use_id: item
                            .get("call_id")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                        content,
                    }],
                });
            }
            // 思维链（`encrypted_content` / `summary`）：IR 没有推理内容的表达，
            // 文本化它就是**编内容**→ 丢弃并登记。
            "reasoning" => {
                losses.push(Loss::UnsupportedPart { path: path.into(), kind: "reasoning".into() })
            }
            // `item_reference`（要靠 previous_response_id 才能解析）、`custom_tool_call`、
            // 内置工具调用…：形状认得，语义我们搬不了 → 登记，不猜。
            other => losses.push(Loss::UnsupportedPart {
                path: path.into(),
                kind: if other.is_empty() { "(缺 type)".into() } else { other.to_string() },
            }),
        }
    }

    /// **响应方向的 `output` 数组**：`message` / `function_call` / `reasoning` 三类条目。
    ///
    /// 与 [`Self::decode_item`]（请求方向的 `input`）成对但**不同**：
    /// 响应里没有"工具结果"（那是请求侧的东西），文本块的名字是 `output_text`。
    ///
    /// 必须与 [`ProtocolAdapter::encode_response`] 的 `output` 形状**严格配对**，
    /// 否则响应方向的往返核对（[`crate::adapt::verify_no_loss_response`]）会红。
    fn decode_output(&self, output: Option<&Value>, losses: &mut Vec<Loss>) -> Vec<IrPart> {
        let mut parts = Vec::new();
        let items = match output {
            None | Some(Value::Null) => return parts,
            Some(Value::Array(items)) => items,
            Some(_) => {
                losses.push(Loss::UnsupportedPart {
                    path: "/output".into(),
                    kind: "output(非数组)".into(),
                });
                return parts;
            }
        };
        for (i, item) in items.iter().enumerate() {
            let path = format!("/output/{i}");
            match item.get("type").and_then(Value::as_str).unwrap_or("") {
                "message" => {
                    parts.extend(self.decode_content(item.get("content"), &path, losses));
                }
                "function_call" => {
                    let id = item.get("call_id").and_then(Value::as_str).unwrap_or("").to_string();
                    let name = item.get("name").and_then(Value::as_str).unwrap_or("").to_string();
                    let input = match item.get("arguments").and_then(Value::as_str) {
                        Some(s) => match parse_json_string(s) {
                            Some(v) => v,
                            None => {
                                // 入参不是合法 JSON：**不猜**。记损失并给一个空对象 ——
                                // 丢掉整个工具调用比"参数空了但调用还在"更糟。
                                losses.push(Loss::UnsupportedPart {
                                    path: format!("{path}/arguments"),
                                    kind: "工具入参不是合法 JSON".into(),
                                });
                                json!({})
                            }
                        },
                        // 与编码侧对齐：`arguments` 永远会写出来（空对象是 `"{}"`），
                        // 所以这里收不到 `None` 除非上游真的没给。
                        None => json!({}),
                    };
                    parts.push(IrPart::ToolUse { id, name, input });
                }
                // `reasoning` 的推理正文 IR 表达不了 → 登记，**不**当正文塞进去
                "reasoning" => losses
                    .push(Loss::UnsupportedPart { path, kind: "reasoning(推理内容)".into() }),
                other => losses.push(Loss::UnsupportedPart {
                    path,
                    kind: if other.is_empty() { "(缺 type)".into() } else { other.to_string() },
                }),
            }
        }
        parts
    }

    /// content：字符串或块数组。文本块有 `input_text`（用户侧）/ `output_text`（助手侧）两个
    /// 名字，语义都是"一段文本"，IR 里是同一个 [`IrPart::Text`]。
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
                        "input_text" | "output_text" => {
                            if let Some(t) = item.get("text").and_then(Value::as_str) {
                                parts.push(text_part(t));
                            }
                        }
                        "input_image" => {
                            if let Some(img) = decode_image(item, &ipath, losses) {
                                parts.push(img);
                            }
                        }
                        other => losses.push(Loss::UnsupportedPart {
                            path: ipath,
                            kind: if other.is_empty() {
                                "(缺 type)".into()
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
}

/// `{type:"function_call", call_id, name, arguments}` → IR 工具调用。
///
/// 请求的 `input` 项与响应的 `output` 项**同形**，所以两边共用这一份（两处各写一遍必然会漂）。
fn decode_function_call(item: &Value, path: &str, losses: &mut Vec<Loss>) -> IrPart {
    let input = match item.get("arguments") {
        // 文档默认：没有 `arguments` = 无入参
        None | Some(Value::Null) => json!({}),
        Some(Value::String(raw)) => match parse_json_string(raw) {
            Some(v) => v,
            None => {
                losses.push(Loss::UnsupportedPart {
                    path: format!("{path}/arguments"),
                    kind: "arguments(非 JSON)".into(),
                });
                json!({})
            }
        },
        Some(_) => {
            losses.push(Loss::UnsupportedPart {
                path: format!("{path}/arguments"),
                kind: "arguments(不是字符串)".into(),
            });
            json!({})
        }
    };
    IrPart::ToolUse {
        id: item.get("call_id").and_then(Value::as_str).unwrap_or("").to_string(),
        name: item.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
        input,
    }
}

/// `{type:"input_image", image_url}` → IR 图片。
///
/// `image_url` 在 Responses 里**是字符串**（Chat 是 `{image_url:{url}}` 嵌套）。
/// 两种都认：形状不同而语义等价，认出来比丢掉强；认不出的（`file_id` 引用、非法 URL）
/// 一律 [`Loss::UnmappableImage`]，**绝不用文本顶替**。
fn decode_image(item: &Value, path: &str, losses: &mut Vec<Loss>) -> Option<IrPart> {
    let raw = match item.get("image_url") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Object(map)) => {
            map.get("url").and_then(Value::as_str).unwrap_or("").to_string()
        }
        _ => String::new(),
    };
    if raw.is_empty() {
        let detail = if item.get("file_id").and_then(Value::as_str).is_some() {
            "input_image 用的是 file_id（Files API 引用），跨协议没有对应物".to_string()
        } else {
            "input_image 缺 image_url".to_string()
        };
        losses.push(Loss::UnmappableImage { path: path.into(), detail });
        return None;
    }
    if let Some(img) = parse_data_url(&raw) {
        return Some(IrPart::Image(img));
    }
    if raw.starts_with("http://") || raw.starts_with("https://") {
        return Some(IrPart::Image(IrImage::Url(raw)));
    }
    losses.push(Loss::UnmappableImage {
        path: path.into(),
        detail: format!("图片 URL 既不是 http(s) 也不是 data:…;base64（{}）", head(&raw)),
    });
    None
}

impl super::ProtocolAdapter for ResponsesAdapter {
    fn protocol(&self) -> ta_core::types::Protocol {
        ta_core::types::Protocol::OpenaiResponses
    }

    /// Responses 的硬约束：
    ///
    /// * `instructions` / system 只收文本；
    /// * 图片只出现在 **user** 消息里（assistant / system 消息里的 `input_image` 上游会 400）；
    /// * 函数工具必须有非空 `name`；
    /// * **没有 `stop` 参数** —— IR 里的停止序列在 Responses 里无处安放（`warn` 下也发不出去，
    ///   必须让调用方看见）。
    fn check_encodable(&self, ir: &IrRequest) -> Vec<Loss> {
        let mut losses = Vec::new();
        check_tool_ids(ir, &mut losses);
        for (i, part) in ir.system.iter().enumerate() {
            match part {
                IrPart::Text(_) => {}
                IrPart::Image(img) => losses.push(Loss::UnmappableImage {
                    path: format!("/system/{i}"),
                    detail: format!(
                        "Responses 的 instructions 只收文本，图片（{}）无处安放",
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
        check_assistant_images(ir, &mut losses);
        if !ir.stop.is_empty() {
            losses.push(Loss::UnsupportedField {
                path: "/stop".into(),
                field: "stop".into(),
                detail: "Responses 没有 stop / stop_sequences 参数，IR 里的停止序列无处安放".into(),
            });
        }
        for (i, tool) in ir.tools.iter().enumerate() {
            if tool.name.is_empty() {
                losses.push(Loss::MissingRequired {
                    path: format!("/tools/{i}/name"),
                    detail: "Responses 的函数工具必须有 name（空名字编不出自洽请求）".into(),
                });
            }
        }
        losses
    }

    fn decode_request(&self, body: &Value) -> IrRequest {
        let mut losses = Vec::new();
        let mut system = self.decode_system(body.get("instructions"), &mut losses);
        let mut messages = Vec::new();
        self.decode_input(body.get("input"), &mut messages, &mut system, &mut losses);

        let mut ir = IrRequest {
            model: body.get("model").and_then(Value::as_str).map(str::to_string),
            stream: body.get("stream").and_then(Value::as_bool).unwrap_or(false),
            max_tokens: u64_of(body, &["max_output_tokens"]),
            temperature: f64_of(body, "temperature"),
            top_p: f64_of(body, "top_p"),
            ..Default::default()
        };
        ir.system = system;
        // 规范形态：`function_call` / `function_call_output` 是独立 item，解出来是相邻同角色消息，
        // 必须合并（见 `ir.rs` 模块头）——否则一边三条一边一条，往返核对永远对不上。
        ir.messages = canonicalize(messages);
        ir.losses = losses;

        if let Some(tools) = body.get("tools").and_then(Value::as_array) {
            for (i, tool) in tools.iter().enumerate() {
                let path = format!("/tools/{i}");
                match tool.get("type").and_then(Value::as_str) {
                    Some("function") => ir.tools.push(IrTool {
                        name: tool.get("name").and_then(Value::as_str).unwrap_or("").into(),
                        description: tool
                            .get("description")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        parameters: tool.get("parameters").cloned().unwrap_or_else(|| json!({})),
                    }),
                    other => ir.losses.push(Loss::UnsupportedField {
                        path,
                        field: "tools[].type".into(),
                        detail: format!(
                            "工具类型 `{}` 没有 IR 表达（内置工具跨协议无处安放）",
                            other.unwrap_or("")
                        ),
                    }),
                }
            }
        }
        if let Some(tc) = body.get("tool_choice") {
            let choice = decode_tool_choice(tc, &mut ir.losses);
            ir.tool_choice = choice;
        }
        // 其余非空顶层字段（`reasoning` / `store` / `include` / `metadata` / `text` /
        // `previous_response_id` …）：IR 表达不了 → **登记**（默认 `on_loss: reject` 会拒发）
        if let Some(map) = body.as_object() {
            for (k, v) in map {
                if v.is_null() || KNOWN_TOP_LEVEL.contains(&k.as_str()) {
                    continue;
                }
                ir.losses.push(Loss::UnsupportedField {
                    path: format!("/{k}"),
                    field: k.clone(),
                    detail: "Responses 协议字段，目标协议没有对应物".into(),
                });
            }
        }
        ir
    }

    fn encode_request(&self, ir: &IrRequest, upstream_model: &str) -> Value {
        // system → 顶层 `instructions`（**不是**一条 role:"system" 的 input item：
        // Responses 的 system 走这个字段，塞进 input 是另一种形状，会让严格上游看不懂）
        //
        // 判定用"有没有文本块"而不是"system 是不是空"：system 里只有图片（编不出去，见
        // `check_encodable`）时不该写一个空字符串出去；而 `[Text("")]` 这种"上游真说了空串"
        // 要照写 —— 丢掉它会让往返核对多报一处不一致。
        let system_text = ir.system.iter().filter_map(part_text).collect::<Vec<_>>().join("\n\n");
        let instructions = if ir.system.iter().any(|p| matches!(p, IrPart::Text(_))) {
            Value::String(system_text)
        } else {
            Value::Null
        };

        let mut input: Vec<Value> = Vec::new();
        for msg in &ir.messages {
            match msg.role {
                IrRole::User => {
                    // 工具结果是**独立 item**，且排在文本之前（IR 规范形态已保证顺序）
                    let mut rest: Vec<IrPart> = Vec::new();
                    for part in &msg.parts {
                        match part {
                            IrPart::ToolResult { tool_use_id, content } => input.push(json!({
                                "type": "function_call_output",
                                "call_id": tool_use_id,
                                "output": encode_tool_output(content),
                            })),
                            other => rest.push(other.clone()),
                        }
                    }
                    if !rest.is_empty() {
                        input.push(json!({
                            "type": "message",
                            "role": "user",
                            "content": encode_input_content(IrRole::User, &rest),
                        }));
                    }
                }
                IrRole::Assistant => {
                    // 文本在一条 message item 里，工具调用是**后面的独立 item**
                    // （规范形态：assistant 文本在前、ToolUse 在后）
                    let text_parts: Vec<IrPart> = msg
                        .parts
                        .iter()
                        .filter(|p| !matches!(p, IrPart::ToolUse { .. }))
                        .cloned()
                        .collect();
                    if !text_parts.is_empty() {
                        input.push(json!({
                            "type": "message",
                            "role": "assistant",
                            "content": encode_input_content(IrRole::Assistant, &text_parts),
                        }));
                    }
                    for part in &msg.parts {
                        if let IrPart::ToolUse { id, name, input: args } = part {
                            input.push(json!({
                                "type": "function_call",
                                "call_id": id,
                                "name": name,
                                "arguments": json_string(args),
                            }));
                        }
                    }
                }
            }
        }

        // 扁平形状：`{type, name, description, parameters}` —— **不套** Chat 的 `function: {…}`
        let tools: Vec<Value> = ir
            .tools
            .iter()
            .map(|t| {
                object(vec![
                    ("type", json!("function")),
                    ("name", json!(t.name)),
                    ("description", json!(t.description)),
                    ("parameters", t.parameters.clone()),
                ])
            })
            .collect();
        let tool_choice = match &ir.tool_choice {
            IrToolChoice::Auto => Value::Null, // Responses 的默认就是 auto：不写
            IrToolChoice::Required => json!("required"),
            IrToolChoice::None => json!("none"),
            IrToolChoice::Tool(name) => json!({ "type": "function", "name": name }),
        };

        // `stop` **不写**：Responses 没有这个参数，写出去就是编一个上游不认识的字段；
        // 这件事由 `check_encodable` 的损失登记负责让调用方看见。
        object(vec![
            ("model", json!(upstream_model)),
            ("input", Value::Array(input)),
            ("instructions", instructions),
            ("stream", json!(ir.stream)),
            ("max_output_tokens", ir.max_tokens.map(|n| json!(n)).unwrap_or(Value::Null)),
            ("temperature", ir.temperature.map(|v| json!(v)).unwrap_or(Value::Null)),
            ("top_p", ir.top_p.map(|v| json!(v)).unwrap_or(Value::Null)),
            ("tools", if tools.is_empty() { Value::Null } else { Value::Array(tools) }),
            ("tool_choice", tool_choice),
        ])
    }

    fn decode_response(&self, body: &Value) -> IrResponse {
        let mut ir = IrResponse {
            id: body.get("id").and_then(Value::as_str).map(str::to_string),
            model: body.get("model").and_then(Value::as_str).map(str::to_string),
            // 第 330 轮：非流式响应也要带 usage
            usage: ir_usage_from_responses(body.get("usage")),
            ..Default::default()
        };
        let parts = self.decode_output(body.get("output"), &mut ir.losses);
        ir.parts = parts;
        let has_tool_use = ir.parts.iter().any(|p| matches!(p, IrPart::ToolUse { .. }));
        ir.stop_reason = stop_reason_of_response(Some(body), has_tool_use);
        if ir.stop_reason.is_none() {
            // `status` 是 Responses 的必填字段：没有它说明这不是一个正常的 response 对象
            // （错误体 / 被截断的报文）→ 登记，**不替它编一个 completed**
            ir.losses.push(Loss::MissingRequired {
                path: "/status".into(),
                detail: "响应里没有 status，无法判断停止原因（不编一个 completed）".into(),
            });
        }
        if body.get("status").and_then(Value::as_str) == Some("failed") {
            ir.losses.push(Loss::UnsupportedPart {
                path: "/error".into(),
                kind: "failed(上游生成失败)".into(),
            });
        }
        if body.get("status").and_then(Value::as_str) == Some("incomplete")
            && body.pointer("/incomplete_details/reason").and_then(Value::as_str).is_none()
        {
            ir.losses.push(Loss::UnsupportedPart {
                path: "/incomplete_details".into(),
                kind: "incomplete(未给 reason)".into(),
            });
        }
        // `usage` **不进 IR**（[`IrResponse`] 没有这个字段，账本读原始报文，见模块头）。
        ir
    }

    fn encode_response(&self, ir: &IrResponse) -> (Value, Vec<Loss>) {
        let mut losses = Vec::new();
        let mut output: Vec<Value> = Vec::new();
        let mut calls: Vec<Value> = Vec::new();
        // 每个文本块**各留一个 `output_text` 块**（不合并成一段）：合并会改变 IR 的块数，
        // 响应方向的往返核对会把它当成"编码器改了东西"。
        let mut texts: Vec<Value> = Vec::new();
        for part in &ir.parts {
            match part {
                IrPart::Text(t) => texts.push(json!({ "type": "output_text", "text": t })),
                IrPart::ToolUse { id, name, input } => calls.push(json!({
                    "type": "function_call",
                    "call_id": id,
                    "name": name,
                    "arguments": json_string(input),
                })),
                other => losses.push(Loss::UnsupportedPart {
                    path: "/output".into(),
                    kind: part_kind(other).into(),
                }),
            }
        }
        if !texts.is_empty() {
            output.push(json!({
                "type": "message",
                "role": "assistant",
                "content": texts,
            }));
        }
        output.extend(calls);

        let (status, incomplete_details) = match &ir.stop_reason {
            Some(reason) => {
                let (status, reason_text) = status_of_stop_reason(reason);
                if matches!(&ir.stop_reason, Some(IrStopReason::StopSequence)) {
                    // Responses 的 `incomplete_details.reason` 只有 max_output_tokens /
                    // content_filter 两个文档值；`stop_sequence` 原样带走并留痕（不谎报截断原因）
                    losses.push(Loss::UnsupportedField {
                        path: "/incomplete_details/reason".into(),
                        field: "stop_reason".into(),
                        detail: "Responses 没有 stop_sequence 这个不完整原因（原样带走）".into(),
                    });
                }
                (json!(status), reason_text.map(|r| json!({ "reason": r })).unwrap_or(Value::Null))
            }
            None => {
                // 不编：`status` 是必填，但"上游没说为什么结束" ≠ "上游说正常结束"
                losses.push(Loss::MissingRequired {
                    path: "/status".into(),
                    detail: "IR 里没有停止原因，Responses 的 status 不替你编一个 completed".into(),
                });
                (Value::Null, Value::Null)
            }
        };

        (
            object(vec![
                ("id", json!(ir.id)),
                ("object", json!("response")),
                ("status", status),
                ("model", json!(ir.model)),
                ("output", Value::Array(output)),
                ("incomplete_details", incomplete_details),
                // 第 330 轮：IR 的响应形态现在**有** usage 了，所以写回去。
                // （从前这里写着"不写：IR 里没有它" —— 那句话现在不成立了。）
                ("usage", ir.usage.as_ref().map(responses_usage).unwrap_or(Value::Null)),
            ]),
            losses,
        )
    }
}

// ---------------------------------------------------------------- 流式（SSE 事件）

/// Responses 的流式映射（[`StreamAdapter`]）。
///
/// # 终止判定的唯一来源
///
/// Responses 的流**不用** `data: [DONE]`：它以 `response.completed` / `response.incomplete` /
/// `response.failed` 收尾。所以那一帧必须落成 [`IrStreamEvent::Stop`]（带从 `response.usage`
/// 搬来的用量）—— 少搬它，跨协议流就永远不结束（客户端一直等）。
///
/// # 身份在 `response` 里面（不是顶层）
///
/// Responses 的 SSE 帧顶层**没有** `id` / `model`（它们在 `response` 对象里），
/// 所以必须覆写 [`StreamAdapter::frame_identity`] —— 不覆写，跨协议流式合成
/// `message_start` 时模型名与报文 id 会永远是 `null`（上游身份取不到）。
///
/// # 无状态是刻意的
///
/// 适配器是全局单例（`stream.rs` 里的 `static`）：任何跨事件的记忆
/// （"这是第几个 output item"、"流开过头没有"）都由**每请求一份**的 [`EncodeCtx`] 持有，
/// 否则并发请求之间会串味。所以这里的 `output_index` / `content_index` 固定写 0，
/// `item_id` **不编**（假 id 比缺字段更难查）。
impl StreamAdapter for ResponsesAdapter {
    fn decode_stream_event(&self, data: &Value) -> Vec<IrStreamEvent> {
        // 连 `type` 都没有 → 不是 Responses 的事件（非对象载荷、`[DONE]` 之类同样落到这里）。
        // Responses 的流以 `response.completed` 收尾，`[DONE]` 在它这里根本不存在。
        // `Vec::new()`（而不是 `Other`）：`Other` 留给"认得出是协议帧、但 IR 表达不了"的情况。
        let Some(kind) = data.get("type").and_then(Value::as_str) else {
            return Vec::new();
        };
        match kind {
            "response.created" => {
                let response = data.get("response");
                vec![IrStreamEvent::Start {
                    model: response
                        .and_then(|r| r.get("model"))
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    id: response
                        .and_then(|r| r.get("id"))
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    // usage 有位置就带上：Anthropic 的 input 侧用量只出现在 message_start，
                    // 上游若是 Anthropic，这里就是它在 IR 里的落脚点。
                    usage: ir_usage_from_responses(response.and_then(|r| r.get("usage"))),
                }]
            }
            // `response.in_progress` 只有**真带了 usage** 时才值得搬（否则这一帧在 IR 里没有
            // 东西可表达，落 `Other`）。reason 为 `None` 是 [`IrStreamEvent::Stop`] 的既有语义：
            // "这是用量补充，不是终止判定"。
            "response.in_progress" => {
                match ir_usage_from_responses(data.pointer("/response/usage")) {
                    Some(usage) => {
                        vec![IrStreamEvent::Stop { reason: None, usage: Some(usage) }]
                    }
                    // 认得出是本协议的帧，但这一帧里没有 IR 能表达的东西 → `Other`
                    None => vec![IrStreamEvent::Other],
                }
            }
            "response.output_item.added" => {
                let Some(item) = data.get("item") else {
                    return vec![IrStreamEvent::Other];
                };
                match item.get("type").and_then(Value::as_str).unwrap_or("") {
                    "function_call" => {
                        // 槽位优先用上游给的 `output_index`（它才是"第几个 item"），
                        // 缺了才退回 item 自带的 `index`
                        vec![IrStreamEvent::ToolUseStart {
                            slot: Slot::Tool(output_index(data, item)),
                            id: item
                                .get("call_id")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string(),
                            name: item
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string(),
                            // ★ 这一帧自带的 `arguments`：上游**一次给全**入参时就在这里
                            //（正常流是空串，入参走后面的
                            //  `response.function_call_arguments.delta` 分片）。
                            // 第 330 轮之前没有这一格 ⇒ 一次给全的入参**整份丢掉**。
                            input: item
                                .get("arguments")
                                .and_then(Value::as_str)
                                .filter(|a| !a.trim().is_empty())
                                .and_then(parse_json_string),
                        }]
                    }
                    // `message` / `reasoning` 的 item 开始只是"下面要发 delta 了"：内容在 delta 里。
                    // 在这里造一个空 `TextDelta` 就是**凭空造内容**（同 `anthropic.rs`）。
                    _ => vec![IrStreamEvent::Other],
                }
            }
            // 空串的 delta 也是**合法增量**，不是"没有内容"
            "response.output_text.delta" => match data.get("delta").and_then(Value::as_str) {
                Some(delta) => vec![IrStreamEvent::TextDelta {
                    slot: Slot::Text(output_index(data, data)),
                    text: delta.to_string(),
                }],
                None => vec![IrStreamEvent::Other],
            },
            "response.function_call_arguments.delta" => {
                match data.get("delta").and_then(Value::as_str) {
                    Some(fragment) => vec![IrStreamEvent::ToolInputDelta {
                        slot: Slot::Tool(output_index(data, data)),
                        fragment: fragment.to_string(),
                    }],
                    None => vec![IrStreamEvent::Other],
                }
            }
            // 三个终止事件：`reason` 由 `status` + `incomplete_details` + output 里的
            // `function_call` 一起决定（**共用** `decode_response` 的那张表）。
            "response.completed" | "response.incomplete" | "response.failed" => {
                let response = data.get("response");
                let usage = ir_usage_from_responses(response.and_then(|r| r.get("usage")));
                let reason = stop_reason_of_response(response, output_has_function_call(response));
                if reason.is_none() && usage.is_none() {
                    // 终止帧里一样能搬的东西都没有（例如一个空壳）：归类，不编
                    return vec![IrStreamEvent::Other];
                }
                vec![IrStreamEvent::Stop { reason, usage }]
            }
            // `content_part.added` / `output_item.done` / `output_text.done` /
            // `function_call_arguments.done`：内容在 delta 里已经搬过了，这些帧再搬一次就是
            // **重复内容**；`refusal.delta`、`error`、将来新增的事件：IR 表达不了。
            // 一律 `Other`（丢还是转由调用方决定），绝不伪造。
            _ => vec![IrStreamEvent::Other],
        }
    }

    fn encode_stream_event(&self, ev: &IrStreamEvent, ctx: &mut EncodeCtx) -> Vec<StreamFrame> {
        match ev {
            IrStreamEvent::Start { model, id, usage } => {
                // 去重：`response.created` 只能有一次（会话层的 `begin_frames` 可能已经补过）
                if ctx.is_opened() {
                    return Vec::new();
                }
                ctx.mark_opened();
                vec![StreamFrame::Json(responses_created(
                    id.as_deref().or_else(|| ctx.upstream_id()),
                    if model.is_empty() { ctx.upstream_model() } else { Some(model.as_str()) },
                    usage.as_ref(),
                ))]
            }
            // `output_index` / `content_index` 固定 0：事件形态里没有"第几个 item"这个维度
            // （同 `openai.rs` 的理由）。`item_id` 不编。
            IrStreamEvent::TextDelta { text, .. } => vec![StreamFrame::Json(object(vec![
                ("type", json!("response.output_text.delta")),
                ("output_index", json!(0)),
                ("content_index", json!(0)),
                ("delta", json!(text)),
            ]))],
            IrStreamEvent::ToolUseStart { slot: _, id, name, input } => {
                // 源协议一次给全入参就写进 `arguments`（它本来就是**字符串化的 JSON**）；
                // 否则留空串、由后续的 `response.function_call_arguments.delta` 分片接上。
                // 第 330 轮：从前这一格不存在，Gemini 上游的 `args` 会整份丢掉。
                let arguments = input.as_ref().map(json_string).unwrap_or_default();
                vec![StreamFrame::Json(object(vec![
                    ("type", json!("response.output_item.added")),
                    ("output_index", json!(0)),
                    (
                        "item",
                        object(vec![
                            ("type", json!("function_call")),
                            ("call_id", json!(id)),
                            ("name", json!(name)),
                            ("arguments", json!(arguments)),
                        ]),
                    ),
                ]))]
            }
            IrStreamEvent::ToolInputDelta { fragment, .. } => {
                vec![StreamFrame::Json(object(vec![
                    ("type", json!("response.function_call_arguments.delta")),
                    ("output_index", json!(0)),
                    ("delta", json!(fragment)),
                ]))]
            }
            IrStreamEvent::Stop { reason, usage } => match reason {
                Some(reason) => {
                    let (status, reason_text) = status_of_stop_reason(reason);
                    // ★ 上游给了终止语义 ⇒ 这一帧就是收尾（`close_stream` 不再补）
                    ctx.mark_closed();
                    vec![StreamFrame::Json(object(vec![
                        ("type", json!(terminal_event_type(reason))),
                        (
                            "response",
                            object(vec![
                                ("status", json!(status)),
                                (
                                    "incomplete_details",
                                    reason_text
                                        .map(|r| json!({ "reason": r }))
                                        .unwrap_or(Value::Null),
                                ),
                                (
                                    "usage",
                                    usage.as_ref().map(responses_usage).unwrap_or(Value::Null),
                                ),
                            ]),
                        ),
                    ]))]
                }
                // `reason: None` = "这是用量补充，不是终止判定"。Responses 没有专用的用量事件，
                // 于是挂到真实存在的 `response.in_progress`（它的载荷本来就是整个 response 对象）
                // 上；**不**再发一个 `response.completed` —— 一次流只能有一个终止帧。
                None => match usage {
                    Some(usage) => vec![StreamFrame::Json(object(vec![
                        ("type", json!("response.in_progress")),
                        (
                            "response",
                            object(vec![
                                ("status", json!("in_progress")),
                                ("usage", responses_usage(usage)),
                            ]),
                        ),
                    ]))],
                    None => Vec::new(),
                },
            },
            // 没有对应物：**什么都不发**，而不是发一个假的心跳/空帧
            IrStreamEvent::Other => Vec::new(),
        }
    }

    /// Responses 的 SSE 帧顶层**没有** `id` / `model`：它们在 `response` 对象里。
    ///
    /// 不覆写这个方法，跨协议流式合成 Anthropic 的 `message_start` 时
    /// `message.id` / `message.model` 会永远是 `null` —— 上游明明说了，我们却没看见。
    fn frame_identity(&self, data: &Value) -> (Option<String>, Option<String>) {
        (
            data.pointer("/response/id").and_then(Value::as_str).map(str::to_string),
            data.pointer("/response/model").and_then(Value::as_str).map(str::to_string),
        )
    }

    /// Responses 的流必须以 `response.created` 开头；上游不是 Responses 时
    /// （OpenAI chat 没有这一帧）由这里补，身份用 `ctx` 记下的上游 `id` / `model`
    /// （**取不到就写 `null`，不编**）。
    fn begin_frames(&self, ctx: &mut EncodeCtx) -> Vec<StreamFrame> {
        if ctx.is_opened() {
            return Vec::new();
        }
        ctx.mark_opened();
        vec![StreamFrame::Json(responses_created(ctx.upstream_id(), ctx.upstream_model(), None))]
    }

    /// 上游流结束时补齐收尾帧。
    ///
    /// ★ `status` 必须是 **`incomplete`**：走到这里说明**我们没等到上游说完成**
    /// （`response.completed` 已经由 `Stop { reason: Some(_) }` 发过了，那时 `is_closed()` 为真）。
    /// 编一个 `completed` 就是把"上游没说"冒充成"上游说正常结束了" ——
    /// `session.rs` 的 `responses_target_is_terminated_with_response_completed_not_done`
    /// 钉着这一条。
    fn close_stream(&self, ctx: &mut EncodeCtx) -> Vec<StreamFrame> {
        if ctx.is_closed() {
            return Vec::new();
        }
        ctx.mark_closed();
        vec![StreamFrame::Json(object(vec![
            ("type", json!("response.completed")),
            (
                "response",
                object(vec![
                    ("status", json!("incomplete")),
                    ("incomplete_details", json!({ "reason": "stream_ended_without_completion" })),
                ]),
            ),
        ]))]
    }
}

/// Responses 帧里的 `output_index`：文本增量与工具入参分片都靠它区分"这是第几个 item"。
///
/// 优先读帧顶层的 `output_index`；`item` 自带的 `index` 次之；都没有才落 0
/// （**不编一个编号**：落 0 只是"认不出它是第几个"，至少不会把增量接到别的 item 上）。
fn output_index(data: &Value, fallback: &Value) -> u32 {
    data.get("output_index").or_else(|| fallback.get("index")).and_then(Value::as_u64).unwrap_or(0)
        as u32
}

/// `response.created` 一帧。`id` / `model` 取不到就写 `null`（**不编**），usage 同理。
fn responses_created(id: Option<&str>, model: Option<&str>, usage: Option<&IrUsage>) -> Value {
    object(vec![
        ("type", json!("response.created")),
        (
            "response",
            object(vec![
                ("id", id.map(|id| json!(id)).unwrap_or(Value::Null)),
                ("object", json!("response")),
                // `in_progress` 是这一帧本身的语义（"响应已创建、正在生成"），不是编出来的数据
                ("status", json!("in_progress")),
                ("model", model.map(|m| json!(m)).unwrap_or(Value::Null)),
                // 此刻确实还没产出任何 output item：空数组是如实的
                ("output", json!([])),
                ("usage", usage.map(responses_usage).unwrap_or(Value::Null)),
            ]),
        ),
    ])
}

/// `status` / `incomplete_details`（+ 同一报文里有没有 `function_call`）→ IR 停止原因。
///
/// # 为什么"completed"不等于 `EndTurn`
///
/// Responses **没有** Chat 的 `finish_reason`：它把"模型这一轮要调工具"表达成 output 里
/// 出现了 `function_call` item，而 `status` 照样是 `completed`。照字面映射成 `EndTurn`
/// 会让目标协议（尤其是 Anthropic，客户端靠 `stop_reason == "tool_use"` 才会去执行工具）
/// **静默地不调工具** —— 所以这里必须看 output，而不是只看 status。
///
/// `response` 为 `None`（例如上游只发了一个空壳终止帧）或没有 `status` 时返回 `None`：
/// **不编**一个停止原因。
fn stop_reason_of_response(response: Option<&Value>, has_tool_use: bool) -> Option<IrStopReason> {
    let status = response?.get("status").and_then(Value::as_str)?;
    Some(match status {
        "completed" => {
            if has_tool_use {
                IrStopReason::ToolUse
            } else {
                IrStopReason::EndTurn
            }
        }
        "incomplete" => {
            match response?.pointer("/incomplete_details/reason").and_then(Value::as_str) {
                Some("max_output_tokens") => IrStopReason::MaxTokens,
                // `content_filter` 之类：**原样带走**（不硬塞进上面四个）
                Some(other) => IrStopReason::Other(other.to_string()),
                None => IrStopReason::Other("incomplete".to_string()),
            }
        }
        // `failed` / `in_progress` / 将来新增的状态：原样带走，绝不冒充成正常结束
        other => IrStopReason::Other(other.to_string()),
    })
}

/// 同一个 response 对象里的 output 有没有函数调用（见 [`stop_reason_of_response`]）。
fn output_has_function_call(response: Option<&Value>) -> bool {
    response
        .and_then(|r| r.get("output"))
        .and_then(Value::as_array)
        .map(|items| {
            items.iter().any(|it| it.get("type").and_then(Value::as_str) == Some("function_call"))
        })
        .unwrap_or(false)
}

/// IR 停止原因 → Responses 的 `(status, incomplete_details.reason)`。
///
/// 返回的 `reason` 是 `None` 表示不写 `incomplete_details`（正常结束 / `failed`）；
/// `status` 认不出来的**原样带走**（Responses 的 `status` 是开放字符串：
/// `cancelled` / `queued` 等我们没见过的值不该被硬塞成 `completed`）。
fn status_of_stop_reason(reason: &IrStopReason) -> (&str, Option<&str>) {
    match reason {
        IrStopReason::EndTurn | IrStopReason::ToolUse => ("completed", None),
        IrStopReason::MaxTokens => ("incomplete", Some("max_output_tokens")),
        // Responses 的文档值只有 max_output_tokens / content_filter；`stop_sequence` 原样带走，
        // 由 `encode_response` 登记一处损失（这里没有损失通道）。
        IrStopReason::StopSequence => ("incomplete", Some("stop_sequence")),
        IrStopReason::Other(raw) => match raw.as_str() {
            "completed" | "incomplete" | "failed" | "in_progress" | "cancelled" | "queued" => {
                (raw.as_str(), None)
            }
            other => ("incomplete", Some(other)),
        },
    }
}

/// 终止事件名。只在 `response.completed` / `response.incomplete` / `response.failed` 三个
/// **真实存在**的终止事件里挑（`session.rs` 的 `is_terminator` 也只认这三个）。
fn terminal_event_type(reason: &IrStopReason) -> &'static str {
    match reason {
        IrStopReason::Other(raw) if raw == "failed" => "response.failed",
        other => {
            let (status, _) = status_of_stop_reason(other);
            if status == "completed" {
                "response.completed"
            } else {
                "response.incomplete"
            }
        }
    }
}

/// Responses 的 usage → [`IrUsage`]。**没出现的字段不写**（`None` ≠ `Some(0)`）。
///
/// 字段名与 Chat **不同**：`input_tokens` / `output_tokens` / `input_tokens_details.cached_tokens`。
/// `total_tokens` 不搬（它是 input+output 的可推导量）；`output_tokens_details.reasoning_tokens`
/// 也不搬（[`IrUsage`] 里没有这一项，细分口径属于 `ta_core::metering`）。
/// Responses 的 usage 字段表（第 330 轮：读/写由 `shared` 的表驱动）。
///
/// 与 chat/completions **不同名**：`input_tokens` / `output_tokens`（不是
/// `prompt_tokens` / `completion_tokens`），cached 也在自己的嵌套对象里。
/// `cache_creation` 为空：Responses 只报缓存命中、不报写入。
pub(crate) const USAGE: UsageShape = UsageShape {
    input: &["input_tokens"],
    output: &["output_tokens"],
    cache_read: &["input_tokens_details/cached_tokens"],
    cache_creation: &[],
    reasoning: &["output_tokens_details/reasoning_tokens"],
};
fn ir_usage_from_responses(usage: Option<&Value>) -> Option<IrUsage> {
    read_usage(usage, &USAGE)
}
/// [`IrUsage`] → Responses 的 usage 对象。**没说的字段不写**（不补 0）。
fn responses_usage(usage: &IrUsage) -> Value {
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
        // 命名工具是**扁平**的 `{type:"function", name}`（Chat 是 `{function:{name}}`）
        Value::Object(_) => match tc.get("name").and_then(Value::as_str) {
            Some(name) => IrToolChoice::Tool(name.to_string()),
            None => {
                losses.push(Loss::UnsupportedField {
                    path: "/tool_choice".into(),
                    field: "tool_choice".into(),
                    detail: "对象形态里没有 name（`allowed_tools` 之类的内置选择没有 IR 表达）"
                        .into(),
                });
                IrToolChoice::Auto
            }
        },
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

/// 工具结果 / 消息内容的块编码。单块文本用字符串形态（Responses 允许，兼容性最好）。
fn encode_tool_output(content: &[IrPart]) -> Value {
    if let [IrPart::Text(t)] = content {
        return json!(t);
    }
    Value::Array(content.iter().filter_map(encode_generic_part).collect())
}

fn encode_input_content(role: IrRole, parts: &[IrPart]) -> Value {
    if let [IrPart::Text(t)] = parts {
        return json!(t);
    }
    // 用户侧是 `input_text`、助手侧是 `output_text`：两个名字在 IR 里都是 [`IrPart::Text`]
    let text_kind = match role {
        IrRole::User => "input_text",
        IrRole::Assistant => "output_text",
    };
    let items: Vec<Value> = parts
        .iter()
        .filter_map(|p| match p {
            IrPart::Text(t) => Some(json!({ "type": text_kind, "text": t })),
            other => encode_generic_part(other),
        })
        .collect();
    Value::Array(items)
}

/// 文本/图片的通用编码。`ToolUse` / `ToolResult` 在 Responses 里是**平级 item**，
/// 不嵌在内容里 → 这里返回 `None`（调用方按 item 处理）。
fn encode_generic_part(part: &IrPart) -> Option<Value> {
    match part {
        IrPart::Text(t) => Some(json!({ "type": "input_text", "text": t })),
        IrPart::Image(IrImage::DataUrl { media_type, data }) => {
            Some(json!({ "type": "input_image", "image_url": data_url(media_type, data) }))
        }
        IrPart::Image(IrImage::Url(url)) => {
            Some(json!({ "type": "input_image", "image_url": url }))
        }
        IrPart::ToolUse { .. } | IrPart::ToolResult { .. } => None,
    }
}

fn part_text(part: &IrPart) -> Option<String> {
    match part {
        IrPart::Text(t) => Some(t.clone()),
        _ => None,
    }
}

/// assistant 消息里的图片：Responses 只在 user 消息里接受 `input_image`，发出去就是 400
/// → 在编码前就拒（[`Loss::UnmappableImage`] 是致命的，`on_loss: warn` 也救不了）。
fn check_assistant_images(ir: &IrRequest, losses: &mut Vec<Loss>) {
    for (i, msg) in ir.messages.iter().enumerate() {
        if msg.role != IrRole::Assistant {
            continue;
        }
        scan_images(&msg.parts, &format!("/messages/{i}/content"), losses);
    }
}

fn scan_images(parts: &[IrPart], path: &str, losses: &mut Vec<Loss>) {
    for (i, part) in parts.iter().enumerate() {
        match part {
            IrPart::Image(img) => losses.push(Loss::UnmappableImage {
                path: format!("{path}/{i}"),
                detail: format!(
                    "Responses 只在 user 消息里接受 input_image（assistant 里的图片 {} 上游会拒）",
                    describe_image(img)
                ),
            }),
            IrPart::ToolResult { content, .. } => {
                scan_images(content, &format!("{path}/{i}/content"), losses)
            }
            _ => {}
        }
    }
}

// ================================================================ 测试

#[cfg(test)]
mod tests {
    //! 判据与 `adapt::tests` / `stream.rs::tests` 一致：一条测试只有在"把被测的那件事破坏掉
    //! 之后会红"时才算证据。所以下面每条都同时钉住"该搬过去的东西"与"不该出现的东西"。

    use super::*;
    use crate::adapt::ProtocolAdapter;
    use pretty_assertions::assert_eq;

    const PNG: &str = "data:image/png;base64,AAAB";

    fn adapter() -> ResponsesAdapter {
        ResponsesAdapter
    }

    /// 恰好一个事件（多一个少一个都是契约被破坏）。
    fn only(events: Vec<IrStreamEvent>) -> IrStreamEvent {
        let mut events = events;
        if events.len() != 1 {
            panic!("应当恰好一个事件，实际 {events:?}");
        }
        events.remove(0)
    }

    /// 取 JSON 帧的载荷（`Literal` 帧在 Responses 侧根本不该出现）。
    fn json_of(frame: &StreamFrame) -> &Value {
        match frame {
            StreamFrame::Json(value) => value,
            StreamFrame::Literal(text) => panic!("Responses 侧不该有字面量帧（{text}）"),
        }
    }

    /// 一个"什么都用上了"的 Responses 请求：instructions、文本+图片、工具调用、工具结果、工具、调参。
    fn rich_responses() -> Value {
        json!({
            "model": "gpt-5",
            "instructions": "be nice",
            "max_output_tokens": 1024,
            "temperature": 0.2,
            "top_p": 0.9,
            "stream": false,
            "input": [
                { "type": "message", "role": "user", "content": [
                    { "type": "input_text", "text": "看图" },
                    { "type": "input_image", "image_url": PNG, "detail": "high" }
                ]},
                { "type": "function_call", "call_id": "call_1", "name": "weather",
                  "arguments": "{\"city\":\"SH\"}" },
                { "type": "function_call_output", "call_id": "call_1", "output": "{\"temp\":21}" }
            ],
            "tools": [
                { "type": "function", "name": "weather", "description": "查天气",
                  "parameters": { "type": "object", "properties": { "city": { "type": "string" } } } }
            ],
            "tool_choice": "auto"
        })
    }

    #[test]
    fn protocol_is_openai_responses() {
        assert_eq!(adapter().protocol(), ta_core::types::Protocol::OpenaiResponses);
    }

    /// ① `input` 的两种形态（字符串 / item 数组）必须解出同一条用户消息。
    #[test]
    fn string_and_array_input_decode_to_the_same_message() {
        let as_string = adapter().decode_request(&json!({ "model": "m", "input": "你好" }));
        let as_array = adapter().decode_request(&json!({
            "model": "m",
            "input": [{ "type": "message", "role": "user",
                        "content": [{ "type": "input_text", "text": "你好" }] }],
        }));
        assert_eq!(as_string.comparable(), as_array.comparable());
        assert_eq!(as_string.messages.len(), 1);
        assert_eq!(as_string.messages[0].role, IrRole::User);
        assert_eq!(as_string.messages[0].parts, vec![IrPart::Text("你好".into())]);
        assert!(as_string.losses.is_empty(), "两种形态都不该有损失");

        // 省略 `type`、只写 `role` 的消息（有的客户端这么发）同样解成消息，不是"认不出的 item"
        let no_type = adapter().decode_request(&json!({
            "model": "m",
            "input": [{ "role": "user", "content": "你好" }],
        }));
        assert_eq!(no_type.comparable(), as_string.comparable());
    }

    /// ② `instructions` → system 消息，**编解码两个方向都不许丢**。
    #[test]
    fn instructions_survive_both_directions() {
        let ir = adapter()
            .decode_request(&json!({ "model": "m", "instructions": "be nice", "input": "hi" }));
        assert_eq!(
            ir.system,
            vec![IrPart::Text("be nice".into())],
            "instructions 必须进 IR 的 system"
        );
        let encoded = adapter().encode_request(&ir, "m");
        assert_eq!(encoded["instructions"], "be nice", "编码方向不许把 system 丢掉");
        assert_eq!(encoded["input"][0]["content"], "hi", "没有 system 形态时 input 也不许乱改");
        assert!(crate::adapt::verify_no_loss(&ir, &adapter(), &encoded).is_empty());

        // 数组形态的 instructions 也认（少数 SDK 这么发）
        let arr = adapter().decode_request(&json!({
            "instructions": [{ "type": "input_text", "text": "be nice" }],
            "input": "hi",
        }));
        assert_eq!(arr.system, vec![IrPart::Text("be nice".into())]);
    }

    /// ③ **扁平**函数工具的双向字段映射（不是 Chat 的 `function: {…}` 嵌套）。
    #[test]
    fn flat_function_tools_round_trip() {
        let body = rich_responses();
        let ir = adapter().decode_request(&body);
        assert_eq!(
            ir.tools,
            vec![IrTool {
                name: "weather".into(),
                description: Some("查天气".into()),
                parameters: json!({ "type": "object", "properties": { "city": { "type": "string" } } }),
            }]
        );
        assert_eq!(ir.tool_choice, IrToolChoice::Auto);

        let encoded = adapter().encode_request(&ir, "gpt-5");
        assert_eq!(encoded["tools"][0]["type"], "function");
        assert_eq!(encoded["tools"][0]["name"], "weather");
        assert_eq!(encoded["tools"][0]["parameters"]["properties"]["city"]["type"], "string");
        assert!(
            encoded["tools"][0].get("function").is_none(),
            "Responses 是扁平形状：不许套一层 function"
        );
        // 命名工具也是扁平的
        let mut forced = ir.clone();
        forced.tool_choice = IrToolChoice::Tool("weather".into());
        let encoded = adapter().encode_request(&forced, "gpt-5");
        assert_eq!(encoded["tool_choice"], json!({ "type": "function", "name": "weather" }));
        assert_eq!(
            adapter().decode_request(&encoded).tool_choice,
            IrToolChoice::Tool("weather".into())
        );

        // 整个富请求的往返：一次都不许丢（工具 + 图片 + 工具调用 + 工具结果 + 调参）
        let ir = adapter().decode_request(&body);
        let encoded = adapter().encode_request(&ir, "gpt-5");
        assert!(
            crate::adapt::verify_no_loss(&ir, &adapter(), &encoded).is_empty(),
            "往返核对必须干净：{:?}",
            crate::adapt::verify_no_loss(&ir, &adapter(), &encoded)
        );
        assert_eq!(adapter().decode_request(&encoded).comparable(), ir.comparable());
        // 工具调用/结果确实是**平级 item**（不是消息里的块）
        assert_eq!(encoded["input"][1]["type"], "function_call");
        assert_eq!(encoded["input"][1]["call_id"], "call_1");
        assert_eq!(encoded["input"][2]["type"], "function_call_output");
        assert_eq!(encoded["input"][2]["output"], "{\"temp\":21}");
    }

    /// ④ usage：`input_tokens` / `output_tokens` ↔ [`IrUsage`]，**不是** Chat 的字段名。
    #[test]
    fn usage_uses_responses_field_names() {
        let raw = json!({
            "input_tokens": 25, "output_tokens": 7, "total_tokens": 32,
            "input_tokens_details": { "cached_tokens": 10 },
            "output_tokens_details": { "reasoning_tokens": 3 },
        });
        let ir = ir_usage_from_responses(Some(&raw)).expect("对象形态的 usage 必须能解");
        assert_eq!(ir.input_tokens, Some(25));
        assert_eq!(ir.output_tokens, Some(7));
        assert_eq!(ir.cache_read_input_tokens, Some(10));

        let back = responses_usage(&ir);
        assert_eq!(back["input_tokens"], 25);
        assert_eq!(back["output_tokens"], 7);
        assert_eq!(back["input_tokens_details"]["cached_tokens"], 10);
        assert!(back.get("prompt_tokens").is_none(), "不许写成 Chat 的字段名");
        assert!(back.get("total_tokens").is_none(), "可推导量不搬");

        // `null` / 缺字段 = 上游说没有，不是"上游说 0"
        assert_eq!(ir_usage_from_responses(None), None);
        assert_eq!(ir_usage_from_responses(Some(&Value::Null)), None);
        // 一个字都没说的用量 → **不补 0**
        assert!(responses_usage(&IrUsage::default()).as_object().unwrap().is_empty());
    }

    /// ⑤ 流式：文本增量往返；`response.completed` → `Stop` 且**带 usage**（终止判定的唯一来源）。
    #[test]
    fn stream_text_delta_round_trips_and_completed_is_stop() {
        let delta = json!({ "type": "response.output_text.delta", "delta": "你好" });
        assert_eq!(
            only(adapter().decode_stream_event(&delta)),
            IrStreamEvent::TextDelta { slot: Slot::Text(0), text: "你好".into() }
        );
        let frames = adapter().encode_stream_event(
            &IrStreamEvent::TextDelta { slot: Slot::Text(0), text: "你好".into() },
            &mut EncodeCtx::new(),
        );
        assert_eq!(frames.len(), 1);
        assert_eq!(json_of(&frames[0])["type"], "response.output_text.delta");
        assert_eq!(json_of(&frames[0])["delta"], "你好");
        assert_eq!(
            only(adapter().decode_stream_event(json_of(&frames[0]))),
            IrStreamEvent::TextDelta { slot: Slot::Text(0), text: "你好".into() },
            "往返必须回到同一个事件"
        );
        // 空串增量是**合法增量**，两个方向都要搬
        assert_eq!(
            only(adapter().decode_stream_event(
                &json!({ "type": "response.output_text.delta", "delta": "" })
            )),
            IrStreamEvent::TextDelta { slot: Slot::Text(0), text: String::new() }
        );
        assert_eq!(
            adapter()
                .encode_stream_event(
                    &IrStreamEvent::TextDelta { slot: Slot::Text(0), text: String::new() },
                    &mut EncodeCtx::new(),
                )
                .len(),
            1
        );
        // ★ `output_index` 是"第几个 item"：它决定槽位（跨协议并行调用靠它区分）
        let second_item = json!({
            "type": "response.output_text.delta", "output_index": 2, "delta": "x",
        });
        assert_eq!(
            only(adapter().decode_stream_event(&second_item)),
            IrStreamEvent::TextDelta { slot: Slot::Text(2), text: "x".into() }
        );

        // 工具入参分片同样是往返的（它在 IR 里不带调用 id，槽位来自 output_index）
        let args = json!({ "type": "response.function_call_arguments.delta",
                           "output_index": 1, "delta": "{\"city\":" });
        assert_eq!(
            only(adapter().decode_stream_event(&args)),
            IrStreamEvent::ToolInputDelta { slot: Slot::Tool(1), fragment: "{\"city\":".into() }
        );

        // response.created → Start（model / id / usage 有就带上）
        let created = json!({
            "type": "response.created",
            "response": { "id": "resp_1", "model": "gpt-5",
                          "usage": { "input_tokens": 25, "output_tokens": 0 } },
        });
        assert_eq!(
            only(adapter().decode_stream_event(&created)),
            IrStreamEvent::Start {
                model: "gpt-5".into(),
                id: Some("resp_1".into()),
                usage: Some(IrUsage {
                    input_tokens: Some(25),
                    output_tokens: Some(0),
                    ..Default::default()
                }),
            }
        );
        let frames = adapter().encode_stream_event(
            &IrStreamEvent::Start { model: "gpt-5".into(), id: Some("resp_1".into()), usage: None },
            &mut EncodeCtx::new(),
        );
        assert_eq!(json_of(&frames[0])["type"], "response.created");
        assert_eq!(json_of(&frames[0])["response"]["id"], "resp_1");
        assert_eq!(
            only(adapter().decode_stream_event(json_of(&frames[0]))),
            IrStreamEvent::Start { model: "gpt-5".into(), id: Some("resp_1".into()), usage: None }
        );

        // ★ 终止帧：usage 必须跟着搬出来
        let completed = json!({
            "type": "response.completed",
            "response": { "id": "resp_1", "status": "completed",
                          "usage": { "input_tokens": 25, "output_tokens": 7 } },
        });
        assert_eq!(
            only(adapter().decode_stream_event(&completed)),
            IrStreamEvent::Stop {
                reason: Some(IrStopReason::EndTurn),
                usage: Some(IrUsage {
                    input_tokens: Some(25),
                    output_tokens: Some(7),
                    ..Default::default()
                }),
            }
        );
        // output 里有 function_call → 这一轮是工具调用回合（Responses 的 status 照样是 completed）
        let completed_with_call = json!({
            "type": "response.completed",
            "response": { "status": "completed", "output": [
                { "type": "function_call", "call_id": "call_1", "name": "weather", "arguments": "{}" }
            ]},
        });
        assert_eq!(
            only(adapter().decode_stream_event(&completed_with_call)),
            IrStreamEvent::Stop { reason: Some(IrStopReason::ToolUse), usage: None },
            "usage 缺失时不许补 0（None 就是 None）"
        );
        // 截断：incomplete + reason → MaxTokens，且原样带出 incomplete 原因
        let incomplete = json!({
            "type": "response.incomplete",
            "response": { "status": "incomplete",
                          "incomplete_details": { "reason": "max_output_tokens" } },
        });
        assert_eq!(
            only(adapter().decode_stream_event(&incomplete)),
            IrStreamEvent::Stop { reason: Some(IrStopReason::MaxTokens), usage: None }
        );
        // `failed`：状态**原样带走**，绝不冒充正常结束
        let failed = json!({
            "type": "response.failed",
            "response": { "status": "failed", "error": { "code": "server_error" } },
        });
        assert_eq!(
            only(adapter().decode_stream_event(&failed)),
            IrStreamEvent::Stop { reason: Some(IrStopReason::Other("failed".into())), usage: None }
        );

        // 编码方向的 Stop：一帧 response.completed，且能解回来
        let mut stop_ctx = EncodeCtx::new();
        let frames = adapter().encode_stream_event(
            &IrStreamEvent::Stop {
                reason: Some(IrStopReason::EndTurn),
                usage: Some(IrUsage {
                    input_tokens: Some(25),
                    output_tokens: Some(7),
                    ..Default::default()
                }),
            },
            &mut stop_ctx,
        );
        assert_eq!(frames.len(), 1);
        assert_eq!(json_of(&frames[0])["type"], "response.completed");
        assert_eq!(json_of(&frames[0])["response"]["status"], "completed");
        assert_eq!(json_of(&frames[0])["response"]["usage"]["output_tokens"], 7);
        assert!(stop_ctx.is_closed(), "上游给了终止语义 ⇒ 标记收尾已发");
        assert_eq!(
            only(adapter().decode_stream_event(json_of(&frames[0]))),
            IrStreamEvent::Stop {
                reason: Some(IrStopReason::EndTurn),
                usage: Some(IrUsage {
                    input_tokens: Some(25),
                    output_tokens: Some(7),
                    ..Default::default()
                }),
            }
        );
        // 只带用量的 Stop（reason: None）**不**发终止帧：一次流只能有一个终止帧
        let mut usage_ctx = EncodeCtx::new();
        let frames = adapter().encode_stream_event(
            &IrStreamEvent::Stop {
                reason: None,
                usage: Some(IrUsage { output_tokens: Some(7), ..Default::default() }),
            },
            &mut usage_ctx,
        );
        assert_eq!(frames.len(), 1);
        assert_eq!(json_of(&frames[0])["type"], "response.in_progress");
        assert!(!usage_ctx.is_closed(), "补用量不是收尾");
        assert_eq!(
            only(adapter().decode_stream_event(json_of(&frames[0]))),
            IrStreamEvent::Stop {
                reason: None,
                usage: Some(IrUsage { output_tokens: Some(7), ..Default::default() }),
            },
            "用量补充帧也要能解回来（否则 usage 在往返里蒸发）"
        );
        // 工具调用开始：output_item.added ↔ ToolUseStart
        let added = json!({
            "type": "response.output_item.added", "output_index": 0,
            "item": { "type": "function_call", "call_id": "call_1", "name": "weather",
                      "arguments": "" },
        });
        assert_eq!(
            only(adapter().decode_stream_event(&added)),
            IrStreamEvent::ToolUseStart {
                slot: Slot::Tool(0),
                id: "call_1".into(),
                name: "weather".into(),
                input: None
            }
        );
        let frames = adapter().encode_stream_event(
            &IrStreamEvent::ToolUseStart {
                slot: Slot::Tool(0),
                id: "call_1".into(),
                name: "weather".into(),
                input: None,
            },
            &mut EncodeCtx::new(),
        );
        assert_eq!(json_of(&frames[0])["item"]["call_id"], "call_1");
        assert!(json_of(&frames[0])["item"].get("id").is_none(), "item id 不编");
        assert_eq!(
            only(adapter().decode_stream_event(json_of(&frames[0]))),
            IrStreamEvent::ToolUseStart {
                slot: Slot::Tool(0),
                id: "call_1".into(),
                name: "weather".into(),
                input: None
            }
        );
    }

    /// ⑥ 无对应物 / 未知事件 → `None` 或 `Other`，**绝不伪造**。
    #[test]
    fn events_without_a_counterpart_are_not_faked() {
        // 不是 Responses 的事件（含 `[DONE]`：Responses 根本不用它）→ 连事件都不产出
        assert!(adapter().decode_stream_event(&json!(null)).is_empty());
        assert!(adapter().decode_stream_event(&json!("[DONE]")).is_empty());
        assert!(adapter().decode_stream_event(&json!({ "no_type": 1 })).is_empty());
        assert!(
            adapter()
                .decode_stream_event(&json!({ "choices": [{ "delta": { "content": "x" } }] }))
                .is_empty(),
            "Chat 的帧不是 Responses 的帧"
        );
        // 认得出是本协议的帧、但 IR 表达不了 → Other（不造内容、不造心跳）
        for frame in [
            json!({ "type": "response.content_part.added", "part": { "type": "output_text", "text": "" } }),
            json!({ "type": "response.output_item.done", "item": { "type": "message" } }),
            json!({ "type": "response.output_item.added", "item": { "type": "message" } }),
            json!({ "type": "response.refusal.delta", "delta": "no" }),
            json!({ "type": "response.some_future_event" }),
            json!({ "type": "error", "code": "x" }),
        ] {
            assert_eq!(
                adapter().decode_stream_event(&frame),
                vec![IrStreamEvent::Other],
                "{frame} 在 IR 里没有对应物，必须是 Other（不是伪造的增量）"
            );
        }
        // `response.in_progress` 不带 usage 时同样没有可搬的东西
        assert_eq!(
            adapter().decode_stream_event(&json!({
                "type": "response.in_progress",
                "response": { "id": "resp_1", "status": "in_progress", "usage": null },
            })),
            vec![IrStreamEvent::Other]
        );
        // `Other` 编码成**零帧**（不是假心跳）
        assert!(adapter()
            .encode_stream_event(&IrStreamEvent::Other, &mut EncodeCtx::new())
            .is_empty());

        // 响应方向的未知 output item / 认不出的 content part：登记损失，不编文本
        let ir = adapter().decode_response(&json!({
            "id": "resp_1", "status": "completed",
            "output": [
                { "type": "reasoning", "content": [{ "type": "reasoning_text", "text": "想想" }] },
                { "type": "message", "role": "assistant",
                  "content": [{ "type": "refusal", "refusal": "不行" }] },
                { "type": "web_search_call" },
            ],
        }));
        assert!(ir.parts.is_empty(), "认不出的东西一个字都不许编成文本");
        assert_eq!(ir.parts, Vec::new());
        assert_eq!(ir.text(), "");
        let kinds: Vec<&str> = ir.losses.iter().map(Loss::kind).collect();
        assert_eq!(kinds, vec!["unsupported_part", "unsupported_part", "unsupported_part"]);
        assert!(ir.losses[0].path().starts_with("/output/0"), "reasoning 要指到它自己那一项");
        assert_eq!(ir.stop_reason, Some(IrStopReason::EndTurn));
    }

    /// ⑦ IR 表达不了的东西必须**登记**（不是静默丢）。
    ///
    /// 顶层调参字段（`reasoning` / `store` / `include` / `metadata`）在**解码**阶段就进
    /// `IrRequest::losses`；`translate_request` 把这份损失与 `check_encodable` 的损失**一起**
    /// 拿去判 `on_loss`，所以拒发路径上看得到它们。
    #[test]
    fn inexpressible_input_is_registered_as_loss_not_swallowed() {
        let body = json!({
            "model": "m",
            "input": "hi",
            "reasoning": { "effort": "high" },
            "store": true,
            "include": ["reasoning.encrypted_content"],
            "metadata": { "k": "v" },
            "previous_response_id": "resp_0",
        });
        let ir = adapter().decode_request(&body);
        for field in ["reasoning", "store", "include", "metadata", "previous_response_id"] {
            assert!(
                ir.losses.iter().any(|l| l.path() == format!("/{field}")),
                "`{field}` 没有 IR 表达，必须登记损失：{:?}",
                ir.losses
            );
        }
        assert!(ir.losses.iter().all(|l| !l.is_fatal()), "调参字段是'可继续'的损失");
        // 拒绝路径的判据 = 解码损失 + check_encodable 损失
        let mut all = ir.losses.clone();
        all.extend(adapter().check_encodable(&ir));
        assert!(all.iter().any(|l| l.path() == "/reasoning"), "拒发时看得见 reasoning 丢了");

        // IR 层面表达不了的：`stop` 在 Responses 里没有对应参数
        let mut with_stop = adapter().decode_request(&json!({ "input": "hi" }));
        with_stop.stop = vec!["END".into()];
        let losses = adapter().check_encodable(&with_stop);
        assert!(losses.iter().any(|l| l.path() == "/stop"), "stop 必须报 loss：{losses:?}");
        let encoded = adapter().encode_request(&with_stop, "m");
        assert!(encoded.get("stop").is_none(), "不许把上游不认识的字段写出去");

        // 图片与停止序列一样：system 里的图片无处安放（编码也绝不会把它文本化）
        let mut with_system_image = adapter().decode_request(&json!({ "input": "hi" }));
        with_system_image.system = vec![IrPart::Image(IrImage::DataUrl {
            media_type: "image/png".into(),
            data: "AAAB".into(),
        })];
        let losses = adapter().check_encodable(&with_system_image);
        assert_eq!(losses.len(), 1);
        assert_eq!(losses[0].kind(), "unmappable_image");
        assert!(losses[0].is_fatal(), "图片类损失任何模式都不能继续");
        let encoded = adapter().encode_request(&with_system_image, "m");
        assert!(encoded.get("instructions").is_none(), "文本化/丢失的图片不许变成文本");

        // assistant 消息里的图片：上游会 400 → 编码前就拒
        let mut assistant_image = adapter().decode_request(&json!({ "input": "hi" }));
        assistant_image.messages.push(IrMessage {
            role: IrRole::Assistant,
            parts: vec![IrPart::Image(IrImage::Url("https://x/y.png".into()))],
        });
        let losses = adapter().check_encodable(&assistant_image);
        assert!(
            losses
                .iter()
                .any(|l| l.kind() == "unmappable_image" && l.path().starts_with("/messages/")),
            "assistant 里的图片必须被拒：{losses:?}"
        );
    }

    /// 响应方向：output 数组 → IR 的块与停止原因；编回去时把丢掉的东西**返回**给调用方。
    #[test]
    fn response_output_items_map_to_ir_and_back() {
        let upstream = json!({
            "id": "resp_1",
            "object": "response",
            "model": "gpt-5",
            "status": "completed",
            "output": [
                { "type": "message", "role": "assistant", "content": [
                    { "type": "output_text", "text": "让我查一下" }
                ]},
                { "type": "function_call", "call_id": "call_1", "name": "weather",
                  "arguments": "{\"city\":\"SH\"}" }
            ],
            "usage": { "input_tokens": 25, "output_tokens": 7 },
        });
        let ir = adapter().decode_response(&upstream);
        assert_eq!(ir.id, Some("resp_1".into()));
        assert_eq!(ir.model, Some("gpt-5".into()));
        assert_eq!(ir.text(), "让我查一下");
        // status 是 completed，但 output 里有 function_call → 这是工具调用回合
        assert_eq!(ir.stop_reason, Some(IrStopReason::ToolUse));
        assert_eq!(ir.tool_uses().len(), 1);
        assert!(ir.losses.is_empty(), "正常响应不该有损失：{:?}", ir.losses);

        let (encoded, losses) = adapter().encode_response(&ir);
        assert!(losses.is_empty(), "无损编码不该有损失：{losses:?}");
        assert_eq!(encoded["object"], "response");
        assert_eq!(encoded["status"], "completed");
        assert_eq!(encoded["id"], "resp_1");
        assert_eq!(encoded["output"][0]["type"], "message");
        assert_eq!(encoded["output"][0]["role"], "assistant");
        assert_eq!(encoded["output"][0]["content"][0]["type"], "output_text");
        assert_eq!(encoded["output"][0]["content"][0]["text"], "让我查一下");
        assert_eq!(encoded["output"][1]["type"], "function_call");
        assert_eq!(encoded["output"][1]["call_id"], "call_1");
        assert_eq!(encoded["output"][1]["arguments"], "{\"city\":\"SH\"}");
        // 编回去再解回来：内容与停止原因必须逐字段相同
        assert_eq!(adapter().decode_response(&encoded).parts, ir.parts);
        assert_eq!(adapter().decode_response(&encoded).stop_reason, ir.stop_reason);

        // `failed` / `incomplete`：状态原样带走 + 登记，绝不冒充正常结束
        let failed = adapter().decode_response(&json!({
            "id": "resp_2", "status": "failed",
            "error": { "code": "server_error", "message": "boom" },
            "output": [],
        }));
        assert_eq!(failed.stop_reason, Some(IrStopReason::Other("failed".into())));
        assert!(failed.losses.iter().any(|l| l.path() == "/error"));
        let incomplete = adapter().decode_response(&json!({
            "id": "resp_3", "status": "incomplete",
            "incomplete_details": { "reason": "content_filter" },
            "output": [],
        }));
        assert_eq!(incomplete.stop_reason, Some(IrStopReason::Other("content_filter".into())));
        let truncated = adapter().decode_response(&json!({
            "id": "resp_4", "status": "incomplete",
            "incomplete_details": { "reason": "max_output_tokens" },
            "output": [],
        }));
        assert_eq!(truncated.stop_reason, Some(IrStopReason::MaxTokens));

        // IR 里没有停止原因 → **不编** `completed`，而是留痕
        let (encoded, losses) = adapter().encode_response(&IrResponse {
            id: Some("resp_5".into()),
            model: Some("m".into()),
            parts: vec![IrPart::Text("hi".into())],
            stop_reason: None,
            ..Default::default()
        });
        assert!(encoded.get("status").is_none(), "没有停止原因就不许编一个 completed");
        assert!(losses.iter().any(|l| l.path() == "/status"));
        // 文本块数不许被合并掉（合并会让响应方向的往返核对报"编码器改了东西"）
        let two_texts = IrResponse {
            parts: vec![IrPart::Text("a".into()), IrPart::Text("b".into())],
            stop_reason: Some(IrStopReason::EndTurn),
            ..Default::default()
        };
        let (encoded, _) = adapter().encode_response(&two_texts);
        assert_eq!(encoded["output"][0]["content"].as_array().unwrap().len(), 2);
        assert_eq!(adapter().decode_response(&encoded).parts, two_texts.parts);
    }

    /// ★ `frame_identity`：Responses 的帧顶层**没有** `id` / `model`（它们在 `response` 里）。
    ///
    /// 不覆写这个方法，跨协议流式合成的 `message_start.model` 会永远是 `null`
    /// （上游明明说了，我们没看见）。把 `frame_identity` 删掉/改成只读顶层，这条立刻红。
    #[test]
    fn frame_identity_reads_the_nested_response_object() {
        let created = json!({
            "type": "response.created",
            "response": { "id": "resp_1", "model": "gpt-5", "status": "in_progress" },
        });
        assert_eq!(
            adapter().frame_identity(&created),
            (Some("resp_1".to_string()), Some("gpt-5".to_string())),
        );
        // 结构上取不到（例如终止帧里只有 status）⇒ 两个都是 None，**不编**
        assert_eq!(
            adapter().frame_identity(&json!({ "type": "error", "code": "x" })),
            (None, None)
        );
    }

    /// ★ `begin_frames`：上游不是 Responses 时（OpenAI chat 没有 `response.created`）
    /// 由这里补一帧，身份用 `ctx` 记下的上游 `id` / `model`；取不到写 `null`，**不编**。
    /// 已经在 `Start` 里开过就不重复。
    #[test]
    fn begin_frames_synthesises_response_created_exactly_once() {
        let mut ctx = EncodeCtx::new();
        ctx.remember_upstream(Some("chatcmpl-1".into()), Some("gpt-4o".into()));
        let frames = adapter().begin_frames(&mut ctx);
        assert_eq!(frames.len(), 1);
        assert_eq!(json_of(&frames[0])["type"], "response.created");
        assert_eq!(json_of(&frames[0])["response"]["id"], "chatcmpl-1");
        assert_eq!(json_of(&frames[0])["response"]["model"], "gpt-4o");
        assert!(ctx.is_opened());
        assert!(adapter().begin_frames(&mut ctx).is_empty(), "只能有一次");

        // 身份取不到 ⇒ `null`，不编
        let mut unknown = EncodeCtx::new();
        let frames = adapter().begin_frames(&mut unknown);
        assert_eq!(json_of(&frames[0])["response"]["id"], Value::Null);
        assert_eq!(json_of(&frames[0])["response"]["model"], Value::Null);

        // 已经在 `Start` 里开过（上游本来就是 Responses）⇒ 不重复
        let mut opened = EncodeCtx::new();
        let frames = adapter().encode_stream_event(
            &IrStreamEvent::Start { model: "gpt-5".into(), id: Some("resp_1".into()), usage: None },
            &mut opened,
        );
        assert_eq!(frames.len(), 1);
        assert!(adapter().begin_frames(&mut opened).is_empty(), "开过就不许再开");
    }

    /// ★ `close_stream`：上游什么都没说就断了时，`response.status` 必须是 **`incomplete`**。
    ///
    /// 我们确实没等到上游说完成，编一个 `completed` 就是撒谎
    /// （`session.rs` 的 `responses_target_is_terminated_with_response_completed_not_done`
    /// 钉着同一条）。把 `"incomplete"` 改成 `"completed"`，两处都会红。
    /// 也不能补 `[DONE]` —— Responses 的流根本没有这个标记。
    #[test]
    fn close_stream_terminates_with_an_incomplete_status_and_no_done_marker() {
        let mut ctx = EncodeCtx::new();
        let frames = adapter().close_stream(&mut ctx);
        assert_eq!(frames.len(), 1);
        assert_eq!(json_of(&frames[0])["type"], "response.completed");
        assert_eq!(
            json_of(&frames[0])["response"]["status"],
            "incomplete",
            "没等到完成就必须如实说 incomplete"
        );
        assert!(
            frames.iter().all(|f| matches!(f, StreamFrame::Json(_))),
            "Responses 的流里不该出现 [DONE]：{frames:?}"
        );
        assert!(ctx.is_closed());
        assert!(adapter().close_stream(&mut ctx).is_empty(), "第二次不许再补");

        // 上游已经给过终止语义 ⇒ 收尾一个字节都不补
        let mut stopped = EncodeCtx::new();
        let frames = adapter().encode_stream_event(
            &IrStreamEvent::Stop { reason: Some(IrStopReason::EndTurn), usage: None },
            &mut stopped,
        );
        assert_eq!(json_of(&frames[0])["type"], "response.completed");
        assert_eq!(json_of(&frames[0])["response"]["status"], "completed");
        assert!(stopped.is_closed());
        assert!(adapter().close_stream(&mut stopped).is_empty(), "一次流一个终止帧");
    }
}
