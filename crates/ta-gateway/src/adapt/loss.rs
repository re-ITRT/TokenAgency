//! 跨协议转换的**损失登记**：源请求里任何一样没进到目标请求里的东西，都必须在这里出现一次。
//!
//! 为什么要有这个类型而不是直接 `Result<(), String>`：
//! `docs/10 §1.2` 规定跨协议路径要**字段核对**，`docs/03 §6.3` 规定"无法映射的图片绝不文本化"。
//! 只让转换函数返回一个 `Value`，那"少了一个 `detail` 字段"和"少了一张图片"在类型上长得一模一样
//! —— 两者都会静默通过。把"每一处损失"变成**必须登记的返回值**之后，"我们丢了什么"才可核对、
//! 可测试、可给用户看（[docs/11 §6] 的同一条纪律：不猜不丢不冒充）。

/// 一处损失。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Loss {
    /// 图片块映射不到目标协议（例如 data URL 的 media type 目标协议不接受）。
    /// **绝不允许**用 `"[image]"` 文本顶替 —— 那会把用户"干成瞎子"（[docs/03 §6.3]）。
    UnmappableImage { path: String, detail: String },
    /// 源协议有、目标协议没有对应物的**字段**（调参类：`presence_penalty`、`seed`…）。
    UnsupportedField { path: String, field: String, detail: String },
    /// 源协议里的内容块类型我们根本不认识（新出的 part 类型）→ 内容会少一块。
    UnsupportedPart { path: String, kind: String },
    /// 消息角色映射不过去（例如 OpenAI 的 `developer`）。
    UnsupportedRole { path: String, role: String },
    /// 目标协议**必填**、源请求里没有（Anthropic Messages 的 `max_tokens`）→ 根本编码不出来。
    MissingRequired { path: String, detail: String },
    /// 编码后再解码回来的**往返核对**不一致：编码器丢了或改了东西。
    RoundTrip { path: String, detail: String },
    /// 跨协议**流式**转换还没实现（响应方向是 SSE 状态机），必须在拨号前拒绝。
    UnsupportedStreaming { detail: String },
}

impl Loss {
    /// JSON 路径，便于用户与测试精确定位。
    ///
    /// 坐标约定：解码阶段的损失用**源请求**的 JSON 路径（`/presence_penalty`）；
    /// 目标协议的硬约束检查跑在 IR 上，那里的 `/messages/{i}` 是 **IR 序号**
    /// （`system` 在 IR 里单独一份，不占 `messages` 的下标）。
    pub fn path(&self) -> &str {
        match self {
            Loss::UnmappableImage { path, .. }
            | Loss::UnsupportedField { path, .. }
            | Loss::UnsupportedPart { path, .. }
            | Loss::UnsupportedRole { path, .. }
            | Loss::MissingRequired { path, .. }
            | Loss::RoundTrip { path, .. } => path,
            Loss::UnsupportedStreaming { .. } => "/stream",
        }
    }

    /// 机器可读的短名（测试与日志用它，**不要**依赖 `Debug` 的输出）。
    pub fn kind(&self) -> &'static str {
        match self {
            Loss::UnmappableImage { .. } => "unmappable_image",
            Loss::UnsupportedField { .. } => "unsupported_field",
            Loss::UnsupportedPart { .. } => "unsupported_part",
            Loss::UnsupportedRole { .. } => "unsupported_role",
            Loss::MissingRequired { .. } => "missing_required",
            Loss::RoundTrip { .. } => "round_trip_mismatch",
            Loss::UnsupportedStreaming { .. } => "unsupported_streaming",
        }
    }

    /// 人话（给用户看的那一句）。
    pub fn detail(&self) -> String {
        match self {
            Loss::UnmappableImage { detail, .. } => detail.clone(),
            Loss::UnsupportedField { field, detail, .. } => format!("字段 `{field}`：{detail}"),
            Loss::UnsupportedPart { kind, .. } => format!("内容块类型 `{kind}` 无法表达"),
            Loss::UnsupportedRole { role, .. } => format!("角色 `{role}` 无法表达"),
            Loss::MissingRequired { detail, .. } => detail.clone(),
            Loss::RoundTrip { detail, .. } => detail.clone(),
            Loss::UnsupportedStreaming { detail } => detail.clone(),
        }
    }

    /// **致命**损失：不管 `passthrough.on_loss` 设成什么，都不能继续。
    ///
    /// 判据不是"严重程度"，而是**能不能编码出一次自洽的请求**：
    /// - 图片/未知内容块/不可映射角色 → 目标请求会**少内容**（用户被干成瞎子，[docs/03 §6.3]）
    /// - 必填字段缺失、跨协议流式 → 目标请求**根本编不出来**
    ///
    /// 其余（没有对应物的调参字段、往返核对不一致）在 `on_loss: warn` 下可以**记录后继续**：
    /// 用户显式选择了"我知道会丢一点"。默认是 `reject` —— 静默降级才是真问题。
    pub fn is_fatal(&self) -> bool {
        matches!(
            self,
            Loss::UnmappableImage { .. }
                | Loss::UnsupportedPart { .. }
                | Loss::UnsupportedRole { .. }
                | Loss::MissingRequired { .. }
                | Loss::UnsupportedStreaming { .. }
        )
    }
}

/// 逐条列成给用户看的一段话（`TA-2006` 的 message 就是它）。
///
/// 必须**逐条列出路径**：只说"转换有损失"用户没法行动；说清"哪个字段、为什么、怎么绕过"，
/// 用户自己就能决定是改请求、换通道，还是开 `passthrough.on_loss: warn`。
pub fn describe(losses: &[Loss]) -> String {
    if losses.is_empty() {
        return "跨协议转换出现损失".to_string();
    }
    let mut out = format!("跨协议转换会丢内容（{} 处），已拒绝转发：", losses.len());
    for (i, loss) in losses.iter().enumerate() {
        out.push_str(&format!(
            "\n  {}. [{}] {} —— {}",
            i + 1,
            loss.kind(),
            loss.path(),
            loss.detail()
        ));
    }
    out
}

/// 把一组损失里的致命项挑出来（`on_loss: warn` 也救不了的那些）。
pub fn fatal_losses(losses: &[Loss]) -> Vec<&Loss> {
    losses.iter().filter(|l| l.is_fatal()).collect()
}
