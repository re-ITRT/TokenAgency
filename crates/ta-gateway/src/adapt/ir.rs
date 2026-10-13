//! canonical IR：四协议之间的**共同可表达**的那部分请求/响应。
//!
//! # 为什么需要 IR
//!
//! 同协议转发**永不走这里**（RAW 路径：只做定点 patch，零改动）。IR 只在
//! 客户端协议 ≠ 上游协议时作为**降级路径**（[ADR-03]）。
//!
//! # 规范形态（canonical form）—— 往返核对的基准
//!
//! `verify_no_loss` 的做法是"源请求 → IR → 目标协议编码 → **再解码回 IR**"，
//! 两次 IR 必须逐字段相同。要让这个判据成立，IR 必须是**规范形态**，否则
//! "协议合法的表达差异"（OpenAI 的 `stop: "x"` vs `["x"]`、Anthropic 把 system 提出来放顶层、
//! OpenAI 把工具结果放成独立 `role:"tool"` 消息）会被误判成"丢了东西"。所以解码时统一：
//!
//! 1. **相邻同角色消息合并成一条**。OpenAI 的 `tool` 消息在 Anthropic 里是
//!    "user 消息里的 `tool_result` 块"，一边多条一边一条，不合并就永远对不上。
//! 2. **user 消息里的 `ToolResult` 排到最前**。Anthropic 要求 `tool_result` 紧随对应的
//!    `tool_use`，而 OpenAI 要求 `role:"tool"` 紧跟 `assistant.tool_calls`；把工具结果
//!    排在文本之前是**两个协议都接受**的顺序，于是编码方向不再需要"看情况倒序"。
//! 3. **assistant 消息里文本在前、`ToolUse` 在后**（同理，两边都接受）。
//!
//! 这些重排都是**语义等价**的（同一个回合里的内容顺序对模型没有区别，而上面两条是
//! 目标协议的硬要求），并且由往返测试钉住。
//!
//! # 不认识的东西
//!
//! 解码只认**文档化的形状**，其它一律登记成 [`Loss`]（见 [`crate::adapt::loss`]），
//! **绝不静默丢**。这里也**不用** `#[derive(Deserialize)]` 去解析请求体：
//! 强类型反序列化会让未知字段直接蒸发，而这正是要核对的东西。

use serde_json::{json, Map, Value};

use crate::adapt::loss::Loss;

/// 规范化的请求。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct IrRequest {
    pub model: Option<String>,
    pub stream: bool,
    /// 系统提示（Anthropic 放顶层、OpenAI 放 `role:"system"` 消息里 —— 两边都归一到这里）
    pub system: Vec<IrPart>,
    pub messages: Vec<IrMessage>,
    pub tools: Vec<IrTool>,
    pub tool_choice: IrToolChoice,
    pub max_tokens: Option<u64>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub stop: Vec<String>,
    /// 解码阶段就已经确定丢掉的源内容（进不了 IR 的东西）。
    /// **往返核对时不比较它**（它是"输入侧的事实"，不是"转换的结果"）。
    pub losses: Vec<Loss>,
}

