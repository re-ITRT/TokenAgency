# docs/59：边缘 N 种协议，内部 1 种（IR）—— 以及"只有直连能用"的真因

用户第 325 轮报了三件事（附截图）：

> 由软件代理出去的 url 和 key 只用在 agent 选 Anthropic 协议的时候扫描是正常的，选 OPENAI
> 会直接穿透拿到服务商的所有模型，另外，自动和网络代理只有用直连的情况下可用，其它情况根本
> 不可用，具体我已经给你看了，而且报错不一样，选 Anthropic 访问不了，只有添加完模型回去切换
> 成 OPENAI 的才能正常输入，当然是在软件开直连，我本地 clash 作为代理的情况下才能连接

用户第 326 轮又明确了架构取舍：

> N->1 1->N 不就好了，所有在内部都是一种，到外面就是多种，无论是访问还是接收

本文记录**根因**与**已修/未修**。它是 [docs/06](06-protocol-adaptation.md)（适配层设计）的
"真机落地"补充：[docs/06](06-protocol-adaptation.md) 描述的是**应该长什么样**，本文记录的是
**它当时根本没被接上线**，以及为此做的修复。

---

## 1. 三个根因

### ① 协议翻译层是**死代码**（用户："可能你哪里做了占位实际逻辑根本没写")

`crates/ta-gateway/src/adapt/` 里 IR、Anthropic 适配器、OpenAI 适配器、损失核算全都写好了，
单测也很全 —— 但 `translate_request` / `translate_response` **只在它们自己的单测里被调用**，
生产路径**一次都没有**。

网关一直走的是 `passthrough`：把客户端 body **原样**转发上游，只定点改写 `model`。于是：

> **客户端协议必须等于上游协议**，否则上游必然 400。

用户"把服务商切回 OPENAI 才能输入"，就是把它蒙对了 —— 他没有修任何东西，只是让两边协议碰巧一致。

### ② "只有直连能出网"= 拨号器端口表被**冻结**

每条 lane 在内核里对应一个 `mixed` listener（`127.0.0.1:3910x`），"走哪条线" = 用哪个本地代理端口。
端口是**运行期**分配的：每写一次配置都会 `allocate_ports` 出一份**新的** `PortMap`。

三份副本：

| 副本 | 谁更新 |
|---|---|
| 内核实况（mihomo listeners） | `ta-kernel/src/process.rs` `apply()` → `PUT /configs` |
| `GeneratedConfigSource.ports`（渲染 YAML） | `config_gen.rs` `set_ports` |
| 网关 `KernelDialer.ports` | **从来没人更新** |

`KernelDialer` 拿的是 `boot` 那一刻的 `Arc<PortMap>`（`src-tauri/src/gateway.rs` 装配一次，
全仓没有第二条重建路径）。而 `direct` 走 `is_direct_lane` 分支**绕过端口查询** —— 于是：

> 只有 `direct` 能出网；boot 之后新建的 lane（「自动」/ 订阅节点）查不到端口 → `TA-3002`。

**三个次因**：

1. `catalog.rs` 的服务商写路径（`save_validated`）**不调 `maybe_apply_kernel`** —— 新建的
   「自动」lane 只写进了"下次生成用"的配置源，正在跑的内核既不重生成也不 reload，
   于是它在内核里**连 listener 都没有**；
2. `ports.rs` 的"落库端口能否复用"用 `bind()` 探针判断，而那个端口**正被我们自己的 mihomo
   占着** ⇒ 永远判"被占" ⇒ 每次保存所有 lane 端口整体前移（`docs/46` 里那条
   `port=39104 → 39105` 的 WARN 就是它，文档当时把它归因于"复制数据目录"）；
3. `is_direct_lane` 只认 id 字符串 `direct`，而**默认配置**里那条直连 lane 叫 `lane-direct` ——
   它不被当作直连，要查端口表（功能上不算坏，因为内核仍给它一个 `proxy: DIRECT` 的 listener，
   但它是同一个 bug 的另一面）。

### ③ "OpenAI 穿透"不在扫描里

扫描（`model_import_from_provider`）对两种协议是**同一条腿**：都用**上游 key** 直打
**上游 `{base_url}/models`**，本来返回的就是上游的全部模型，且受 lane 控制。
`protocol` 在扫描链路里**一次都没被读**（`upstream.rs` / `probe.rs` 生产段 `grep protocol` 零命中）。

