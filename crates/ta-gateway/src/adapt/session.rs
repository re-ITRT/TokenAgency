//! **跨协议流式的会话层**—— 每请求一份状态，把上游 SSE 逐帧转成客户端协议。
//!
//! # 它负责什么（而且**只**负责这些）
//!
//! 1. **切帧**：TCP 分片与 SSE 事件边界无关，半包必须留在缓冲里（[`take_sse_events`]）。
//! 2. **字面量与 JSON 的分流**：`data: [DONE]` **不是 JSON**，也不是"一个事件" ——
//!    它只是"上游说完了"，翻译成任何东西都是编造；真正的收尾由目标适配器给。
//! 3. **每请求状态**（[`EncodeCtx`]）：块编号、损失登记、上游身份 —— 随请求生灭。
//! 4. **惰性补开头帧**：第一批真实帧写出**之前**问一次目标适配器
//!    （[`StreamAdapter::begin_frames`]），并在上游结束时问一次收尾
//!    （[`StreamAdapter::close_stream`]）。
//!
//! # 它**不**负责什么（这是第 328 轮的结构调整）
//!
//! **它不认识任何协议。** 收尾帧与容器帧（Anthropic 的 `message_start` /
//! `content_block_start`、OpenAI 的 `[DONE]`、Responses 的 `response.completed`、
//! Gemini 的"什么都不发"）全部由**目标适配器**给。
//!
//! 在此之前，会话层里写着 `if self.to == Protocol::Anthropic { … } else { [DONE] }` ——
//! 那意味着**加一个协议就要改这个共享文件**，而且 `[DONE]` 对 Gemini 是错的、
//! 只是靠"Gemini 永远不会是客户端协议"这条**不可达性**兜着。现在协议形状回到适配器，
//! 会话层只剩切帧与状态 —— 「边缘 N 种、内部 1 种」的 N 侧又可以独立增长了。
//!
//! # 一条边界声明（写成可执行的断言，而不是注释）
//!
//! * **只接受"整份入参"的目标协议**（Gemini 的 `functionCall.args` 没有分片形态）：
//!   `ToolInputDelta` 往那个方向发 0 帧 + 留痕。今天不会发生 —— 见下一条。
//! * **Gemini 永远不会是"客户端协议"**：`to` 来自 `Endpoint::protocol()`，而网关只注册了
//!   `/v1/chat/completions`、`/v1/responses`、`/v1/messages`（Gemini 形状走 wildcard
//!   **原样透传**，不经过这里）。所以"往 Gemini 流式发工具入参分片"这条路**不可达**：
//!   为它建一套缓冲合并机制就是**为不存在的场景写代码**。
//!   这条由 `stream::tests::gemini_is_never_a_client_protocol` 钉住 —— 哪天有人加了
//!   `/v1beta` 入站路由，那个测试会红，提醒他这里缺一层"整份入参"的收口。

use bytes::Bytes;
use serde_json::Value;
use ta_core::types::Protocol;

use super::loss::Loss;
use super::stream::{
    sse_data_of, stream_adapter_for, take_sse_events, translate_stream_event, EncodeCtx,
    StreamFrame,
};

/// 上游 SSE → 客户端 SSE 的逐帧翻译器。**一个请求一份**（内部有跨帧状态）。
pub struct SseTranslator {
    from: Protocol,
    to: Protocol,
    /// 未切出完整事件的残余字节（半包）。
    buf: Vec<u8>,
    /// 每请求的编码状态（块编号 / 损失 / 上游身份）。
    ctx: EncodeCtx,
    /// 目标协议的**开头帧**是否已经补过（惰性一次）。
    began: bool,
}

impl SseTranslator {
    pub fn new(from: Protocol, to: Protocol) -> Self {
        Self { from, to, buf: Vec::new(), ctx: EncodeCtx::new(), began: false }
    }

