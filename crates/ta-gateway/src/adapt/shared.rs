//! **四个协议适配器共用的工具**（第 328 轮）。
//!
//! # 为什么要有这个模块（这是一次结构修复，不是整理癖）
//!
//! 这些函数原来住在 `openai.rs` 里，然后 `anthropic.rs` / `responses.rs` / `gemini.rs`
//! 都写着 `use crate::adapt::openai::{…}`。那有三个具体问题：
//!
//! 1. **依赖方向错了**：`openai.rs` 是一个**协议适配器**，不是一个工具库。
//!    三个兄弟协议依赖它，意味着你不能单独重构/替换 OpenAI 那一侧 ——
//!    改一个取值辅助函数会牵动另外三个协议。
//! 2. **发现成本**："检查工具 id 配对"这种通用约束，新写一个协议的人不可能想到
//!    它住在 `openai.rs` 里。于是他会自己再写一遍（Gemini 那份就是），
//!    两份实现迟早分叉 —— 而这两份正是"不丢内容"的判据。
//! 3. **命名说谎**：`part_kind` / `u64_of` / `head` / `check_images` 与 OpenAI 毫无关系。
//!
//! 所以把它们挪到这里，四个适配器都从 `super::shared` 取。
//! 判据很简单：**一个函数如果四个协议都会用到，它就不属于其中任何一个。**
//!
//! # 这里放什么、不放什么
//!
//! * 放：对 [`IrRequest`] / [`IrPart`] / `Value` 的**协议无关**检查与取值。
//! * 不放：任何协议自己的字段名、白名单、路径。例如图片 media type 白名单
//!   （`IMAGE_MEDIA_TYPES`）**留在各自的适配器里** —— 那是协议事实，
//!   由一个协议替另一个协议声明就是错的（OpenAI 收 webp、Anthropic 也收，
//!   但 Gemini 收 heic —— 合成一份必然有一边是错的）。
//! * 不放：`super::ir` 里已有的东西（那是 IR 自己的编解码辅助）。

use serde_json::{json, Value};

use super::ir::{IrImage, IrPart, IrRequest};
use super::loss::Loss;
use super::stream::IrUsage;

/// IR 内容块的短名，用于损失与错误消息。
pub(crate) fn part_kind(part: &IrPart) -> &'static str {
    match part {
        IrPart::Text(_) => "text",
        IrPart::Image(_) => "image",
        IrPart::ToolUse { .. } => "tool_use",
        IrPart::ToolResult { .. } => "tool_result",
    }
}

/// 图片的**人话描述**（给用户看的那一句）。
///
/// 图片本身**绝不文本化**，所以出问题时用户只能看到这个描述 ——
/// 它要够精确定位是哪一张（data URL 给 media type、URL 给前 48 字符）。
pub(crate) fn describe_image(img: &IrImage) -> String {
    match img {
        IrImage::DataUrl { media_type, .. } => format!("data URL {media_type}"),
        IrImage::Url(u) => format!("URL {}", head(u)),
    }
}

/// JSON 值的**类型名**（损失消息里用）。
pub(crate) fn kind_of(v: &Value) -> String {
    match v {
        Value::Array(_) => "array",
        Value::Object(_) => "object",
        Value::Number(_) => "number",
        Value::Bool(_) => "bool",
        Value::Null => "null",
        Value::String(_) => "string",
    }
    .to_string()
}

/// 长字符串的截断（按**字符**而不是字节，中文不会被切坏）。
pub(crate) fn head(s: &str) -> String {
    let cut = s.char_indices().nth(48).map_or(s.len(), |(i, _)| i);
    format!("{}…", &s[..cut])
}

/// 工具调用/结果**必须成对**：任一侧 id 为空，目标协议就配不上它们。
///
/// 这是四个协议的共同硬约束（Anthropic 靠 `tool_use_id`、OpenAI 靠 `tool_call_id`、
/// Responses 靠 `call_id`、Gemini 靠函数名 + 序号），所以放在共用层。
///
/// **注意 Gemini 的偏离**：它靠**函数名**配对，空 id 对它是合法的 ——
/// 所以 Gemini 不调用这个函数（它有自己的 `check_encodable` 分支）。
pub(crate) fn check_tool_ids(ir: &IrRequest, losses: &mut Vec<Loss>) {
    for (i, msg) in ir.messages.iter().enumerate() {
        let path = format!("/messages/{i}/content");
        for (j, part) in msg.parts.iter().enumerate() {
            match part {
                IrPart::ToolUse { id, .. } if id.is_empty() => losses.push(Loss::MissingRequired {
                    path: format!("{path}/{j}/id"),
                    detail: "工具调用缺 id（两侧协议都要 id 才能配对 tool_result）".into(),
                }),
                IrPart::ToolResult { tool_use_id, .. } if tool_use_id.is_empty() => {
                    losses.push(Loss::MissingRequired {
                        path: format!("{path}/{j}/tool_use_id"),
                        detail: "工具结果缺 tool_use_id（无法配对到调用）".into(),
                    })
                }
                _ => {}
            }
        }
    }
}