真正能复现"穿透"的是**取值路径**：只有恰好 `GET /v1/models` 是网关**本地作答**，
路径稍有偏差（客户端把 base_url 填成不带 `/v1` 的形态 → `GET /models`）就落进
`fallback(any(handle))` **原样透传上游** → 端回**服务商自己的完整模型清单**。

Anthropic 协议的客户端不发这个请求，所以"看起来正常" —— 差别不在协议分支，在**路径是否命中**。

---

## 2. 架构：N 个适配器，不是 N×N

用户第 326 轮的裁决与仓库既有的 `adapt` 设计**完全一致**：

```
        客户端协议                内部规范形态           上游协议
   ┌──────────────┐          ┌──────────────┐      ┌──────────────┐
   │ Anthropic    │──decode─▶│              │─encode──▶│ Anthropic  │
   │ OpenAI chat  │          │      IR      │          │ OpenAI chat│
   │ Responses    │          │ (IrRequest / │          │ Responses  │
   │ Gemini       │◀─encode──│  IrResponse) │◀─decode──│ Gemini     │
   └──────────────┘          └──────────────┘      └──────────────┘
        N 个解码器               1 种               N 个编码器
```

**中心是 1 种，边缘是 N 种** ⇒ 只需要 **N 个适配器**（每个协议一解码 + 一编码），
而不是 N×N 对转换器。加一个协议 = 加一个适配器，**不动已有代码**。

流式同理，只是中心多一层**事件流**：`adapt/stream.rs` 定义 `IrStreamEvent`
（`Start` / `TextDelta` / `ToolUseStart` / `ToolInputDelta` / `Stop` / `Other`），
`take_sse_events` 做**协议无关的切帧**（只写一次），每个协议各写
`decode_stream_event` / `encode_stream_event`。

---

## 3. 已修

| # | 修了什么 | 位置 |
|---|---|---|
| ③ | 不带 `/v1` 的 `GET /models` 也**本地作答**（不再透传上游） | `ingress.rs` `classify` + router |
| ②-2 | **端口表热更新**：新增 `SharedPorts`（内核 YAML 与拨号器读**同一个**对象）；client 缓存改按 `(lane, port)` 存，端口变了自动重建 | `dispatch.rs` / `gateway.rs` / `state.rs` |
| ②-3 | 端口**不再漂移**：落库端口就是我们的，不用 `bind()` 探针判"被占" | `ports.rs` |
| ②-4 | 服务商写路径补 `maybe_apply_kernel`（新 lane 真的会被内核吃下） | `catalog.rs` `save_validated` |
| ①-1 | **IR hub 接进 pipeline（请求方向）**：跨协议时 body 走 `客户端协议 → IR → 上游协议`，**上游路径也换成目标协议的端点**（`upstream_path_for`） | `ingress.rs` `build_prepared_request` |
| ①-2 | **IR hub 接进 pipeline（响应方向）**：非流式把上游响应翻回客户端协议；流式走新增的**会话层** `adapt/session.rs`（切帧 → IR → 目标协议，每请求一份状态） | `pipeline.rs` / `adapt/session.rs` |
| ①-5 | **Gemini 适配器**（`adapt/gemini.rs`）：`contents`/`parts`/`systemInstruction`/`generationConfig`/`functionDeclarations`/`toolConfig`，`candidates[0]` + `finishReason` + `usageMetadata`；**`stream` 在路径里**（靠 `stream_is_out_of_band()`） | `adapt/gemini.rs` |`r`n| ①-4 | **Responses 适配器**（`adapt/responses.rs`）：`input` 字符串/数组、`instructions`、**扁平**函数工具、`output` 数组、`input_tokens`/`output_tokens`、`response.completed` 收尾 —— 与 chat/completions 的形状差异都在这一个文件里 | `adapt/responses.rs` |`r`n| ①-3 | **下游 API 家族开关**（用户需求 2）：`gateway.downstream.{openai_chat, openai_responses, anthropic_messages}` 三个**独立**勾选框；关掉的端点**当场明确报错并指出是哪个开关** | `ta-core` 域 / `ingress.rs` `disabled_downstream` / 设置页 |

