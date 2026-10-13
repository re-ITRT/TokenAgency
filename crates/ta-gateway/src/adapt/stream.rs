//! 跨协议**流式**（SSE）转换：把 N 种线协议的流事件收敛到一套与协议无关的事件（[`IrStreamEvent`]）。
//!
//! 与 [`super`]（整包请求/响应）同一条纪律：**每个协议各写一解码/一编码，公共部分只写一次**。
//! 整包路径的"公共部分"是 [`super::ir::IrRequest`] / [`super::translate_request`]，
//! 流式路径的是 [`IrStreamEvent`] / [`translate_stream_event`] —— 于是新增一个协议是
//! **+2 个函数**，而不是 +N 个组合。
//!
//! # 为什么不能像整包那样"解码 → 编码 → 核对"
//!
//! 整包可以"缓冲全量 → 解码 → 编码 → 往返核对 → 一次写出"；流式**没有第二次机会**：
//! 字节一旦推给客户端就收不回来（["流式请求一旦写出第一个 SSE chunk"]）。
//! 所以这一层的判据不是"往返核对"，而是**逐事件的语义搬运**：
//! 一个有内容的事件要么被搬成目标协议的等价事件，要么被**明确丢弃**
//! （[`translate_stream_event`] 返回 `None`），**绝不造一个"看起来像"的帧**把客户端骗过去
//! —— 编出来的帧比丢掉更难查，因为客户端会以为自己收到了上游的话。
//!
//! # 三处"只写一次"
//!
//! 1. **切帧** [`take_sse_events`] / [`sse_data_of`]：分帧规则与 `data:` 取值对四个协议完全一样，
//!    写第二遍只会得到第二套边界 bug（粘包、半包、多行 `data:`、注释行）。
//! 2. **事件形态** [`IrStreamEvent`]：只保留**所有协议都能表达**的东西，表达不了的一律 [`IrStreamEvent::Other`]。
//! 3. **翻译** [`translate_stream_event`]（解码 → 编码）与 [`is_same_protocol_stream`]（同协议**不重编码**）。
//!
//! # 有意的取舍
//!
//! * **同协议原样返回**：重编码会把协议里我们不认识的字段全部蒸发（整包路径"RAW 优先"同理）。
//! * **usage 是"片段"**：Anthropic 把 usage 分在 `message_start`（input 侧）与 `message_delta`
//!   （output 侧）两处，OpenAI 的 `stream_options.include_usage` 又把它单独放一帧 ——
//!   事件层把**每一处都原样带出来**（[`IrUsage`]，用 [`IrUsage::merge`] 合并），
//!   口径归一化（cached 是子集、5m/1h 细分）**不在这里做**，那是 `ta_core::metering` 的唯一职责。
//! * **不编元信息**：`id` / `model` / `created` 缺了就**不写这个键**，而不是编一个假 id 或假时间戳
//!   —— 假数据比缺字段更难查，而且会被下游当成上游的原话。
//! * **并行工具调用**：由 [`Slot`] 承担"这是哪一个块"这个维度。
//!   OpenAI 的 `tool_calls[].index` 只是**数组下标**，Anthropic 的 `index` 是**内容块序号**
//!   （文本与工具**共用**一个编号空间）—— 两者编号空间不同，直接搬运必然串号。
//!   所以 IR 带的是**语义身份**（[`Slot`]），目标协议的具体编号由 [`EncodeCtx`] 分配。
//! * **协议形状归适配器**：收尾帧（`message_stop` / `[DONE]` / `response.completed` / 什么都不发）
//!   与容器帧（Anthropic 的 `content_block_start`）都由**目标适配器**给
//!   （[`StreamAdapter::begin_frames`] / [`StreamAdapter::close_stream`]）。
//!   会话层因此**不认识任何协议** —— 加一个协议不用改它，这正是「边缘 N 种」该有的样子。

use serde_json::Value;
use ta_core::types::Protocol;

use super::ir::IrStopReason;
use super::loss::Loss;
use super::{anthropic, openai};

/// 内容块的**语义槽位** —— "这是哪一个块"的协议无关表达。
///
/// # 为什么不是裸 `u32`
///
/// 源协议的 `index` 与目标协议的 `index` 是**两套编号**：
///
/// * Anthropic 的 `index` 是**内容块序号**，文本块与工具块**共用一个空间**
///   （`content_block_start.content_block.type` 决定它是哪一种）；
/// * OpenAI 的 `tool_calls[].index` 只是**工具调用数组的下标**，文本增量根本没有编号；
/// * Responses 的 `output_index` 又是另一套（按 output item 编）。
///
/// 直接搬运编号一定串号：OpenAI → Anthropic 时，第一个工具的 `index: 0`
/// 会撞上文本块 `index: 0`。所以 IR 只带**语义身份**，
/// 目标协议的具体编号由 [`EncodeCtx::index_of`] 统一分配。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Slot {
    /// 第 n 段文本（同一段文本的所有增量用同一个槽位）
    Text(u32),
    /// 第 n 个工具调用
    Tool(u32),
}

/// 内部规范流的**事件**（与协议无关）。
///
/// 只有"所有协议都能表达"的东西才配进这个枚举：加一个只对某个协议有意义的变体，
/// 就等于把 N×N 的矩阵塞回了 IR 里（那正是这一层要消灭的东西）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IrStreamEvent {
    /// 流开始：模型名 + 可选报文 id + 可选的**输入侧** usage 片段。
    ///
    /// `usage` 挂在这里是**被逼的**，不是设计品味：Anthropic 的 `message_start` 是
    /// `input_tokens` / `cache_read_input_tokens` / `cache_creation_input_tokens` 出现的**唯一**位置
    /// （`message_delta` 只给 `output_tokens`）。事件层是"一帧进、若干事件出"，
    /// 不在这里带上它，流式请求的输入 token 就会**整份丢掉**。
    Start { model: String, id: Option<String>, usage: Option<IrUsage> },
    /// 文本增量。`slot` 说明它属于哪一段文本。
    TextDelta { slot: Slot, text: String },
    /// 工具调用开始（id + 名字 + **可能已经完整的入参**）。
    ///
    /// # 为什么 `input` 是可选的，而不是"开始"与"入参"严格分开
    ///
    /// 两个协议族对工具入参的**线上形态根本不同**，这不是风格差异：
    ///
    /// * **OpenAI chat / Responses / Anthropic**：增量流 —— 先给 id+name，再用若干个
    ///   `arguments` / `input_json_delta` 分片把 JSON 拼出来。所以 `input: None`，
    ///   内容跟着一串 [`IrStreamEvent::ToolInputDelta`]；
    /// * **Gemini**：`functionCall: {name, args}` 里 `args` 就是**一个完整对象**，
    ///   协议里没有"分片"这个形态。所以 `input: Some(args)`，后面**不会**再有分片。
    ///
    /// 第 330 轮之前没有这一格，于是 **Gemini 作上游时的工具入参被整份丢掉**
    /// （解码侧 `IrPart::ToolUse { id, name, .. }` 的 `..` 把它扔了）——
    /// 那是真数据丢失，不是"没实现"。现在它能一路带到目标协议；
    /// 目标协议若只认分片（OpenAI/Anthropic/Responses），编码侧会把它**序列化成一片**发出去。
    ToolUseStart { slot: Slot, id: String, name: String, input: Option<Value> },
    /// 工具入参的 JSON 片段。`slot` 就是**分片归属** —— 并行工具调用靠它区分。
    ToolInputDelta { slot: Slot, fragment: String },
    /// 结束（含 stop reason 与可选 usage）。
    ///
    /// `reason` 是 `Option` 也是**被逼的**：OpenAI 开 `stream_options.include_usage` 时，
    /// usage 走的是**单独一帧**（`choices: []`），那一帧里**没有** `finish_reason`。
    /// 要求这里必填，就只能替上游编一个 `end_turn` —— 而"上游没说为什么结束"
    /// 与"上游说正常结束"是两件事，编出来就是**冒充上游的话**。
    ///
    /// 因此 `reason: None` 的含义是"这是流结尾的**补充信息**（用量），不是一个终止判定"；
    /// 编码侧据此只发"补用量"的帧，不重复发终止帧（一次流只能有一个终止帧）。
    Stop { reason: Option<IrStopReason>, usage: Option<IrUsage> },
    /// 心跳 / 注释 / 认得出但 IR 表达不了的帧（`ping`、只有 `reasoning_content` 的增量…）。
    /// 调用方决定丢还是转发；[`translate_stream_event`] 对它一律丢弃。
    Other,
}