    /// 喂一段上游字节，返回**要原样写给客户端**的 SSE 字节（可能为空）。
    pub fn push(&mut self, chunk: &[u8]) -> Vec<u8> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        for event in take_sse_events(&mut self.buf) {
            let Some(data) = sse_data_of(&event) else {
                continue; // 注释行 / 心跳：没有载荷要搬
            };
            // 上游的结束标记：**不是 JSON**、也不是"一个事件"。
            // 翻译成任何东西都是编造；收尾交给 `finish()`（它会问目标适配器）。
            if data.trim() == "[DONE]" {
                continue;
            }
            let Ok(value) = serde_json::from_str::<Value>(&data) else {
                continue; // 上游发了非 JSON 的 data：丢掉，不猜
            };
            let frames = translate_stream_event(self.from, self.to, &value, &mut self.ctx);
            if frames.is_empty() {
                continue; // 这一帧没有可搬运的东西（`ping`、纯元信息…）：**不开流**
            }
            // 开头帧只在"确实要写东西"之前补 —— 不让一个孤立的心跳把流开起来
            self.flush_begin(&mut out);
            for frame in &frames {
                write_frame(&mut out, frame);
            }
        }
        out
    }

    /// 上游流结束。让**目标适配器**补齐它缺的收尾帧（已经发过就不补）。
    pub fn finish(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        // **同协议：一个字节都不补。** 同协议的语义是"逐字节透传"——往里面加
        // `message_stop` / `[DONE]` 就是**凭空造帧**（客户端会以为上游说了什么）。
        // `translate_sse_stream` 对同协议直接返回上游流，但 `SseTranslator` 是 `pub` 的，
        // 防住比注释强。
        if self.from == self.to {
            return out;
        }
        // 空流也要有合法的开头：Anthropic 客户端拿到"只有 message_stop 没有 message_start"
        // 的流会直接判非法。
        self.flush_begin(&mut out);
        if let Some(dst) = stream_adapter_for(self.to) {
            for frame in dst.close_stream(&mut self.ctx) {
                write_frame(&mut out, &frame);
            }
        }
        out
    }

    /// 取走这一路流记下的**损失**（会话层负责写日志 —— 翻译器本身不产生副作用）。
    pub fn take_notes(&mut self) -> Vec<Loss> {
        self.ctx.take_notes()
    }

    /// 惰性补一次目标协议的开头帧。
    ///
    /// **同协议时绝不补**：`from == to` 的语义是"逐字节透传"，
    /// 往里加一个 `message_start`/`[DONE]` 就是**凭空造帧** —— 客户端会以为上游说了什么。
    /// （`translate_sse_stream` 对同协议直接返回上游流，所以这条只在有人**直接**用
    /// `SseTranslator` 时才触发；但这个类型是 `pub` 的，防住比注释强。）
    fn flush_begin(&mut self, out: &mut Vec<u8>) {
        if self.began || self.from == self.to {
            return;
        }
        self.began = true;
        if let Some(dst) = stream_adapter_for(self.to) {
            for frame in dst.begin_frames(&mut self.ctx) {
                write_frame(out, &frame);
            }
        }
    }
}

/// 一帧写成 SSE 字节。JSON 帧与字面量帧走同一条出口 —— 出口只有一个，
/// 才不会有"某个分支忘了写 `\n\n`"这类错误。
fn write_frame(out: &mut Vec<u8>, frame: &StreamFrame) {
    out.extend_from_slice(b"data: ");
    match frame {
        StreamFrame::Json(value) => out.extend_from_slice(value.to_string().as_bytes()),
        StreamFrame::Literal(text) => out.extend_from_slice(text.as_bytes()),
    }
    out.extend_from_slice(b"\n\n");
}