两条 `loss` 相关的边界（都是"不制造新失败"）：

* **只在两边都有适配器时才转**（`adapt::can_translate`）：`Responses` / `Gemini` 还没有适配器，
  硬转会得到 `CapabilityLoss` —— 而在此之前它们能原样透传。接手一个改动不该把原本能走的路变成 415。
* **流式跨协议暂时不转**：事件层有了，但"每请求一份状态"的**会话层**还没写。
  在它落地前保持原样透传 —— 不能把"上游会 400"换成"我们直接 415"，那只是把失败换个地方。

---

## 4. 还剩什么（诚实清单）

**用户报的三个问题 —— 全部收口；四种协议 —— 两两可转。** 下面是与"完整性"无关、
但诚实起见必须列出的东西（每条都写清了**为什么不修**，而不是"以后再说"）：

### 4.1 协议本身的表达能力差（**不可修**，只能如实留痕）

| 缺口 | 为什么这样处理 |
| --- | --- |
| Anthropic 的 `cache_creation_input_tokens` 跨到 OpenAI/Responses/Gemini 时没有对应格 | 那三家**只报缓存命中、不报写入**。编一个 0 就是"冒充上游说的话"。会话层在流式方向把这类损失写进日志（`EncodeCtx::note`），非流式方向的往返核对会报 `RoundTrip` |
| Anthropic 的 usage 里没有"推理 token"这一格 | 它的思考 token **已含在 `output_tokens`** 里，不单列。所以 `IrUsage.reasoning_tokens` 从 Anthropic 跨出去时是空的（**不编 0**）—— 有测试钉住"哪个协议本该有哪几格" |
| OpenAI 的 chat/completions 与 Responses **没有** `content_block_stop` 这种"块结束"事件 | IR 里因此**没有**"块结束"这个事件（造一个就是伪事件）。块的开关由目标适配器按 `EncodeCtx::enter_block` / `leave_block` 负责 |
| Gemini 的 `functionCall.args` 是**完整对象**，没有分片形态 | 往 Gemini **发**分片时发 0 帧 + 留痕（发半个 JSON 就是编一个上游不会发的形状）。**反过来不再丢**：`ToolUseStart.input` 现在带得动完整入参（第 330 轮修） |
| Gemini 的 part 没有序号 | 流式里的槽位只能是"这一帧里的第几个 `functionCall`"。上游通常把同一轮的调用放在一帧里；跨帧的两个调用会撞号 —— 协议里没有能区分它们的东西 |

### 4.2 有意的"不为不存在的场景写代码"

* **Gemini 永远不是客户端协议**（网关没有 `/v1beta/...` 入站路由），所以"往 Gemini 流式发
  工具入参分片"这条路**不可达** ⇒ 不建缓冲合并机制。
  这条不可达性由 `stream::tests::gemini_is_never_a_client_protocol` **用真实的
  `Endpoint::protocol()` 集合**钉住：哪天有人加了 `/v1beta` 路由，它会红，提醒先补那层收口。
* **`totalTokenCount` / `toolUsePromptTokenCount` 不进 `IrUsage`**：前者是可推导量
  （input + output + thoughts），后者是 `promptTokenCount` 的子集细分 —— 没有任何别的协议
  有对应格，搬它只会多出一个永远空的字段。

### 4.3 验证边界（**这一点比上面所有缺口都重要**）

**没有拿真实上游 key 端到端跑过。** 能证明的到"协议转换 + 应用不回归"为止：

* 转换正确性：`adapt` 层的单测（284 条）与跨协议会话层的端到端用例
  （含"Anthropic 流式客户端 → OpenAI 上游 → 客户端收到合法 Anthropic SSE"）；
* 应用不回归：真机 e2e（应用起停 / 配置 / 界面 / 内核）。

**没证明的**：真实中转站的鉴权、模型名、限流、以及它对转换后请求的实际接受度。
那需要用户提供真实 key —— 属于"环境给不了条件"，不是"没做"。

## 5. 验证（已修部分）

| 门 | 结果 |
|---|---|
| `cargo fmt --all -- --check` | 干净 |
| `cargo clippy --workspace --all-targets -- -D warnings` | 0 warning |
| `cargo test --workspace` | 全绿 |
| 真机 e2e | **PASS=30 SKIP=2 FAIL=0** |