/// 要写给客户端的**一帧**。
///
/// # 为什么不是裸 `Value`
///
/// OpenAI 家族的收尾帧 `[DONE]` **不是 JSON** —— 它是字面量，`data: [DONE]`。
/// 用 `Value` 表示它就只能编一个 `"DONE"` 字符串，而那会把"协议要求的字面量"
/// 和"某个恰好长得像的 JSON"混为一谈。分开表达，编码侧就不必猜。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamFrame {
    /// `data: <json>`
    Json(Value),
    /// `data: <字面量>`（目前只有 OpenAI 家族的 `[DONE]`）
    Literal(&'static str),
}

impl StreamFrame {
    /// 这一帧的**载荷**（JSON 帧给对象，字面量帧给字符串）。测试与日志用它。
    pub fn payload(&self) -> &str {
        match self {
            StreamFrame::Literal(text) => text,
            // 只在测试/日志里用；真序列化走会话层的 `write_frame`
            StreamFrame::Json(_) => "[json]",
        }
    }
}

/// 规范化的**用量片段**。
///
/// 每个字段都是 `Option`：`None` = 上游**没说**，`Some(0)` = 上游**说了 0**。
/// 这两者混为一谈就是静默少记账（同一条口径），所以这里不做"缺省为 0"。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IrUsage {
    /// Anthropic 的 `input_tokens`（**不含**缓存读/写）；
    /// OpenAI 的 `prompt_tokens`（**含**缓存命中，是"输入总量"）。
    ///
    /// ★ 两者**口径不同**：本层只做字段搬运，不做换算 —— 换算会引入"我们凭猜调整过的数字"，
    /// 而账本要的是上游原话（`ta_core::metering` 才有权决定怎么归一化）。
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    /// Anthropic `cache_read_input_tokens` / OpenAI `prompt_tokens_details.cached_tokens`
    pub cache_read_input_tokens: Option<u64>,
    /// Anthropic `cache_creation_input_tokens`。
    /// **OpenAI 的 usage 形状里没有对应字段**（OpenAI 只报命中，不报写入）→ 跨到 OpenAI 时这一项会少，
    /// 计费口径必须由会话层拿原始帧兜底（见模块头"usage 是片段"）。
    pub cache_creation_input_tokens: Option<u64>,
    /// **推理（思考）token**：OpenAI `completion_tokens_details.reasoning_tokens` /
    /// Gemini `thoughtsTokenCount` / Responses `output_tokens_details.reasoning_tokens`。
    ///
    /// 它**已包含在 `output_tokens` 里**（不是另一份总量）—— 加它是为了让"思考花了多少"
    /// 能跨协议带过去。Anthropic 的 usage 形状里**没有**这一项 ⇒ 从它跨出去时这格会空着
    /// （**不编 0**）。
    ///
    /// Gemini 的 `totalTokenCount` 与 `toolUsePromptTokenCount` **不在这里**：
    /// 前者 = input + output + thoughts（能算，不必搬）；后者是 `promptTokenCount` 的
    /// 一个**子集细分**，没有任何其它协议有对应格 —— 搬它只会多出一个永远空的字段。
    pub reasoning_tokens: Option<u64>,
}

/// `IrUsage` 的字段个数。**加字段时必须同时改 `fields()` / `fields_mut()`** ——
/// 那两处漏掉一个，`merge` 就会**静默丢掉**那个字段（而 `is_empty` 会漏判"这个 usage 空不空"）。
pub(crate) const IR_USAGE_FIELDS: usize = 5;

impl IrUsage {
    /// 所有字段的读取视图（**加字段只改这里与 `fields_mut`**）。
    pub fn fields(&self) -> [Option<u64>; IR_USAGE_FIELDS] {
        [
            self.input_tokens,
            self.output_tokens,
            self.cache_read_input_tokens,
            self.cache_creation_input_tokens,
            self.reasoning_tokens,
        ]
    }

    /// 与 [`Self::fields`] **顺序一一对应**的可变视图。
    fn fields_mut(&mut self) -> [&mut Option<u64>; IR_USAGE_FIELDS] {
        [
            &mut self.input_tokens,
            &mut self.output_tokens,
            &mut self.cache_read_input_tokens,
            &mut self.cache_creation_input_tokens,
            &mut self.reasoning_tokens,
        ]
    }

    /// 把**后到**的片段并进来：只覆盖它自己带了的字段，没带的保持不动。
    ///
    /// 为什么是"覆盖"而不是"保留第一个值"：Anthropic 的 `message_start` 会给一个
    /// `output_tokens: 1` 的**占位值**，真值在 `message_delta`。取"先到为准"就会把 1 当成最终用量。
    /// 也不是"取大值"：上游修正成更小的数（例如重算）照样要听上游的。
    ///
    /// 实现走[字段视图](Self::fields)：**新增字段不需要改这个函数**
    /// （靠手写 5 段 `if let` 的话，漏一段就是"新字段永远合并不进来"的静默 bug）。
    pub fn merge(&mut self, other: &IrUsage) {
        for (slot, incoming) in self.fields_mut().into_iter().zip(other.fields()) {
            if incoming.is_some() {
                *slot = incoming;
            }
        }
    }

    /// 所有字段一个都没说（`Some(0)` **不算**空）。
    ///
    /// 它**不能**用来判断"上游给没给 usage"：空对象 `{}` 与根本没有 `usage` 字段都会得到 `true`，
    /// 而这两件事在账本上不一样。要判"给没给"，看 `Option<IrUsage>` 是不是 `None`。
    pub fn is_empty(&self) -> bool {
        self.fields().iter().all(Option::is_none)
    }
}

/// **每请求一份**的编码上下文。
///
/// # 为什么需要它（第 328 轮）
///
/// 适配器是**全局单例**（下面的 `static`），自己存状态会让并发请求串味。
/// 但流式编码确实需要两类**每请求**状态：
///
/// 1. **目标协议的块编号**：`Slot` 只是语义身份，"这是第几号块"要按目标协议的编号空间分配，
///    而且同一个槽位必须**稳定**（Anthropic 的 `content_block_delta` 与它前面的
///    `content_block_start` 必须写同一个 `index`）；
/// 2. **流式损失的登记**：请求方向与响应方向都有 `Vec<Loss>`，**唯独流式没有** ——
///    于是"Anthropic 的 `cache_creation_input_tokens` 跨到 OpenAI"这类事情是**静默发生**的，
///    与"不猜不丢不冒充"的纪律直接冲突。
///
/// 把这个结构按 `&mut` 传进编码函数，两个约束同时成立：适配器保持无状态
/// （静态单例、可并发），而状态有**明确归属**（会话层持有，随请求生灭）。
pub struct EncodeCtx {
    /// 语义槽位 → 目标协议里的块编号。同一个槽位永远得到同一个编号。
    indices: std::collections::HashMap<Slot, u32>,
    next_index: u32,
    /// 目标协议"当前打开的块"（Anthropic 需要先关旧块再开新块）。
    open: Option<Slot>,
    /// 已经写过收尾帧？一次流只能有一个终止帧。
    closed: bool,
    /// 目标协议的流已经"开过头"了吗？（Anthropic 的 `message_start` 只能有一次）
    stream_opened: bool,
    /// 流式方向的损失登记。
    notes: Vec<Loss>,
    /// 上游帧自带的身份（id / model）—— 目标协议合成缺帧时要用。
    upstream_id: Option<String>,
    upstream_model: Option<String>,
}