impl IrRequest {
    /// 比较用：把输入侧损失清空（损失的比较在 [`crate::adapt::translate_request`] 里统一做）。
    pub fn comparable(&self) -> Self {
        Self { losses: Vec::new(), ..self.clone() }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct IrMessage {
    pub role: IrRole,
    pub parts: Vec<IrPart>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IrRole {
    User,
    Assistant,
}

impl IrRole {
    pub fn as_str(&self) -> &'static str {
        match self {
            IrRole::User => "user",
            IrRole::Assistant => "assistant",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum IrPart {
    Text(String),
    Image(IrImage),
    ToolUse { id: String, name: String, input: Value },
    ToolResult { tool_use_id: String, content: Vec<IrPart> },
}

/// 图片。两种来源形态**必须分开表达** —— 合成一种就会在编码时被迫做选择（丢信息）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IrImage {
    /// `data:<media_type>;base64,<data>`
    DataUrl { media_type: String, data: String },
    /// `http(s)://…`（Anthropic 的 `source.type = "url"`、OpenAI 的纯 URL）
    Url(String),
}

impl IrImage {
    /// 目标协议接受这张图吗？`supported` 是目标协议的 media type 白名单。
    pub fn media_type(&self) -> Option<&str> {
        match self {
            IrImage::DataUrl { media_type, .. } => Some(media_type.as_str()),
            IrImage::Url(_) => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IrTool {
    pub name: String,
    pub description: Option<String>,
    /// JSON Schema（原样搬运，**不改写**）
    pub parameters: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum IrToolChoice {
    #[default]
    Auto,
    Required,
    None,
    Tool(String),
}

/// 规范化的响应。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct IrResponse {
    pub id: Option<String>,
    pub model: Option<String>,
    pub parts: Vec<IrPart>,
    pub stop_reason: Option<IrStopReason>,
    /// **用量**（第 330 轮补）。
    ///
    /// # 为什么响应方向也需要它
    ///
    /// 请求方向一直有 `IrRequest`，流式方向有 [`super::stream::IrUsage`]，
    /// **唯独非流式响应没有** —— 于是跨协议翻译非流式响应时，客户端拿到的
    /// `usage` 是**空的**：OpenAI 客户端看到 `usage: null`、Anthropic 客户端连
    /// `message.usage` 都没有。上游明明给了。
    ///
    /// # 与计费的关系（必须说清楚，免得被误读成"账本靠它"）
    ///
    /// **计费不读它。** 账本的口径权威是 `ta_core::metering`，它直接吃**上游原始报文**
    /// （翻译发生在 `meter.observe_full` **之后**）。这一格存在的意义是**保真**：
    /// 客户端看到的用量应当是上游说的那个数，而不是因为我们翻译了一下就消失。
    pub usage: Option<super::stream::IrUsage>,
    pub losses: Vec<Loss>,
}

impl IrResponse {
    pub fn text(&self) -> String {
        let mut out = String::new();
        for part in &self.parts {
            if let IrPart::Text(t) = part {
                out.push_str(t);
            }
        }
        out
    }

    pub fn tool_uses(&self) -> Vec<(&str, &str, &Value)> {
        self.parts
            .iter()
            .filter_map(|p| match p {
                IrPart::ToolUse { id, name, input } => Some((id.as_str(), name.as_str(), input)),
                _ => None,
            })
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IrStopReason {
    EndTurn,
    MaxTokens,
    StopSequence,
    ToolUse,
    /// 协议特有的其它原因，原样带着（别硬塞进上面四个）
    Other(String),
}

impl IrStopReason {
    pub fn as_str(&self) -> &str {
        match self {
            IrStopReason::EndTurn => "end_turn",
            IrStopReason::MaxTokens => "max_tokens",
            IrStopReason::StopSequence => "stop_sequence",
            IrStopReason::ToolUse => "tool_use",
            IrStopReason::Other(s) => s,
        }
    }
}

// ---------------------------------------------------------------- 解码辅助

/// `text` 块。
pub(crate) fn text_part(s: impl Into<String>) -> IrPart {
    IrPart::Text(s.into())
}

/// 解析 `data:` URL。**只认 `;base64,` 形态**（其它形态交给调用方登记损失）。
pub(crate) fn parse_data_url(url: &str) -> Option<IrImage> {
    let rest = url.strip_prefix("data:")?;
    let (meta, data) = rest.split_once(',')?;
    let media_type = meta.strip_suffix(";base64")?;
    if media_type.is_empty() {
        return None;
    }
    Some(IrImage::DataUrl { media_type: media_type.to_string(), data: data.to_string() })
}

/// 组装 `data:` URL（编码方向）。
pub(crate) fn data_url(media_type: &str, data: &str) -> String {
    format!("data:{media_type};base64,{data}")
}

/// JSON 值 → 紧凑字符串（工具调用参数在两个协议里都是"字符串化的 JSON"）。
pub(crate) fn json_string(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "{}".to_string())
}

/// 字符串 → JSON 值。合法 JSON 原样用；否则包成 `{"_raw": "…"}`？
///
/// **不包**：工具参数必须是 JSON 对象。解不出来就是源请求坏了 —— 原样送一个 `{"input":…}`？
/// 也不行。这里返回 `None`，由调用方登记 [`Loss::UnsupportedPart`]（绝不猜）。
pub(crate) fn parse_json_string(s: &str) -> Option<Value> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return Some(json!({}));
    }
    serde_json::from_str::<Value>(trimmed).ok()
}

/// 把 `Value::Object` 的键拷进一个新对象（保持插入顺序）。
pub(crate) fn object(pairs: Vec<(&str, Value)>) -> Value {
    let mut map = Map::new();
    for (k, v) in pairs {
        if !v.is_null() {
            map.insert(k.to_string(), v);
        }
    }
    Value::Object(map)
}

/// 规范化：合并相邻同角色消息 + 按角色把块排序（见模块头"规范形态"）。
pub(crate) fn canonicalize(messages: Vec<IrMessage>) -> Vec<IrMessage> {
    let mut out: Vec<IrMessage> = Vec::with_capacity(messages.len());
    for msg in messages {
        let parts = order_parts(msg.role, msg.parts);
        if parts.is_empty() {
            continue; // 空消息（例如只有空文本）不产生占位
        }
        match out.last_mut() {
            Some(last) if last.role == msg.role => last.parts.extend(parts),
            _ => out.push(IrMessage { role: msg.role, parts }),
        }
    }
    out
}

fn order_parts(role: IrRole, parts: Vec<IrPart>) -> Vec<IrPart> {
    let is_tool = |p: &IrPart| matches!(p, IrPart::ToolResult { .. });
    match role {
        IrRole::User => {
            // 工具结果在前（Anthropic 硬要求，OpenAI 也接受），其余保持原相对顺序
            let (mut head, tail): (Vec<IrPart>, Vec<IrPart>) =
                parts.into_iter().partition(|p| is_tool(p));
            head.extend(tail);
            head
        }
        IrRole::Assistant => {
            // 文本在前、ToolUse 在后（两边协议都接受；文本内部保持原顺序）
            let (mut head, tail): (Vec<IrPart>, Vec<IrPart>) =
                parts.into_iter().partition(|p| !matches!(p, IrPart::ToolUse { .. }));
            head.extend(tail);
            head
        }
    }
}