**两条既有测试被改判**（它们原来断言的是 bug 本身）：

* `pipeline::prepare_uses_policy_selector_and_g1_rewrite` 与
  `runtime::assembled_pipeline_reaches_the_dialer` 原来断言
  `https://a.example/v1/messages` —— 那正是"把 Anthropic 形状的请求打给 OpenAI 端点"。
  改成断言转译后的 `…/v1/chat/completions`。
* 新增齿 `pipeline::cross_protocol_request_body_is_translated_not_passed_through`：
  **只钉路径不够** —— Anthropic 的顶层 `system` 必须被搬进 `messages[0]`，
  否则"路径对了、body 还是错形状"，上游照样 400。

`ta-kernel::ports::reallocates_when_persisted_port_is_taken` 也按新语义重写为
`keeps_the_persisted_port_even_when_something_is_listening_on_it`（端口漂移是 bug，不是特性）。

---

## 6. 与其它文档的关系

* [docs/06](06-protocol-adaptation.md)：适配层的**设计**；本文记录它**没被接线**这件事与修复进展。
* [docs/09 §5.2](09-architecture-design.md)：lane → 内核 listener 端口的模型（本文 ② 就是它落地时漏了一环）。
* [docs/46](46-bundled-kernel.md)：那条 `端口已被占用，重新分配` 的 WARN 的**真实成因**见本文 §1②。
* [docs/55](55-provider-owns-the-lane.md)：线路归属（`provider.lane`）不变。


---

## 7. 第 328 轮：把"协议形状"从会话层搬回适配器

§4 里几条已知缺口追下去，发现**根因是同一处结构问题**：会话层在自己的代码里写死了
"目标协议长什么样"：

```rust
// 旧代码（在 adapt/session.rs 里）
if self.to == Protocol::Anthropic { /* message_stop */ } else { /* data: [DONE] */ }
```

于是：加第五个协议要改**共享文件**（N 侧不能独立增长）；而 `[DONE]` 对 Responses/Gemini
都是错的，只是靠"它们永远不会是客户端协议"这条**不可达**兜着。

### 改了什么

| 改动 | 为什么 |
| --- | --- |
| `StreamFrame { Json, Literal }` | `[DONE]` **不是 JSON**。用 `Value` 表示它只能编一个 `"DONE"` 字符串 —— 把"协议要求的字面量"和"恰好长得像的 JSON"混为一谈 |
| `IrStreamEvent` 带 `Slot { Text(n), Tool(n) }` | **并行工具调用**：源协议与目标协议的 `index` 是**两套编号**（Anthropic 的 index 是内容块序号、文本与工具共用一个空间；OpenAI 的 `tool_calls[].index` 只是数组下标）。直接搬运必然串号 —— 旧实现把目标编号写死 `0`，两个并行调用的分片会挤进同一个块、参数互相污染 |
| `decode_stream_event -> Vec<IrStreamEvent>` | 真实上游**一帧含多件事**：Gemini 的收尾帧里"最后一块文字 + `finishReason` + `usageMetadata`"全挤在同一个 `candidates[0]`。旧签名逼适配器二选一 —— 那不是取舍，那是丢数据 |
| `EncodeCtx`（每请求一份） | 目标协议的块编号与**流式损失**都需要跨帧状态。适配器仍是全局单例（拿不到 `&mut self`），状态有明确归属（会话层持有，随请求生灭） |
| `begin_frames` / `close_stream` 进 `StreamAdapter` | 开头帧（Anthropic 的 `message_start` —— OpenAI 的流里**根本没有这一帧**）与收尾帧的形状**回到适配器**。会话层因此不认识任何协议 |
| `frame_identity` 可覆写 | Gemini 的身份在 `responseId` / `modelVersion` 里，不在顶层 `id` / `model`。不覆写的话，跨协议流式合成的 `message_start.model` 永远是 `null` |
| **流式损失通道**（`EncodeCtx::note`） | 请求/响应方向都有 `Vec<Loss>`，**唯独流式没有** ⇒ "Anthropic 的 `cache_creation_input_tokens` 跨到 OpenAI"这类事情**静默发生**，与"不猜不丢不冒充"直接冲突 |
| `take_sse_events` 改"游标 + 末尾一次压实" | 旧实现每个事件都 `buf.drain(..n)`；一次写入含 k 个事件时总搬运量是 **O(全长 × k)** = O(n²)。32MB 上限附近会变成可观测卡顿 |