impl EncodeCtx {
    pub fn new() -> Self {
        Self {
            indices: std::collections::HashMap::new(),
            next_index: 0,
            open: None,
            closed: false,
            stream_opened: false,
            notes: Vec::new(),
            upstream_id: None,
            upstream_model: None,
        }
    }

    /// 这个槽位在**目标协议**里的编号。同一个槽位永远得到同一个编号。
    pub fn index_of(&mut self, slot: Slot) -> u32 {
        if let Some(index) = self.indices.get(&slot) {
            return *index;
        }
        let index = self.next_index;
        self.next_index += 1;
        self.indices.insert(slot, index);
        index
    }

    /// 目标协议用：切到某个块。返回 `(被关掉的前一个块, 这个块是不是新开的)`。
    ///
    /// Anthropic 的 `index` 空间是文本与工具**共用**的，所以"开新块"必须先"关旧块"
    /// （它没有嵌套块）。这个状态只能由每请求的 ctx 记住 —— 编码器是无状态的。
    pub fn enter_block(&mut self, slot: Slot) -> (Option<Slot>, bool) {
        if self.open == Some(slot) {
            return (None, false);
        }
        let previous = self.open.replace(slot);
        (previous, true)
    }

    /// 目标协议用：关掉当前块（返回它）。没有开着的块则返回 `None`。
    pub fn leave_block(&mut self) -> Option<Slot> {
        self.open.take()
    }

    /// 已经发过收尾帧了吗？`true` 时不要再发第二个。
    pub fn is_closed(&self) -> bool {
        self.closed
    }

    /// 标记收尾帧已发。
    pub fn mark_closed(&mut self) {
        self.closed = true;
    }

    /// 目标协议的流"开过头"了吗？（Anthropic 的 `message_start` 只能有一次）
    pub fn is_opened(&self) -> bool {
        self.stream_opened
    }

    /// 标记流已开头。
    pub fn mark_opened(&mut self) {
        self.stream_opened = true;
    }

    /// 登记一处**流式**损失。丢掉的东西都要在这里出现一次。
    pub fn note(&mut self, loss: Loss) {
        self.notes.push(loss);
    }

    /// 这一路流下来记下的所有损失（会话层负责写日志）。
    pub fn notes(&self) -> &[Loss] {
        &self.notes
    }

    /// 取走损失（会话层写完日志后调用，避免同一批被记两次）。
    pub fn take_notes(&mut self) -> Vec<Loss> {
        std::mem::take(&mut self.notes)
    }

    pub fn upstream_id(&self) -> Option<&str> {
        self.upstream_id.as_deref()
    }

    pub fn upstream_model(&self) -> Option<&str> {
        self.upstream_model.as_deref()
    }

    /// 记下上游的身份（只在第一次记到非空值时生效 —— 后面的帧多半没有 id/model）。
    pub fn remember_upstream(&mut self, id: Option<String>, model: Option<String>) {
        if self.upstream_id.is_none() {
            self.upstream_id = id;
        }
        if self.upstream_model.is_none() {
            self.upstream_model = model;
        }
    }
}

impl Default for EncodeCtx {
    fn default() -> Self {
        Self::new()
    }
}

/// 每个协议实现它（和 [`super::ProtocolAdapter`] 分开的 impl 块，见 `openai.rs` / `anthropic.rs`）。
///
/// # 无状态
///
/// **所有方法都拿不到 `&mut self`**：适配器是全局单例，任何跨事件的记忆都必须落在
/// [`EncodeCtx`]（每请求一份）里，否则并发请求之间会串味。
///
/// # 协议形状归这里，不归会话层
///
/// 收尾帧与容器帧是**协议形状**，所以由目标适配器给
/// （[`begin_frames`](StreamAdapter::begin_frames) / [`close_stream`](StreamAdapter::close_stream)）。
/// 会话层因此**不认识任何协议** —— 加一个协议不用改它。
pub trait StreamAdapter: Send + Sync {
    /// 一个 SSE `data:` 载荷（已 JSON 解析）→ **0..n** 个规范事件。
    ///
    /// 返回 `Vec` 而不是 `Option` 是必须的：真实上游常常**一帧含多件事**。
    /// 例：Gemini 的收尾帧里"最后一块文字 + `finishReason` + `usageMetadata`"
    /// 全挤在同一个 `candidates[0]` 里；OpenAI 的 `response.output_item.added`
    /// 也可能同时带内容与状态。旧签名"一帧只能出一个事件"逼适配器**二选一** ——
    /// 那不是取舍，那是丢数据。
    fn decode_stream_event(&self, data: &Value) -> Vec<IrStreamEvent>;

    /// 解码这一帧时**丢掉的内容**（第 328 轮补的通道）。
    ///
    /// # 为什么是独立方法，而不是给 `decode_stream_event` 加一个参数
    ///
    /// `decode_stream_event` 的调用点很多（每个协议自己的测试都直接调它，几十处）。
    /// 加参数会把改动扩散到那些**与被测行为无关**的测试上 —— 风险大于收益。
    /// 默认空实现 ⇒ 只有**真的会丢内容**的协议才覆写。
    ///
    /// # 什么时候该覆写
    ///
    /// 这一帧里有**内容**（不是心跳、不是纯元信息）被丢掉时。
    /// 例：Anthropic 的 `thinking_delta`（推理正文）、Gemini 的 `thought` part 与未知 part。
    ///
    /// **心跳 / `ping` / 纯元信息不要记**：它们丢了不损失内容，记了就是噪音 ——
    /// 噪音会把真损失淹没（：留痕是为了可行动，不是为了好看）。
    fn decode_losses(&self, _data: &Value) -> Vec<Loss> {
        Vec::new()
    }

    /// 一个规范事件 → 0..n 帧（要发几帧就返回几个）。
    ///
    /// 允许 0 帧：目标协议里没有对应物就**什么都不发**，而不是发一个假的。
    /// 允许 n 帧：Anthropic 的文本增量要"先开块、再发增量"两帧，收尾是
    /// `message_delta` + `message_stop` 两帧。
    fn encode_stream_event(&self, ev: &IrStreamEvent, ctx: &mut EncodeCtx) -> Vec<StreamFrame>;

    /// 从**任意一帧**里提取上游的身份（`id` / `model`）。
    ///
    /// 默认读顶层的 `id` / `model` —— OpenAI 家族就是这样。Gemini 放在
    /// `responseId` / `modelVersion` 里，必须覆写：不覆写，跨协议流式合成的
    /// `message_start.model` 会永远是 `null`（合成时拿不到模型名）。
    fn frame_identity(&self, data: &Value) -> (Option<String>, Option<String>) {
        (
            data.get("id").and_then(Value::as_str).map(str::to_string),
            data.get("model").and_then(Value::as_str).map(str::to_string),
        )
    }

    /// 写出**第一批真实帧之前**要先补的帧（惰性调用一次）。
    ///
    /// Anthropic 的流必须以 `message_start` 开头，而 OpenAI 的流里**根本没有这一帧** ——
    /// 所以这个"开头"只能由**目标适配器**在拿到上游身份后补出来。
    /// 默认不发任何东西。
    fn begin_frames(&self, _ctx: &mut EncodeCtx) -> Vec<StreamFrame> {
        Vec::new()
    }

    /// 上游流结束时**补齐目标协议缺的收尾帧**。
    ///
    /// 必须由适配器实现，因为收尾形状每个协议都不同，而且**有的协议什么都不发**：
    /// Anthropic 是 `content_block_stop`* + `message_stop`、OpenAI chat 是 `[DONE]`、
    /// Responses 是 `response.completed`、Gemini 什么都不发（它的流就是"最后一帧带
    /// `finishReason`"，没有任何额外标记）。
    ///
    /// 放在会话层就等于"会话层要知道每个协议的收尾形状" —— 那正是 N 模型要消灭的东西。
    fn close_stream(&self, _ctx: &mut EncodeCtx) -> Vec<StreamFrame> {
        Vec::new()
    }
}