/// 逐张图片检查 media type 是否在**目标协议**的白名单里（含 `tool_result` 里的图片）。
///
/// `allowed` 由调用方给：那是协议事实，不能由这一层替四个协议猜一份。
pub(crate) fn check_images(ir: &IrRequest, allowed: &[&str], losses: &mut Vec<Loss>) {
    for (i, msg) in ir.messages.iter().enumerate() {
        check_images_in(&msg.parts, &format!("/messages/{i}/content"), allowed, losses);
    }
}

fn check_images_in(parts: &[IrPart], path: &str, allowed: &[&str], losses: &mut Vec<Loss>) {
    for (i, part) in parts.iter().enumerate() {
        match part {
            IrPart::Image(IrImage::DataUrl { media_type, .. })
                if !allowed.iter().any(|a| a.eq_ignore_ascii_case(media_type)) =>
            {
                losses.push(Loss::UnmappableImage {
                    path: format!("{path}/{i}"),
                    detail: format!(
                        "图片格式 {media_type} 目标协议不接受（可用：{}）",
                        allowed.join(" / ")
                    ),
                })
            }
            IrPart::ToolResult { content, .. } => {
                check_images_in(content, &format!("{path}/{i}/content"), allowed, losses);
            }
            _ => {}
        }
    }
}

/// 按**多个候选键名**取一个无符号整数（各协议的字段名不同：`max_tokens` /
/// `max_output_tokens` / `maxOutputTokens`…）。
pub(crate) fn u64_of(body: &Value, keys: &[&str]) -> Option<u64> {
    keys.iter().find_map(|k| body.get(*k).and_then(Value::as_u64))
}

/// **全空**的 usage（四个字段一个都没说）没有东西可搬 ⇒ 归一成 `None`。
///
/// # 为什么不能"原样带一个说空话的 usage"
///
/// `usage: {}` / `usageMetadata: {}` 这种"字段在、但里面什么都没有"的载荷，
/// 若原样带成 `Some(IrUsage { 全 None })`，目标协议会因此发一个**每个字段都缺席**的
/// "补用量"帧 —— 客户端得解析一个什么都没说的帧。而它携带的信息**恰好为零**。
///
/// # 为什么这不影响计费口径（这一点必须说清楚）
///
/// 账本读的是**上游原始字节**（`ta_core::metering::normalize_*` 直接吃原始报文），
/// 不读 IR 事件。所以这里清掉的只是"要不要多发一个空帧"，**不是**"要不要记账"。
///
/// # `Some(0)` 不算空
///
/// "上游说 0"与"上游没说"是两回事—— 这里只清"什么都没说"，
/// 只要有一个字段带了值（哪怕是 0）就原样保留。
pub(crate) fn non_empty_usage(usage: IrUsage) -> Option<IrUsage> {
    (!usage.is_empty()).then_some(usage)
}

// ---------------------------------------------------------------- usage 字段表

/// **一个协议的 usage 字段名**（第 330 轮）。
///
/// # 为什么是表，而不是四个协议各写一对函数
///
/// 四个协议原本各有一对 `ir_usage_from_X` / `X_usage`，而它们**结构完全相同** ——
/// 唯一的差别是字段名（OpenAI/Responses 还把 cached 放在一个嵌套对象里）。
/// 八份结构相同的代码意味着：加一个字段（例如本轮加的 `reasoning_tokens`）
/// 要改**四个文件的八个地方**，而漏掉任何一处都是**静默丢一个数**。
///
/// 现在每个协议只声明一张表，读/写各一份通用实现。
///
/// # 命名约定
///
/// * 每格给一组**候选键名**（读的时候按顺序试）：同一个协议可能有 camelCase 与
///   snake_case 两种上游实现（Gemini 两种都有）。
/// * **写的时候只用第一个**：写多个同义键会让上游看到它没定义过的字段。
/// * `a/b/c` 表示嵌套路径（OpenAI 的 `prompt_tokens_details.cached_tokens`）。
/// * **空数组 = 这个协议没有这一格**（例如 OpenAI 不报缓存写入）——
///   读永远拿不到、写永远不产出。这是"说清楚没有"，不是"忘了填"。
pub(crate) struct UsageShape {
    pub input: &'static [&'static str],
    pub output: &'static [&'static str],
    pub cache_read: &'static [&'static str],
    pub cache_creation: &'static [&'static str],
    pub reasoning: &'static [&'static str],
}