### 结构修复：`adapt/shared.rs`

`part_kind` / `describe_image` / `head` / `check_tool_ids` / `check_images` / `u64_of` /
`f64_of` / `strings_of` 这些**与 OpenAI 毫无关系**的通用工具原来住在 `openai.rs` 里，
另外三个适配器都写着 `use crate::adapt::openai::{…}`。三个问题：

1. **依赖方向错了** —— 改一个取值辅助会牵动另外三个协议；
2. **发现成本** —— "检查工具 id 配对"这种通用约束，新写协议的人不可能想到它住在**兄弟协议**里，于是自己再写一遍（Gemini 那份就是），两份迟早分叉；
3. **命名说谎**。

现在住在 `shared.rs`。判据：**一个函数如果四个协议都会用到，它就不属于其中任何一个。**

**留在各自适配器里的**：`IMAGE_MEDIA_TYPES` —— 那是**协议事实**，由一个协议替另一个协议
声明就是错的（OpenAI/Anthropic 收 webp、Gemini 收 heic）。

### 边界：把"不可达"写成可执行的断言

会话层假定 **Gemini 永远不会是"客户端协议"**（网关没有 `/v1beta/...` 入站路由，
它只会作为**上游**出现）—— 所以"往 Gemini 流式发工具入参分片"这条路不可达，
**不为它建一套缓冲合并机制**（那是为不存在的场景写代码）。

这条不可达性现在由 `stream::tests::gemini_is_never_a_client_protocol` 钉住：
哪天有人加了 `/v1beta` 路由，那个测试会红，提醒他**先补那层收口**。


---

## 8. 第 329 轮：补最后两处缺口（解码侧损失通道 + 空 usage 归一）

第 328 轮把**编码**侧的损失通道补上了（`EncodeCtx::note`），但**解码**侧还没有：
"这一帧没有可搬的东西"（`Other`）与"这一帧里有**内容**被丢掉"是两件事 ——
前者是心跳/元信息，后者是**真的少了内容**。前者可以静默丢，后者不行。

于是推理正文在跨协议流式时会**安静地消失**：

* Anthropic 的 `thinking_delta` / `signature_delta` / `citations_delta`；
* Gemini 的 `thought: true` part 与认不出的 part。

（`adapt/gemini.rs` 的注释里原本就写着"事件层没有损失通道 ⇒ 这里注定丢掉" ——
那句话本身就是这条缺口的证据。）

### 改法：加法式的方法，不动既有签名

```rust
fn decode_losses(&self, _data: &Value) -> Vec<Loss> { Vec::new() }   // 默认空
```

**为什么不做成给 `decode_stream_event` 加一个 `&mut Vec<Loss>` 参数**：
那个方法的调用点有几十处（每个协议自己的测试都直接调它），
加参数会把改动扩散到**与被测行为无关**的测试上 —— 风险大于收益。
默认空实现 ⇒ 只有真的会丢内容的协议才覆写（本次只有 Anthropic 与 Gemini）。

`translate_stream_event` 里把它们并进 `ctx.notes()`，
会话层照旧在写日志时取走 —— 与编码侧共用**同一条出口**。

**边界（很重要）**：心跳 / `ping` / `content_block_stop` / `message_stop` /
候选级元信息（`safetyRatings` / `citationMetadata` / `groundingMetadata`）
**一律不记**。它们丢了不损失内容，记了就是**噪音**，
而噪音会把上面那两类真损失淹没 —— 留痕是为了**可行动**，不是为了好看。
这条由 `stream::tests::dropped_content_is_recorded_not_silently_lost` 的**反面断言**钉住。

### 顺带：空 usage 归一（`shared::non_empty_usage`）

`usage: {}` / `usageMetadata: {}` 这种"字段在、里面什么都没有"的载荷，
原本会被带成 `Some(IrUsage { 全 None })`，目标协议于是发一个**每个字段都缺席**的
"补用量"帧 —— 客户端得解析一个什么都没说的帧，而它的信息量**恰好为零**。