static OPENAI_STREAM: openai::OpenaiAdapter = openai::OpenaiAdapter;
static ANTHROPIC_STREAM: anthropic::AnthropicAdapter = anthropic::AnthropicAdapter;
static RESPONSES_STREAM: super::responses::ResponsesAdapter = super::responses::ResponsesAdapter;
static GEMINI_STREAM: super::gemini::GeminiAdapter = super::gemini::GeminiAdapter;

/// 流式适配器。[`super::adapter_for`] 的流式孪生：**两边的可用性都要查**
/// （少了这个判断，缺适配器的组合会静默走空编码，客户端只会收到一个空流）。
pub fn stream_adapter_for(protocol: Protocol) -> Option<&'static dyn StreamAdapter> {
    match protocol {
        Protocol::Openai | Protocol::Custom => Some(&OPENAI_STREAM),
        Protocol::Anthropic => Some(&ANTHROPIC_STREAM),
        Protocol::OpenaiResponses => Some(&RESPONSES_STREAM),
        Protocol::Gemini => Some(&GEMINI_STREAM),
    }
}

/// 同协议：**原样返回，不重编码**（重编码会引入损失 —— 我们不认识的字段一个都活不下来）。
///
/// 只判"协议相同"，**不**把 `Custom` 折进 `Openai`：`Custom` 的线格式由配置决定，
/// 那是配置层的知识，不是这一层的；调用方若知道某条 `Custom` 通道就是 OpenAI 兼容，
/// 应当自己决定走不走直通。
pub fn is_same_protocol_stream(from: Protocol, to: Protocol) -> bool {
    from == to
}

/// 跨协议事件翻译：返回**要写给客户端**的帧（空 = 这一帧在目标协议里没有对应物）。
///
/// 同协议**短路**（原样返回同一个 `Value`），所以调用方即使不查 [`is_same_protocol_stream`]
/// 也不会把直通流量洗一遍。
///
/// `ctx` 是**每请求一份**的（块编号 + 损失登记），由会话层持有并逐帧传进来。
/// 顺带在这里把上游身份记进 `ctx` —— 目标适配器的 [`StreamAdapter::begin_frames`]
/// 要用它合成开头帧（例如 Anthropic 的 `message_start`）。
pub fn translate_stream_event(
    from: Protocol,
    to: Protocol,
    data: &Value,
    ctx: &mut EncodeCtx,
) -> Vec<StreamFrame> {
    if is_same_protocol_stream(from, to) {
        return vec![StreamFrame::Json(data.clone())];
    }
    let (Some(src), Some(dst)) = (stream_adapter_for(from), stream_adapter_for(to)) else {
        // 某一边没有适配器：什么都不发（**不造**一个看起来像的帧）
        return Vec::new();
    };
    let (id, model) = src.frame_identity(data);
    ctx.remember_upstream(id, model);

    // 解码侧丢掉的**内容**要留痕（`Other` 只说明"这一帧没有可搬的东西"，
    // 而"推理正文被丢掉"是**内容损失**，两者不能混为一谈）。
    for loss in src.decode_losses(data) {
        ctx.note(loss);
    }

    let mut out: Vec<StreamFrame> = Vec::new();
    for ev in src.decode_stream_event(data) {
        // `Other` 没有跨协议对应物：丢掉，而不是给目标协议编一个心跳/注释
        //（那会让客户端以为上游说了什么）。要不要转发它，由持有原始字节的会话层决定。
        if ev == IrStreamEvent::Other {
            continue;
        }
        out.extend(dst.encode_stream_event(&ev, ctx));
    }
    out
}

// ---------------------------------------------------------------- SSE 切帧（协议无关）

/// 从字节缓冲里切出完整的 SSE 事件（按空行分隔），返回事件列表。
///
/// 为什么参数是 `&mut Vec<u8>` 而不是 `&[u8]`：**半包必须留在缓冲里**。
/// 上游的 TCP 分片与 SSE 事件边界毫无关系，一个 JSON 事件常被切成两三个 chunk；
/// 只在"看到完整事件"时才吐出来，是这里唯一的正确做法（否则客户端会拿到半个 JSON）。
///
/// 空事件（只有空行的心跳）**不产生条目**：它没有任何字段可看，返回一个空串只会让调用方
/// 多写一个分支。
///
/// # 为什么是"游标 + 末尾一次压实"，而不是每切一个事件就 `drain`
///
/// 旧实现每个事件都 `buf.drain(..n)` —— 而 `drain` 要把**后面所有字节**往前挪。
/// 一次写入里含 k 个事件时，总搬运量是 O(全长 × k)，也就是 **O(n²)**：
/// 上游把整个响应一次性灌进来（或 32MB 上限附近）时，这一条会成为可观测的卡顿。
/// 现在只用一个 `start` 游标推进，最后**压实一次**：O(n)。
/// 半包语义完全不变（没切出完整事件的残余同样留在缓冲里）。
pub fn take_sse_events(buf: &mut Vec<u8>) -> Vec<String> {
    let mut events = Vec::new();
    let mut start = 0usize;
    // 注意每次都要从 `start` 之后重新扫描：`find_event_boundary` 需要的是"相对剩余缓冲"的
    // 下标与行长，所以传切片、并把返回值换算回绝对下标。
    while let Some((body_end, sep_len)) = find_event_boundary(&buf[start..]) {
        let body = &buf[start..start + body_end];
        // 事件体末尾那个行结束符属于**上一行**，不属于事件内容 → 切掉，事件就是干净的一串行
        let body = trim_one_line_ending(body);
        if !body.iter().all(u8::is_ascii_whitespace) {
            // 完整事件按定义是完整 UTF-8；真收到非法字节时用 lossy 而不是 panic
            // —— 网关不能因为上游的一个坏字节就把整条流断掉。
            events.push(String::from_utf8_lossy(body).into_owned());
        }
        start += body_end + sep_len;
    }
    // 压实一次：把已消费的前缀丢掉，未完成的残余（半包）留在缓冲里。
    if start > 0 {
        buf.drain(..start);
    }
    events
}

/// 扫描缓冲，返回第一个"空行"的位置：`(空行的起始下标, 这个空行本身的长度)`。
///
/// 行结束符三种都要认（`\n`、`\r\n`、`\r`），因为 SSE 规范允许它们混用 ——
/// 只认 `\n\n` 的实现会在 `\r\n\r\n` 的上游上"永不切帧"，表现是客户端**一个字节都收不到**。
fn find_event_boundary(buf: &[u8]) -> Option<(usize, usize)> {
    let mut line_start = 0usize;
    let mut i = 0usize;
    while i < buf.len() {
        let (line_end, sep_len) = match buf[i] {
            b'\n' => (i, 1),
            b'\r' if buf.get(i + 1) == Some(&b'\n') => (i, 2),
            b'\r' => (i, 1),
            _ => {
                i += 1;
                continue;
            }
        };
        if line_end == line_start {
            return Some((line_start, sep_len)); // 空行 = 事件边界
        }
        i = line_end + sep_len;
        line_start = i;
    }
    None
}

fn trim_one_line_ending(body: &[u8]) -> &[u8] {
    if let Some(stripped) = body.strip_suffix(b"\r\n") {
        stripped
    } else if let Some(stripped) = body.strip_suffix(b"\n") {
        stripped
    } else if let Some(stripped) = body.strip_suffix(b"\r") {
        stripped
    } else {
        body
    }
}