impl UsageShape {
    /// 五格字段（顺序与 [`read_usage`] / [`write_usage`] 的实现一致）。
    ///
    /// 存在的意义是**测试**：可以遍历它来确保"每格都真的被读/写到了"。
    #[cfg(test)]
    fn all(&self) -> [&'static [&'static str]; 5] {
        [self.input, self.output, self.cache_read, self.cache_creation, self.reasoning]
    }
}

/// 按 `a/b/c` 路径取值（没有 `/` 就是顶层）。
fn at_path<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    let mut cur = value;
    for segment in path.split('/') {
        cur = cur.get(segment)?;
    }
    Some(cur)
}

/// 按路径写入（沿途缺的对象按需创建）。遇到"同名键存在但不是对象"就**换掉它** ——
/// 上游给了形状不对的中间层时，编出正确的嵌套比把值丢掉好。
fn set_path(map: &mut serde_json::Map<String, Value>, segments: &[&str], value: Value) {
    match segments {
        [] => {}
        [last] => {
            map.insert((*last).to_string(), value);
        }
        [head, rest @ ..] => {
            let entry =
                map.entry((*head).to_string()).or_insert_with(|| Value::Object(Default::default()));
            if !entry.is_object() {
                *entry = Value::Object(Default::default());
            }
            if let Value::Object(inner) = entry {
                set_path(inner, rest, value);
            }
        }
    }
}

/// 从上游的 usage 对象读成 [`IrUsage`]。
///
/// 三条口径与既有实现逐字一致（不能因为"统一了"就漂）：
/// * 不是对象（`null` / 脏数据）→ `None`（"上游说没有" ≠ "上游说 0"）；
/// * 每格给一组候选键名，按顺序试；
/// * 全空 ⇒ `None`（见 [`non_empty_usage`]）。
pub(crate) fn read_usage(usage: Option<&Value>, shape: &UsageShape) -> Option<IrUsage> {
    let usage = usage?;
    if !usage.is_object() {
        return None;
    }
    let get = |keys: &[&str]| keys.iter().find_map(|k| at_path(usage, k).and_then(Value::as_u64));
    non_empty_usage(IrUsage {
        input_tokens: get(shape.input),
        output_tokens: get(shape.output),
        cache_read_input_tokens: get(shape.cache_read),
        cache_creation_input_tokens: get(shape.cache_creation),
        reasoning_tokens: get(shape.reasoning),
    })
}

/// [`IrUsage`] → 上游协议的 usage 对象。**没说的字段不写**（不补 0、也不写 `null`）。
pub(crate) fn write_usage(usage: &IrUsage, shape: &UsageShape) -> Value {
    let mut out = serde_json::Map::new();
    let pairs = [
        (shape.input, usage.input_tokens),
        (shape.output, usage.output_tokens),
        (shape.cache_read, usage.cache_read_input_tokens),
        (shape.cache_creation, usage.cache_creation_input_tokens),
        (shape.reasoning, usage.reasoning_tokens),
    ];
    for (keys, value) in pairs {
        if let (Some(first), Some(n)) = (keys.first(), value) {
            let segments: Vec<&str> = first.split('/').collect();
            set_path(&mut out, &segments, json!(n));
        }
    }
    Value::Object(out)
}

pub(crate) fn f64_of(body: &Value, key: &str) -> Option<f64> {
    body.get(key).and_then(Value::as_f64)
}