**这不影响计费口径**：账本读的是**上游原始字节**（`ta_core::metering::normalize_*`
直接吃原始报文），不读 IR 事件。清掉的只是"要不要多发一个空帧"。
`Some(0)` **不算空**："上游说 0"与"上游没说"是两回事（[docs/11 §6]）。

---

## 9. 第 330 轮：补完 usage 保真 + 工具入参丢失 + 表驱动收敛

### 9.1 两个**真数据丢失**（不是"没实现"）

| 丢失 | 为什么发生 | 修法 |
| --- | --- | --- |
| **非流式响应的 usage** | `IrResponse` 里**没有** usage 这一格。请求方向有 `IrRequest`、流式有 `IrUsage`，**唯独非流式响应没有** ⇒ 翻译过的响应交出空用量：OpenAI 客户端看到 `usage: null`、Anthropic 客户端连 `message.usage` 都没有 | `IrResponse` 加 `usage: Option<IrUsage>`；四个适配器的 `decode_response` 读、`encode_response` 写 |
| **Gemini 上游的工具入参** | `IrStreamEvent::ToolUseStart` 只有 id+name。Gemini 的 `functionCall.args` 是**完整对象**，解码时被 `IrPart::ToolUse { .. }` 的 `..` 扔掉 ⇒ 客户端拿到"名字对、参数空"的工具调用 | `ToolUseStart` 加 `input: Option<Value>`。**`None` = 入参走分片**（OpenAI/Anthropic/Responses），**`Some` = 一次给全**（Gemini） |

**这两个都是静默的**：HTTP 200、JSON 合法、客户端不报错，只是少了东西 ——
正是 [docs/11 §6] 那类"不报错但不对"的失败。所以修复都配了**会红的测试**
（`non_streaming_response_usage_survives_cross_protocol`、
`gemini_tool_arguments_survive_to_an_openai_client`）。

### 9.2 复用性：usage 的八份重复代码 → 一张表

四个协议原本各有一对 `ir_usage_from_X` / `X_usage`，**结构完全相同**，只差字段名
（OpenAI/Responses 还把 cached 放在嵌套对象里）。八份同构代码的代价：
加一格字段（本轮加的 `reasoning_tokens`）要改**四个文件的八个地方**，
漏掉任何一处都是**静默丢一个数**。

现在每个协议只声明一张 [`UsageShape`](crates/ta-gateway/src/adapt/shared.rs) 表：

```rust
pub(crate) const USAGE: UsageShape = UsageShape {
    input: &["prompt_tokens"],
    output: &["completion_tokens"],
    cache_read: &["prompt_tokens_details/cached_tokens"],
    cache_creation: &[],                                   // 空 = 协议没有这一格（显式声明）
    reasoning: &["completion_tokens_details/reasoning_tokens"],
};
```

读/写各一份通用实现。**空数组是"这个协议没有这一格"的显式声明，不是漏填** ——
它同时驱动"读永远拿不到、写永远不产出"。

配套测试 `every_protocol_usage_shape_round_trips_all_five_fields` 对四个协议各跑一遍
**全字段往返**，并断言"没有那一格的协议既读不到、也写不出"。这条测试当场抓到一处
真实不对称：Anthropic 的 usage 里**没有**"推理 token"这一格（它的思考 token 已含在
`output_tokens` 里）—— 那是协议事实，测试的期望因此按协议分别写。

### 9.3 稳定性：`IrUsage` 的字段漏写不再可能

`merge` / `is_empty` 原来是手写的逐字段 `if let`。加第五格时**漏一段**就会变成
"新字段永远合并不进来"的静默 bug。现在走**字段视图**：

```rust
pub fn fields(&self) -> [Option<u64>; IR_USAGE_FIELDS]        // 读
fn fields_mut(&mut self) -> [&mut Option<u64>; IR_USAGE_FIELDS]  // 写
```

`merge` 由两者 `zip` 驱动，**新增字段不需要改 `merge`**。

### 9.4 诚实清理

* `docs/59` §4 原来**重复且陈旧**（残留着"并行工具调用固定写 index 0"这类已经被修掉的描述）
  ⇒ 重写成"还剩什么"，并逐条写清**为什么不修**；