/// 一个事件里所有 `data:` 行拼起来（SSE 允许一行事件多个 data 行，用 `\n` 连接）；
/// 注释行/心跳返回 `None`。
///
/// `data: [DONE]` 会原样返回 `"[DONE]"`：**它不是 JSON**，怎么处理是上层的决定
/// （这是流式里唯一一个"不是 JSON 的 data"，所以它必须能被这一层原样送到上层）。
pub fn sse_data_of(event: &str) -> Option<String> {
    let mut out: Option<String> = None;
    for line in event.split(['\n', '\r']) {
        if line.is_empty() {
            continue; // CRLF 拆出来的空串，或事件里的空行
        }
        // `:` 开头是注释/心跳；`event:` / `id:` / `retry:` 不属于 data —— 都跳过
        let value = if let Some(rest) = line.strip_prefix("data:") {
            // 规范：`data:` 后紧跟一个空格时，那个空格属于分隔符，不属于值
            Some(rest.strip_prefix(' ').unwrap_or(rest))
        } else if line == "data" {
            // 规范：只有字段名、没有冒号 = 空值（**不是**"没有这个字段"）
            Some("")
        } else {
            None
        };
        if let Some(value) = value {
            match &mut out {
                // 多个 data 行按规范用 `\n` 连接
                Some(acc) => {
                    acc.push('\n');
                    acc.push_str(value);
                }
                None => out = Some(value.to_string()),
            }
        }
    }
    out
}

// ================================================================ 测试