/// `stop` / `stop_sequences` / `stopSequences` 这类**字符串或字符串数组**的归一化。
pub(crate) fn strings_of(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Array(items)) => {
            items.iter().filter_map(Value::as_str).map(str::to_string).collect()
        }
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn head_truncates_on_character_boundaries_not_bytes() {
        // 48 个汉字：按字节切会切出半个字（UTF-8 三字节），按字符切不会
        let long: String = "汉".repeat(60);
        let cut = head(&long);
        assert!(cut.ends_with('…'));
        assert_eq!(cut.chars().count(), 49, "48 个字符 + 省略号");
        assert!(long.starts_with(cut.trim_end_matches('…')));
        // 短的照原样带省略号（不产生"切坏"的可能）
        assert_eq!(head("abc"), "abc…");
    }

    #[test]
    fn tool_ids_and_images_are_checked_across_the_whole_request() {
        use super::super::ir::{IrMessage, IrRole};

        let ir = IrRequest {
            messages: vec![IrMessage {
                role: IrRole::User,
                parts: vec![
                    IrPart::ToolResult {
                        tool_use_id: String::new(),
                        // 嵌在 tool_result 里的图片也要被查到（递归那条路径）
                        content: vec![IrPart::Image(IrImage::DataUrl {
                            media_type: "image/svg+xml".into(),
                            data: "AAAA".into(),
                        })],
                    },
                    IrPart::Image(IrImage::Url("https://x.example/a.png".into())),
                ],
            }],
            ..Default::default()
        };

        let mut losses = Vec::new();
        check_tool_ids(&ir, &mut losses);
        check_images(&ir, &["image/png"], &mut losses);

        assert!(
            losses.iter().any(|l| l.detail().contains("tool_use_id")),
            "空 tool_use_id 必须被查出来：{losses:?}"
        );
        assert!(
            losses.iter().any(|l| l.detail().contains("svg")),
            "**tool_result 里的**图片也必须被查到（递归）：{losses:?}"
        );
        // URL 图片不参与 media type 白名单（它没有 media type）
        assert_eq!(losses.len(), 2, "只该有这两条：{losses:?}");
    }

    /// **每个协议的 5 格 usage 都真的接通了**（第 330 轮）。
    ///
    /// 这是"表驱动"的代价对应的测试：表里声明了某一格，而 `read_usage` / `write_usage`
    /// 忘了实现它，就会变成**静默丢一个数** —— 本地单测还是绿的。
    /// 所以这里对**四个协议**各跑一遍全字段往返。
    #[test]
    fn every_protocol_usage_shape_round_trips_all_five_fields() {
        use crate::adapt::{anthropic, gemini, openai, responses};

        // 一个"五格都有值"的 usage —— 用它逼出任何一格没被接线的情况
        let full = IrUsage {
            input_tokens: Some(1),
            output_tokens: Some(2),
            cache_read_input_tokens: Some(3),
            cache_creation_input_tokens: Some(4),
            reasoning_tokens: Some(5),
        };
        // 每一格在哪个协议里"本该有"是**协议事实**，不是实现细节：
        // * `cache_creation`：只有 Anthropic 报缓存**写入**（别的只报命中）；
        // * `reasoning`：Anthropic 的 usage 里**没有**这一格 —— 它的思考 token
        //   已经算在 `output_tokens` 里，不单列。
        let wired: [(&str, &UsageShape, bool, bool); 4] = [
            ("openai", &openai::USAGE, false, true),
            ("anthropic", &anthropic::USAGE, true, false),
            ("gemini", &gemini::USAGE, false, true),
            ("responses", &responses::USAGE, false, true),
        ];

        for (name, shape, has_cache_creation, has_reasoning) in wired {
            // 表本身必须每格都有声明（空数组 = 显式声明"没有"，不是漏填）
            assert_eq!(shape.all().len(), 5, "{name} 的表格子数不对");

            let written = write_usage(&full, shape);
            let back = read_usage(Some(&written), shape).unwrap_or_else(|| {
                panic!("{name}：写了 usage 却读不回来（有格子没接线）：{written}")
            });

            assert_eq!(back.input_tokens, Some(1), "{name} 的 input 没接通");
            assert_eq!(back.output_tokens, Some(2), "{name} 的 output 没接通");
            assert_eq!(back.cache_read_input_tokens, Some(3), "{name} 的 cache_read 没接通");
            if has_reasoning {
                assert_eq!(back.reasoning_tokens, Some(5), "{name} 的 reasoning 没接通");
            } else {
                assert_eq!(
                    back.reasoning_tokens, None,
                    "{name} 没有 reasoning 这一格，不该凭空冒出来"
                );
                assert!(
                    !written.to_string().contains("reasoning"),
                    "{name} 不该写出协议里没有的字段：{written}"
                );
            }
            if has_cache_creation {
                assert_eq!(
                    back.cache_creation_input_tokens,
                    Some(4),
                    "{name} 声明了 cache_creation 却没接通"
                );
            } else {
                assert_eq!(
                    back.cache_creation_input_tokens, None,
                    "{name} 没有 cache_creation 这一格，不该凭空冒出来"
                );
                // 写出来的对象里也不该出现它
                assert!(
                    !written.to_string().contains("cache_creation"),
                    "{name} 不该写出协议里没有的字段：{written}"
                );
            }
        }
    }
    #[test]
    fn value_coercion_helpers_accept_every_documented_shape() {
        assert_eq!(u64_of(&json!({"b": 7, "a": 3}), &["a"]), Some(3));
        assert_eq!(u64_of(&json!({"b": 7}), &["a", "b"]), Some(7), "候选键名要按顺序试");
        assert_eq!(u64_of(&json!({"a": -1}), &["a"]), None, "负数不是无符号整数");
        assert_eq!(f64_of(&json!({"t": 0.5}), "t"), Some(0.5));
        assert_eq!(strings_of(Some(&json!("one"))), vec!["one"]);
        assert_eq!(strings_of(Some(&json!(["a", "b"]))), vec!["a", "b"]);
        assert_eq!(strings_of(Some(&json!([]))), Vec::<String>::new());
        assert_eq!(strings_of(None), Vec::<String>::new());
        assert_eq!(kind_of(&json!([1])), "array");
    }
}