* `gemini.rs` / `session.rs` / `responses.rs` 的模块头里，已关掉的缺口全部改掉 ——
  陈旧注释比没有注释更坏（它会让人以为问题还在，或者以为已经修了）。


---

## 10. 第 331 轮：真机"日志一条都没有"的真因 + 计量从未接线

用户真机报告（附截图）：**成功发过一次请求**，日志页却一条都没有，
并且**感到明显延迟**。

### 10.1 日志为什么没记（根因，有真机证据）

查用户真实数据库（`%APPDATA%\TokenAgency\db.sqlite`）：

```sql
select * from store_setting;
-- ('trace.mode', '"off"', 1790106999793)
-- ('trace.retention', '"7d"', 1790107003252)
```

而配置里 `request_log.mode` 是 `meta`（默认值）。**两份真相源，从来没有同步过**：

* 配置侧：`request_log.{mode, retention, max_rows, max_size_mb}`；
* 存储侧：`store_setting` 的 `trace.mode` / `trace.retention` / `trace.max_rows` / `trace.max_size_bytes`。

全仓搜 `request_log` 在 `src-tauri` 里**零命中** —— 也就是说，**没有任何一处把配置喂给 store**。
而 `SqliteTraceStore::write` 在 `mode == Off` 时**直接 `return Ok(())`**：

```rust
let mode = self.mode().await?;
if !mode.enabled() {
    return Ok(());       // off：什么都不记
}
```

⇒ 用户改了日志设置看不出任何变化；一条古老的 `off` 让**所有**请求都不落库，
而界面上只会说"可能是 off 模式"，用户根本查不到原因。

**修法**：配置是唯一真相源。

1. 新增 `commands::log::sync_request_log_settings(&state, &doc)`；
2. **启动时**同步一次（`lib.rs` setup，服务对外之前）；
3. **每次 `config_apply`** 同步一次；
4. 日志页的即时开关 `log_set_mode` / `log_set_retention` **回写配置** ——
   两边再也不会分叉。

行为测试：`syncing_request_log_settings_recovers_from_a_stale_off_mode`
先**制造**那个坏状态（store 设成 off、配置是 meta），再断言同步治好它、并且**真的能写进去**。

### 10.2 计量器从来没接线（同一类错误，第三次）

`RuntimeParts::with_usage_meter` 早就写好了，它的文档写着"生产装配应当显式调这一句" ——
但 `src-tauri` **从来没调**。于是 `meter: None`：

* `usage_request` 表恒为空（真机查询确认：0 行）；
* 成本中心永远是 0；
* 用户截图里那张参考界面的"Token 与成本"在**我们这边没有数据可显示**。

这不是"少一个功能"：它是**静默失效** —— 界面上看不出原因。
`gateway.rs` 里那句"`meter` 保持 None：G4 在途"在本轮不再成立，已改成实接。

**这是同一个模式的第三次**（前两次：`with_api_keys` / `with_trace_store`），
所以这次也照那个模式加了**装配守卫**：生产装配段必须出现
`.with_usage_meter(` 与 `sync_request_log_settings`，少一句就红。

### 10.3 延迟：查过了，**不是我们重新缓冲**

用户问"是不是等流输出完再给本地"。代码与测试都确认**不是**：

* `watch_stream` 用 `.inspect()`（逐块观察），**不缓冲**；
* `meter.observe_stream` 是逐 chunk 直通的包装；
* 跨协议会话层 `translate_sse_stream` 用 `unfold`，**逐块转发**；
* 新增测试 `first_bytes_are_forwarded_before_the_upstream_stream_ends`：
  上游发完第一帧后**永不结束**（`pending()`），我们必须在 2 秒内吐出第一块 ——
  "攒完再发"的实现会**超时**。

**用户看到的延迟有另一个更可能的来源**：`request_log.mode` 被理解成"记 body"。
本轮确认语义是 —— `Meta`（默认）只记元数据，`Shape` 记结构，**只有 `Full` 才记 body**
（`mode.records_body()` 只在 Full 为真）。所以**不必**为了"不拖慢"而关掉日志。

> 仍待真机确认的部分：真实上游的 TTFB 本身、以及线路（Clash）的额外一跳。
> 我们这一层**没有**引入"攒完再发"。