#[cfg(test)]
mod tests {
    //! 判据与 `adapt::tests` 一致：一条测试只有在"把被测的那件事破坏掉之后会红"时才算证据。
    //! 所以下面每条都同时钉住"该搬过去的东西"和"不该出现的东西"（伪事件 / 伪帧）。

    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    fn openai_adapter() -> &'static dyn StreamAdapter {
        stream_adapter_for(Protocol::Openai).expect("必须有 OpenAI 流适配器")
    }

    fn anthropic_adapter() -> &'static dyn StreamAdapter {
        stream_adapter_for(Protocol::Anthropic).expect("必须有 Anthropic 流适配器")
    }

    /// 切帧：完整事件、粘包、半包、多行 `data:`、注释行、`[DONE]`。
    #[test]
    fn sse_framing_handles_complete_glued_and_partial_events() {
        // ① 一个完整事件（带 `event:` 字段行，它不参与 data 取值）
        let mut buf = b"event: message_start\ndata: {\"a\":1}\n\n".to_vec();
        assert_eq!(
            take_sse_events(&mut buf),
            vec!["event: message_start\ndata: {\"a\":1}"],
            "完整事件必须切出来，且不带尾部的空行"
        );
        assert!(buf.is_empty(), "切完不留残渣");
        assert_eq!(
            sse_data_of("event: message_start\ndata: {\"a\":1}").as_deref(),
            Some("{\"a\":1}")
        );
        // `\r\n` 结尾的上游：只认 `\n\n` 的实现会在这里永远切不出事件
        let mut buf = b"data: {\"a\":1}\r\n\r\n".to_vec();
        assert_eq!(take_sse_events(&mut buf), vec!["data: {\"a\":1}"]);

        // ② 粘包：两个事件挤在一次写入里，必须切成两条
        let mut buf = b"data: {\"i\":1}\n\ndata: {\"i\":2}\r\n\r\n".to_vec();
        assert_eq!(take_sse_events(&mut buf), vec!["data: {\"i\":1}", "data: {\"i\":2}"]);
        assert!(buf.is_empty());

        // ③ 半包：第二个事件只到一半 → 它必须**留在缓冲里**
        let mut buf = b"data: {\"i\":1}\n\ndata: {\"i\":".to_vec();
        assert_eq!(take_sse_events(&mut buf), vec!["data: {\"i\":1}"]);
        assert_eq!(buf, b"data: {\"i\":".to_vec(), "半个事件不许被当成完整事件发出去");
        // 补齐剩下的字节 → 它才成为第二个事件（钉住"缓冲真的接上了，而不是被丢掉"）
        buf.extend_from_slice(b"2}\n\n");
        assert_eq!(take_sse_events(&mut buf), vec!["data: {\"i\":2}"]);

        // ④ 一个事件里多个 `data:` 行：按规范用 `\n` 拼
        let mut buf = b"data: {\"a\":\ndata: 1}\n\n".to_vec();
        assert_eq!(take_sse_events(&mut buf), vec!["data: {\"a\":\ndata: 1}"]);
        assert_eq!(sse_data_of("data: {\"a\":\ndata: 1}").as_deref(), Some("{\"a\":\n1}"));
        assert_eq!(sse_data_of("data:x").as_deref(), Some("x"), "没有空格也算 data");

        // ⑤ 注释行 / 心跳：**没有 data** → None（调用方据此丢弃，而不是猜一个空 JSON）
        assert_eq!(sse_data_of(""), None);
        assert_eq!(sse_data_of(": keep-alive"), None);
        assert_eq!(sse_data_of("event: ping\n: 心跳"), None);
        // 心跳的空行不产生事件条目
        let mut buf = b"\n\ndata: x\n\n".to_vec();
        assert_eq!(take_sse_events(&mut buf), vec!["data: x"]);

        // ⑥ `[DONE]`：切帧与取 data 都要原样交出去（它不是 JSON，解析是上层的事）
        let mut buf = b"data: [DONE]\n\n".to_vec();
        assert_eq!(take_sse_events(&mut buf), vec!["data: [DONE]"]);
        assert_eq!(sse_data_of("data: [DONE]").as_deref(), Some("[DONE]"));
    }

    /// 一次写入里含**很多**事件 + 尾部半包：全部切出来，且只留真正的残余。
    ///
    /// 这条同时是"游标 + 末尾一次压实"（替代每事件 `drain`，把 O(n²) 拉回 O(n)）
    /// 的**语义等价**证据：半包留在缓冲里、已消费的前缀被丢掉、事件顺序不变。
    #[test]
    fn many_events_in_one_chunk_followed_by_a_half_packet() {
        const N: usize = 2000;
        let mut buf = Vec::new();
        for i in 0..N {
            buf.extend_from_slice(format!("data: {{\"i\":{i}}}\n\n").as_bytes());
        }
        buf.extend_from_slice(b"data: {\"i\":9999"); // 尾部半包（没有终止空行）

        let events = take_sse_events(&mut buf);
        assert_eq!(events.len(), N, "一个都不能少");
        assert_eq!(events[0], "data: {\"i\":0}");
        assert_eq!(events[N - 1], format!("data: {{\"i\":{}}}", N - 1), "顺序必须保持");
        // 残余**恰好**是那个半包 —— 已在旧实现里丢过一遍的东西
        assert_eq!(buf, b"data: {\"i\":9999".to_vec(), "半包必须原样留着");
        // 补齐之后它才成为最后一个事件
        buf.extend_from_slice(b"}\n\n");
        assert_eq!(take_sse_events(&mut buf), vec!["data: {\"i\":9999}"]);
        assert!(buf.is_empty(), "切完之后缓冲必须是空的");
    }
    // ---------------------------------------------------------------- 新接口的测试助手

    /// 跨协议翻译一次，返回**原始帧**（含 `Literal`）。
    fn tr(from: Protocol, to: Protocol, data: &Value) -> (Vec<StreamFrame>, EncodeCtx) {
        let mut ctx = EncodeCtx::new();
        let frames = translate_stream_event(from, to, data, &mut ctx);
        (frames, ctx)
    }

    /// 跨协议翻译一次，只取 **JSON** 帧（碰到 `Literal` 直接 panic —— 想断言 `[DONE]`
    /// 的测试请用 `tr` 拿原始帧）。
    fn js(from: Protocol, to: Protocol, data: &Value) -> Vec<Value> {
        tr(from, to, data)
            .0
            .into_iter()
            .map(|f| match f {
                StreamFrame::Json(v) => v,
                other => panic!("这一帧应当是 JSON：{other:?}"),
            })
            .collect()
    }

    /// 恰好解出 1 个事件（多于/少于一个都是被测行为的失败，直接 panic 好定位）。
    fn only(events: Vec<IrStreamEvent>) -> IrStreamEvent {
        assert_eq!(events.len(), 1, "这一帧应当恰好解出 1 个事件：{events:?}");
        events.into_iter().next().unwrap()
    }

    /// 文本增量：OpenAI → Anthropic → OpenAI，事件级往返必须落到同一个 IR 事件。
    #[test]
    fn text_delta_round_trips_between_openai_and_anthropic() {
        let mut ctx = EncodeCtx::new();
        // ① OpenAI 的**首帧**（`delta.role`）就是它的"流开始"标记 ——
        //    OpenAI 侧唯一能识别"流开头"的信号（`model` 每帧都重复、按它发会发出几十个 Start）。
        //    所以这一帧**确实有东西可搬**：翻成一帧 `message_start`，且身份来自上游帧。
        let first = json!({
            "id": "chatcmpl-1", "model": "gpt-4o",
            "choices": [{ "index": 0, "delta": { "role": "assistant" }, "finish_reason": null }],
        });
        let head = js_with(&mut ctx, Protocol::Openai, Protocol::Anthropic, &first);
        assert_eq!(head.len(), 1, "首帧应当翻出 message_start：{head:?}");
        assert_eq!(head[0]["type"], "message_start");
        assert_eq!(head[0]["message"]["model"], "gpt-4o", "上游身份要带过来，不编");
        assert_eq!(head[0]["message"]["id"], "chatcmpl-1");
        // 已经开过头 → `begin_frames` 不该补第二帧
        assert!(anthropic_adapter().begin_frames(&mut ctx).is_empty(), "开头帧只能有一次");

        // ② 文本增量 → 先开文本块、再发增量（两帧）
        let chunk = json!({
            "choices": [{ "index": 0, "delta": { "content": "你好" }, "finish_reason": null }],
        });
        let to_anthropic = js_with(&mut ctx, Protocol::Openai, Protocol::Anthropic, &chunk);
        assert_eq!(to_anthropic.len(), 2, "开块 + 增量：{to_anthropic:?}");
        assert_eq!(to_anthropic[0]["type"], "content_block_start");
        assert_eq!(to_anthropic[0]["content_block"]["type"], "text");
        assert_eq!(to_anthropic[1]["type"], "content_block_delta");
        assert_eq!(to_anthropic[1]["delta"]["text"], "你好");

        // ③ 反向：Anthropic 的 text_delta → OpenAI 的 `delta.content`
        let back = js(Protocol::Anthropic, Protocol::Openai, &to_anthropic[1]);
        assert_eq!(back.len(), 1);
        assert_eq!(back[0]["choices"][0]["index"], 0);
        assert_eq!(back[0]["choices"][0]["delta"]["content"], "你好");
        // 往返的判据落在**事件**上：目标协议的帧再解一次必须回到同一个事件
        assert_eq!(
            only(openai_adapter().decode_stream_event(&back[0])),
            IrStreamEvent::TextDelta { slot: Slot::Text(0), text: "你好".into() }
        );
    }

    /// 工具调用增量：两个方向都要能搬 id / name / 入参片段，且**分片归属**要带上。
    #[test]
    fn tool_call_deltas_round_trip_in_both_directions() {
        // ① Anthropic → OpenAI：调用开始
        let start = json!({
            "type": "content_block_start", "index": 0,
            "content_block": { "type": "tool_use", "id": "toolu_1", "name": "weather", "input": {} },
        });
        let to_openai = js(Protocol::Anthropic, Protocol::Openai, &start);
        let call = &to_openai[0]["choices"][0]["delta"]["tool_calls"][0];
        assert_eq!(call["id"], "toolu_1");
        assert_eq!(call["type"], "function");
        assert_eq!(call["function"]["name"], "weather");
        assert_eq!(call["index"], 0, "单个调用时分配到的编号就是 0");
        assert_eq!(
            only(openai_adapter().decode_stream_event(&to_openai[0])),
            IrStreamEvent::ToolUseStart {
                slot: Slot::Tool(0),
                id: "toolu_1".into(),
                name: "weather".into(),
                input: None
            }
        );

        // ② Anthropic → OpenAI：入参分片
        let fragment = json!({
            "type": "content_block_delta", "index": 0,
            "delta": { "type": "input_json_delta", "partial_json": "{\"city\":" },
        });
        let to_openai = js(Protocol::Anthropic, Protocol::Openai, &fragment);
        assert_eq!(
            to_openai[0]["choices"][0]["delta"]["tool_calls"][0]["function"]["arguments"],
            "{\"city\":"
        );

        // ③ OpenAI → Anthropic：首片带 id + name（arguments 为空）
        let head = json!({
            "choices": [{ "index": 0, "delta": { "tool_calls": [
                { "index": 0, "id": "call_1", "type": "function",
                  "function": { "name": "weather", "arguments": "" } }
            ]}, "finish_reason": null }],
        });
        let to_anthropic = js(Protocol::Openai, Protocol::Anthropic, &head);
        assert_eq!(to_anthropic[0]["type"], "content_block_start");
        assert_eq!(to_anthropic[0]["content_block"]["type"], "tool_use");
        assert_eq!(to_anthropic[0]["content_block"]["id"], "call_1");
        assert_eq!(to_anthropic[0]["content_block"]["name"], "weather");

        // ④ OpenAI → Anthropic：后续片只有 arguments → input_json_delta，且**不重复** name
        let tail = json!({
            "choices": [{ "index": 0, "delta": { "tool_calls": [
                { "index": 0, "function": { "arguments": "{\"city\":" } }
            ]}, "finish_reason": null }],
        });
        let to_anthropic = js(Protocol::Openai, Protocol::Anthropic, &tail);
        assert_eq!(to_anthropic[0]["type"], "content_block_delta");
        assert_eq!(to_anthropic[0]["delta"]["type"], "input_json_delta");
        assert_eq!(to_anthropic[0]["delta"]["partial_json"], "{\"city\":");
        assert_eq!(
            only(anthropic_adapter().decode_stream_event(&to_anthropic[0])),
            IrStreamEvent::ToolInputDelta { slot: Slot::Tool(0), fragment: "{\"city\":".into() }
        );
    }

    /// **并行工具调用**：源协议用 `index` 区分，目标协议的编号**另外分配**。
    ///
    /// 这是第 328 轮修掉的缺口：旧实现把目标编号写死 `index: 0`，
    /// 两个并行调用的分片会挤进同一个块、参数互相污染。
    #[test]
    fn parallel_tool_call_fragments_keep_their_own_slot_across_protocols() {
        let mut ctx = EncodeCtx::new();
        let head = |i: u64, id: &str, name: &str| {
            json!({ "choices": [{ "index": 0, "delta": { "tool_calls": [
                { "index": i, "id": id, "type": "function",
                  "function": { "name": name, "arguments": "" } }
            ]}, "finish_reason": null }] })
        };
        let frag = |i: u64, args: &str| {
            json!({ "choices": [{ "index": 0, "delta": { "tool_calls": [
                { "index": i, "function": { "arguments": args } }
            ]}, "finish_reason": null }] })
        };

        let a =
            js_with(&mut ctx, Protocol::Openai, Protocol::Anthropic, &head(0, "c_a", "weather"));
        let b = js_with(&mut ctx, Protocol::Openai, Protocol::Anthropic, &head(1, "c_b", "time"));
        let a2 = js_with(&mut ctx, Protocol::Openai, Protocol::Anthropic, &frag(0, "{\"c\":1}"));
        let b2 = js_with(&mut ctx, Protocol::Openai, Protocol::Anthropic, &frag(1, "{\"z\":2}"));

        // 取"块开始"那一帧：**切块时会先发一个 `content_block_stop`**（Anthropic 的
        // index 空间文本与工具共用，不能两个块同时开着），所以开始帧不一定在第 0 位。
        let start_of = |frames: &[Value]| -> Value {
            frames
                .iter()
                .find(|f| f["type"] == "content_block_start")
                .unwrap_or_else(|| panic!("找不到 content_block_start：{frames:?}"))
                .clone()
        };
        let (sa, sb) = (start_of(&a), start_of(&b));
        assert_eq!(sa["content_block"]["name"], "weather");
        assert_eq!(sb["content_block"]["name"], "time");
        assert_ne!(sa["index"], sb["index"], "两个调用必须落在不同编号：{sa} vs {sb}");
        // 切块时必须先关掉上一个块（Anthropic 不允许两个块同时开着）
        assert_eq!(b[0]["type"], "content_block_stop", "换块要先关旧块：{b:?}");
        assert_eq!(b[0]["index"], sa["index"], "关掉的正是上一个块：{b:?}");

        // 分片回到**自己**那个块 —— 这就是旧实现（写死 index 0）做不到的地方。
        // 注意分片帧不一定是第 0 位：切回旧块时要先关掉当时开着的那个块。
        let delta_of = |frames: &[Value]| -> Value {
            frames
                .iter()
                .find(|f| f["type"] == "content_block_delta")
                .unwrap_or_else(|| panic!("找不到 content_block_delta：{frames:?}"))
                .clone()
        };
        let (da, db) = (delta_of(&a2), delta_of(&b2));
        assert_eq!(da["index"], sa["index"], "call_a 的第二片必须回到自己的块：{a2:?}");
        assert_eq!(db["index"], sb["index"], "call_b 的第二片必须落在自己的块：{b2:?}");
        assert_eq!(da["delta"]["partial_json"], "{\"c\":1}");
        assert_eq!(db["delta"]["partial_json"], "{\"z\":2}");
        // 切回旧块同样要先关掉"当时开着"的块（这里开着的是 call_b）
        assert_eq!(a2[0]["type"], "content_block_stop", "{a2:?}");
        assert_eq!(a2[0]["index"], sb["index"], "关掉的应当是 call_b 的块：{a2:?}");

        // 反向：Anthropic 的两个块编号 → OpenAI 的 tool_calls[].index（同样是分开的）
        let mut ctx2 = EncodeCtx::new();
        let c0 = json!({ "type": "content_block_start", "index": 0,
            "content_block": { "type": "tool_use", "id": "t0", "name": "f0", "input": {} } });
        let c1 = json!({ "type": "content_block_start", "index": 1,
            "content_block": { "type": "tool_use", "id": "t1", "name": "f1", "input": {} } });
        let o0 = js_with(&mut ctx2, Protocol::Anthropic, Protocol::Openai, &c0);
        let o1 = js_with(&mut ctx2, Protocol::Anthropic, Protocol::Openai, &c1);
        assert_eq!(o0[0]["choices"][0]["delta"]["tool_calls"][0]["index"], 0);
        assert_eq!(o1[0]["choices"][0]["delta"]["tool_calls"][0]["index"], 1);
    }

    /// 带着调用方的 `ctx`（并行用例要跨帧共享它）。
    fn js_with(ctx: &mut EncodeCtx, from: Protocol, to: Protocol, data: &Value) -> Vec<Value> {
        translate_stream_event(from, to, data, ctx)
            .into_iter()
            .map(|f| match f {
                StreamFrame::Json(v) => v,
                other => panic!("这一帧应当是 JSON：{other:?}"),
            })
            .collect()
    }

    /// `EncodeCtx` 的编号分配：**同一槽位永远同一编号**，不同槽位不同编号。
    #[test]
    fn index_allocation_is_stable_and_distinct_per_slot() {
        let mut ctx = EncodeCtx::new();
        let a = ctx.index_of(Slot::Text(0));
        let b = ctx.index_of(Slot::Tool(0));
        assert_ne!(a, b, "文本块与工具块必须落在不同编号（Anthropic 的 index 空间是共用的）");
        assert_eq!(ctx.index_of(Slot::Text(0)), a, "同一槽位必须稳定 —— 分片要回到自己的块");
        assert_eq!(ctx.index_of(Slot::Tool(0)), b);
        assert_eq!(ctx.index_of(Slot::Tool(1)), 2);
    }

    /// usage：Anthropic 分两处、OpenAI 单独一帧 —— 一处都不许丢。
    #[test]
    fn usage_split_across_frames_is_never_lost() {
        // ★ Anthropic 的第一处：input 侧（含缓存）只在 message_start
        let start = json!({ "type": "message_start", "message": {
            "id": "msg_1", "model": "claude-x",
            "usage": { "input_tokens": 25, "cache_read_input_tokens": 100,
                       "cache_creation_input_tokens": 7, "output_tokens": 1 },
        }});
        let head = only(anthropic_adapter().decode_stream_event(&start));
        let (model, head_usage) = match &head {
            IrStreamEvent::Start { model, usage, .. } => {
                (model.clone(), usage.clone().expect("message_start 必须带上 input 侧 usage"))
            }
            other => panic!("message_start 应当解成 Start：{other:?}"),
        };
        assert_eq!(model, "claude-x");
        assert_eq!(head_usage.input_tokens, Some(25));
        assert_eq!(head_usage.cache_read_input_tokens, Some(100));
        assert_eq!(head_usage.cache_creation_input_tokens, Some(7));

        // ★ Anthropic 的第二处：output_tokens 只在 message_delta
        let delta = json!({ "type": "message_delta", "delta": { "stop_reason": "max_tokens" },
                            "usage": { "output_tokens": 15 } });
        let tail = only(anthropic_adapter().decode_stream_event(&delta));
        let (reason, tail_usage) = match &tail {
            IrStreamEvent::Stop { reason, usage } => {
                (reason.clone(), usage.clone().expect("message_delta 必须带上 output 侧 usage"))
            }
            other => panic!("message_delta 应当解成 Stop：{other:?}"),
        };
        assert_eq!(reason, Some(IrStopReason::MaxTokens));
        assert_eq!(tail_usage.output_tokens, Some(15));

        // 两处合并：一个数都不能少，且 output 必须被真值覆盖（`message_start` 里那个 1 是占位）
        let mut merged = head_usage;
        merged.merge(&tail_usage);
        assert_eq!(merged.input_tokens, Some(25));
        assert_eq!(merged.cache_read_input_tokens, Some(100));
        assert_eq!(merged.cache_creation_input_tokens, Some(7));
        assert_eq!(merged.output_tokens, Some(15), "占位的 1 必须被 message_delta 的真值覆盖");

        // OpenAI 的 `stream_options.include_usage`：usage 单独一帧，且这一帧没有 finish_reason
        let usage_only = json!({ "choices": [],
            "usage": { "prompt_tokens": 9, "completion_tokens": 3, "total_tokens": 12 } });
        assert_eq!(
            only(openai_adapter().decode_stream_event(&usage_only)),
            IrStreamEvent::Stop {
                reason: None,
                usage: Some(IrUsage {
                    input_tokens: Some(9),
                    output_tokens: Some(3),
                    ..Default::default()
                }),
            },
            "只有 usage 的末帧必须落成 Stop.usage（reason 不许编）"
        );
        // `usage: null` 是"上游说没有"，**不是**"上游说 0" ⇒ 搬不出事件。
        // 统一规则：**认得出是协议帧、但这一帧没有可搬的内容 → `Other`**
        //（`translate_stream_event` 会丢弃它，所以外向行为与"空"一致；
        //  `Vec::new()` 留给"根本不是协议帧"的情况，例如非对象载荷）。
        assert_eq!(
            only(openai_adapter().decode_stream_event(&json!({ "choices": [], "usage": null }))),
            IrStreamEvent::Other
        );
    }

    /// 停止原因：映射不过去的值**原样带走**，不硬塞成"正常结束"。
    #[test]
    fn stop_reasons_map_without_inventing_values() {
        // finish_reason → Anthropic 的收尾是**两帧**：message_delta + message_stop
        let finish = json!({
            "choices": [{ "index": 0, "delta": {}, "finish_reason": "tool_calls" }],
        });
        let frames = js(Protocol::Openai, Protocol::Anthropic, &finish);
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0]["type"], "message_delta");
        assert_eq!(frames[0]["delta"]["stop_reason"], "tool_use");
        assert_eq!(frames[1]["type"], "message_stop");

        // 反向：end_turn → stop；顺带带出的 usage 落成"choices 为空"的末帧（宁可多一帧也不丢数）
        let delta = json!({ "type": "message_delta", "delta": { "stop_reason": "end_turn" },
                            "usage": { "output_tokens": 5 } });
        let frames = js(Protocol::Anthropic, Protocol::Openai, &delta);
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0]["choices"][0]["finish_reason"], "stop");
        assert_eq!(frames[1]["choices"], json!([]));
        assert_eq!(frames[1]["usage"]["completion_tokens"], 5);

        // OpenAI 特有的 `content_filter` 在 Anthropic 里没有对应值 → 原样带着
        let filtered = json!({
            "choices": [{ "index": 0, "delta": {}, "finish_reason": "content_filter" }],
        });
        let frames = js(Protocol::Openai, Protocol::Anthropic, &filtered);
        assert_eq!(frames[0]["delta"]["stop_reason"], "content_filter");

        // 只带 usage 的 Stop（`reason: None`）**不补** message_stop：一次流只有一个终止帧
        let usage_only =
            json!({ "choices": [], "usage": { "prompt_tokens": 1, "completion_tokens": 2 } });
        let frames = js(Protocol::Openai, Protocol::Anthropic, &usage_only);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0]["type"], "message_delta");
        assert_eq!(frames[0]["usage"]["output_tokens"], 2);
    }

    /// 没有对应物的事件 → **一个帧都不产出**（不造伪事件），而不是编一个像样的帧。
    #[test]
    fn events_without_a_counterpart_are_dropped_not_faked() {
        for frame in [
            json!({ "type": "ping" }),
            json!({ "type": "content_block_stop", "index": 0 }),
            json!({ "type": "message_stop" }),
        ] {
            assert!(
                translate_stream_event(
                    Protocol::Anthropic,
                    Protocol::Openai,
                    &frame,
                    &mut EncodeCtx::new()
                )
                .is_empty(),
                "{frame} 在 OpenAI 里没有对应物，必须丢弃"
            );
        }
        // OpenAI 的空帧（`delta: {}`、没有 finish_reason）同样没有对应物
        let keepalive = json!({ "choices": [{ "index": 0, "delta": {}, "finish_reason": null }] });
        assert!(js(Protocol::Openai, Protocol::Anthropic, &keepalive).is_empty());

        // 目标协议**有**适配器时，这一帧应当真的产出（起 Gemini 也有了）
        let text = json!({ "choices": [{ "index": 0, "delta": { "content": "x" } }] });
        let to_gemini = js(Protocol::Openai, Protocol::Gemini, &text);
        assert!(
            to_gemini.iter().any(|f| f.to_string().contains("\"text\":\"x\"")),
            "应当产出带 text 的 Gemini 帧：{to_gemini:?}"
        );

        // `[DONE]` 不是 JSON 事件：交给上层按字面处理，这里连事件都不该产出
        assert!(openai_adapter().decode_stream_event(&json!("[DONE]")).is_empty());
        assert!(openai_adapter().decode_stream_event(&json!(null)).is_empty());
        assert!(anthropic_adapter().decode_stream_event(&json!({ "no_type": 1 })).is_empty());
    }

    /// 同协议：真值 + **逐字段原样返回**（不重编码 —— 重编码会把未知字段全部蒸发）。
    #[test]
    fn same_protocol_stream_is_not_re_encoded() {
        assert!(is_same_protocol_stream(Protocol::Openai, Protocol::Openai));
        assert!(is_same_protocol_stream(Protocol::Anthropic, Protocol::Anthropic));
        assert!(!is_same_protocol_stream(Protocol::Openai, Protocol::Anthropic));
        assert!(!is_same_protocol_stream(Protocol::Anthropic, Protocol::Openai));

        let chunk = json!({
            "id": "chatcmpl-1", "model": "gpt-4o", "created": 1, "usage": null,
            "choices": [{ "index": 0,
                          "delta": { "content": "hi", "x-upstream-only": 42 },
                          "finish_reason": null }],
            "x-upstream-envelope": { "keep": true },
        });
        let out = js(Protocol::Openai, Protocol::Openai, &chunk);
        assert_eq!(out, vec![chunk.clone()], "同协议必须原样返回");
        assert!(out[0].get("x-upstream-envelope").is_some(), "我们不认识的字段也要在");
    }

    /// **Gemini 永远不会是"客户端协议"** —— `session.rs` 的模块头依赖这条
    /// 不可达性（"往 Gemini 流式发工具入参分片"这条路不可达，所以不为它建缓冲合并机制）。
    ///
    /// 哪天有人加了 `/v1beta/...` 入站路由，`Endpoint` 会多一个变体、`protocol()` 会返回
    /// `Gemini`，这个数组就得跟着改 —— 于是这条测试会红，**提醒他补那层收口**。
    /// 这就是"把不可达性写成可执行的断言"，而不是一句注释。
    #[test]
    fn gemini_is_never_a_client_protocol() {
        let client_protocols: Vec<Protocol> = [
            crate::ingress::Endpoint::ChatCompletions,
            crate::ingress::Endpoint::Responses,
            crate::ingress::Endpoint::Messages,
        ]
        .into_iter()
        .map(|endpoint| endpoint.protocol())
        .collect();
        assert!(
            !client_protocols.contains(&Protocol::Gemini),
            "会话层假定 Gemini 只作上游；若这条红了，请先给 session.rs 补上\
             「只接受整份入参的目标协议」的缓冲收口：{client_protocols:?}"
        );
    }

    /// **解码侧的内容损失必须留痕**（第 328 轮补的通道）。
    ///
    /// "这一帧没有可搬的东西"（`Other`）与"这一帧里有**内容**被丢掉"是两件事：
    /// 前者是心跳/元信息（丢了不损失什么），后者是**真的少了内容**。
    /// 从前没有一个地方记后者 ⇒ 推理正文在跨协议时**静默消失**。
    #[test]
    fn dropped_content_is_recorded_not_silently_lost() {
        // Anthropic 的 thinking_delta：推理正文，IR 里没有位置
        let mut ctx = EncodeCtx::new();
        let _ = translate_stream_event(
            Protocol::Anthropic,
            Protocol::Openai,
            &json!({ "type": "content_block_delta", "index": 0,
                     "delta": { "type": "thinking_delta", "thinking": "我先想想…" } }),
            &mut ctx,
        );
        let notes = ctx.take_notes();
        assert!(
            notes.iter().any(|l| l.detail().contains("thinking_delta")),
            "推理正文被丢掉必须留痕：{notes:?}"
        );

        // Gemini 的 thought part：同一个 `text` 字段靠 `thought: true` 区分
        let mut ctx = EncodeCtx::new();
        let _ = translate_stream_event(
            Protocol::Gemini,
            Protocol::Openai,
            &json!({ "candidates": [{ "content": { "role": "model", "parts": [
                { "text": "推理…", "thought": true }, { "text": "正文" }
            ]}}]}),
            &mut ctx,
        );
        let notes = ctx.take_notes();
        assert!(
            notes.iter().any(|l| l.detail().contains("thought")),
            "思维链被丢掉必须留痕：{notes:?}"
        );

        // ★ 反面：**流程帧不记** —— 心跳/块结束丢了不损失内容，
        //   记了就是噪音，而噪音会把上面那两类的真损失淹没。
        let mut ctx = EncodeCtx::new();
        for frame in [
            json!({ "type": "ping" }),
            json!({ "type": "content_block_stop", "index": 0 }),
            json!({ "type": "message_stop" }),
            json!({ "candidates": [{ "content": { "role": "model", "parts": [] },
                                     "safetyRatings": [{ "category": "HARM" }] }] }),
        ] {
            let _ = translate_stream_event(Protocol::Anthropic, Protocol::Openai, &frame, &mut ctx);
            let _ = translate_stream_event(Protocol::Gemini, Protocol::Openai, &frame, &mut ctx);
        }
        assert!(
            ctx.take_notes().is_empty(),
            "心跳/块结束/候选级元信息不该记损失（噪音会淹没真损失）"
        );
    }
}