/// 把一个上游字节流包成"翻译过"的字节流。
///
/// 只在**跨协议**时用；同协议直接原样返回上游流（重编码会引入损失，也白费 CPU）。
pub fn translate_sse_stream(
    from: Protocol,
    to: Protocol,
    inner: crate::dispatch::ByteStream,
) -> crate::dispatch::ByteStream {
    use futures_util::StreamExt;

    if from == to {
        return inner;
    }
    // 日志要带上 `request_id`：这一路是被 axum 在 `handle_request` 的 span **之外** poll 的，
    // 不显式带着 span，流式路径的告警就会丢掉 `request_id`。
    // 用 `in_scope`（同步闭包）而不是 `span.enter()` —— 后者跨 `.await` 会串 span。
    let span = tracing::Span::current();

    let inner = inner;
    let translator = SseTranslator::new(from, to);
    Box::pin(futures_util::stream::unfold(
        (inner, translator, Vec::<u8>::new(), false),
        move |(mut inner, mut tr, mut pending, mut done)| {
            let span = span.clone();
            async move {
                loop {
                    if !pending.is_empty() {
                        let chunk = Bytes::from(std::mem::take(&mut pending));
                        return Some((Ok(chunk), (inner, tr, pending, done)));
                    }
                    if done {
                        return None;
                    }
                    match inner.next().await {
                        Some(Ok(chunk)) => pending = tr.push(&chunk),
                        // 上游流断了：**原样把错误交给下游**（两分法）。
                        // 不能在这里补一个"看起来正常"的收尾 —— 那会把断流说成正常结束。
                        Some(Err(err)) => return Some((Err(err), (inner, tr, pending, true))),
                        None => {
                            pending = tr.finish();
                            done = true;
                        }
                    }
                    let notes = tr.take_notes();
                    if !notes.is_empty() {
                        span.in_scope(|| {
                            tracing::warn!(
                                "跨协议流式有损失（不拒绝，如实转发并留痕）：{}",
                                super::loss::describe(&notes)
                            );
                        });
                    }
                }
            }
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sse(value: &Value) -> Vec<u8> {
        let mut out = Vec::new();
        write_frame(&mut out, &StreamFrame::Json(value.clone()));
        out
    }

    fn text_of(bytes: &[u8]) -> String {
        String::from_utf8_lossy(bytes).into_owned()
    }

    /// 从 OpenAI 的一串帧拼出给 Anthropic 客户端的东西。
    fn openai_to_anthropic(chunks: &[Vec<u8>]) -> String {
        let mut tr = SseTranslator::new(Protocol::Openai, Protocol::Anthropic);
        let mut out = Vec::new();
        for c in chunks {
            out.extend_from_slice(&tr.push(c));
        }
        out.extend_from_slice(&tr.finish());
        text_of(&out)
    }

    #[test]
    fn first_text_delta_gets_a_content_block_start_synthesised() {
        // OpenAI 只发 `delta.content`，Anthropic 必须先有 `content_block_start`
        let out = openai_to_anthropic(&[
            sse(&json!({"id":"c1","model":"m","choices":[{"delta":{"content":"你"}}]})),
            sse(&json!({"choices":[{"delta":{"content":"好"}}]})),
        ]);
        let start = out.find("content_block_start").expect("必须补块开始");
        let first_delta = out.find("content_block_delta").expect("有增量");
        assert!(start < first_delta, "块开始必须在第一条增量之前：{out}");
        // **只补一次**：两条增量不该补两个块开始
        assert_eq!(out.matches("content_block_start").count(), 1, "{out}");
        assert!(out.contains("你") && out.contains("好"), "{out}");
    }

    /// **流必须以 `message_start` 开头**，而 OpenAI 的流里没有这一帧。
    #[test]
    fn anthropic_target_always_opens_with_message_start() {
        let out = openai_to_anthropic(&[sse(&json!({
            "id": "chatcmpl-1",
            "model": "gpt-4o",
            "choices": [{"delta": {"content": "hi"}}]
        }))]);
        let start = out.find("message_start").expect("必须有 message_start");
        let delta = out.find("content_block_delta").expect("有增量");
        assert!(start < delta, "message_start 必须最先：{out}");
        // 上游身份**不编**：用上游帧自带的 id / model
        assert!(out.contains("chatcmpl-1"), "应当带上上游的报文 id：{out}");
        assert!(out.contains("gpt-4o"), "应当带上上游的模型名：{out}");
        assert_eq!(out.matches("message_start").count(), 1, "只能有一个：{out}");
    }

    /// **空流也要有合法开头**（只有 `message_stop` 没有 `message_start` 的流是非法的）。
    #[test]
    fn empty_upstream_still_produces_a_well_formed_anthropic_stream() {
        let mut tr = SseTranslator::new(Protocol::Openai, Protocol::Anthropic);
        let out = text_of(&tr.finish());
        assert!(out.contains("message_start"), "{out}");
        assert!(out.contains("message_stop"), "{out}");
    }

    #[test]
    fn upstream_done_is_not_translated_but_still_terminates_the_stream() {
        let out = openai_to_anthropic(&[
            sse(&json!({"choices":[{"delta":{"content":"hi"},"finish_reason":"stop"}]})),
            b"data: [DONE]\n\n".to_vec(),
        ]);
        // `[DONE]` 本身不翻成 JSON，但 Anthropic 客户端必须看到收尾
        assert!(!out.contains("[DONE]"), "[DONE] 不该原样发给 Anthropic 客户端：{out}");
        assert!(out.contains("message_stop"), "必须有终止帧：{out}");
        // 一次流只有一个终止帧
        assert_eq!(out.matches("message_stop").count(), 1, "{out}");
        // 开过的文本块要关掉
        assert!(out.contains("content_block_stop"), "{out}");
    }

    #[test]
    fn finish_does_not_double_terminate_when_upstream_already_said_stop() {
        let mut tr = SseTranslator::new(Protocol::Anthropic, Protocol::Openai);
        let out = tr.push(&sse(&json!({
            "type":"message_delta",
            "delta":{"stop_reason":"end_turn"},
            "usage":{"output_tokens":3}
        })));
        let tail = tr.finish();
        let text = text_of(&out) + &text_of(&tail);
        assert_eq!(text.matches("[DONE]").count(), 1, "只能有一个 [DONE]：{text}");
    }

    #[test]
    fn half_packet_stays_in_the_buffer_until_complete() {
        let mut tr = SseTranslator::new(Protocol::Openai, Protocol::Anthropic);
        let full = sse(&json!({"choices":[{"delta":{"content":"hello"}}]}));
        let (head, tail) = full.split_at(12);
        assert!(tr.push(head).is_empty(), "半包不该产出任何字节");
        let out = tr.push(tail);
        assert!(text_of(&out).contains("hello"), "补齐后才产出：{}", text_of(&out));
    }

    #[test]
    fn non_json_data_and_comments_are_dropped_not_faked() {
        let mut tr = SseTranslator::new(Protocol::Anthropic, Protocol::Openai);
        assert!(tr.push(b": keep-alive\n\n").is_empty(), "注释行不该产出");
        assert!(tr.push(b"data: not-json\n\n").is_empty(), "非 JSON 不该产出");
    }

    #[test]
    fn unknown_events_are_dropped() {
        let mut tr = SseTranslator::new(Protocol::Anthropic, Protocol::Openai);
        // `ping` 在 IR 里是 Other → 丢弃（不造伪事件）
        assert!(tr.push(&sse(&json!({"type":"ping"}))).is_empty());
    }

    #[test]
    fn same_protocol_stream_is_passed_through_untouched() {
        let inner: crate::dispatch::ByteStream = Box::pin(futures_util::stream::iter(vec![Ok(
            Bytes::from_static(b"data: {\"a\":1}\n\n"),
        )]));
        // 同协议不该被重编码 —— 这里只验"返回的是同一个流"（内容一致由上层断言）
        let wrapped = translate_sse_stream(Protocol::Openai, Protocol::Openai, inner);
        let _ = wrapped;
    }

    /// **不许"等流输出完再给客户端"**（用户第 330 轮问的那件事）。
    ///
    /// 这是跨协议流式最容易被写坏的地方：一个 `collect` 就能把流式变成"攒完再吐"，
    /// 而**功能测试全都会过**（最终内容一模一样）—— 只有"延迟"这一个维度会坏。
    /// 所以它必须被单独钉住。
    ///
    /// 构造：上游发完第一帧后**永远不结束**（`pending()`）。
    /// * 正确实现 → 立刻能拿到第一块；
    /// * "攒完再发"的实现 → 这个测试**超时**（超时同样算红，不是"跳过"）。
    #[tokio::test]
    async fn first_bytes_are_forwarded_before_the_upstream_stream_ends() {
        use futures_util::StreamExt;

        let first =
            Bytes::from_static(b"data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n");
        let inner: crate::dispatch::ByteStream = Box::pin(
            futures_util::stream::once(async move { Ok(first) })
                .chain(futures_util::stream::pending()),
        );

        let mut out = translate_sse_stream(Protocol::Openai, Protocol::Anthropic, inner);
        let chunk = tokio::time::timeout(std::time::Duration::from_secs(2), out.next())
            .await
            .expect("超时 = 实现在等上游流结束（那正是用户感到的延迟）")
            .expect("上游发过一帧，这里必须已经能取到东西")
            .expect("不该出错");

        let text = String::from_utf8_lossy(&chunk);
        assert!(text.contains("hi"), "第一块就该带上游的内容：{text}");
    }

    /// **同协议时绝不补帧** —— 连"收尾"也不补。
    ///
    /// 同协议的语义是"逐字节透传"：上游的帧**原样**过去（包括认不出的 `ping`），
    /// 而我们**不加**任何自己的帧。往里面加 `message_start` / `[DONE]` 就是**凭空造帧** ——
    /// 客户端会以为上游说了什么。`translate_sse_stream` 对同协议直接返回上游流，
    /// 但 `SseTranslator` 是 `pub` 的 —— 防住比注释强。
    #[test]
    fn same_protocol_translator_passes_frames_through_and_invents_nothing() {
        let mut tr = SseTranslator::new(Protocol::Anthropic, Protocol::Anthropic);
        let out = text_of(&tr.push(&sse(&json!({"type": "ping"}))));
        assert!(out.contains("\"type\":\"ping\""), "同协议必须原样透传：{out}");
        assert!(tr.finish().is_empty(), "同协议不许补 message_stop");

        let mut tr = SseTranslator::new(Protocol::Openai, Protocol::Openai);
        let out = text_of(&tr.push(&sse(&json!({"choices": [{"delta": {"content": "x"}}]}))));
        assert!(out.contains("\"content\":\"x\""), "{out}");
        assert!(tr.finish().is_empty(), "同协议不许补 [DONE]");
    }

    /// **Gemini 上游的工具入参不许丢**（第 330 轮修的真数据丢失）。
    ///
    /// Gemini 的 `functionCall.args` 是**完整 JSON 对象**，而 OpenAI 客户端要的是
    /// `tool_calls[].function.arguments` 字符串。从前 IR 的"调用开始"里没有入参这一格，
    /// 于是整份 `args` **静默消失** —— 客户端拿到一个名字对、参数空的工具调用。
    #[test]
    fn gemini_tool_arguments_survive_to_an_openai_client() {
        let mut tr = SseTranslator::new(Protocol::Gemini, Protocol::Openai);
        let out = text_of(&tr.push(&sse(&json!({
            "candidates": [{ "content": { "role": "model", "parts": [
                { "functionCall": { "name": "weather", "args": { "city": "上海" } } }
            ]}, "index": 0 }],
            "responseId": "resp_1", "modelVersion": "gemini-2.5-pro",
        }))));

        assert!(out.contains("weather"), "工具名要带过去：{out}");
        // `arguments` 是**字符串化的 JSON**，所以引号在 SSE 里是转义的
        assert!(
            out.contains("city") && out.contains("上海"),
            "★ 入参必须带过去（从前这里整份丢掉）：{out}"
        );
    }

    /// 反向：OpenAI 的**分片**入参 → Anthropic 客户端也要保真（这一路是分片，不是一次给全）。
    #[test]
    fn openai_tool_argument_fragments_survive_to_an_anthropic_client() {
        let mut tr = SseTranslator::new(Protocol::Openai, Protocol::Anthropic);
        let mut out = Vec::new();
        out.extend_from_slice(&tr.push(&sse(&json!({"choices": [{"delta": {"tool_calls": [
            {"index": 0, "id": "call_1", "function": {"name": "weather", "arguments": ""}}
        ]}}]}))));
        out.extend_from_slice(&tr.push(&sse(&json!({"choices": [{"delta": {"tool_calls": [
            {"index": 0, "function": {"arguments": "{\"city\":\"上海\"}"}}
        ]}}]}))));
        let text = text_of(&out);

        assert!(text.contains("tool_use"), "{text}");
        assert!(text.contains("weather"), "{text}");
        assert!(text.contains("input_json_delta"), "分片要走 input_json_delta：{text}");
        assert!(text.contains("上海"), "分片内容要带过去：{text}");
    }

    /// **Responses 目标的收尾不是 `[DONE]`。**
    ///
    /// 上游什么都没说就断了时，必须补一个 Responses 的终止帧，且**不能**补 `[DONE]`；
    /// `status` 要如实写 `incomplete` —— 我们确实没等到完成，编一个 `completed` 就是撒谎。
    #[test]
    fn responses_target_is_terminated_with_response_completed_not_done() {
        let mut tr = SseTranslator::new(Protocol::Anthropic, Protocol::OpenaiResponses);
        let tail = text_of(&tr.finish());
        assert!(tail.contains("response.completed"), "缺 Responses 的终止帧：{tail}");
        assert!(!tail.contains("[DONE]"), "Responses 的流不该出现 [DONE]：{tail}");
        assert!(
            tail.contains("\"status\":\"incomplete\""),
            "没等到完成就要如实说 incomplete，不能编 completed：{tail}"
        );
    }

    /// 上游**已经**给过终止语义 → 收尾一个字节都不补（一次流一个终止帧）。
    #[test]
    fn responses_target_does_not_double_terminate() {
        let mut tr = SseTranslator::new(Protocol::Anthropic, Protocol::OpenaiResponses);
        // 先给一个真实的 Anthropic 内容帧，让流真的开起来
        let _ = tr.push(&sse(&json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": {"type": "text_delta", "text": "hi"}
        })));
        let _ = tr.push(&sse(&json!({
            "type": "message_delta",
            "delta": {"stop_reason": "end_turn"}
        })));
        let tail = text_of(&tr.finish());
        assert!(!tail.contains("response.completed"), "上游已经给了终止语义，不该再补一个：{tail}");
    }

    /// **并行工具调用**：两个工具的分片必须落在**不同的块编号**上。
    ///
    /// OpenAI 用 `tool_calls[].index` 区分，Anthropic 用 content block `index` ——
    /// 旧实现把两者都写死 `index: 0`，两个调用会挤进同一个块、参数互相污染。
    #[test]
    fn parallel_tool_calls_get_distinct_block_indices() {
        let chunks = vec![
            sse(&json!({"choices":[{"delta":{"tool_calls":[
                {"index":0,"id":"call_a","function":{"name":"weather","arguments":"{\"c\":"}}
            ]}}]})),
            sse(&json!({"choices":[{"delta":{"tool_calls":[
                {"index":1,"id":"call_b","function":{"name":"time","arguments":"{\"z\":"}}
            ]}}]})),
            sse(&json!({"choices":[{"delta":{"tool_calls":[
                {"index":0,"function":{"arguments":"\"bj\"}"}}
            ]}}]})),
        ];
        let out = openai_to_anthropic(&chunks);

        // 两个 tool_use 块开始，编号不同
        let starts: Vec<&str> = out
            .lines()
            .filter(|l| l.contains("content_block_start") && l.contains("tool_use"))
            .collect();
        assert_eq!(starts.len(), 2, "两个工具调用应当各有自己的块开始：{out}");
        assert!(starts[0].contains("\"index\":0"), "{}", starts[0]);
        assert!(starts[1].contains("\"index\":1"), "{}", starts[1]);

        // `call_a` 的后半片必须回到 index 0，`call_b` 的落在 index 1
        let a_tail = out
            .lines()
            .find(|l| l.contains("bj") && l.contains("input_json_delta"))
            .expect("call_a 的第二片");
        assert!(a_tail.contains("\"index\":0"), "分片要回到它自己的块：{a_tail}");
        let b_first = out
            .lines()
            .find(|l| l.contains("input_json_delta") && l.contains("z"))
            .expect("call_b 的第一片");
        assert!(b_first.contains("\"index\":1"), "分片要落在它自己的块：{b_first}");
    }

    /// 文本块与工具块交替时，Anthropic 要求"先关旧块再开新块"（index 空间共用）。
    #[test]
    fn switching_from_text_to_tool_closes_the_text_block_first() {
        let out = openai_to_anthropic(&[
            sse(&json!({"choices":[{"delta":{"content":"先说一句"}}]})),
            sse(&json!({"choices":[{"delta":{"tool_calls":[
                {"index":0,"id":"call_a","function":{"name":"f","arguments":"{}"}}
            ]}}]})),
        ]);
        let text_start = out.find("\"type\":\"content_block_start\"").unwrap();
        let stop = out.find("content_block_stop").expect("关掉文本块");
        let tool_start = out.find("tool_use").expect("开工具块");
        assert!(
            text_start < stop && stop < tool_start,
            "顺序必须是 开文本块 → 关文本块 → 开工具块：{out}"
        );
    }
}
